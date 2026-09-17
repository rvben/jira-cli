//! Resolution and safety checks for changing an existing issue's type.

use super::{ApiError, Fields, IssueType, JiraClient, decode, option_names, validate_issue_key};
use serde::Deserialize;
use serde_json::Value;

/// First Data Center / Server release that refuses an issue type edit whose
/// workflows are incompatible. Earlier releases accept it with 204 and leave
/// the issue in a workflow state that does not belong to its new type
/// (JRASERVER-71292).
const SAFE_SERVER_VERSION: [u64; 3] = [9, 10, 0];

const MOVE_HINT: &str = "use More > Move in the Jira web UI instead";

#[derive(Deserialize)]
struct CurrentIssue {
    fields: CurrentFields,
}

#[derive(Deserialize)]
struct CurrentFields {
    issuetype: IssueType,
    project: ProjectRef,
}

#[derive(Deserialize)]
struct ProjectRef {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerInfo {
    #[serde(default)]
    version_numbers: Vec<u64>,
    version: Option<String>,
}

impl JiraClient {
    /// Resolve `--type` for `key` to a type the issue can be switched to by an
    /// edit, refusing any change the edit API cannot make safely.
    pub(super) async fn resolve_type_change(
        &self,
        key: &str,
        input: &str,
        meta: Option<&Fields>,
    ) -> Result<IssueType, ApiError> {
        validate_issue_key(key)?;
        if self.api_version < 3 {
            self.ensure_server_type_edit_is_safe().await?;
        }
        let current: CurrentIssue = self
            .get(&format!("issue/{key}?fields=issuetype,project"))
            .await?;
        let current = current.fields;

        let options = match meta
            .and_then(|m| m.get("issuetype"))
            .and_then(|f| f["allowedValues"].as_array())
        {
            Some(allowed) => allowed.clone(),
            None => {
                let project: Value = self.get(&format!("project/{}", current.project.id)).await?;
                project["issueTypes"].as_array().cloned().ok_or_else(|| {
                    ApiError::Other(format!(
                        "Jira returned no issue types for the project of {key}"
                    ))
                })?
            }
        };
        let target: IssueType = decode(select_type(input, &options)?.clone())?;
        if target.id.is_empty() {
            return Err(ApiError::Other(format!(
                "Jira reported issue type {:?} without an ID",
                target.name
            )));
        }
        check_same_level(&current.issuetype, &target, self.api_version >= 3)?;
        if target.id == current.issuetype.id {
            return Err(ApiError::InvalidInput(format!(
                "{key} is already of issue type {:?}",
                current.issuetype.name
            )));
        }
        Ok(IssueType {
            fields: None,
            ..target
        })
    }

    async fn ensure_server_type_edit_is_safe(&self) -> Result<(), ApiError> {
        let info: ServerInfo = self.get("serverInfo").await?;
        let version = info.version.as_deref().unwrap_or("unknown");
        if info.version_numbers.is_empty() {
            return Err(ApiError::InvalidInput(format!(
                "Cannot determine the Jira server version ({version}); changing an issue type through the REST API is only safe from Jira 9.10.0, so {MOVE_HINT}"
            )));
        }
        let mut parts = info.version_numbers.clone();
        parts.resize(3, 0);
        if parts[..3] < SAFE_SERVER_VERSION[..] {
            return Err(ApiError::InvalidInput(format!(
                "Jira {version} accepts issue type changes without checking workflow compatibility, which can leave the issue in an invalid workflow state (fixed in Jira 9.10.0); {MOVE_HINT}"
            )));
        }
        Ok(())
    }
}

/// Pick the option for `input`: an exact ID wins outright, otherwise a unique
/// case-insensitive name.
fn select_type<'a>(input: &str, options: &'a [Value]) -> Result<&'a Value, ApiError> {
    let input = input.trim();
    if let Some(found) = options.iter().find(|o| o["id"].as_str() == Some(input)) {
        return Ok(found);
    }
    let lower = input.to_lowercase();
    let named: Vec<&Value> = options
        .iter()
        .filter(|o| {
            o["name"]
                .as_str()
                .is_some_and(|name| name.to_lowercase() == lower)
        })
        .collect();
    match named.as_slice() {
        [only] => Ok(only),
        [] => Err(ApiError::InvalidInput(format!(
            "Invalid issue type {input:?} for this issue; valid: {}",
            option_names(options)
        ))),
        _ => Err(ApiError::InvalidInput(format!(
            "Ambiguous issue type {input:?}; use an ID from: {}",
            option_names(options)
        ))),
    }
}

