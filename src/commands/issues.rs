use std::path::{Path, PathBuf};

use owo_colors::OwoColorize;

use crate::api::{
    ApiError, Attachment, Issue, IssueDraft, IssueLink, IssueUpdate, JiraClient, Version,
    escape_jql,
};
use crate::output::{OutputConfig, use_color};

/// Filter set shared by `issues list` and `issues mine`.
///
/// Each `Option<&str>` field maps to one CLI flag and emits one JQL clause when set.
/// `components`, `labels`, and `fix_versions` are slices because the CLI accepts those flags repeatably.
#[derive(Default)]
pub struct ListFilters<'a> {
    pub project: Option<&'a str>,
    pub status: Option<&'a str>,
    pub assignee: Option<&'a str>,
    pub issue_type: Option<&'a str>,
    pub sprint: Option<&'a str>,
    pub components: Option<&'a [&'a str]>,
    pub labels: Option<&'a [&'a str]>,
    pub fix_versions: Option<&'a [&'a str]>,
    pub jql_extra: Option<&'a str>,
}

pub async fn list(
    client: &JiraClient,
    out: &OutputConfig,
    filters: ListFilters<'_>,
    limit: usize,
    offset: usize,
    all: bool,
    fields: Option<&[String]>,
) -> Result<(), ApiError> {
    let jql = build_list_jql(&filters);
    if all {
        let issues = fetch_all_issues(client, &jql).await?;
        let n = issues.len();
        render_results(
            out,
            &issues,
            PageInfo {
                total: Some(n),
                start_at: 0,
                max_results: n,
                more: false,
            },
            client,
            fields,
        );
    } else {
        let resp = client.search(&jql, limit, offset).await?;
        let more = !resp.is_last;
        render_results(
            out,
            &resp.issues,
            PageInfo {
                total: resp.total,
                start_at: resp.start_at,
                max_results: resp.max_results,
                more,
            },
            client,
            fields,
        );
    }
    Ok(())
}

/// List issues assigned to the current user.
pub async fn mine(
    client: &JiraClient,
    out: &OutputConfig,
    mut filters: ListFilters<'_>,
    limit: usize,
    all: bool,
    fields: Option<&[String]>,
) -> Result<(), ApiError> {
    filters.assignee = Some("me");
    list(client, out, filters, limit, 0, all, fields).await
}

/// List comments on an issue.
pub async fn comments(client: &JiraClient, out: &OutputConfig, key: &str) -> Result<(), ApiError> {
    let issue = client.get_issue(key).await?;
    let comment_list = issue.fields.comment.as_ref();

    if out.json {
        let comments_json: Vec<serde_json::Value> = comment_list
            .map(|cl| {
                cl.comments
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "id": c.id,
                            "author": {
                                "displayName": c.author.display_name,
                                "accountId": c.author.account_id,
                            },
                            "body": c.body_text(),
                            "created": c.created,
                            "updated": c.updated,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let total = comment_list.map(|cl| cl.total).unwrap_or(0);
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!({
                "issue": key,
                "total": total,
                "comments": comments_json,
            }))
            .expect("failed to serialize JSON"),
        );
    } else {
        match comment_list {
            None => {
                out.print_message(&format!("No comments on {key}."));
            }
            Some(cl) if cl.comments.is_empty() => {
                out.print_message(&format!("No comments on {key}."));
            }
            Some(cl) => {
                let color = use_color();
                out.print_message(&format!("Comments on {key} ({}):", cl.total));
                for c in &cl.comments {
                    println!();
                    let author = if color {
                        c.author.display_name.bold().to_string()
                    } else {
                        c.author.display_name.clone()
                    };
                    println!("  {} — {}", author, format_date(&c.created));
                    for line in c.body_text().lines() {
                        println!("    {line}");
                    }
                }
            }
        }
    }
    Ok(())
}

/// Fetch every page of a JQL search, returning all issues.
pub async fn fetch_all_issues(client: &JiraClient, jql: &str) -> Result<Vec<Issue>, ApiError> {
    const PAGE_SIZE: usize = 100;
    let mut all: Vec<Issue> = Vec::new();
    let mut offset = 0;
    loop {
        let resp = client.search(jql, PAGE_SIZE, offset).await?;
        let fetched = resp.issues.len();
        all.extend(resp.issues);
        offset += fetched;
        if resp.is_last || fetched == 0 {
            break;
        }
    }
    Ok(all)
}

struct PageInfo {
    total: Option<usize>,
    start_at: usize,
    max_results: usize,
    more: bool,
}

fn render_results(
    out: &OutputConfig,
    issues: &[Issue],
    page: PageInfo,
    client: &JiraClient,
    fields: Option<&[String]>,
) {
    if out.json {
        let total_json: serde_json::Value = match page.total {
            Some(n) => serde_json::json!(n),
            None => serde_json::Value::Null,
        };
        let items: Vec<serde_json::Value> = issues
            .iter()
            .map(|i| filter_fields(issue_to_json(i, client), fields))
            .collect();
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!({
                "items": items,
                "total": total_json,
                "startAt": page.start_at,
                "maxResults": page.max_results,
            }))
            .expect("failed to serialize JSON"),
        );
    } else {
        render_issue_table(issues, out);
        if page.more {
            match page.total {
                Some(n) => out.print_message(&format!(
                    "Showing {}-{} of {} issues - use --limit/--offset or --all to paginate",
                    page.start_at + 1,
                    page.start_at + issues.len(),
                    n
                )),
                None => out.print_message(&format!(
                    "Showing {}-{} issues (more available) - use --limit/--offset or --all to paginate",
                    page.start_at + 1,
                    page.start_at + issues.len()
                )),
            }
        } else {
            out.print_message(&format!("{} issues", issues.len()));
        }
    }
}

/// Retain only the requested fields from a JSON object. When `fields` is `None`
/// or empty, the original value is returned unchanged.
pub fn filter_fields(mut value: serde_json::Value, fields: Option<&[String]>) -> serde_json::Value {
    let Some(names) = fields else { return value };
    if names.is_empty() {
        return value;
    }
    if let Some(obj) = value.as_object_mut() {
        obj.retain(|k, _| names.iter().any(|f| f == k));
    }
    value
}

