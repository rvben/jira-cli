//! Project-scoped metadata and conservative normalization for issue writes.

use super::{ApiError, JiraClient, validate_issue_key};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

type Fields = BTreeMap<String, Value>;
const EPIC_LINK_SCHEMA: &str = "com.pyxis.greenhopper.jira:gh-epic-link";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IssueType {
    #[serde(default)]
    id: String,
    name: String,
    #[serde(default)]
    subtask: bool,
    hierarchy_level: Option<i64>,
    fields: Option<Fields>,
}

impl IssueType {
    fn is_epic(&self) -> bool {
        self.hierarchy_level == Some(1) || self.name.eq_ignore_ascii_case("Epic")
    }

    fn can_belong_to_epic(&self) -> bool {
        !self.subtask
            && self.hierarchy_level.is_none_or(|level| level == 0)
            && !self.is_epic()
            && !matches!(
                self.name.to_ascii_lowercase().as_str(),
                "subtask" | "sub-task"
            )
    }
}

pub(super) struct CreateMetadata {
    issue_type: IssueType,
    fields: Option<Fields>,
}

impl JiraClient {
    // Unsupported endpoints are optional. Authentication, rate limits, network
    // failures and server errors must not be mistaken for absent metadata.
    async fn optional_metadata(&self, path: &str) -> Result<Option<Value>, ApiError> {
        match self.get(path).await {
            Ok(value) => Ok(Some(value)),
            Err(ApiError::NotFound(_))
            | Err(ApiError::Api {
                status: 405 | 410 | 501,
                ..
            }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Cloud calls its arrays `issueTypes`/`fields`; DC uses `values`.
    async fn metadata_pages(
        &self,
        path: &str,
        array: &str,
    ) -> Result<Option<Vec<Value>>, ApiError> {
        let mut values = Vec::new();
        let mut start = 0;
        loop {
            let Some(page) = self
                .optional_metadata(&format!("{path}?startAt={start}&maxResults=100"))
                .await?
            else {
                if start == 0 {
                    return Ok(None);
                }
                return Err(ApiError::Other(
                    "Jira metadata pagination became unavailable".into(),
                ));
            };
            let items = page
                .get(array)
                .or_else(|| page.get("values"))
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ApiError::Other("Invalid Jira metadata page: missing items".into())
                })?;
            let next = start + items.len();
            if page["startAt"]
                .as_u64()
                .is_some_and(|n| n as usize != start)
            {
                return Err(ApiError::Other(
                    "Jira metadata pagination did not advance".into(),
                ));
            }
            values.extend(items.iter().cloned());
            let total = page["total"].as_u64().map(|n| n as usize);
            if page["isLast"] == true || total.is_some_and(|n| next >= n) {
                return Ok(Some(values));
            }
            if items.is_empty() {
                return Err(ApiError::Other(
                    "Jira metadata pagination did not advance".into(),
                ));
            }
            if total.is_none() && page["isLast"].as_bool().is_none() {
                return Err(ApiError::Other(
                    "Invalid Jira metadata page: missing pagination".into(),
                ));
            }
            start = next;
        }
    }

    async fn legacy_create_metadata(
        &self,
        project: &str,
    ) -> Result<Option<Vec<IssueType>>, ApiError> {
        let parameter = if project.bytes().all(|b| b.is_ascii_digit()) {
            "projectIds"
        } else {
            "projectKeys"
        };
        let project_query: String =
            reqwest::Url::parse_with_params("http://localhost", &[(parameter, project)])
                .expect("static URL")
                .query()
                .unwrap()
                .into();
        let Some(meta) = self
            .optional_metadata(&format!(
                "issue/createmeta?{project_query}&expand=projects.issuetypes.fields"
            ))
            .await?
        else {
            return Ok(None);
        };
        let projects = meta["projects"].as_array().ok_or_else(|| {
            ApiError::Other("Invalid Jira create metadata: missing projects".into())
        })?;
        let types = projects
            .iter()
            .find(|p| {
                p["key"]
                    .as_str()
                    .is_some_and(|key| key.eq_ignore_ascii_case(project))
                    || p["id"].as_str() == Some(project)
            })
            .map(|p| p["issuetypes"].clone())
            .unwrap_or(json!([]));
        decode(types).map(Some)
    }

    pub(super) async fn create_metadata(
        &self,
        project: &str,
        input: &str,
    ) -> Result<Option<CreateMetadata>, ApiError> {
        let path = format!("issue/createmeta/{}/issuetypes", encode_segment(project));
        let types: Vec<IssueType> = match self.metadata_pages(&path, "issueTypes").await? {
            Some(types) => decode(json!(types))?,
            None => match self.legacy_create_metadata(project).await? {
                Some(types) => types,
                None => return Ok(None),
            },
        };
        let options = types
            .iter()
            .map(|t| json!({"id": t.id, "name": t.name}))
            .collect::<Vec<_>>();
        let selected = match_option("issue type", input, &options, false)?;
        let issue_type = types[selected].clone();
        let fields = if let Some(fields) = &issue_type.fields {
            Some(fields.clone())
        } else {
            match self
                .metadata_pages(
                    &format!("{path}/{}", encode_segment(&issue_type.id)),
                    "fields",
                )
                .await?
            {
                Some(fields) => Some(
                    fields
                        .into_iter()
                        .map(|field| {
                            let key = field["fieldId"]
                                .as_str()
                                .or_else(|| field["key"].as_str())
                                .ok_or_else(|| {
                                    ApiError::Other(
                                        "Invalid Jira field metadata: missing field ID".into(),
                                    )
                                })?;
                            Ok((key.to_owned(), field))
                        })
                        .collect::<Result<_, ApiError>>()?,
                ),
                None => self
                    .legacy_create_metadata(project)
                    .await?
                    .and_then(|types| types.into_iter().find(|t| t.id == issue_type.id))
                    .and_then(|t| t.fields),
            }
        };
        Ok(Some(CreateMetadata { issue_type, fields }))
    }

    pub(super) async fn issue_type_for_link(&self, key: &str) -> Result<IssueType, ApiError> {
        validate_issue_key(key)?;
        let issue: Value = self.get(&format!("issue/{key}?fields=issuetype")).await?;
        decode(issue["fields"]["issuetype"].clone())
    }

    async fn epic_field(&self, fields: Option<&Fields>) -> Result<String, ApiError> {
        if let Some(fields) = fields {
            // Cloud has retired Epic Link. DC can expose parent for subtasks
            // alongside Epic Link, so prefer the custom field there.
            if self.api_version >= 3 && fields.contains_key("parent") {
                return Ok("parent".into());
            }
            if let Some(id) = find_epic_field(fields)? {
                return Ok(id);
            }
            if fields.contains_key("parent") {
                return Ok("parent".into());
            }
        } else if self.api_version >= 3 {
            return Ok("parent".into());
        } else {
            let fields = self
                .list_fields()
                .await?
                .into_iter()
                .map(|field| {
                    (
                        field.id.clone(),
                        serde_json::to_value(field).expect("field serializes"),
                    )
                })
                .collect();
            if let Some(id) = find_epic_field(&fields)? {
                return Ok(id);
            }
        }
        Err(ApiError::InvalidInput(
            "Cannot resolve epic linkage: neither Epic Link nor an epic parent is available in this issue's metadata. Enable Epic Link on the create/edit screen or inspect `jira fields list` and use --field <ID>=<EPIC>.".into()
        ))
    }

    async fn set_epic(
        &self,
        fields: &mut Value,
        meta: Option<&Fields>,
        issue_type: &IssueType,
        epic: &str,
    ) -> Result<(), ApiError> {
        if !issue_type.can_belong_to_epic() {
            return Err(ApiError::InvalidInput(format!(
                "Issue type {:?} cannot be added to an Epic; use --epic {epic} with a standard issue type such as Story or Task. For a subtask, use --parent <STORY-OR-TASK>.",
                issue_type.name
            )));
        }
        let field = self.epic_field(meta).await?;
        let conflict = meta
            .and_then(|meta| {
                meta.iter()
                    .find(|(id, definition)| {
                        is_epic_field(id, definition) && fields.get(*id).is_some()
                    })
                    .map(|(id, _)| id.as_str())
            })
            .or_else(|| fields.get("parent").map(|_| "parent"))
            .or_else(|| fields.get(&field).map(|_| field.as_str()));
        if let Some(conflict) = conflict {
            return Err(ApiError::InvalidInput(format!(
                "Epic linkage conflicts with --field {conflict}; specify the relationship only once"
            )));
        }
        fields[&field] = if field == "parent" {
            json!({"key": epic})
        } else {
            json!(epic)
        };
        Ok(())
    }

    pub(super) async fn prepare_create(
        &self,
        fields: &mut Value,
        draft: &super::IssueDraft<'_>,
        custom: &[(String, Value)],
    ) -> Result<Option<CreateMetadata>, ApiError> {
        if draft.parent.is_some() && draft.epic.is_some() {
            return Err(ApiError::InvalidInput(
                "--parent and --epic cannot be used together".into(),
            ));
        }
        let input = fields["issuetype"]["id"]
            .as_str()
            .or_else(|| fields["issuetype"]["name"].as_str())
            .ok_or_else(|| ApiError::InvalidInput("issuetype requires a name or ID".into()))?
            .to_owned();
        let project = fields["project"]["key"]
            .as_str()
            .or_else(|| fields["project"]["id"].as_str())
            .ok_or_else(|| ApiError::InvalidInput("project requires a key or ID".into()))?;
        let meta = self.create_metadata(project, &input).await?;
        let fallback = IssueType {
            id: String::new(),
            name: input,
            subtask: false,
            hierarchy_level: None,
            fields: None,
        };
        let issue_type = meta.as_ref().map(|m| &m.issue_type).unwrap_or(&fallback);
        if meta.is_some() {
            fields["issuetype"] = named_id(&issue_type.id, &issue_type.name);
        }
        let field_meta = meta.as_ref().and_then(|m| m.fields.as_ref());
        if let Some(priority) = draft
            .priority
            .filter(|_| !custom.iter().any(|(key, _)| key == "priority"))
        {
            normalize_priority(fields, field_meta, priority)?;
        }
        if let Some(key) = draft.epic.or(draft.parent) {
            let target = self.issue_type_for_link(key).await?;
            if draft.epic.is_some() && !target.is_epic() {
                return Err(ApiError::InvalidInput(format!(
                    "--epic {key} targets issue type {:?}, not an Epic",
                    target.name
                )));
            }
            if target.is_epic() {
                // The ordinary parent is added by this method only after we
                // know the target type, so an explicit --field stays detectable.
                self.set_epic(fields, field_meta, issue_type, key).await?;
            } else if fields.get("parent").is_none() {
                fields["parent"] = json!({"key": key});
            }
        }
        Ok(meta)
    }

    pub(super) async fn prepare_update(
        &self,
        key: &str,
        fields: &mut Value,
        update: &super::IssueUpdate<'_>,
        custom: &[(String, Value)],
    ) -> Result<Option<Fields>, ApiError> {
        let priority = update
            .priority
            .filter(|_| !custom.iter().any(|(key, _)| key == "priority"));
        if priority.is_none() && update.epic.is_none() {
            return Ok(None);
        }
        let meta = self
            .optional_metadata(&format!("issue/{key}/editmeta"))
            .await?
            .map(|meta| decode::<Fields>(meta["fields"].clone()))
            .transpose()?;
        if let Some(priority) = priority {
            normalize_priority(fields, meta.as_ref(), priority)?;
        }
        if let Some(epic) = update.epic {
            if key.eq_ignore_ascii_case(epic) {
                return Err(ApiError::InvalidInput(
                    "An issue cannot be its own epic".into(),
                ));
            }
            let target = self.issue_type_for_link(epic).await?;
            if !target.is_epic() {
                return Err(ApiError::InvalidInput(format!(
                    "--epic {epic} targets issue type {:?}, not an Epic",
                    target.name
                )));
            }
            let issue_type = self.issue_type_for_link(key).await?;
            self.set_epic(fields, meta.as_ref(), &issue_type, epic)
                .await?;
        }
        Ok(meta)
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ApiError> {
    serde_json::from_value(value)
        .map_err(|err| ApiError::Other(format!("Invalid Jira metadata: {err}")))
}

fn encode_segment(value: &str) -> String {
    let mut url = reqwest::Url::parse("http://localhost").expect("static URL");
    url.path_segments_mut()
        .expect("URL supports paths")
        .push(value);
    url.path().trim_start_matches('/').into()
}

fn named_id(id: &str, name: &str) -> Value {
    if id.is_empty() {
        json!({"name": name})
    } else {
        json!({"id": id})
    }
}

pub(super) fn unresolved_option(input: &str) -> Value {
    let input = input.trim();
    if !input.is_empty() && input.bytes().all(|b| b.is_ascii_digit()) {
        json!({"id": input})
    } else {
        json!({"name": input})
    }
}

fn find_epic_field(fields: &Fields) -> Result<Option<String>, ApiError> {
    let schema_matches = fields
        .iter()
        .filter(|(_, f)| f["schema"]["custom"] == EPIC_LINK_SCHEMA)
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    let matches = if schema_matches.is_empty() {
        fields
            .iter()
            .filter(|(id, f)| is_epic_field(id, f))
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
    } else {
        schema_matches
    };
    match matches.as_slice() {
        [] => Ok(None),
        [id] => Ok(Some((*id).clone())),
        _ => Err(ApiError::InvalidInput(format!(
            "Ambiguous Epic Link fields: {}; use --field <ID>=<EPIC> to choose one",
            matches.into_iter().cloned().collect::<Vec<_>>().join(", ")
        ))),
    }
}

fn is_epic_field(id: &str, field: &Value) -> bool {
    field["schema"]["custom"] == EPIC_LINK_SCHEMA
        || (id.starts_with("customfield_")
            && field["name"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case("Epic Link")))
}

fn normalize_priority(
    fields: &mut Value,
    meta: Option<&Fields>,
    input: &str,
) -> Result<(), ApiError> {
    let Some(meta) = meta else {
        return Ok(());
    };
    let priority = meta.get("priority").ok_or_else(|| ApiError::InvalidInput(
        "Priority is not available on this issue's create/edit screen; omit --priority to keep the project default or ask an administrator to enable it".into()
    ))?;
    if let Some(options) = priority["allowedValues"].as_array() {
        let selected = &options[match_option("priority", input, options, true)?];
        fields["priority"] = named_id(
            selected["id"].as_str().unwrap_or(""),
            selected["name"].as_str().unwrap_or(input),
        );
    }
    Ok(())
}

/// Prefer an exact name/ID, then case-insensitive names, then a unique label
/// with a numeric rank removed, then prefixes. Never guess between ties.
fn match_option(
    field: &str,
    input: &str,
    options: &[Value],
    prefixes: bool,
) -> Result<usize, ApiError> {
    let input = input.trim();
    let lower = input.to_lowercase();
    for tier in 0..4 {
        let candidates = options
            .iter()
            .enumerate()
            .filter(|(_, option)| {
                let name = option["name"].as_str().unwrap_or("");
                !input.is_empty()
                    && match tier {
                        0 => name == input || option["id"].as_str() == Some(input),
                        1 => name.to_lowercase() == lower,
                        2 => prefixes && priority_label(name).to_lowercase() == lower,
                        _ => {
                            prefixes
                                && (name.to_lowercase().starts_with(&lower)
                                    || priority_label(name).to_lowercase().starts_with(&lower))
                        }
                    }
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if let [index] = candidates.as_slice() {
            return Ok(*index);
        }
        if candidates.len() > 1 {
            return Err(option_error(field, input, options, true));
        }
    }
    Err(option_error(field, input, options, false))
}

fn priority_label(name: &str) -> &str {
    let name = name.trim();
    let rest = name.trim_start_matches(|c: char| c.is_ascii_digit());
    if rest.len() != name.len() {
        let rest = rest.trim_start();
        if let Some(label) = rest.strip_prefix(['-', '\u{2013}', '\u{2014}', ':', '.']) {
            return label.trim();
        }
    }
    name
}

fn option_error(field: &str, input: &str, options: &[Value], ambiguous: bool) -> ApiError {
    ApiError::InvalidInput(format!(
        "{} {field} {input:?}; valid: {}",
        if ambiguous { "Ambiguous" } else { "Invalid" },
        option_names(options)
    ))
}

fn option_names(options: &[Value]) -> String {
    if options.is_empty() {
        return "(none available)".into();
    }
    options
        .iter()
        .map(|option| {
            let name = option["name"].as_str().unwrap_or("(unnamed)");
            match option["id"].as_str() {
                Some(id) => format!("{name:?} (ID {id})"),
                None => format!("{name:?}"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn create_error(
    err: ApiError,
    meta: Option<&CreateMetadata>,
    draft: &super::IssueDraft<'_>,
) -> ApiError {
    write_error(
        err,
        meta.and_then(|m| m.fields.as_ref()),
        draft.priority,
        draft.epic.or(draft.parent),
    )
}

pub(super) fn write_error(
    err: ApiError,
    meta: Option<&Fields>,
    priority: Option<&str>,
    link: Option<&str>,
) -> ApiError {
    if let ApiError::Api {
        status: 400,
        mut message,
    } = err
    {
        if message.contains("priority")
            && let Some(input) = priority
        {
            message.push_str(&format!("; priority {input:?}"));
            if let Some(options) = meta
                .and_then(|m| m.get("priority"))
                .and_then(|p| p["allowedValues"].as_array())
            {
                message.push_str(&format!("; valid: {}", option_names(options)));
            } else {
                message.push_str("; use the exact project priority name or ID, or omit --priority to keep the default");
            }
        }
        if let Some(key) = link
            && (message.contains("issuetype")
                || message.contains("parent")
                || message.contains("customfield_"))
        {
            message.push_str(&format!("; to add a Story or Task to an Epic use --epic {key}; subtasks require --parent <STORY-OR-TASK>. Check that Epic Link/parent is enabled on the create/edit screen"));
        }
        ApiError::Api {
            status: 400,
            message,
        }
    } else {
        err
    }
}
