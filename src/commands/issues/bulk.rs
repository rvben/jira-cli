use super::transitions;
use crate::api::{ApiError, JiraClient, TransitionInput};
use crate::confirm::{self, Consent};
use crate::output::{OutputConfig, contract_for, error_envelope_for};
use serde_json::{Value, json};

fn failed_item(key: &str, error: &ApiError, phase: &str) -> Value {
    let envelope = error_envelope_for(error);
    let outcome = if phase == "write" && error.write_outcome_unknown() {
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
                let mut detail = item["action"].as_str().unwrap_or("ready").to_owned();
                if let Some(to) = item["to"].as_str() {
                    detail.push_str(&format!(" {to}"));
                }
                if let Some(id) = item["transitionId"].as_str() {
                    detail.push_str(&format!(" (transition {id})"));
                }
                if let Some(summary) = item["summary"].as_str() {
                    detail.push_str(&format!(" {summary:?}"));
                }
                if let Some(issue_type) = item["issueType"].as_str() {
                    detail.push_str(&format!(" [{issue_type}]"));
                }
                if let Some(subtasks) = item["subtasks"].as_array().filter(|s| !s.is_empty()) {
                    let keys: Vec<&str> = subtasks.iter().filter_map(Value::as_str).collect();
                    detail.push_str(&format!(" and its subtasks {}", keys.join(", ")));
                }
                if let Some(parent) = item["deletedWith"].as_str() {
                    detail.push_str(&format!(" (with {parent})"));
                }
                detail
            } else if let Some(parent) = item["deletedWith"].as_str() {
                format!("deleted with {parent}")
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
    input: &TransitionInput<'_>,
    dry_run: bool,
    consent: Consent,
) -> Result<(), ApiError> {
    let issues = client.search_all(jql).await?;
    if !dry_run && !issues.is_empty() {
        let keys: Vec<&str> = issues.iter().map(|i| i.key.as_str()).collect();
        let mut question = format!(
            "Transition {} ({}) to {to:?}",
            confirm::count(keys.len(), "issue"),
            confirm::sample(&keys)
        );
        let mut sets: Vec<String> = Vec::new();
        if let Some(resolution) = input.resolution {
            sets.push(format!("resolution={resolution}"));
        }
        sets.extend(input.fields.iter().map(|(name, value)| match value {
            Value::String(text) => format!("{name}={text}"),
            other => format!("{name}={other}"),
        }));
        if !sets.is_empty() {
            question.push_str(&format!(", setting {}", sets.join(", ")));
        }
        if input.comment.is_some() {
            question.push_str(", and comment on each");
        }
        consent.confirm(&format!("{question}?"))?;
    }
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
            let prepared = client.prepare_transition(&issue.key, selected, input).await?;
            if dry_run {
                return Ok(json!({"key": issue.key, "status": issue.status(),
                    "action": "would transition", "to": to, "transitionId": selected.id,
                    "destinationStatus": selected.to.as_ref().map(|s| &s.name),
                    "fields": prepared.fields, "warnings": prepared.warnings}));
            }
            phase = "write";
            let comment = client.transition_issue(&issue.key, &prepared).await?;
            Ok::<_, ApiError>(
                json!({"key": issue.key, "from": issue.status(), "to": to, "ok": true,
                    "transitionId": selected.id, "destinationStatus": selected.to.as_ref().map(|s| &s.name),
                    "fields": prepared.fields, "commentId": comment.map(|c| c.id),
                    "warnings": prepared.warnings}),
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
                if transitioned(&error) {
                    phase = "comment";
                }
                results.push(failed_item(&issue.key, &error, phase));
            }
        }
    }
    finish(out, dry_run, results, succeeded)
}

/// Whether the transition was applied and only its follow-up comment failed.
fn transitioned(error: &ApiError) -> bool {
    matches!(error, ApiError::WithDetails { details, .. } if details["transitioned"] == true)
}

