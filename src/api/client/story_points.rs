//! Story points as read back from issues.
//!
//! Jira keeps story points in a custom field whose ID differs per site. Jira
//! Software's own estimate field ("Story point estimate" on Cloud) carries a
//! schema that identifies it; the "Story Points" number field that
//! company-managed projects and Data Center estimate with does not, so that one
//! is recognised by its name. A profile can pin the field instead, which skips
//! discovery and settles which field counts when a site uses both.

use super::{ApiError, JiraClient};
use crate::api::types::{Field, Issue};
use std::sync::atomic::Ordering;

const STORY_POINTS_SCHEMA: &str = "com.pyxis.greenhopper.jira:jsw-story-points";
const STORY_POINTS_NAME: &str = "Story Points";

/// What pins the story points field, for error messages that tell the reader
/// how to settle an ambiguity.
const PIN_ADVICE: &str = "Set story_points_field in the profile (or JIRA_STORY_POINTS_FIELD) \
                          to the field your boards estimate with";

impl JiraClient {
    /// Pin the story points field instead of discovering it from the site's
    /// field catalog.
    pub fn with_story_points_field(mut self, field: Option<String>) -> Self {
        self.story_points_field = field;
        self
    }

    /// Fill in `Issue::story_points` on later issue reads.
    ///
    /// Off by default: discovery costs a field-catalog request, and the
    /// commands that never report story points should not pay for it.
    pub fn enable_story_points_lookup(&self) {
        self.story_points_lookup.store(true, Ordering::Relaxed);
    }

    /// The fields story points are read from, or an empty list when the site
    /// has none. A pinned field is used as given, without a catalog request.
    pub async fn story_point_fields(&self) -> Result<Vec<String>, ApiError> {
        if let Some(field) = &self.story_points_field {
            return Ok(vec![field.clone()]);
        }
        let fields = self
            .story_point_field_ids
            .get_or_try_init(|| async {
                Ok::<_, ApiError>(
                    self.field_catalog()
                        .await?
                        .iter()
                        .filter(|f| is_story_points_field(f))
                        .map(|f| f.id.clone())
                        .collect(),
                )
            })
            .await?;
        Ok(fields.clone())
    }

    /// The story point fields for an issue read: `None` when the lookup is off,
    /// or when only the field catalog could name them and `catalog` says it is
    /// unavailable.
    pub(super) async fn story_point_fields_for_read(
        &self,
        catalog: bool,
    ) -> Result<Option<Vec<String>>, ApiError> {
        if !self.story_points_lookup.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if !catalog && self.story_points_field.is_none() {
            return Ok(None);
        }
        self.story_point_fields().await.map(Some)
    }

    /// Whether issue reads can report story points without the field catalog.
    pub(super) fn story_points_pinned(&self) -> bool {
        self.story_points_field.is_some()
    }
}

/// Jira Software's estimate field, or a number field named "Story Points".
/// The name alone is not enough: a text field of that name holds no estimate.
fn is_story_points_field(field: &Field) -> bool {
    let Some(schema) = &field.schema else {
        return false;
    };
    schema.custom.as_deref() == Some(STORY_POINTS_SCHEMA)
        || (field.custom
            && schema.field_type == "number"
            && field.name.trim().eq_ignore_ascii_case(STORY_POINTS_NAME))
}

/// Set `Issue::story_points` on each issue from `fields`. An empty `fields`
/// means the site has no story points field, which leaves the value unknown
/// rather than unset.
pub(super) fn fill_story_points(issues: &mut [Issue], fields: &[String]) -> Result<(), ApiError> {
    if fields.is_empty() {
        return Ok(());
    }
    for issue in issues {
        issue.story_points = Some(story_points(issue, fields)?);
    }
    Ok(())
}

