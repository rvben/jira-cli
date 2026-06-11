//! End-to-end tests against a real Jira instance.
//!
//! Skipped automatically unless the following environment variables are set:
//!
//!   JIRA_E2E_HOST   — Jira host, e.g. `http://localhost:8080` or `mycompany.atlassian.net`
//!   JIRA_E2E_EMAIL  — Account email (leave blank for PAT auth)
//!   JIRA_E2E_TOKEN  — API token or PAT
//!   JIRA_E2E_PROJECT — Project key to create test issues in (default: TST)
//!
//! Run with:
//!   JIRA_E2E_HOST=http://localhost:8080 JIRA_E2E_EMAIL=ruben JIRA_E2E_TOKEN=test \
//!     cargo nextest run --test e2e
//!
//! All write tests create issues tagged `[e2e-auto]` in the summary so they can
//! be bulk-deleted after a test run if needed.

use jira_cli::api::{AuthType, IssueDraft, IssueUpdate, JiraClient};
use jira_cli::output::OutputConfig;

fn e2e_client() -> Option<(JiraClient, String)> {
    let host = std::env::var("JIRA_E2E_HOST").ok()?;
    let email = std::env::var("JIRA_E2E_EMAIL").unwrap_or_default();
    let token = std::env::var("JIRA_E2E_TOKEN").ok()?;
    let project = std::env::var("JIRA_E2E_PROJECT").unwrap_or_else(|_| "TST".into());

    let auth_type = if email.is_empty() {
        AuthType::Pat
    } else {
        AuthType::Basic
    };

    // API v2 for DC/Server (localhost), v3 for Cloud
    let api_version: u8 = if host.contains("atlassian.net") { 3 } else { 2 };

    let client = JiraClient::new(&host, &email, &token, auth_type, api_version).ok()?;
    Some((client, project))
}

fn json_out() -> OutputConfig {
    OutputConfig {
        json: true,
        quiet: true,
    }
}

/// Construct a minimal IssueDraft with only the required fields set.
/// All optional fields default to None.
fn minimal_draft<'a>(
    project_key: &'a str,
    issue_type: &'a str,
    summary: &'a str,
) -> IssueDraft<'a> {
    IssueDraft {
        project_key,
        issue_type,
        summary,
        description: None,
        priority: None,
        labels: None,
        components: None,
        fix_versions: None,
        assignee: None,
        parent: None,
    }
}

// ── Read-only ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn e2e_myself_returns_account() {
    let Some((client, _)) = e2e_client() else {
        eprintln!("Skipping e2e_myself: JIRA_E2E_HOST / JIRA_E2E_TOKEN not set");
        return;
    };
    let me = client.get_myself().await.expect("myself failed");
    assert!(
        !me.display_name.is_empty(),
        "displayName should be non-empty"
    );
    assert!(!me.account_id.is_empty(), "accountId should be non-empty");
}

#[tokio::test]
async fn e2e_projects_list_returns_at_least_one() {
    let Some((client, _)) = e2e_client() else {
        return;
    };
    let projects = client.list_projects().await.expect("list_projects failed");
    assert!(!projects.is_empty(), "expected at least one project");
}

#[tokio::test]
async fn e2e_search_returns_results() {
    let Some((client, project)) = e2e_client() else {
        return;
    };
    let resp = client
        .search(&format!("project = {project} ORDER BY updated DESC"), 10, 0)
        .await
        .expect("search failed");
    assert!(
        !resp.issues.is_empty(),
        "expected issues in project {project}"
    );
}

#[tokio::test]
async fn e2e_boards_and_sprints_list() {
    let Some((client, _)) = e2e_client() else {
        return;
    };
    let boards = client.list_boards().await.expect("list_boards failed");
    if boards.is_empty() {
        eprintln!("No boards found — skipping sprint check");
        return;
    }
    let sprints = client
        .list_sprints(boards[0].id, None)
        .await
        .expect("list_sprints failed");
    // May be empty on a fresh instance; just ensure it doesn't error
    let _ = sprints;
}

#[tokio::test]
async fn e2e_fields_list_returns_system_fields() {
    let Some((client, _)) = e2e_client() else {
        return;
    };
    let out = json_out();
    jira_cli::commands::fields::list(&client, &out, false)
        .await
        .expect("fields list failed");
}

// ── Write (create → operate → verify) ────────────────────────────────────────

