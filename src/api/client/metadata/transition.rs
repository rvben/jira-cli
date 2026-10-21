//! Fields, resolution and comment for a workflow transition, checked against
//! the transition screen Jira reports with `expand=transitions.fields`.

use super::validation::{is_empty_value, normalize_field_values, validate_required};
use super::{ApiError, Fields, JiraClient, unresolved_option, validate_issue_key};
use crate::api::types::{Comment, Transition};
use serde_json::{Map, Value, json};

/// What to send with a transition besides its ID.
#[derive(Default)]
pub struct TransitionInput<'a> {
    pub resolution: Option<&'a str>,
    pub comment: Option<&'a str>,
    /// `--field` pairs, keyed by field ID or name.
    pub fields: &'a [(String, Value)],
}

/// A checked transition request, ready to send or to preview.
pub struct PreparedTransition {
    /// The `POST issue/{key}/transitions` body.
    pub payload: Value,
    /// The normalised field values (also inside `payload`).
    pub fields: Value,
    /// A comment the transition screen cannot take, added after the
    /// transition as a regular comment.
    pub separate_comment: Option<String>,
    pub warnings: Vec<String>,
}

impl JiraClient {
    /// Validate fields, resolution and comment for `transition` on `key` and
    /// build the request. Field names map to IDs and values are typed against
    /// the screen; required screen fields that are neither supplied, defaulted
    /// nor already set on the issue are reported before anything is written.
    pub async fn prepare_transition(
        &self,
        key: &str,
        transition: &Transition,
        input: &TransitionInput<'_>,
    ) -> Result<PreparedTransition, ApiError> {
        validate_issue_key(key)?;
        let meta = transition.fields.as_ref();
        let mut warnings = Vec::new();
        let mut supplied = Vec::new();
        if let Some(resolution) = input.resolution {
            supplied.push((
                screen_field(meta, "resolution", &transition.name, "--resolution")?,
                Value::String(resolution.to_owned()),
            ));
        }
        for (field, value) in input.fields {
            let id = screen_field(meta, field, &transition.name, "--field")?;
            if supplied.iter().any(|(seen, _)| *seen == id) {
                return Err(ApiError::InvalidInput(if id == "resolution" {
                    "--resolution conflicts with --field resolution; choose one source".into()
                } else {
                    format!("Field {id} is given more than once")
                }));
            }
            supplied.push((id, value.clone()));
        }
        if meta.is_none() && !supplied.is_empty() {
            warnings.push(format!(
                "Jira did not report the fields of the {:?} transition; field values are sent unchecked.",
                transition.name
            ));
        }

        let mut fields = Value::Object(
            supplied
                .iter()
                .map(|(id, value)| (id.clone(), value.clone()))
                .collect::<Map<_, _>>(),
        );
        normalize_field_values(&mut fields, meta, &supplied, self.api_version)?;
        self.normalize_user_fields(&mut fields, meta, &supplied)
            .await?;
        if meta.is_none()
            && let Some(Value::String(resolution)) = fields.get("resolution")
        {
            fields["resolution"] = unresolved_option(resolution);
        }
        if let Some(meta) = meta {
            let required = self.unmet_required(key, meta, &fields).await?;
            validate_required(&fields, Some(&required), true)?;
        }

        let mut payload = json!({ "transition": { "id": transition.id } });
        if fields.as_object().is_some_and(|f| !f.is_empty()) {
            payload["fields"] = fields.clone();
        }
        let mut separate_comment = None;
        if let Some(comment) = input.comment {
            super::super::preflight::validate_comment(comment)?;
            match meta {
                Some(meta) if !meta.contains_key("comment") => {
                    warnings.push(format!(
                        "The {:?} transition screen has no comment field; the comment is added separately after the transition.",
                        transition.name
                    ));
                    separate_comment = Some(comment.to_owned());
                }
                _ => {
                    payload["update"] =
                        json!({ "comment": [{ "add": { "body": self.make_body(comment) } }] });
                }
            }
        }
        Ok(PreparedTransition {
            payload,
            fields,
            separate_comment,
            warnings,
        })
    }

    /// Send a prepared transition. Returns the comment when it had to be
    /// added separately. If that comment fails, the error says the transition
    /// itself was applied (details `{"transitioned": true}`), so it is not
    /// retried.
    pub async fn transition_issue(
        &self,
        key: &str,
        prepared: &PreparedTransition,
    ) -> Result<Option<Comment>, ApiError> {
        validate_issue_key(key)?;
        self.post_empty_response(&format!("issue/{key}/transitions"), &prepared.payload)
            .await?;
        let Some(body) = &prepared.separate_comment else {
            return Ok(None);
        };
        self.add_comment(key, body)
            .await
            .map(Some)
            .map_err(|err| ApiError::WithDetails {
                source: Box::new(ApiError::Other(format!(
                    "{key} was transitioned, but adding the comment failed: {err}. Add it with `jira issues comment {key}`; do not repeat the transition."
                ))),
                details: json!({ "transitioned": true }),
            })
    }