pub async fn show(
    client: &JiraClient,
    out: &OutputConfig,
    key: &str,
    open: bool,
) -> Result<(), ApiError> {
    let issue = client.get_issue(key).await?;

    if open {
        open_in_browser(&client.browse_url(&issue.key));
    }

    if out.json {
        out.print_data(
            &serde_json::to_string_pretty(&issue_detail_to_json(&issue, client))
                .expect("failed to serialize JSON"),
        );
    } else {
        render_issue_detail(&issue);
    }
    Ok(())
}

pub async fn create(
    client: &JiraClient,
    out: &OutputConfig,
    draft: &IssueDraft<'_>,
    sprint: Option<&str>,
    custom_fields: &[(String, serde_json::Value)],
) -> Result<(), ApiError> {
    let resp = client.create_issue(draft, custom_fields).await?;
    let url = client.browse_url(&resp.key);

    let mut result = serde_json::json!({ "key": resp.key, "id": resp.id, "url": url });
    if let Some(p) = draft.parent {
        result["parent"] = serde_json::json!(p);
    }
    if let Some(s) = sprint {
        let resolved = client.resolve_sprint(s).await?;
        client.move_issue_to_sprint(&resp.key, resolved.id).await?;
        result["sprintId"] = serde_json::json!(resolved.id);
        result["sprintName"] = serde_json::json!(resolved.name);
    }
    out.print_result(&result, &resp.key);
    Ok(())
}

pub async fn update(
    client: &JiraClient,
    out: &OutputConfig,
    key: &str,
    update: &IssueUpdate<'_>,
    custom_fields: &[(String, serde_json::Value)],
) -> Result<(), ApiError> {
    client.update_issue(key, update, custom_fields).await?;
    out.print_result(
        &serde_json::json!({ "key": key, "updated": true }),
        &format!("Updated {key}"),
    );
    Ok(())
}

/// Move an issue to a sprint.
pub async fn move_to_sprint(
    client: &JiraClient,
    out: &OutputConfig,
    key: &str,
    sprint: &str,
) -> Result<(), ApiError> {
    let resolved = client.resolve_sprint(sprint).await?;
    client.move_issue_to_sprint(key, resolved.id).await?;
    out.print_result(
        &serde_json::json!({
            "issue": key,
            "sprintId": resolved.id,
            "sprintName": resolved.name,
        }),
        &format!("Moved {key} to {} ({})", resolved.name, resolved.id),
    );
    Ok(())
}

pub async fn comment(
    client: &JiraClient,
    out: &OutputConfig,
    key: &str,
    body: &str,
) -> Result<(), ApiError> {
    let c = client.add_comment(key, body).await?;
    let url = client.browse_url(key);
    out.print_result(
        &serde_json::json!({
            "id": c.id,
            "issue": key,
            "url": url,
            "author": c.author.display_name,
            "created": c.created,
        }),
        &format!("Comment added to {key}"),
    );
    Ok(())
}

pub async fn transition(
    client: &JiraClient,
    out: &OutputConfig,
    key: &str,
    to: &str,
) -> Result<(), ApiError> {
    let transitions = client.get_transitions(key).await?;

    let matched = transitions
        .iter()
        .find(|t| t.name.to_lowercase() == to.to_lowercase() || t.id == to);

    match matched {
        Some(t) => {
            let name = t.name.clone();
            let id = t.id.clone();
            let status =
                t.to.as_ref()
                    .map(|tt| tt.name.clone())
                    .unwrap_or_else(|| name.clone());
            client.do_transition(key, &id).await?;
            out.print_result(
                &serde_json::json!({ "issue": key, "transition": name, "status": status, "id": id }),
                &format!("Transitioned {key} → {status}"),
            );
        }
        None => {
            let hint = transitions
                .iter()
                .map(|t| format!("  {} ({})", t.name, t.id))
                .collect::<Vec<_>>()
                .join("\n");
            out.print_message(&format!(
                "Transition '{to}' not found for {key}. Available:\n{hint}"
            ));
            out.print_message(&format!(
                "Tip: `jira issues list-transitions {key}` shows transitions as JSON."
            ));
            return Err(ApiError::NotFound(format!(
                "Transition '{to}' not found for {key}"
            )));
        }
    }
    Ok(())
}

pub async fn list_transitions(
    client: &JiraClient,
    out: &OutputConfig,
    key: &str,
) -> Result<(), ApiError> {
    let ts = client.get_transitions(key).await?;

    if out.json {
        out.print_data(&serde_json::to_string_pretty(&ts).expect("failed to serialize JSON"));
    } else {
        let color = use_color();
        let header = format!("{:<6} {}", "ID", "Name");
        if color {
            println!("{}", header.bold());
        } else {
            println!("{header}");
        }
        for t in &ts {
            println!("{:<6} {}", t.id, t.name);
        }
    }
    Ok(())
}

pub async fn assign(
    client: &JiraClient,
    out: &OutputConfig,
    key: &str,
    assignee: &str,
) -> Result<(), ApiError> {
    let account_id = if assignee == "me" {
        let me = client.get_myself().await?;
        me.account_id
    } else if assignee == "none" || assignee == "unassign" {
        client.assign_issue(key, None).await?;
        out.print_result(
            &serde_json::json!({ "issue": key, "assignee": null }),
            &format!("Unassigned {key}"),
        );
        return Ok(());
    } else {
        assignee.to_string()
    };

    client.assign_issue(key, Some(&account_id)).await?;
    out.print_result(
        &serde_json::json!({ "issue": key, "accountId": account_id }),
        &format!("Assigned {key} to {assignee}"),
    );
    Ok(())
}

/// List available issue link types.
pub async fn link_types(client: &JiraClient, out: &OutputConfig) -> Result<(), ApiError> {
    let types = client.get_link_types().await?;

    if out.json {
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!(
                types
                    .iter()
                    .map(|t| serde_json::json!({
                        "id": t.id,
                        "name": t.name,
                        "inward": t.inward,
                        "outward": t.outward,
                    }))
                    .collect::<Vec<_>>()
            ))
            .expect("failed to serialize JSON"),
        );
        return Ok(());
    }

    for t in &types {
        println!(
            "{:<20}  outward: {}  /  inward: {}",
            t.name, t.outward, t.inward
        );
    }
    Ok(())
}