/// A site can carry more than one story points field. They agree when every
/// field that holds a value holds the same one; anything else cannot be read
/// as a single estimate.
fn story_points(issue: &Issue, fields: &[String]) -> Result<Option<serde_json::Number>, ApiError> {
    let mut found: Option<(&str, &serde_json::Number)> = None;
    for field in fields {
        let value = match issue.fields.extra.get(field) {
            None | Some(serde_json::Value::Null) => continue,
            Some(serde_json::Value::Number(value)) => value,
            Some(other) => {
                return Err(ApiError::Other(format!(
                    "Unexpected story points value on {} in {field}: {other}. \
                     Story points must be a number; if {field} is not the story points field, {PIN_ADVICE}",
                    issue.key
                )));
            }
        };
        match found {
            Some((seen_field, seen)) if seen.as_f64() != value.as_f64() => {
                return Err(ApiError::Other(format!(
                    "{} has conflicting story points: {seen} in {seen_field} and {value} in {field}. {PIN_ADVICE}",
                    issue.key
                )));
            }
            Some(_) => {}
            None => found = Some((field, value)),
        }
    }
    Ok(found.map(|(_, value)| value.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(value: serde_json::Value) -> Field {
        serde_json::from_value(value).unwrap()
    }

    fn issue(extra: serde_json::Value) -> Issue {
        let mut fields = json!({
            "summary": "s", "status": {"name": "To Do"}, "issuetype": {"name": "Story"}
        });
        fields
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(json!({"id": "1", "key": "PROJ-1", "fields": fields})).unwrap()
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn recognises_the_jira_software_estimate_field_by_schema_whatever_its_name() {
        assert!(is_story_points_field(&field(json!({
            "id": "customfield_10016", "name": "Story point estimate", "custom": true,
            "schema": {"type": "number", "custom": STORY_POINTS_SCHEMA}
        }))));
        assert!(is_story_points_field(&field(json!({
            "id": "customfield_10016", "name": "Renamed estimate", "custom": true,
            "schema": {"type": "number", "custom": STORY_POINTS_SCHEMA}
        }))));
    }

    #[test]
    fn recognises_a_custom_number_field_named_story_points() {
        assert!(is_story_points_field(&field(json!({
            "id": "customfield_10028", "name": "Story Points", "custom": true,
            "schema": {"type": "number", "custom": "com.atlassian.jira.plugin.system.customfieldtypes:float"}
        }))));
        assert!(is_story_points_field(&field(json!({
            "id": "customfield_10002", "name": "story points", "custom": true,
            "schema": {"type": "number", "custom": "com.atlassian.jira.plugin.system.customfieldtypes:float"}
        }))));
    }

    #[test]
    fn rejects_fields_that_only_look_like_story_points() {
        // Named right, but text cannot hold an estimate.
        assert!(!is_story_points_field(&field(json!({
            "id": "customfield_1", "name": "Story Points", "custom": true,
            "schema": {"type": "string", "custom": "com.atlassian.jira.plugin.system.customfieldtypes:textfield"}
        }))));
        // A number, but some other number.
        assert!(!is_story_points_field(&field(json!({
            "id": "customfield_2", "name": "Story Points Remaining", "custom": true,
            "schema": {"type": "number", "custom": "com.atlassian.jira.plugin.system.customfieldtypes:float"}
        }))));
        // Time tracking is a system number field, not an estimate in points.
        assert!(!is_story_points_field(&field(json!({
            "id": "timeoriginalestimate", "name": "Original estimate", "custom": false,
            "schema": {"type": "number", "system": "timeoriginalestimate"}
        }))));
        assert!(!is_story_points_field(&field(json!({
            "id": "customfield_3", "name": "Story Points", "custom": true
        }))));
    }

    #[test]
    fn reads_the_value_from_whichever_field_holds_it() {
        let fields = ids(&["customfield_10016", "customfield_10028"]);
        let read = |extra| {
            story_points(&issue(extra), &fields)
                .unwrap()
                .map(|n| n.as_f64().unwrap())
        };
        assert_eq!(read(json!({"customfield_10028": 5.0})), Some(5.0));
        assert_eq!(
            read(json!({"customfield_10016": 0.5, "customfield_10028": null})),
            Some(0.5)
        );
        assert_eq!(read(json!({"customfield_10016": null})), None);
        assert_eq!(read(json!({})), None);
    }

    #[test]
    fn zero_points_is_a_value_not_an_absence() {
        let value = story_points(
            &issue(json!({"customfield_10016": 0})),
            &ids(&["customfield_10016"]),
        );
        assert_eq!(value.unwrap().unwrap().as_f64(), Some(0.0));
    }

    #[test]
    fn fields_that_agree_read_as_one_value() {
        let value = story_points(
            &issue(json!({"customfield_10016": 3, "customfield_10028": 3.0})),
            &ids(&["customfield_10016", "customfield_10028"]),
        );
        assert_eq!(value.unwrap().unwrap().as_f64(), Some(3.0));
    }

    #[test]
    fn fields_that_disagree_are_an_error_naming_both_and_the_fix() {
        let err = story_points(
            &issue(json!({"customfield_10016": 3, "customfield_10028": 5})),
            &ids(&["customfield_10016", "customfield_10028"]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("PROJ-1"), "{err}");
        assert!(err.contains("3 in customfield_10016"), "{err}");
        assert!(err.contains("5 in customfield_10028"), "{err}");
        assert!(err.contains("story_points_field"), "{err}");
    }

    #[test]
    fn a_non_number_value_is_an_error_not_a_silent_absence() {
        let err = story_points(
            &issue(json!({"customfield_1": "five"})),
            &ids(&["customfield_1"]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("customfield_1"), "{err}");
        assert!(err.contains("\"five\""), "{err}");
    }

    #[test]
    fn a_site_without_a_field_leaves_story_points_unknown() {
        let mut issues = [issue(json!({}))];
        fill_story_points(&mut issues, &[]).unwrap();
        assert_eq!(issues[0].story_points, None);
        fill_story_points(&mut issues, &ids(&["customfield_10016"])).unwrap();
        assert_eq!(issues[0].story_points, Some(None));
    }
}
