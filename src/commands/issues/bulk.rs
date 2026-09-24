use super::{fetch_all_issues, transitions};
use crate::api::{ApiError, JiraClient};
use crate::output::{OutputConfig, contract_for, error_envelope_for};
use serde_json::{Value, json};

fn failed_item(key: &str, error: &ApiError, phase: &str) -> Value {
    let envelope = error_envelope_for(error);
    let outcome = if phase == "write" && uncertain_response(error) {
        "unknown"
    } else {
        "failed"
    };
    let mut item = json!({"key": key, "ok": false, "error": error.to_string(),
        "errorKind": envelope["error"]["kind"], "retryable": contract_for(error).retryable, "phase":phase, "outcome":outcome});
    if let Some(details) = envelope["error"].get("details") {
        item["errorDetails"] = details.clone();
    }
    item
}

fn finish(
    out: &OutputConfig,
    dry_run: bool,
    results: Vec<Value>,
    succeeded: usize,
) -> Result<(), ApiError> {
    let total = results.len();
    let failed = results.iter().filter(|r| r["ok"] == false).count();
    let not_attempted = results.iter().filter(|r| r["notAttempted"] == true).count();
    let ready = if dry_run {
        total - failed - not_attempted
    } else {
        0
    };
    let summary = json!({"dryRun": dry_run, "total": total, "succeeded": succeeded,
        "failed": failed, "notAttempted": not_attempted, "ready": ready, "issues": results});
    if !out.json {
        for item in summary["issues"].as_array().expect("result array") {
            let key = item["key"].as_str().unwrap_or("?");
            let detail = if item["ok"] == false {
                format!(
                    "{}: {}",
                    if item["outcome"] == "unknown" {
                        "UNKNOWN (check issue state)"
                    } else {
                        "FAILED"
                    },
                    item["error"].as_str().unwrap_or("unknown failure")
                )
            } else if item["notAttempted"] == true {
                "not attempted".to_owned()
            } else if dry_run {
                format!(
                    "{} {}{}",
                    item["action"].as_str().unwrap_or("ready"),
                    item["to"].as_str().unwrap_or(""),
                    item["transitionId"]
                        .as_str()
                        .map(|id| format!(" (transition {id})"))
                        .unwrap_or_default()
                )
            } else {
                "succeeded".to_owned()
            };
            out.print_data(&format!("{key}  {detail}"));
        }
    }
    let message = if dry_run {
        format!(
            "Dry run: {} of {total} issues ready; {failed} failed validation; {not_attempted} not attempted",
            ready
        )
    } else {
        format!(
            "Bulk operation: {succeeded} succeeded, {failed} failed, {not_attempted} not attempted out of {total}"
        )
    };
    out.print_result(&summary, &message);
    if failed > 0 {
        Err(ApiError::BulkFailure {
            total,
            succeeded,
            failed,
            not_attempted,
        })
    } else {
        Ok(())
    }
}

/// Each lookup and write belongs to one issue. Later failures never discard
/// completed results, and successful writes are never automatically retried.
pub async fn bulk_transition(
    client: &JiraClient,
    out: &OutputConfig,
    jql: &str,
    to: &str,
    dry_run: bool,
) -> Result<(), ApiError> {
    let issues = fetch_all_issues(client, jql).await?;
    let mut results = Vec::new();
    let mut succeeded = 0;
    let mut aborted = false;
    for issue in &issues {
        if aborted {
            results.push(json!({"key": issue.key, "notAttempted": true}));
            continue;
        }
        let mut phase = "lookup";
        let attempt = async {
            let available = client.get_transitions(&issue.key).await?;
            let selected = transitions::resolve(&issue.key, to, &available)?;
            if dry_run {
                return Ok(json!({"key": issue.key, "status": issue.status(),
                    "action": "would transition", "to": to, "transitionId": selected.id,
                    "destinationStatus": selected.to.as_ref().map(|s| &s.name)}));
            }
            phase = "write";
            client.do_transition(&issue.key, &selected.id).await?;
            Ok::<_, ApiError>(
                json!({"key": issue.key, "from": issue.status(), "to": to, "ok": true,
                    "transitionId": selected.id, "destinationStatus": selected.to.as_ref().map(|s| &s.name)}),
            )
        }
        .await;
        match attempt {
            Ok(result) => {
                if !dry_run {
                    succeeded += 1;
                }
                results.push(result);
            }
            Err(error) => {
                aborted = aborts_run(&error);
                results.push(failed_item(&issue.key, &error, phase));
            }
        }
    }
    finish(out, dry_run, results, succeeded)
}

pub async fn bulk_assign(
    client: &JiraClient,
    out: &OutputConfig,
    jql: &str,
    assignee: &str,
    dry_run: bool,
) -> Result<(), ApiError> {
    let account_id = super::resolve_assignee_arg(client, Some(assignee))
        .await?
        .flatten();
    let issues = fetch_all_issues(client, jql).await?;
    let mut results = Vec::new();
    let mut succeeded = 0;
    let mut aborted = false;
    for issue in &issues {
        if aborted {
            results.push(json!({"key": issue.key, "notAttempted": true}));
            continue;
        }
        if dry_run {
            results.push(json!({"key": issue.key,
                "currentAssignee": issue.fields.assignee.as_ref().map(|a| &a.display_name),
                "action": "would assign", "to": assignee}));
            continue;
        }
        match client.assign_issue(&issue.key, account_id.as_deref()).await {
            Ok(()) => {
                succeeded += 1;
                results.push(json!({"key": issue.key, "assignee": assignee, "ok": true}));
            }
            Err(error) => {
                aborted = aborts_run(&error);
                results.push(failed_item(&issue.key, &error, "write"));
            }
        }
    }
    finish(out, dry_run, results, succeeded)
}

// Stop after failures that affect the connection or entire Jira session. Auth
// includes forbidden responses: conservatively stop rather than repeat writes.
fn aborts_run(error: &ApiError) -> bool {
    match error {
        ApiError::WithDetails { source, .. } => aborts_run(source),
        ApiError::Auth { .. }
        | ApiError::Forbidden(_)
        | ApiError::RateLimit
        | ApiError::Http(_) => true,
        ApiError::Api { status, .. } => *status >= 500,
        _ => false,
    }
}

fn uncertain_response(error: &ApiError) -> bool {
    match error {
        ApiError::WithDetails { source, .. } => uncertain_response(source),
        ApiError::Http(_) => true,
        ApiError::Api { status, .. } => *status >= 500,
        _ => false,
    }
}