    /// The required screen fields this request must satisfy: those supplied
    /// (so an empty value is caught) and those without a default that the
    /// issue does not already have a value for.
    async fn unmet_required(
        &self,
        key: &str,
        meta: &Fields,
        fields: &Value,
    ) -> Result<Fields, ApiError> {
        let required = meta
            .iter()
            .filter(|(_, definition)| definition["required"] == true)
            .collect::<Vec<_>>();
        let absent = required
            .iter()
            .filter(|(id, definition)| {
                fields.get(id.as_str()).is_none() && definition["hasDefaultValue"] != true
            })
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>();
        let empty = self.empty_fields(key, &absent).await?;
        Ok(required
            .into_iter()
            .filter(|(id, _)| fields.get(id.as_str()).is_some() || empty.contains(&id.as_str()))
            .map(|(id, definition)| (id.clone(), definition.clone()))
            .collect())
    }

    /// The fields among `ids` that `key` has no value for. Asks Jira only
    /// when there is something to check.
    pub async fn empty_fields<'f>(
        &self,
        key: &str,
        ids: &[&'f str],
    ) -> Result<Vec<&'f str>, ApiError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        validate_issue_key(key)?;
        let current = self
            .get::<Value>(&format!("issue/{key}?fields={}", ids.join(",")))
            .await?;
        Ok(ids
            .iter()
            .copied()
            .filter(|id| current["fields"].get(*id).is_none_or(is_empty_value))
            .collect())
    }
}

/// Map a field ID or name to the ID on the transition screen, ignoring case.
/// Without screen metadata the input is used as given.
fn screen_field(
    meta: Option<&Fields>,
    input: &str,
    transition: &str,
    flag: &str,
) -> Result<String, ApiError> {
    let input = input.trim();
    let Some(meta) = meta else {
        return Ok(input.to_owned());
    };
    if meta.contains_key(input) {
        return Ok(input.to_owned());
    }
    let matches = meta
        .iter()
        .filter(|(id, definition)| {
            id.eq_ignore_ascii_case(input)
                || definition["name"]
                    .as_str()
                    .is_some_and(|name| name.trim().eq_ignore_ascii_case(input))
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] if meta.is_empty() => Err(ApiError::InvalidInput(format!(
            "The {transition:?} transition has no screen, so it takes no fields; drop {flag} {input}"
        ))),
        [] => Err(ApiError::InvalidInput(format!(
            "{flag} {input}: not on the {transition:?} transition screen; available: {}",
            screen_fields(meta)
        ))),
        _ => Err(ApiError::InvalidInput(format!(
            "{flag} {input}: several fields on the {transition:?} transition screen match; use the field ID ({})",
            matches.join(", ")
        ))),
    }
}

fn screen_fields(meta: &Fields) -> String {
    meta.iter()
        .filter(|(id, _)| id.as_str() != "comment")
        .map(|(id, definition)| match definition["name"].as_str() {
            Some(name) => format!("{name:?} ({id})"),
            None => id.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Fields {
        serde_json::from_value(json!({
            "resolution": {"name": "Resolution", "required": true},
            "customfield_1": {"name": "Root cause", "required": false},
            "comment": {"name": "Comment", "required": false}
        }))
        .unwrap()
    }

    #[test]
    fn screen_fields_match_ids_and_names_ignoring_case() {
        let meta = meta();
        assert_eq!(
            screen_field(Some(&meta), "Resolution", "Done", "--field").unwrap(),
            "resolution"
        );
        assert_eq!(
            screen_field(Some(&meta), "root CAUSE", "Done", "--field").unwrap(),
            "customfield_1"
        );
        assert_eq!(
            screen_field(None, "anything", "Done", "--field").unwrap(),
            "anything"
        );
    }

    #[test]
    fn unknown_screen_fields_list_what_is_available() {
        let err = screen_field(Some(&meta()), "Severity", "Done", "--field").unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("not on the \"Done\" transition screen"),
            "{text}"
        );
        assert!(text.contains("\"Root cause\" (customfield_1)"), "{text}");
        assert!(!text.contains("(comment)"), "{text}");

        let err =
            screen_field(Some(&Fields::new()), "resolution", "Start", "--resolution").unwrap_err();
        assert!(err.to_string().contains("has no screen"), "{err}");
    }
}