pub async fn bulk_assign(
    client: &JiraClient,
    out: &OutputConfig,
    jql: &str,
    assignee: &str,
    dry_run: bool,
    consent: Consent,
) -> Result<(), ApiError> {
    let account_id = super::resolve_assignee_arg(client, Some(assignee))
        .await?
        .flatten();
    let issues = client.search_all(jql).await?;
    if !dry_run && !issues.is_empty() {
        let keys: Vec<&str> = issues.iter().map(|i| i.key.as_str()).collect();
        let selection = format!(
            "{} ({})",
            confirm::count(keys.len(), "issue"),
            confirm::sample(&keys)
        );
        consent.confirm(&match &account_id {
            Some(id) => format!("Assign {selection} to {assignee:?} (account {id})?"),
            None => format!("Unassign {selection}?"),
        })?;
    }
    let mut results = Vec::new();
    let mut succeeded = 0;
    let mut aborted = false;
    for issue in &issues {
        if aborted {
            results.push(json!({"key": issue.key, "notAttempted": true}));
            continue;
        }
        if dry_run {
            results.push(
                match client.check_assign(&issue.key, account_id.as_deref()).await {
                    Ok(warnings) => json!({"key": issue.key,
                    "currentAssignee": issue.fields.assignee.as_ref().map(|a| &a.display_name),
                    "action": "would assign", "to": assignee, "accountId": account_id,
                    "warnings": warnings}),
                    Err(error) => {
                        aborted = aborts_run(&error);
                        failed_item(&issue.key, &error, "lookup")
                    }
                },
            );
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

/// Delete issues by key. A deleted issue cannot be restored, so every key is
/// checked first and nothing is deleted unless all of them pass. A subtask
/// listed with its parent is removed by the parent's delete and reported as
/// `deletedWith` that parent rather than deleted a second time.
pub async fn delete_issues(
    client: &JiraClient,
    out: &OutputConfig,
    keys: &[String],
    delete_subtasks: bool,
    dry_run: bool,
    consent: Consent,
) -> Result<(), ApiError> {
    let mut unique: Vec<&str> = Vec::new();
    for key in keys {
        if !unique.contains(&key.as_str()) {
            unique.push(key.as_str());
        }
    }

    let mut checked = Vec::new();
    let mut aborted = false;
    for key in &unique {
        if aborted {
            checked.push(Err(None));
            continue;
        }
        let check = async {
            let target = client.preview_delete_issue(key).await?;
            if !delete_subtasks && !target.subtasks.is_empty() {
                return Err(ApiError::InvalidInput(format!(
                    "{key} has {} subtask(s) ({}); pass --delete-subtasks to delete them with it",
                    target.subtasks.len(),
                    target.subtasks.join(", ")
                )));
            }
            Ok(target)
        }
        .await;
        checked.push(check.map_err(|error| {
            aborted = aborts_run(&error);
            Some(error)
        }));
    }

    let parent_of = |key: &str| {
        unique.iter().zip(&checked).find_map(|(parent, check)| {
            check
                .as_ref()
                .ok()
                .filter(|target| target.subtasks.iter().any(|s| s == key))
                .map(|_| *parent)
        })
    };
    let all_passed = checked.iter().all(Result::is_ok);
    if !dry_run && all_passed {
        let listed: Vec<&str> = unique
            .iter()
            .copied()
            .filter(|key| parent_of(key).is_none())
            .collect();
        let subtasks: Vec<&str> = checked
            .iter()
            .flatten()
            .flat_map(|target| target.subtasks.iter().map(String::as_str))
            .collect();
        let mut question = format!(
            "Permanently delete {} ({})",
            confirm::count(listed.len(), "issue"),
            confirm::sample(&listed)
        );
        // Jira deletes the subtasks present when the delete runs, not those
        // seen here, so the question covers every subtask under the issues.
        if delete_subtasks {
            let under = if listed.len() == 1 { "it" } else { "them" };
            question.push_str(&format!(" and every subtask under {under}"));
            if !subtasks.is_empty() {
                question.push_str(&format!(
                    ", now {} ({})",
                    confirm::count(subtasks.len(), "subtask"),
                    confirm::sample(&subtasks)
                ));
            }
        }
        consent.confirm(&format!("{question}? Deleted issues cannot be restored."))?;
    }

    let mut results: Vec<Value> = Vec::new();
    let mut succeeded = 0;
    for (key, check) in unique.iter().zip(&checked) {
        let target = match check {
            Err(Some(error)) => {
                results.push(failed_item(key, error, "lookup"));
                continue;
            }
            Err(None) => {
                results.push(json!({"key": key, "notAttempted": true}));
                continue;
            }
            Ok(target) => target,
        };
        let parent = parent_of(key);
        if dry_run {
            let mut entry = json!({"key": key, "summary": target.summary,
                "issueType": target.issue_type, "subtasks": target.subtasks,
                "action": "would delete", "warnings": target.warnings});
            if let Some(parent) = parent {
                entry["deletedWith"] = json!(parent);
            }
            results.push(entry);
            continue;
        }
        if !all_passed || aborted {
            results.push(json!({"key": key, "notAttempted": true}));
            continue;
        }
        if parent.is_some() {
            // Settled below, once the parent's delete has run.
            results.push(Value::Null);
            continue;
        }
        match client.delete_issue(key, delete_subtasks).await {
            Ok(()) => {
                succeeded += 1;
                results.push(json!({"key": key, "ok": true,
                    "subtasksDeleted": target.subtasks}));
            }
            Err(error) => {
                aborted = aborts_run(&error);
                results.push(failed_item(key, &error, "write"));
            }
        }
    }

    // A subtask listed with its parent shares the parent's fate.
    for index in 0..results.len() {
        if !results[index].is_null() {
            continue;
        }
        let key = unique[index];
        let parent = parent_of(key).expect("only subtasks of a listed parent are deferred");
        let parent_index = unique.iter().position(|k| *k == parent).expect("listed");
        results[index] = match &results[parent_index] {
            done if done["ok"] == true => {
                succeeded += 1;
                json!({"key": key, "ok": true, "deletedWith": parent})
            }
            failed if failed["outcome"] == "unknown" => {
                let mut item = failed.clone();
                item["key"] = json!(key);
                item["deletedWith"] = json!(parent);
                item
            }
            _ => json!({"key": key, "notAttempted": true}),
        };
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
