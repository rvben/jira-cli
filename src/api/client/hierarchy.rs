//! Parent and epic membership as read back from issues.

use super::metadata::EPIC_LINK_SCHEMA;
use super::{ApiError, JiraClient};
use crate::api::types::Issue;
use std::sync::atomic::Ordering;

impl JiraClient {
    /// Fill in `Issue::epic` on later issue reads.
    ///
    /// Off by default: on Data Center it costs a field lookup and can fail on
    /// conflicting Epic Link values, which must not break commands that never
    /// report the epic.
    pub fn enable_epic_lookup(&self) {
        self.epic_lookup.store(true, Ordering::Relaxed);
    }

    /// The fields to request for an issue read, plus the Data Center Epic Link
    /// fields when the instance has any.
    pub(super) async fn issue_fields(&self, base: &[&str]) -> Result<Vec<String>, ApiError> {
        let mut fields: Vec<String> = base.iter().map(|f| (*f).to_owned()).collect();
        fields.extend(self.epic_link_fields().await?.iter().cloned());
        Ok(fields)
    }

    /// Resolve the Data Center Epic Link fields once per client.
    ///
    /// Cloud expresses epic membership through `parent`, so no lookup happens
    /// there. Only the Jira Software schema identifies the field: a custom
    /// field that merely carries the name "Epic Link" is not epic membership.
    /// An instance without Jira Software has no such field, and so no epics.
    async fn epic_link_fields(&self) -> Result<&[String], ApiError> {
        if self.api_version >= 3 || !self.epic_lookup.load(Ordering::Relaxed) {
            return Ok(&[]);
        }
        let fields = self
            .epic_link_fields
            .get_or_try_init(|| async {
                Ok::<_, ApiError>(
                    self.list_fields()
                        .await?
                        .into_iter()
                        .filter(|f| {
                            f.schema
                                .as_ref()
                                .is_some_and(|s| s.custom.as_deref() == Some(EPIC_LINK_SCHEMA))
                        })
                        .map(|f| f.id)
                        .collect(),
                )
            })
            .await?;
        Ok(fields)
    }

    /// Set `Issue::epic` on each fetched issue.
    pub(super) async fn fill_epics(&self, issues: &mut [Issue]) -> Result<(), ApiError> {
        if !self.epic_lookup.load(Ordering::Relaxed) {
            return Ok(());
        }
        if self.api_version >= 3 {
            for issue in issues {
                issue.epic = cloud_epic(issue);
            }
            return Ok(());
        }
        let fields = self.epic_link_fields().await?;
        for issue in issues {
            issue.epic = data_center_epic(issue, fields)?;
        }
        Ok(())
    }
}

/// An instance can carry more than one Epic Link field. They agree when at
/// most one distinct epic is set across them; anything else cannot be read as
/// a single epic.
fn data_center_epic(issue: &Issue, fields: &[String]) -> Result<Option<String>, ApiError> {
    let mut epic: Option<&str> = None;
    for field in fields {
        let key = match issue.fields.extra.get(field) {
            None | Some(serde_json::Value::Null) => continue,
            Some(serde_json::Value::String(key)) => key.as_str(),
            Some(other) => {
                return Err(ApiError::Other(format!(
                    "Unexpected Epic Link value on {} in {field}: {other}",
                    issue.key
                )));
            }
        };
        match epic {
            Some(seen) if seen != key => {
                return Err(ApiError::Other(format!(
                    "{} has conflicting Epic Link values in {}: {seen} and {key}",
                    issue.key,
                    fields.join(", ")
                )));
            }
            _ => epic = Some(key),
        }
    }
    Ok(epic.map(str::to_owned))
}

/// On Cloud an issue's epic is its parent when that parent sits at the epic
/// hierarchy level. The name is consulted only when Jira omits the level.
fn cloud_epic(issue: &Issue) -> Option<String> {
    let parent = issue.fields.parent.as_ref()?;
    let issue_type = parent.fields.as_ref()?.issuetype.as_ref()?;
    let is_epic = match issue_type.hierarchy_level {
        Some(level) => level == 1,
        None => issue_type.name.eq_ignore_ascii_case("Epic"),
    };
    is_epic.then(|| parent.key.clone())
}
