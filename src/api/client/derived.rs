//! Values the client derives after an issue read from fields whose IDs differ
//! per site: the epic an issue belongs to and its story points.

use super::story_points::fill_story_points;
use super::{ApiError, JiraClient};
use crate::api::types::{Field, Issue};

/// The site-specific fields one issue read requests beyond its base list.
pub(super) struct DerivedFields {
    epic_link: Vec<String>,
    /// `None` when this read does not report story points.
    story_points: Option<Vec<String>>,
}

impl DerivedFields {
    /// `base` plus every site-specific field the derived values are read from.
    pub(super) fn request(&self, base: &[&str]) -> Vec<String> {
        let mut fields: Vec<String> = base.iter().map(|f| (*f).to_owned()).collect();
        fields.extend(self.epic_link.iter().cloned());
        fields.extend(self.story_points.iter().flatten().cloned());
        fields
    }
}

impl JiraClient {
    /// Every field the site defines, fetched at most once per client and
    /// shared by the epic, sprint and story points lookups. A failure is not
    /// remembered, so a later read tries again.
    pub(super) async fn field_catalog(&self) -> Result<&[Field], ApiError> {
        self.field_catalog
            .get_or_try_init(|| self.list_fields())
            .await
            .map(Vec::as_slice)
    }

    /// Resolve the fields for one issue read. `catalog` false says the field
    /// catalog already failed for this read: the lookups that need it are
    /// skipped rather than requested again.
    pub(super) async fn derived_fields(&self, catalog: bool) -> Result<DerivedFields, ApiError> {
        let epic_link = if catalog {
            self.epic_link_fields().await?.to_vec()
        } else {
            Vec::new()
        };
        Ok(DerivedFields {
            epic_link,
            story_points: self.story_point_fields_for_read(catalog).await?,
        })
    }

    /// Fill in every derived value on freshly read issues.
    pub(super) fn fill_derived(
        &self,
        issues: &mut [Issue],
        derived: &DerivedFields,
    ) -> Result<(), ApiError> {
        self.fill_epics(issues, &derived.epic_link)?;
        if let Some(fields) = &derived.story_points {
            fill_story_points(issues, fields)?;
        }
        Ok(())
    }

    /// Name what a field-catalog failure leaves out of an issue read, for the
    /// warning that keeps the resulting gaps from reading as empty values.
    pub(super) fn catalog_dependents(&self) -> String {
        let mut parts = vec!["Sprint"];
        if self.api_version < 3 {
            parts.push("epic");
        }
        if self
            .story_points_lookup
            .load(std::sync::atomic::Ordering::Relaxed)
            && !self.story_points_pinned()
        {
            parts.push("story point");
        }
        match parts.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            _ => parts.concat(),
        }
    }
}
