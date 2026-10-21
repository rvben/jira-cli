//! Read-only checks shared by write commands and their `--dry-run` previews:
//! user lookup, permissions, assignability and the targets a write touches.

use super::metadata::{OptionMatch, match_option};
use super::{ApiError, JiraClient, percent_encode, validate_issue_key};
use crate::api::types::{IssueLinkType, User};
use serde_json::{Value, json};

impl JiraClient {
    /// Resolve a user argument to the identifier Jira writes expect: the
    /// account ID on Cloud, the username on Data Center. Accepts `me`, an
    /// e-mail address (looked up exactly), or a literal username/account ID.
    pub async fn resolve_user_id(&self, input: &str) -> Result<String, ApiError> {
        self.resolve_user(input, &mut None).await
    }

    /// [`Self::resolve_user_id`] with a cache for `me`, so a write naming
    /// several users asks Jira who you are at most once.
    pub(super) async fn resolve_user(
        &self,
        input: &str,
        myself: &mut Option<String>,
    ) -> Result<String, ApiError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ApiError::InvalidInput(
                "User must not be empty; pass a username, account ID, e-mail address or \"me\""
                    .into(),
            ));
        }
        if input.eq_ignore_ascii_case("me") {
            return Ok(match myself {
                Some(id) => id.clone(),
                None => myself.insert(self.get_myself().await?.account_id).clone(),
            });
        }
        if !input.contains('@') {
            return Ok(input.to_owned());
        }
        let matches = self
            .search_users(input)
            .await?
            .into_iter()
            .filter(|u| {
                u.email_address
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(input))
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [user] => Ok(user.account_id.clone()),
            [] => Err(ApiError::InvalidInput(format!(
                "No Jira user has the e-mail address {input:?}; pass the username or account ID instead (see `jira users search`)"
            ))),
            _ => Err(ApiError::InvalidInput(format!(
                "Several Jira users share the e-mail address {input:?}; pass the username or account ID instead (see `jira users search`)"
            ))),
        }
    }

    /// Whether you hold `permission` on `key`; `None` when Jira cannot say
    /// (older servers reject the permission filter).
    async fn has_permission(&self, key: &str, permission: &str) -> Result<Option<bool>, ApiError> {
        let path = format!("mypermissions?issueKey={key}&permissions={permission}");
        match self.get::<Value>(&path).await {
            Ok(perms) => Ok(perms["permissions"][permission]["havePermission"].as_bool()),
            Err(ApiError::NotFound(_) | ApiError::Api { status: 400, .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Fail when you lack `permission` on every one of `keys`; when Jira
    /// cannot tell, add a warning instead. `action` completes the sentence
    /// "You lack permission to {action} {keys}".
    pub(super) async fn check_permission(
        &self,
        keys: &[&str],
        permission: &str,
        action: &str,
        warnings: &mut Vec<String>,
    ) -> Result<(), ApiError> {
        let mut unknown = false;
        for key in keys {
            match self.has_permission(key, permission).await? {
                Some(true) => return Ok(()),
                Some(false) => {}
                None => unknown = true,
            }
        }
        let target = keys.join(" or ");
        if unknown {
            warnings.push(format!(
                "Jira did not report whether you may {action} {target}; the permission could not be checked."
            ));
            return Ok(());
        }
        Err(ApiError::InvalidInput(format!(
            "You lack permission to {action} {target}"
        )))
    }

    /// Fail when `id` cannot be assigned issues in `key`'s project; when the
    /// assignable-user search is unavailable, add a warning instead.
    pub async fn check_assignable(
        &self,
        key: &str,
        id: &str,
        warnings: &mut Vec<String>,
    ) -> Result<(), ApiError> {
        let param = if self.api_version() >= 3 {
            "accountId"
        } else {
            "username"
        };
        let path = format!(
            "user/assignable/search?issueKey={key}&{param}={}&maxResults=50",
            percent_encode(id)
        );
        match self.get::<Vec<User>>(&path).await {
            Ok(users) if users.iter().any(|u| u.account_id.eq_ignore_ascii_case(id)) => Ok(()),
            Ok(_) => Err(ApiError::InvalidInput(format!(
                "{id} cannot be assigned issues in {key}'s project (not an assignable user)"
            ))),
            Err(ApiError::NotFound(_) | ApiError::Api { status: 400, .. }) => {
                warnings.push(format!(
                    "Jira did not report whether {id} is assignable to {key}; it could not be checked."
                ));
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// Check that a comment can be added to `key`.
    pub async fn preview_comment(&self, key: &str, body: &str) -> Result<Vec<String>, ApiError> {
        validate_issue_key(key)?;
        validate_comment(body)?;
        self.issue_project(key).await?;
        let mut warnings = Vec::new();
        self.check_permission(&[key], "ADD_COMMENTS", "comment on", &mut warnings)
            .await?;
        Ok(warnings)
    }

    /// Check an assignment and return the current assignee's display name
    /// (`None` when unassigned) with any warnings.
    pub async fn preview_assign(
        &self,
        key: &str,
        account_id: Option<&str>,
    ) -> Result<(Option<String>, Vec<String>), ApiError> {
        validate_issue_key(key)?;
        let issue: Value = self.get(&format!("issue/{key}?fields=assignee")).await?;
        let warnings = self.check_assign(key, account_id).await?;
        let current = issue["fields"]["assignee"]["displayName"]
            .as_str()
            .map(str::to_owned);
        Ok((current, warnings))
    }

    /// Check that you may assign `key` and that `account_id` (when given) can
    /// be assigned it; returns warnings for checks Jira could not answer.
    pub async fn check_assign(
        &self,
        key: &str,
        account_id: Option<&str>,
    ) -> Result<Vec<String>, ApiError> {
        validate_issue_key(key)?;
        let mut warnings = Vec::new();
        self.check_permission(&[key], "ASSIGN_ISSUES", "assign", &mut warnings)
            .await?;
        if let Some(id) = account_id {
            self.check_assignable(key, id, &mut warnings).await?;
        }
        Ok(warnings)
    }

    /// Resolve a link type by name or ID, ignoring case.
    pub async fn resolve_link_type(&self, input: &str) -> Result<IssueLinkType, ApiError> {
        let types = match self.get_link_types().await {
            Ok(types) => types,
            Err(ApiError::NotFound(_)) => {
                return Err(ApiError::InvalidInput(
                    "Issue link types are unavailable; issue linking may be disabled on this Jira site"
                        .into(),
                ));
            }
            Err(err) => return Err(err),
        };
        let options = types
            .iter()
            .map(|t| json!({ "id": t.id, "name": t.name }))
            .collect::<Vec<_>>();
        let index = match_option("link type", input, &options, OptionMatch::NameOrId)?;
        Ok(types[index].clone())
    }

    /// Check a link between two issues and resolve its type.
    pub async fn preview_link(
        &self,
        from: &str,
        to: &str,
        link_type: &str,
    ) -> Result<(IssueLinkType, Vec<String>), ApiError> {
        validate_link_keys(from, to)?;
        self.issue_project(from).await?;
        self.issue_project(to).await?;
        let link_type = self.resolve_link_type(link_type).await?;
        let mut warnings = Vec::new();
        self.check_permission(&[from, to], "LINK_ISSUES", "link", &mut warnings)
            .await?;
        Ok((link_type, warnings))
    }

    /// Fetch the link an `unlink` would remove and check you may remove it.
    pub async fn preview_unlink(&self, link_id: &str) -> Result<(Value, Vec<String>), ApiError> {
        validate_link_id(link_id)?;
        let link: Value = self.get(&format!("issueLink/{link_id}")).await?;
        let keys = ["inwardIssue", "outwardIssue"]
            .iter()
            .filter_map(|side| link[side]["key"].as_str())
            .collect::<Vec<_>>();
        let mut warnings = Vec::new();
        if !keys.is_empty() {
            self.check_permission(&keys, "LINK_ISSUES", "unlink", &mut warnings)
                .await?;
        }
        Ok((link, warnings))
    }

    /// Check that work can be logged on `key`.
    pub async fn preview_worklog(&self, key: &str) -> Result<Vec<String>, ApiError> {
        validate_issue_key(key)?;
        self.issue_project(key).await?;
        let mut warnings = Vec::new();
        self.check_permission(&[key], "WORK_ON_ISSUES", "log work on", &mut warnings)
            .await?;
        match self.get::<Value>("configuration").await {
            Ok(config) if config["timeTrackingEnabled"] == false => {
                return Err(ApiError::InvalidInput(
                    "Time tracking is disabled on this Jira site".into(),
                ));
            }
            Ok(_) => {}
            Err(ApiError::NotFound(_) | ApiError::Api { status: 403, .. }) => warnings.push(
                "Jira did not report whether time tracking is enabled; it could not be checked."
                    .into(),
            ),
            Err(err) => return Err(err),
        }
        Ok(warnings)
    }
}

/// Reject comments Jira would store as empty.
pub(super) fn validate_comment(body: &str) -> Result<(), ApiError> {
    if body.trim().is_empty() {
        return Err(ApiError::InvalidInput(
            "Comment body must not be empty".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_link_keys(from: &str, to: &str) -> Result<(), ApiError> {
    validate_issue_key(from)?;
    validate_issue_key(to)?;
    if from == to {
        return Err(ApiError::InvalidInput(format!(
            "Cannot link {from} to itself"
        )));
    }
    Ok(())
}

pub(super) fn validate_link_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::InvalidInput(format!(
            "Invalid link ID '{id}'. Expected a number (see `jira issues show <KEY>`)."
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_keys_reject_self_links() {
        assert!(validate_link_keys("PROJ-1", "PROJ-2").is_ok());
        let err = validate_link_keys("PROJ-1", "PROJ-1").unwrap_err();
        assert!(err.to_string().contains("to itself"), "{err}");
    }

    #[test]
    fn link_ids_must_be_numeric() {
        assert!(validate_link_id("10042").is_ok());
        for bad in ["", "abc", "10/../1", "1 2"] {
            assert!(validate_link_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn comments_must_have_text() {
        assert!(validate_comment("hi").is_ok());
        assert!(validate_comment(" \n\t").is_err());
    }
}