/// An edit can only swap types within one hierarchy level. Moving between
/// subtask and standard issue, or across levels, needs Jira's Move operation.
///
/// Only reported classification counts, never names. Cloud reports a
/// hierarchy level for every type, so a missing one there means the change
/// cannot be checked; Data Center has no levels and relies on the subtask flag.
fn check_same_level(current: &IssueType, target: &IssueType, cloud: bool) -> Result<(), ApiError> {
    let unverifiable = |what: &str| {
        ApiError::InvalidInput(format!(
            "Jira did not report {what} for {:?} and {:?}, so the change cannot be checked; {MOVE_HINT}",
            current.name, target.name
        ))
    };
    let (Some(from_subtask), Some(to_subtask)) = (current.subtask, target.subtask) else {
        return Err(unverifiable("whether they are subtask types"));
    };
    let levels_differ = match (current.hierarchy_level, target.hierarchy_level) {
        (Some(from), Some(to)) => from != to,
        (None, None) if !cloud => false,
        _ => return Err(unverifiable("the hierarchy level")),
    };
    if from_subtask != to_subtask || levels_differ {
        return Err(ApiError::InvalidInput(format!(
            "Cannot change {:?} to {:?}: converting between subtask and standard issue types, or across hierarchy levels, is not supported by the Jira edit API; {MOVE_HINT}",
            current.name, target.name
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reported(name: &str, subtask: Option<bool>, level: Option<i64>) -> IssueType {
        IssueType {
            id: String::new(),
            name: name.into(),
            subtask,
            hierarchy_level: level,
            fields: None,
        }
    }

    #[test]
    fn exact_id_beats_a_type_named_like_that_id() {
        let options = [
            json!({"id": "10001", "name": "Story"}),
            json!({"id": "20002", "name": "10001"}),
        ];
        assert_eq!(select_type("10001", &options).unwrap()["name"], "Story");
    }

    #[test]
    fn names_match_case_insensitively_and_must_be_unique() {
        let options = [
            json!({"id": "1", "name": "Task"}),
            json!({"id": "2", "name": "TASK"}),
            json!({"id": "3", "name": "Story"}),
        ];
        assert_eq!(select_type("story", &options).unwrap()["id"], "3");
        let err = select_type("task", &options).unwrap_err().to_string();
        assert!(err.contains("Ambiguous"), "{err}");
        let err = select_type("Bug", &options).unwrap_err().to_string();
        assert!(err.contains("Invalid issue type \"Bug\""), "{err}");
    }

    #[test]
    fn classification_comes_from_metadata_not_names() {
        // A standard type literally named "Sub-task" is still a standard type.
        let misnamed = reported("Sub-task", Some(false), Some(0));
        let story = reported("Story", Some(false), Some(0));
        assert!(check_same_level(&misnamed, &story, true).is_ok());
        // An actual subtask with an ordinary name is still a subtask.
        let subtask = reported("Chore", Some(true), None);
        let dc_story = reported("Story", Some(false), None);
        let err = check_same_level(&subtask, &dc_story, false).unwrap_err();
        assert!(err.to_string().contains("not supported"), "{err}");
    }

    #[test]
    fn hierarchy_and_unknown_classification_are_refused() {
        let story = reported("Story", Some(false), Some(0));
        let epic = reported("Initiative", Some(false), Some(1));
        let err = check_same_level(&story, &epic, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not supported") && err.contains("Move"),
            "{err}"
        );

        let unknown = reported("Task", None, Some(0));
        let err = check_same_level(&unknown, &story, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not report whether"), "{err}");

        // Cloud always reports levels; a missing one is not a match.
        let unlevelled = reported("Epic", Some(false), None);
        for (from, to) in [(&story, &unlevelled), (&unlevelled, &story)] {
            let err = check_same_level(from, to, true).unwrap_err().to_string();
            assert!(err.contains("did not report the hierarchy level"), "{err}");
        }
        // Data Center has no levels at all, and one-sided levels are suspect.
        let dc_task = reported("Task", Some(false), None);
        assert!(check_same_level(&dc_task, &unlevelled, false).is_ok());
        assert!(check_same_level(&story, &unlevelled, false).is_err());
    }
}