/// Link two issues.
pub async fn link(
    client: &JiraClient,
    out: &OutputConfig,
    from_key: &str,
    to_key: &str,
    link_type: &str,
) -> Result<(), ApiError> {
    client.link_issues(from_key, to_key, link_type).await?;
    out.print_result(
        &serde_json::json!({
            "from": from_key,
            "to": to_key,
            "type": link_type,
        }),
        &format!("Linked {from_key} → {to_key} ({link_type})"),
    );
    Ok(())
}

/// Remove an issue link by link ID.
pub async fn unlink(
    client: &JiraClient,
    out: &OutputConfig,
    link_id: &str,
) -> Result<(), ApiError> {
    client.unlink_issues(link_id).await?;
    out.print_result(
        &serde_json::json!({ "linkId": link_id }),
        &format!("Removed link {link_id}"),
    );
    Ok(())
}

/// Log work (time) on an issue.
pub async fn log_work(
    client: &JiraClient,
    out: &OutputConfig,
    key: &str,
    time_spent: &str,
    comment: Option<&str>,
    started: Option<&str>,
) -> Result<(), ApiError> {
    let entry = client.log_work(key, time_spent, comment, started).await?;
    out.print_result(
        &serde_json::json!({
            "id": entry.id,
            "issue": key,
            "timeSpent": entry.time_spent,
            "timeSpentSeconds": entry.time_spent_seconds,
            "author": entry.author.display_name,
            "started": entry.started,
            "created": entry.created,
        }),
        &format!("Logged {} on {key}", entry.time_spent),
    );
    Ok(())
}

/// List the attachments on an issue.
pub async fn attachments(
    client: &JiraClient,
    out: &OutputConfig,
    key: &str,
) -> Result<(), ApiError> {
    let attachments = client.list_attachments(key).await?;

    if out.json {
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!({
                "issue": key,
                "total": attachments.len(),
                "attachments": attachments.iter().map(attachment_to_json).collect::<Vec<_>>(),
            }))
            .expect("failed to serialize JSON"),
        );
        return Ok(());
    }

    if attachments.is_empty() {
        out.print_message(&format!("No attachments on {key}."));
        return Ok(());
    }

    let color = use_color();
    let sizes: Vec<String> = attachments.iter().map(|a| format_size(a.size)).collect();
    let id_w = col_width("ID", attachments.iter().map(|a| a.id.len()));
    let name_w = col_width("Filename", attachments.iter().map(|a| a.filename.len()));
    let size_w = col_width("Size", sizes.iter().map(String::len));
    let type_w = col_width("Type", attachments.iter().map(|a| a.mime_type().len()));
    let author_w = col_width("Author", attachments.iter().map(|a| a.author().len()));

    let header = format!(
        "{:<id_w$} {:<name_w$} {:<size_w$} {:<type_w$} {:<author_w$} {}",
        "ID", "Filename", "Size", "Type", "Author", "Created"
    );
    if color {
        println!("{}", header.bold());
    } else {
        println!("{header}");
    }

    for (a, size) in attachments.iter().zip(&sizes) {
        let id = if color {
            format!("{:<id_w$}", a.id).yellow().to_string()
        } else {
            format!("{:<id_w$}", a.id)
        };
        println!(
            "{id} {:<name_w$} {:<size_w$} {:<type_w$} {:<author_w$} {}",
            a.filename,
            size,
            a.mime_type(),
            a.author(),
            format_date(&a.created),
        );
    }
    out.print_message(&format!("{} attachments", attachments.len()));
    Ok(())
}

/// Upload one or more local files to an issue.
pub async fn attach(
    client: &JiraClient,
    out: &OutputConfig,
    key: &str,
    files: &[PathBuf],
) -> Result<(), ApiError> {
    let uploaded = client.upload_attachments(key, files).await?;
    let names: Vec<&str> = uploaded.iter().map(|a| a.filename.as_str()).collect();
    out.print_result(
        &serde_json::json!({
            "issue": key,
            "attachments": uploaded.iter().map(attachment_to_json).collect::<Vec<_>>(),
        }),
        &format!("Attached {} to {key}", names.join(", ")),
    );
    Ok(())
}

/// Download an attachment into `dir`, using the filename Jira reports for it.
///
/// The directory is created and the target checked for an existing file before
/// any content is fetched, so a refusal never happens after the download.
pub async fn download_attachment(
    client: &JiraClient,
    out: &OutputConfig,
    id: &str,
    dir: &Path,
    force: bool,
) -> Result<(), ApiError> {
    let attachment = client.get_attachment(id).await?;
    let file_name = safe_file_name(&attachment.filename).ok_or_else(|| {
        ApiError::Other(format!(
            "Attachment {id} has an unusable filename '{}'",
            attachment.filename
        ))
    })?;

    std::fs::create_dir_all(dir)
        .map_err(|e| ApiError::Other(format!("cannot create {}: {e}", dir.display())))?;

    let path = dir.join(file_name);
    if !force && path.exists() {
        return Err(ApiError::Conflict(format!(
            "{} already exists. Pass --force to overwrite.",
            path.display()
        )));
    }

    let data = client.download_attachment(id).await?;
    std::fs::write(&path, &data)
        .map_err(|e| ApiError::Other(format!("cannot write {}: {e}", path.display())))?;

    out.print_result(
        &serde_json::json!({
            "id": attachment.id,
            "filename": file_name,
            "path": path.display().to_string(),
            "size": data.len(),
        }),
        &format!("Downloaded {file_name} to {}", path.display()),
    );
    Ok(())
}

/// Delete an attachment by its ID.
pub async fn delete_attachment(
    client: &JiraClient,
    out: &OutputConfig,
    id: &str,
) -> Result<(), ApiError> {
    client.delete_attachment(id).await?;
    out.print_result(
        &serde_json::json!({ "id": id, "deleted": true }),
        &format!("Deleted attachment {id}"),
    );
    Ok(())
}

