//! `issues delete`: permanent deletes need `--yes`, every key is checked
//! before anything is deleted, and subtasks go only with `--delete-subtasks`.

use super::usability::{assert_preview_schema, parse_success};
use super::*;
use serde_json::{Value, json};
use wiremock::matchers::query_param;

/// Serve `key` as an issue with the given subtasks.
async fn issue(server: &MockServer, key: &str, subtasks: &[&str]) {
    let subtasks: Vec<Value> = subtasks.iter().map(|k| json!({"key": k})).collect();
    Mock::given(method("GET"))
        .and(path(format!("/rest/api/3/issue/{key}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "key": key,
            "fields": {"summary": format!("Summary of {key}"),
                "issuetype": {"name": "Task"}, "subtasks": subtasks}
        })))
        .mount(server)
        .await;
}

async fn may_delete(server: &MockServer, allowed: bool) {
    Mock::given(method("GET"))
        .and(path("/rest/api/3/mypermissions"))
        .and(query_param("permissions", "DELETE_ISSUES"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "permissions": {"DELETE_ISSUES": {"havePermission": allowed}}
        })))
        .mount(server)
        .await;
}

async fn accept_deletes(server: &MockServer) {
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}

/// The deletes Jira received, as `path?query`.
async fn deletes(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .map(|r| format!("{}?{}", r.url.path(), r.url.query().unwrap_or("")))
        .collect()
}

fn bulk_failure(output: &std::process::Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit_codes::BULK_FAILURE),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn without_yes_nothing_is_sent() {
    let server = MockServer::start().await;
    let output = run_jira_against(&server, &["issues", "delete", "PROJ-1"]);
    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    let envelope = error_envelope(&String::from_utf8_lossy(&output.stderr));
    assert_eq!(envelope["error"]["kind"], "confirmation_required");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--yes"),
        "{envelope}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn read_only_profiles_refuse_before_any_request() {
    let server = MockServer::start().await;
    let output = run_jira_against_read_only(&server, &["issues", "delete", "PROJ-1", "--yes"]);
    assert!(!output.status.success());
    let envelope = error_envelope(&String::from_utf8_lossy(&output.stderr));
    assert_eq!(envelope["error"]["kind"], "invalid_input", "{envelope}");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("read-only mode"),
        "{envelope}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn dry_run_needs_no_yes_and_only_reads() {
    let server = MockServer::start().await;
    issue(&server, "PROJ-1", &["PROJ-2"]).await;
    issue(&server, "PROJ-2", &[]).await;
    may_delete(&server, true).await;
    let data = parse_success(run_jira_against(
        &server,
        &[
            "issues",
            "delete",
            "PROJ-1",
            "PROJ-2",
            "--delete-subtasks",
            "--dry-run",
        ],
    ));
    assert_preview_schema("issues delete", &data);
    assert_eq!(data["dryRun"], true);
    assert_eq!(data["ready"], 2);
    assert_eq!(data["issues"][0]["action"], "would delete");
    assert_eq!(data["issues"][0]["subtasks"], json!(["PROJ-2"]));
    assert_eq!(data["issues"][0]["summary"], "Summary of PROJ-1");
    assert_eq!(data["issues"][0]["issueType"], "Task");
    assert_eq!(data["issues"][1]["deletedWith"], "PROJ-1");
    let requests = server.received_requests().await.unwrap();
    assert!(requests.iter().all(|r| r.method.as_str() == "GET"));
}

#[tokio::test]
async fn deletes_every_key_once_in_order() {
    let server = MockServer::start().await;
    issue(&server, "PROJ-1", &[]).await;
    issue(&server, "PROJ-3", &[]).await;
    may_delete(&server, true).await;
    accept_deletes(&server).await;
    let output = run_jira_against(
        &server,
        &["issues", "delete", "PROJ-3", "PROJ-1", "PROJ-3", "--yes"],
    );
    let data = parse_success(output);
    assert_json_keys_match_schema("issues delete", &data, &[]);
    assert_eq!(data["total"], 2, "duplicate keys collapse: {data}");
    assert_eq!(data["succeeded"], 2);
    assert_eq!(data["issues"][0]["key"], "PROJ-3");
    assert_eq!(data["issues"][0]["ok"], true);
    assert_eq!(data["issues"][0]["subtasksDeleted"], json!([]));
    assert_eq!(
        deletes(&server).await,
        [
            "/rest/api/3/issue/PROJ-3?deleteSubtasks=false",
            "/rest/api/3/issue/PROJ-1?deleteSubtasks=false",
        ]
    );
}

