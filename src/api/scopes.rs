//! Scopes a scoped Jira Cloud API token needs for the requests this client makes.
//!
//! Each list is read from the `security` blocks of Atlassian's Jira Cloud
//! platform and Jira Software OpenAPI specifications for the operations the
//! client calls. Platform requests accept the classic scopes. Board and sprint
//! requests list only granular Jira Software scopes, so a token holding just the
//! classic set works everywhere except `boards` and `sprints`.

/// Every platform read: issues, comments, transitions, attachments, links,
/// projects, fields, metadata, search, users and the current user.
pub const READ: &[&str] = &["read:jira-work", "read:jira-user"];

/// Platform writes: creating and editing issues, comments, worklogs, links,
/// transitions, assignments and attachments.
pub const WRITE: &[&str] = &["write:jira-work"];

/// Board and sprint reads (`jira boards`, `jira sprints`).
pub const AGILE_READ: &[&str] = &[
    "read:board-scope:jira-software",
    "read:project:jira",
    "read:issue-details:jira",
    "read:sprint:jira-software",
];

/// Moving issues into a sprint.
pub const AGILE_WRITE: &[&str] = &["write:sprint:jira-software"];

/// The Jira Cloud API gateway's reason for refusing a scoped token that
/// lacks a scope the request needs. It answers with HTTP 401, the same status
/// as a revoked token, so the body is what tells the two apart.
const SCOPE_MISMATCH: &str = "scope does not match";

/// Whether a 401 body says the token lacks a scope, rather than being invalid.
pub fn is_scope_mismatch(body: &str) -> bool {
    body.to_ascii_lowercase().contains(SCOPE_MISMATCH)
}

/// The platform scopes a profile needs, before the board and sprint extras.
pub fn platform(read_only: bool) -> Vec<&'static str> {
    with_writes(READ, WRITE, read_only)
}

/// The board and sprint scopes a profile needs.
pub fn agile(read_only: bool) -> Vec<&'static str> {
    with_writes(AGILE_READ, AGILE_WRITE, read_only)
}

fn with_writes(
    read: &[&'static str],
    write: &[&'static str],
    read_only: bool,
) -> Vec<&'static str> {
    let mut scopes = read.to_vec();
    if !read_only {
        scopes.extend_from_slice(write);
    }
    scopes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_profile_needs_no_write_scope() {
        let all: Vec<&str> = platform(true).into_iter().chain(agile(true)).collect();
        assert!(
            all.iter().all(|scope| !scope.starts_with("write:")),
            "{all:?}"
        );
        assert_eq!(platform(true), ["read:jira-work", "read:jira-user"]);
    }

    #[test]
    fn read_write_profile_adds_the_write_scopes() {
        assert_eq!(
            platform(false),
            ["read:jira-work", "read:jira-user", "write:jira-work"]
        );
        assert!(agile(false).contains(&"write:sprint:jira-software"));
        assert!(agile(false).contains(&"read:sprint:jira-software"));
    }

    #[test]
    fn scope_mismatch_is_recognised_from_the_gateway_body() {
        assert!(is_scope_mismatch(
            r#"{"code":401,"message":"Unauthorized; scope does not match"}"#
        ));
        assert!(!is_scope_mismatch(
            r#"{"errorMessages":["You are not authenticated."]}"#
        ));
        assert!(!is_scope_mismatch(""));
    }
}