/// Transition all issues matching a JQL query to a new status.
pub async fn bulk_transition(
    client: &JiraClient,
    out: &OutputConfig,
    jql: &str,
    to: &str,
    dry_run: bool,
) -> Result<(), ApiError> {
    let issues = fetch_all_issues(client, jql).await?;

    if issues.is_empty() {
        out.print_message("No issues matched the query.");
        return Ok(());
    }

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for issue in &issues {
        if dry_run {
            results.push(serde_json::json!({
                "key": issue.key,
                "status": issue.status(),
                "action": "would transition",
                "to": to,
            }));
            continue;
        }

        let transitions = client.get_transitions(&issue.key).await?;
        let matched = transitions.iter().find(|t| {
            t.name.eq_ignore_ascii_case(to)
                || t.to
                    .as_ref()
                    .is_some_and(|tt| tt.name.eq_ignore_ascii_case(to))
                || t.id == to
        });

        match matched {
            Some(t) => match client.do_transition(&issue.key, &t.id).await {
                Ok(()) => {
                    succeeded += 1;
                    results.push(serde_json::json!({
                        "key": issue.key,
                        "from": issue.status(),
                        "to": to,
                        "ok": true,
                    }));
                }
                Err(e) => {
                    failed += 1;
                    results.push(serde_json::json!({
                        "key": issue.key,
                        "ok": false,
                        "error": e.to_string(),
                    }));
                }
            },
            None => {
                failed += 1;
                results.push(serde_json::json!({
                    "key": issue.key,
                    "ok": false,
                    "error": format!("transition '{to}' not available"),
                }));
            }
        }
    }

    if out.json {
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!({
                "dryRun": dry_run,
                "total": issues.len(),
                "succeeded": succeeded,
                "failed": failed,
                "issues": results,
            }))
            .expect("failed to serialize JSON"),
        );
    } else if dry_run {
        render_issue_table(&issues, out);
        out.print_message(&format!(
            "Dry run: {} issues would be transitioned to '{to}'",
            issues.len()
        ));
    } else {
        out.print_message(&format!(
            "Transitioned {succeeded}/{} issues to '{to}'{}",
            issues.len(),
            if failed > 0 {
                format!(" ({failed} failed)")
            } else {
                String::new()
            }
        ));
    }
    Ok(())
}

/// Assign all issues matching a JQL query to a user.
pub async fn bulk_assign(
    client: &JiraClient,
    out: &OutputConfig,
    jql: &str,
    assignee: &str,
    dry_run: bool,
) -> Result<(), ApiError> {
    // Resolve "me" once before the loop.
    let account_id: Option<String> = match assignee {
        "me" => {
            let me = client.get_myself().await?;
            Some(me.account_id)
        }
        "none" | "unassign" => None,
        id => Some(id.to_string()),
    };

    let issues = fetch_all_issues(client, jql).await?;

    if issues.is_empty() {
        out.print_message("No issues matched the query.");
        return Ok(());
    }

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut succeeded = 0usize;
    let mut failed = 0usize;

    for issue in &issues {
        if dry_run {
            results.push(serde_json::json!({
                "key": issue.key,
                "currentAssignee": issue.assignee(),
                "action": "would assign",
                "to": assignee,
            }));
            continue;
        }

        match client.assign_issue(&issue.key, account_id.as_deref()).await {
            Ok(()) => {
                succeeded += 1;
                results.push(serde_json::json!({
                    "key": issue.key,
                    "assignee": assignee,
                    "ok": true,
                }));
            }
            Err(e) => {
                failed += 1;
                results.push(serde_json::json!({
                    "key": issue.key,
                    "ok": false,
                    "error": e.to_string(),
                }));
            }
        }
    }

    if out.json {
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!({
                "dryRun": dry_run,
                "total": issues.len(),
                "succeeded": succeeded,
                "failed": failed,
                "issues": results,
            }))
            .expect("failed to serialize JSON"),
        );
    } else if dry_run {
        render_issue_table(&issues, out);
        out.print_message(&format!(
            "Dry run: {} issues would be assigned to '{assignee}'",
            issues.len()
        ));
    } else {
        out.print_message(&format!(
            "Assigned {succeeded}/{} issues to '{assignee}'{}",
            issues.len(),
            if failed > 0 {
                format!(" ({failed} failed)")
            } else {
                String::new()
            }
        ));
    }
    Ok(())
}

// ── Rendering ─────────────────────────────────────────────────────────────────

pub(crate) fn render_issue_table(issues: &[Issue], out: &OutputConfig) {
    if issues.is_empty() {
        out.print_message("No issues found.");
        return;
    }

    let color = use_color();
    let term_width = terminal_width();

    let key_w = issues.iter().map(|i| i.key.len()).max().unwrap_or(4).max(4) + 1;
    let status_w = issues
        .iter()
        .map(|i| i.status().len())
        .max()
        .unwrap_or(6)
        .clamp(6, 14)
        + 2;
    let assignee_w = issues
        .iter()
        .map(|i| i.assignee().len())
        .max()
        .unwrap_or(8)
        .clamp(8, 18)
        + 2;
    let type_w = issues
        .iter()
        .map(|i| i.issue_type().len())
        .max()
        .unwrap_or(4)
        .clamp(4, 12)
        + 2;

    // Give remaining width to summary, minimum 20
    let fixed = key_w + 1 + status_w + 1 + assignee_w + 1 + type_w + 1;
    let summary_w = term_width.saturating_sub(fixed).max(20);

    let header = format!(
        "{:<key_w$} {:<status_w$} {:<assignee_w$} {:<type_w$} {}",
        "Key", "Status", "Assignee", "Type", "Summary"
    );
    if color {
        println!("{}", header.bold());
    } else {
        println!("{header}");
    }

    for issue in issues {
        let key = if color {
            format!("{:<key_w$}", issue.key).yellow().to_string()
        } else {
            format!("{:<key_w$}", issue.key)
        };
        let status_val = truncate(issue.status(), status_w - 2);
        let status = if color {
            colorize_status(issue.status(), &format!("{:<status_w$}", status_val))
        } else {
            format!("{:<status_w$}", status_val)
        };
        println!(
            "{key} {status} {:<assignee_w$} {:<type_w$} {}",
            truncate(issue.assignee(), assignee_w - 2),
            truncate(issue.issue_type(), type_w - 2),
            truncate(issue.summary(), summary_w),
        );
    }
}