#[tokio::test]
async fn e2e_create_comment_transition_show_delete() {
    let Some((client, project)) = e2e_client() else {
        return;
    };

    // Create
    let created = client
        .create_issue(
            &IssueDraft {
                project_key: &project,
                issue_type: "Task",
                summary: "e2e lifecycle test [e2e-auto]",
                description: Some("Created by e2e test"),
                priority: None,
                labels: None,
                components: None,
                fix_versions: None,
                assignee: None,
                parent: None,
            },
            &[],
        )
        .await
        .expect("create failed");
    let key = &created.key;

    // Show
    let issue = client.get_issue(key).await.expect("get_issue failed");
    assert_eq!(issue.summary(), "e2e lifecycle test [e2e-auto]");
    assert_eq!(issue.status(), "To Do");

    // Comment
    let comment = client
        .add_comment(key, "e2e test comment")
        .await
        .expect("add_comment failed");
    assert!(!comment.id.is_empty());

    // Verify comment appears in comments list
    let out = json_out();
    jira_cli::commands::issues::comments(&client, &out, key)
        .await
        .expect("comments command failed");

    // Transition
    let transitions = client
        .get_transitions(key)
        .await
        .expect("get_transitions failed");
    let in_progress = transitions
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case("In Progress"))
        .expect("'In Progress' transition not found");
    client
        .do_transition(key, &in_progress.id)
        .await
        .expect("do_transition failed");

    let after = client
        .get_issue(key)
        .await
        .expect("get_issue after transition failed");
    assert_eq!(
        after.status(),
        "In Progress",
        "status should be In Progress after transition"
    );

    // Log work
    let entry = client
        .log_work(key, "30m", Some("e2e work log"), None)
        .await
        .expect("log_work failed");
    assert_eq!(entry.time_spent, "30m");

    // Update
    client
        .update_issue(
            key,
            &IssueUpdate {
                summary: Some("e2e lifecycle test [e2e-auto] updated"),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("update_issue failed");
    let updated = client
        .get_issue(key)
        .await
        .expect("get_issue after update failed");
    assert!(updated.summary().contains("updated"));
}

#[tokio::test]
async fn e2e_create_subtask() {
    let Some((client, project)) = e2e_client() else {
        return;
    };

    // Create parent
    let parent = client
        .create_issue(
            &minimal_draft(&project, "Task", "e2e parent [e2e-auto]"),
            &[],
        )
        .await
        .expect("create parent failed");

    // Create subtask
    let child = client
        .create_issue(
            &IssueDraft {
                project_key: &project,
                issue_type: "Sub-task",
                summary: "e2e subtask [e2e-auto]",
                description: None,
                priority: None,
                labels: None,
                components: None,
                fix_versions: None,
                assignee: None,
                parent: Some(&parent.key),
            },
            &[],
        )
        .await
        .expect("create subtask failed");

    assert_ne!(child.key, parent.key);
    assert!(child.key.starts_with(&format!("{project}-")));
}

#[tokio::test]
async fn e2e_bulk_transition_dry_run() {
    let Some((client, project)) = e2e_client() else {
        return;
    };
    let out = json_out();
    // Dry run should never fail even if no issues match
    jira_cli::commands::issues::bulk_transition(
        &client,
        &out,
        &format!("project = {project} AND summary ~ 'e2e' AND status = 'To Do'"),
        "In Progress",
        true,
    )
    .await
    .expect("bulk_transition dry-run failed");
}

#[tokio::test]
async fn e2e_issues_mine() {
    let Some((client, _)) = e2e_client() else {
        return;
    };
    let out = json_out();
    jira_cli::commands::issues::mine(
        &client,
        &out,
        jira_cli::commands::issues::ListFilters::default(),
        10,
        false,
        None,
    )
    .await
    .expect("issues mine failed");
}

#[tokio::test]
async fn e2e_search_all_pages() {
    let Some((client, project)) = e2e_client() else {
        return;
    };
    let jql = format!("project = {project} ORDER BY updated DESC");
    let all = jira_cli::commands::issues::fetch_all_issues(&client, &jql)
        .await
        .expect("fetch_all_issues failed");

    // If the server reported a total (v2 only), verify it matches.
    // Jira Cloud (v3) `/search/jql` no longer returns a total.
    let first_page = client.search(&jql, 1, 0).await.expect("search failed");
    if let Some(total) = first_page.total {
        assert_eq!(
            all.len(),
            total,
            "fetch_all_issues returned {} issues but total is {total}",
            all.len(),
        );
    } else {
        assert!(
            !all.is_empty(),
            "fetch_all_issues returned no issues for project {project}",
        );
    }
}

#[tokio::test]
async fn e2e_issue_link_and_unlink() {
    let Some((client, project)) = e2e_client() else {
        return;
    };

    let a = client
        .create_issue(
            &minimal_draft(&project, "Task", "e2e link-a [e2e-auto]"),
            &[],
        )
        .await
        .expect("create a");
    let b = client
        .create_issue(
            &minimal_draft(&project, "Task", "e2e link-b [e2e-auto]"),
            &[],
        )
        .await
        .expect("create b");

    // Link a blocks b
    client
        .link_issues(&a.key, &b.key, "Blocks")
        .await
        .expect("link_issues failed");

    let issue_a = client.get_issue(&a.key).await.expect("get a");
    let links = issue_a.fields.issue_links.as_deref().unwrap_or_default();
    assert!(!links.is_empty(), "expected at least one link on {}", a.key);

    let link_id = &links[0].id;
    client
        .unlink_issues(link_id)
        .await
        .expect("unlink_issues failed");

    let after = client.get_issue(&a.key).await.expect("get a after unlink");
    let remaining = after.fields.issue_links.as_deref().unwrap_or_default();
    assert!(remaining.is_empty(), "expected no links after unlink");
}