#[tokio::test]
async fn subtasks_need_the_flag_and_block_every_delete() {
    let server = MockServer::start().await;
    issue(&server, "PROJ-1", &[]).await;
    issue(&server, "PROJ-2", &["PROJ-5", "PROJ-6"]).await;
    may_delete(&server, true).await;
    accept_deletes(&server).await;
    let data = bulk_failure(&run_jira_against(
        &server,
        &["issues", "delete", "PROJ-1", "PROJ-2", "--yes"],
    ));
    assert_json_keys_match_schema("issues delete", &data, &[]);
    assert_eq!(data["issues"][0]["notAttempted"], true, "{data}");
    let failure = &data["issues"][1];
    assert_eq!(failure["phase"], "lookup");
    assert_eq!(failure["outcome"], "failed");
    let message = failure["error"].as_str().unwrap();
    assert!(
        message.contains("PROJ-5, PROJ-6") && message.contains("--delete-subtasks"),
        "{message}"
    );
    assert!(deletes(&server).await.is_empty());
}

#[tokio::test]
async fn a_subtask_listed_with_its_parent_goes_with_the_parent() {
    let server = MockServer::start().await;
    issue(&server, "PROJ-5", &[]).await;
    issue(&server, "PROJ-2", &["PROJ-5"]).await;
    may_delete(&server, true).await;
    accept_deletes(&server).await;
    let data = parse_success(run_jira_against(
        &server,
        &[
            "issues",
            "delete",
            "PROJ-5",
            "PROJ-2",
            "--delete-subtasks",
            "--yes",
        ],
    ));
    assert_json_keys_match_schema("issues delete", &data, &[]);
    assert_eq!(data["succeeded"], 2);
    assert_eq!(
        data["issues"][0],
        json!({"key":"PROJ-5","ok":true,"deletedWith":"PROJ-2"})
    );
    assert_eq!(data["issues"][1]["subtasksDeleted"], json!(["PROJ-5"]));
    assert_eq!(
        deletes(&server).await,
        ["/rest/api/3/issue/PROJ-2?deleteSubtasks=true"]
    );
}

#[tokio::test]
async fn an_unknown_key_deletes_nothing() {
    let server = MockServer::start().await;
    issue(&server, "PROJ-1", &[]).await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-404"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "errorMessages": ["Issue does not exist or you do not have permission to see it."]
        })))
        .mount(&server)
        .await;
    may_delete(&server, true).await;
    accept_deletes(&server).await;
    let data = bulk_failure(&run_jira_against(
        &server,
        &["issues", "delete", "PROJ-404", "PROJ-1", "--yes"],
    ));
    assert_eq!(data["issues"][0]["errorKind"], "not_found", "{data}");
    assert_eq!(data["issues"][1]["notAttempted"], true);
    assert_eq!(data["succeeded"], 0);
    assert!(deletes(&server).await.is_empty());
}

#[tokio::test]
async fn missing_delete_permission_deletes_nothing() {
    let server = MockServer::start().await;
    issue(&server, "PROJ-1", &[]).await;
    may_delete(&server, false).await;
    accept_deletes(&server).await;
    let data = bulk_failure(&run_jira_against(
        &server,
        &["issues", "delete", "PROJ-1", "--yes"],
    ));
    let message = data["issues"][0]["error"].as_str().unwrap();
    assert!(message.contains("permission to delete PROJ-1"), "{message}");
    assert!(deletes(&server).await.is_empty());
}

#[tokio::test]
async fn a_failed_delete_is_reported_and_later_keys_still_run() {
    let server = MockServer::start().await;
    issue(&server, "PROJ-1", &[]).await;
    issue(&server, "PROJ-2", &[]).await;
    may_delete(&server, true).await;
    Mock::given(method("DELETE"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "errorMessages": ["Cannot delete"]
        })))
        .mount(&server)
        .await;
    accept_deletes(&server).await;
    let data = bulk_failure(&run_jira_against(
        &server,
        &["issues", "delete", "PROJ-1", "PROJ-2", "--yes"],
    ));
    assert_json_keys_match_schema("issues delete", &data, &[]);
    assert_eq!(data["issues"][0]["phase"], "write", "{data}");
    assert_eq!(data["issues"][1]["ok"], true);
    assert_eq!(data["succeeded"], 1);
}

#[tokio::test]
async fn text_preview_names_what_would_be_destroyed() {
    let server = MockServer::start().await;
    issue(&server, "PROJ-1", &["PROJ-5"]).await;
    may_delete(&server, true).await;
    let output = run_jira_against(
        &server,
        &[
            "issues",
            "delete",
            "PROJ-1",
            "--delete-subtasks",
            "--dry-run",
            "--output",
            "text",
        ],
    );
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .contains(r#"PROJ-1  would delete "Summary of PROJ-1" [Task] and its subtasks PROJ-5"#),
        "{stdout}"
    );
}