fn render_issue_detail(issue: &Issue) {
    let mut stdout = std::io::stdout().lock();
    write_issue_detail(&mut stdout, issue).expect("stdout write");
}

fn write_issue_detail<W: std::io::Write>(out: &mut W, issue: &Issue) -> std::io::Result<()> {
    let color = use_color();
    let key = if color {
        issue.key.yellow().bold().to_string()
    } else {
        issue.key.clone()
    };
    writeln!(out, "{key}  {}", issue.summary())?;
    writeln!(out)?;
    writeln!(out, "  Type:       {}", issue.issue_type())?;
    let status_str = if color {
        colorize_status(issue.status(), issue.status())
    } else {
        issue.status().to_string()
    };
    writeln!(out, "  Status:     {status_str}")?;
    writeln!(out, "  Priority:   {}", issue.priority())?;
    writeln!(out, "  Assignee:   {}", issue.assignee())?;
    if let Some(ref reporter) = issue.fields.reporter {
        writeln!(out, "  Reporter:   {}", reporter.display_name)?;
    }
    if let Some(ref labels) = issue.fields.labels
        && !labels.is_empty()
    {
        writeln!(out, "  Labels:     {}", labels.join(", "))?;
    }
    if let Some(ref components) = issue.fields.components
        && !components.is_empty()
    {
        let names: Vec<&str> = components.iter().map(|c| c.name.as_str()).collect();
        writeln!(out, "  Components: {}", names.join(", "))?;
    }
    if let Some(ref fix_versions) = issue.fields.fix_versions
        && !fix_versions.is_empty()
    {
        let names: Vec<&str> = fix_versions.iter().map(|v| v.name.as_str()).collect();
        writeln!(out, "  Fix Versions:     {}", names.join(", "))?;
    }
    if let Some(ref versions) = issue.fields.versions
        && !versions.is_empty()
    {
        let names: Vec<&str> = versions.iter().map(|v| v.name.as_str()).collect();
        writeln!(out, "  Affects Versions: {}", names.join(", "))?;
    }
    if let Some(ref created) = issue.fields.created {
        writeln!(out, "  Created:    {}", format_date(created))?;
    }
    if let Some(ref updated) = issue.fields.updated {
        writeln!(out, "  Updated:    {}", format_date(updated))?;
    }

    let desc = issue.description_text();
    if !desc.is_empty() {
        writeln!(out)?;
        writeln!(out, "Description:")?;
        for line in desc.lines() {
            writeln!(out, "  {line}")?;
        }
    }

    if let Some(ref links) = issue.fields.issue_links
        && !links.is_empty()
    {
        writeln!(out)?;
        writeln!(out, "Links:")?;
        for link in links {
            write_issue_link(out, link)?;
        }
    }

    if let Some(ref comment_list) = issue.fields.comment
        && !comment_list.comments.is_empty()
    {
        writeln!(out)?;
        writeln!(out, "Comments ({}):", comment_list.total)?;
        for c in &comment_list.comments {
            writeln!(out)?;
            let author = if color {
                c.author.display_name.bold().to_string()
            } else {
                c.author.display_name.clone()
            };
            writeln!(out, "  {} — {}", author, format_date(&c.created))?;
            let body = c.body_text();
            for line in body.lines() {
                writeln!(out, "    {line}")?;
            }
        }
    }
    Ok(())
}

fn write_issue_link<W: std::io::Write>(out: &mut W, link: &IssueLink) -> std::io::Result<()> {
    if let Some(ref out_issue) = link.outward_issue {
        writeln!(
            out,
            "  [{}] {} {} — {}",
            link.id, link.link_type.outward, out_issue.key, out_issue.fields.summary
        )?;
    }
    if let Some(ref in_issue) = link.inward_issue {
        writeln!(
            out,
            "  [{}] {} {} — {}",
            link.id, link.link_type.inward, in_issue.key, in_issue.fields.summary
        )?;
    }
    Ok(())
}

// ── JSON serialization ────────────────────────────────────────────────────────

pub(crate) fn issue_to_json(issue: &Issue, client: &JiraClient) -> serde_json::Value {
    serde_json::json!({
        "key": issue.key,
        "id": issue.id,
        "url": client.browse_url(&issue.key),
        "summary": issue.summary(),
        "status": issue.status(),
        "assignee": {
            "displayName": issue.assignee(),
            "accountId": issue.fields.assignee.as_ref().and_then(|a| a.account_id.as_deref()),
        },
        "priority": issue.priority(),
        "type": issue.issue_type(),
        "created": issue.fields.created,
        "updated": issue.fields.updated,
    })
}

fn attachment_to_json(a: &Attachment) -> serde_json::Value {
    serde_json::json!({
        "id": a.id,
        "filename": a.filename,
        "size": a.size,
        "mimeType": a.mime_type,
        // `author()` renders "-" for the table. JSON carries the absence itself,
        // so a consumer can tell an unattributed upload from one by a user
        // literally named "-".
        "author": a.author.as_ref().map(|u| u.display_name.as_str()),
        "created": a.created,
    })
}

fn version_to_json(v: &Version) -> serde_json::Value {
    serde_json::json!({
        "id": v.id,
        "name": v.name,
        "description": v.description,
        "released": v.released,
        "archived": v.archived,
        "releaseDate": v.release_date,
    })
}

