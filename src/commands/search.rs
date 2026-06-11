use crate::api::{ApiError, JiraClient};
use crate::output::OutputConfig;

use super::issues::{fetch_all_issues, filter_fields, issue_to_json, render_issue_table};

/// Run a raw JQL search and render the results.
///
/// The JQL string is passed verbatim to the Jira search API - no clauses or
/// ORDER BY are appended. Use `issues list` for a filtered view with automatic
/// ordering.
pub async fn run(
    client: &JiraClient,
    out: &OutputConfig,
    jql: &str,
    limit: usize,
    offset: usize,
    all: bool,
    fields: Option<&[String]>,
) -> Result<(), ApiError> {
    if all {
        let issues = fetch_all_issues(client, jql).await?;
        let count = issues.len();
        if out.json {
            let items: Vec<serde_json::Value> = issues
                .iter()
                .map(|i| filter_fields(issue_to_json(i, client), fields))
                .collect();
            out.print_data(
                &serde_json::to_string_pretty(&serde_json::json!({
                    "items": items,
                    "total": count,
                    "startAt": 0,
                    "maxResults": count,
                }))
                .expect("failed to serialize JSON"),
            );
        } else {
            render_issue_table(&issues, out);
            out.print_message(&format!("{count} issues"));
        }
        return Ok(());
    }

    let resp = client.search(jql, limit, offset).await?;

    if out.json {
        let total_json: serde_json::Value = match resp.total {
            Some(n) => serde_json::json!(n),
            None => serde_json::Value::Null,
        };
        let items: Vec<serde_json::Value> = resp
            .issues
            .iter()
            .map(|i| filter_fields(issue_to_json(i, client), fields))
            .collect();
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!({
                "items": items,
                "total": total_json,
                "startAt": resp.start_at,
                "maxResults": resp.max_results,
            }))
            .expect("failed to serialize JSON"),
        );
    } else {
        render_issue_table(&resp.issues, out);
        if !resp.is_last {
            match resp.total {
                Some(n) => out.print_message(&format!(
                    "Showing {}-{} of {} issues - use --limit/--offset or --all to paginate",
                    resp.start_at + 1,
                    resp.start_at + resp.issues.len(),
                    n
                )),
                None => out.print_message(&format!(
                    "Showing {}-{} issues (more available) - use --limit/--offset or --all to paginate",
                    resp.start_at + 1,
                    resp.start_at + resp.issues.len()
                )),
            }
        } else {
            out.print_message(&format!("{} issues", resp.issues.len()));
        }
    }
    Ok(())
}