pub fn issue_detail_to_json(issue: &Issue, client: &JiraClient) -> serde_json::Value {
    let comments: Vec<serde_json::Value> = issue
        .fields
        .comment
        .as_ref()
        .map(|cl| {
            cl.comments
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "id": c.id,
                        "author": {
                            "displayName": c.author.display_name,
                            "accountId": c.author.account_id,
                        },
                        "body": c.body_text(),
                        "created": c.created,
                        "updated": c.updated,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let issue_links: Vec<serde_json::Value> = issue
        .fields
        .issue_links
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|link| {
            let sentence = if let Some(ref out_issue) = link.outward_issue {
                format!("{} {} {}", issue.key, link.link_type.outward, out_issue.key)
            } else if let Some(ref in_issue) = link.inward_issue {
                format!("{} {} {}", issue.key, link.link_type.inward, in_issue.key)
            } else {
                String::new()
            };
            serde_json::json!({
                "id": link.id,
                "sentence": sentence,
                "type": {
                    "id": link.link_type.id,
                    "name": link.link_type.name,
                    "inward": link.link_type.inward,
                    "outward": link.link_type.outward,
                },
                "outwardIssue": link.outward_issue.as_ref().map(|i| serde_json::json!({
                    "key": i.key,
                    "summary": i.fields.summary,
                    "status": i.fields.status.name,
                })),
                "inwardIssue": link.inward_issue.as_ref().map(|i| serde_json::json!({
                    "key": i.key,
                    "summary": i.fields.summary,
                    "status": i.fields.status.name,
                })),
            })
        })
        .collect();

    serde_json::json!({
        "key": issue.key,
        "id": issue.id,
        "url": client.browse_url(&issue.key),
        "summary": issue.summary(),
        "status": issue.status(),
        "type": issue.issue_type(),
        "priority": issue.priority(),
        "assignee": {
            "displayName": issue.assignee(),
            "accountId": issue.fields.assignee.as_ref().and_then(|a| a.account_id.as_deref()),
        },
        "reporter": issue.fields.reporter.as_ref().map(|r| serde_json::json!({
            "displayName": r.display_name,
            "accountId": r.account_id,
        })),
        "labels": issue.fields.labels,
        "components": issue.fields.components,
        "fixVersions": issue.fields.fix_versions.as_ref().map(|fvs| {
            fvs.iter().map(version_to_json).collect::<Vec<_>>()
        }),
        "affectedVersions": issue.fields.versions.as_ref().map(|vs| {
            vs.iter().map(version_to_json).collect::<Vec<_>>()
        }),
        "description": issue.description_text(),
        "created": issue.fields.created,
        "updated": issue.fields.updated,
        "comments": comments,
        "issueLinks": issue_links,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn jql_multi_value(field: &str, values: &[&str]) -> Option<String> {
    match values.len() {
        0 => None,
        1 => Some(format!(r#"{field} = "{}""#, escape_jql(values[0]))),
        _ => {
            let quoted: Vec<String> = values
                .iter()
                .map(|v| format!(r#""{}""#, escape_jql(v)))
                .collect();
            Some(format!("{field} in ({})", quoted.join(", ")))
        }
    }
}

fn build_list_jql(filters: &ListFilters<'_>) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(p) = filters.project {
        parts.push(format!(r#"project = "{}""#, escape_jql(p)));
    }
    if let Some(s) = filters.status {
        parts.push(format!(r#"status = "{}""#, escape_jql(s)));
    }
    if let Some(a) = filters.assignee {
        if a == "me" {
            parts.push("assignee = currentUser()".into());
        } else {
            parts.push(format!(r#"assignee = "{}""#, escape_jql(a)));
        }
    }
    if let Some(t) = filters.issue_type {
        parts.push(format!(r#"issuetype = "{}""#, escape_jql(t)));
    }
    if let Some(s) = filters.sprint {
        if s == "active" || s == "open" {
            parts.push("sprint in openSprints()".into());
        } else {
            parts.push(format!(r#"sprint = "{}""#, escape_jql(s)));
        }
    }
    if let Some(comps) = filters.components {
        parts.extend(jql_multi_value("component", comps));
    }
    if let Some(lbls) = filters.labels {
        parts.extend(jql_multi_value("labels", lbls));
    }
    if let Some(fvs) = filters.fix_versions {
        parts.extend(jql_multi_value("fixVersion", fvs));
    }
    if let Some(e) = filters.jql_extra {
        parts.push(format!("({e})"));
    }

    if parts.is_empty() {
        "ORDER BY updated DESC".into()
    } else {
        format!("{} ORDER BY updated DESC", parts.join(" AND "))
    }
}

/// Color-code a Jira status string for terminal output.
fn colorize_status(status: &str, display: &str) -> String {
    let lower = status.to_lowercase();
    if lower.contains("done") || lower.contains("closed") || lower.contains("resolved") {
        display.green().to_string()
    } else if lower.contains("progress") || lower.contains("review") || lower.contains("testing") {
        display.yellow().to_string()
    } else if lower.contains("blocked") || lower.contains("impediment") {
        display.red().to_string()
    } else {
        display.to_string()
    }
}

/// Open a URL in the system default browser, printing a warning if it fails.
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).status();
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(url).status();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/c", "start", url])
        .status();

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    if let Err(e) = result {
        eprintln!("Warning: could not open browser: {e}");
    }
}

/// Truncate a string to `max` characters (not bytes), appending `…` if cut.
fn truncate(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let mut result: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

/// Shorten an ISO-8601 timestamp to just the date portion.
fn format_date(s: &str) -> String {
    s.chars().take(10).collect()
}

/// Width of a table column: the widest cell, but never narrower than the
/// column header, plus a two-space gap.
fn col_width(header: &str, cells: impl Iterator<Item = usize>) -> usize {
    cells.max().unwrap_or(0).max(header.len()) + 2
}

/// Render a byte count as a short human-readable size.
fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Reduce a filename reported by Jira to a single path component, so a crafted
/// attachment name cannot write outside the target directory.
fn safe_file_name(filename: &str) -> Option<&str> {
    let name = filename.rsplit(['/', '\\']).next()?;
    if name.is_empty() || name == "." || name == ".." || name.contains(':') {
        None
    } else {
        Some(name)
    }
}

/// Minimum width to clamp narrow terminals to, so fixed columns (key, status,
/// assignee, type) still leave at least 20 characters for the summary.
const MIN_TERMINAL_WIDTH: usize = 60;

/// Fallback width used when neither the TTY nor `COLUMNS` advertises a size —
/// matches the historical default.
const DEFAULT_TERMINAL_WIDTH: usize = 120;

/// Determine the terminal width for rendering the issues table.
///
/// Query the live TTY first (via `ioctl(TIOCGWINSZ)` / Windows console APIs),
/// fall back to `COLUMNS` for non-TTY contexts where the caller still wants
/// to pin the width, and finally to a reasonable default.
fn terminal_width() -> usize {
    use std::io::IsTerminal;

    let tty_width = std::io::stdout()
        .is_terminal()
        .then(terminal_size::terminal_size)
        .flatten()
        .map(|(terminal_size::Width(w), _)| w as usize);
    let columns = std::env::var("COLUMNS").ok().and_then(|v| v.parse().ok());

    resolve_terminal_width(tty_width, columns)
}

/// Pure resolution of the three width sources, in priority order. Extracted so
/// the decision logic is testable without mocking the process environment or
/// the TTY.
fn resolve_terminal_width(tty_width: Option<usize>, columns: Option<usize>) -> usize {
    if let Some(w) = tty_width {
        return w.max(MIN_TERMINAL_WIDTH);
    }
    columns.unwrap_or(DEFAULT_TERMINAL_WIDTH)
}

/// Resolve a CLI `--assignee` argument into the three-state `IssueUpdate.assignee` value.
///
/// `--assignee me` triggers a `GET /myself` round-trip to fetch the current user's account ID.
/// `--assignee none` returns `Some(None)` (the unassign sentinel).
/// `--assignee <id>` returns `Some(Some(id))` (set to the literal account ID).
/// `None` (flag absent) returns `None` (leave the field untouched).
pub async fn resolve_assignee_arg(
    client: &JiraClient,
    arg: Option<&str>,
) -> Result<Option<Option<String>>, ApiError> {
    match arg {
        None => Ok(None),
        Some("none") => Ok(Some(None)),
        Some("me") => {
            let me = client.get_myself().await?;
            Ok(Some(Some(me.account_id)))
        }
        Some(id) => Ok(Some(Some(id.to_string()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{IssueFields, IssueTypeField, StatusField, Version};

    fn issue_fixture(fix: Option<Vec<Version>>, aff: Option<Vec<Version>>) -> Issue {
        Issue {
            id: "10001".into(),
            key: "PROJ-1".into(),
            url: None,
            fields: IssueFields {
                summary: "Test".into(),
                status: StatusField {
                    name: "Open".into(),
                },
                assignee: None,
                reporter: None,
                priority: None,
                issuetype: IssueTypeField { name: "Bug".into() },
                description: None,
                labels: None,
                components: None,
                fix_versions: fix,
                versions: aff,
                created: None,
                updated: None,
                comment: None,
                issue_links: None,
            },
        }
    }

    fn make_version(id: &str, name: &str) -> Version {
        Version {
            id: id.into(),
            name: name.into(),
            description: None,
            released: None,
            archived: None,
            release_date: None,
        }
    }

    #[test]
    fn write_issue_detail_renders_fix_versions_line() {
        let issue = issue_fixture(
            Some(vec![make_version("1", "1.2.0"), make_version("2", "1.3.0")]),
            None,
        );
        let mut buf = Vec::new();
        write_issue_detail(&mut buf, &issue).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("  Fix Versions:     1.2.0, 1.3.0"),
            "expected rendered fix-versions line, got:\n{out}"
        );
    }

    #[test]
    fn write_issue_detail_renders_affects_versions_line() {
        let issue = issue_fixture(None, Some(vec![make_version("5", "1.1.0")]));
        let mut buf = Vec::new();
        write_issue_detail(&mut buf, &issue).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("  Affects Versions: 1.1.0"),
            "expected affects-versions line, got:\n{out}"
        );
    }

    #[test]
    fn write_issue_detail_omits_version_lines_when_empty() {
        let issue = issue_fixture(Some(vec![]), None);
        let mut buf = Vec::new();
        write_issue_detail(&mut buf, &issue).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("Fix Versions:"),
            "should omit fix versions header for empty slice, got:\n{out}"
        );
        assert!(
            !out.contains("Affects Versions:"),
            "should omit affects versions header when None, got:\n{out}"
        );
    }

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    #[test]
    fn truncate_multibyte_safe() {
        let result = truncate("日本語テスト", 3);
        assert_eq!(result, "日本語…");
    }

    #[test]
    fn build_list_jql_empty() {
        assert_eq!(
            build_list_jql(&ListFilters::default()),
            "ORDER BY updated DESC"
        );
    }

    #[test]
    fn build_list_jql_escapes_quotes() {
        let jql = build_list_jql(&ListFilters {
            status: Some(r#"Done" OR 1=1"#),
            ..Default::default()
        });
        // The double quote must be backslash-escaped so it cannot break out of the JQL string.
        // The resulting clause should be:  status = "Done\" OR 1=1"
        assert!(jql.contains(r#"\""#), "double quote must be escaped");
        assert!(
            jql.contains(r#"status = "Done\""#),
            "escaped quote must remain inside the status value string"
        );
    }

    #[test]
    fn build_list_jql_project_and_status() {
        let jql = build_list_jql(&ListFilters {
            project: Some("PROJ"),
            status: Some("In Progress"),
            ..Default::default()
        });
        assert!(jql.contains(r#"project = "PROJ""#));
        assert!(jql.contains(r#"status = "In Progress""#));
    }

    #[test]
    fn build_list_jql_assignee_me() {
        let jql = build_list_jql(&ListFilters {
            assignee: Some("me"),
            ..Default::default()
        });
        assert!(jql.contains("currentUser()"));
    }

    #[test]
    fn build_list_jql_issue_type() {
        let jql = build_list_jql(&ListFilters {
            issue_type: Some("Bug"),
            ..Default::default()
        });
        assert!(jql.contains(r#"issuetype = "Bug""#));
    }

    #[test]
    fn build_list_jql_sprint_active() {
        let jql = build_list_jql(&ListFilters {
            sprint: Some("active"),
            ..Default::default()
        });
        assert!(jql.contains("sprint in openSprints()"));
    }

    #[test]
    fn build_list_jql_sprint_named() {
        let jql = build_list_jql(&ListFilters {
            sprint: Some("Sprint 42"),
            ..Default::default()
        });
        assert!(jql.contains(r#"sprint = "Sprint 42""#));
    }

    #[test]
    fn build_list_jql_single_component() {
        let jql = build_list_jql(&ListFilters {
            components: Some(&["Backend"]),
            ..Default::default()
        });
        assert!(
            jql.contains(r#"component = "Backend""#),
            "expected single-component clause, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_multiple_components() {
        let jql = build_list_jql(&ListFilters {
            components: Some(&["Backend", "API"]),
            ..Default::default()
        });
        assert!(
            jql.contains(r#"component in ("Backend", "API")"#),
            "expected `component in (...)` clause, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_escapes_component_quotes() {
        let jql = build_list_jql(&ListFilters {
            components: Some(&[r#"weird "name""#]),
            ..Default::default()
        });
        assert!(
            jql.contains(r#"component = "weird \"name\"""#),
            "expected escaped quotes, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_empty_components_emits_no_clause() {
        let jql = build_list_jql(&ListFilters {
            components: Some(&[]),
            ..Default::default()
        });
        assert!(
            !jql.contains("component"),
            "expected no component clause for empty slice, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_single_label() {
        let jql = build_list_jql(&ListFilters {
            labels: Some(&["backend"]),
            ..Default::default()
        });
        assert!(
            jql.contains(r#"labels = "backend""#),
            "expected single-label clause, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_multiple_labels() {
        let jql = build_list_jql(&ListFilters {
            labels: Some(&["backend", "urgent"]),
            ..Default::default()
        });
        assert!(
            jql.contains(r#"labels in ("backend", "urgent")"#),
            "expected `labels in (...)` clause, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_escapes_label_quotes() {
        let jql = build_list_jql(&ListFilters {
            labels: Some(&[r#"weird "name""#]),
            ..Default::default()
        });
        assert!(
            jql.contains(r#"labels = "weird \"name\"""#),
            "expected escaped quotes, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_empty_labels_emits_no_clause() {
        let jql = build_list_jql(&ListFilters {
            labels: Some(&[]),
            ..Default::default()
        });
        assert!(
            !jql.contains("labels"),
            "expected no labels clause for empty slice, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_single_fix_version() {
        let jql = build_list_jql(&ListFilters {
            fix_versions: Some(&["1.2.0"]),
            ..Default::default()
        });
        assert!(
            jql.contains(r#"fixVersion = "1.2.0""#),
            "expected single fixVersion clause, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_multiple_fix_versions() {
        let jql = build_list_jql(&ListFilters {
            fix_versions: Some(&["1.2.0", "1.3.0"]),
            ..Default::default()
        });
        assert!(
            jql.contains(r#"fixVersion in ("1.2.0", "1.3.0")"#),
            "expected fixVersion in (...) clause, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_escapes_fix_version_quotes() {
        let jql = build_list_jql(&ListFilters {
            fix_versions: Some(&[r#"weird "ver""#]),
            ..Default::default()
        });
        assert!(
            jql.contains(r#"fixVersion = "weird \"ver\"""#),
            "expected escaped quotes, got: {jql}"
        );
    }

    #[test]
    fn build_list_jql_empty_fix_versions_emits_no_clause() {
        let jql = build_list_jql(&ListFilters {
            fix_versions: Some(&[]),
            ..Default::default()
        });
        assert!(
            !jql.contains("fixVersion"),
            "expected no fixVersion clause for empty slice, got: {jql}"
        );
    }

    #[test]
    fn colorize_status_done_is_green() {
        let result = colorize_status("Done", "Done");
        assert!(result.contains("Done"));
        // Green ANSI escape code starts with \x1b[32m
        assert!(result.contains("\x1b["));
    }

    #[test]
    fn colorize_status_unknown_unchanged() {
        let result = colorize_status("Backlog", "Backlog");
        assert_eq!(result, "Backlog");
    }

    /// Ensures an environment variable is removed even if the test panics.
    struct EnvVarGuard(&'static str);

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var(self.0) }
        }
    }

    #[test]
    fn terminal_width_fallback_parses_columns() {
        unsafe { std::env::set_var("COLUMNS", "200") };
        let _guard = EnvVarGuard("COLUMNS");
        assert_eq!(terminal_width(), 200);
    }

    #[test]
    fn resolve_terminal_width_prefers_tty_over_columns() {
        assert_eq!(resolve_terminal_width(Some(200), Some(80)), 200);
    }

    #[test]
    fn resolve_terminal_width_clamps_narrow_tty_to_minimum() {
        assert_eq!(resolve_terminal_width(Some(40), None), MIN_TERMINAL_WIDTH);
    }

    #[test]
    fn resolve_terminal_width_does_not_clamp_columns_fallback() {
        // Users who explicitly pin COLUMNS (e.g. for non-TTY output or tests)
        // get exactly what they asked for; only the TTY-measured width is
        // clamped.
        assert_eq!(resolve_terminal_width(None, Some(40)), 40);
    }

    #[test]
    fn resolve_terminal_width_defaults_when_nothing_available() {
        assert_eq!(resolve_terminal_width(None, None), DEFAULT_TERMINAL_WIDTH);
    }

    #[tokio::test]
    async fn resolve_assignee_arg_absent_returns_none() {
        let server = wiremock::MockServer::start().await;
        let client = crate::api::JiraClient::new(
            &server.uri(),
            "test@example.com",
            "test-token",
            crate::api::AuthType::Basic,
            3,
        )
        .unwrap();
        let result = resolve_assignee_arg(&client, None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_assignee_arg_none_sentinel_returns_some_none() {
        let server = wiremock::MockServer::start().await;
        let client = crate::api::JiraClient::new(
            &server.uri(),
            "test@example.com",
            "test-token",
            crate::api::AuthType::Basic,
            3,
        )
        .unwrap();
        let result = resolve_assignee_arg(&client, Some("none")).await.unwrap();
        assert!(matches!(result, Some(None)));
    }

    #[tokio::test]
    async fn resolve_assignee_arg_literal_id_passes_through() {
        let server = wiremock::MockServer::start().await;
        let client = crate::api::JiraClient::new(
            &server.uri(),
            "test@example.com",
            "test-token",
            crate::api::AuthType::Basic,
            3,
        )
        .unwrap();
        let result = resolve_assignee_arg(&client, Some("literal-id-999"))
            .await
            .unwrap();
        assert_eq!(result, Some(Some("literal-id-999".to_string())));
    }
}
