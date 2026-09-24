use assert_cmd::prelude::*;
use jira_cli::api::{ApiError, AuthType, JiraClient};
use jira_cli::output::{PARTIAL_SUCCESS, exit_codes};
use jira_cli::test_support::{config_dir_env_name, write_config};
use serde_json::{Value, json};
use std::process::{Command, Output};
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> JiraClient {
    JiraClient::new(&server.uri(), "", "mock-token", AuthType::Pat, 3).unwrap()
}

fn run(server: &MockServer, args: &[&str]) -> Output {
    let dir = TempDir::new().unwrap();
    write_config(dir.path(), &format!("[default]\nhost = {:?}\nauth_type = \"pat\"\napi_version = 3\ntoken = \"mock-token\"\n", server.uri())).unwrap();
    Command::cargo_bin("jira")
        .unwrap()
        .args(args)
        .env(config_dir_env_name(), dir.path())
        .env("NO_COLOR", "1")
        .env_remove("JIRA_HOST")
        .env_remove("JIRA_EMAIL")
        .env_remove("JIRA_TOKEN")
        .env_remove("JIRA_PROFILE")
        .env_remove("JIRA_READ_ONLY")
        .output()
        .unwrap()
}

async fn get(server: &MockServer, endpoint: &str, value: Value) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .respond_with(ResponseTemplate::new(200).set_body_json(value))
        .mount(server)
        .await;
}

async fn boards(server: &MockServer) {
    get(
        server,
        "/rest/agile/1.0/board",
        json!({"isLast": true, "values": [
            {"id": 1, "name": "First board", "type": "scrum"},
            {"id": 2, "name": "Second board", "type": "scrum"},
            {"id": 3, "name": "Kanban board", "type": "kanban"}
        ]}),
    )
    .await;
}

fn sprint(id: u64, name: &str) -> Value {
    json!({"id": id, "name": name, "state": "active"})
}

async fn sprints(server: &MockServer, board: u64, values: Vec<Value>) {
    get(
        server,
        &format!("/rest/agile/1.0/board/{board}/sprint"),
        json!({"isLast": true, "values": values}),
    )
    .await;
}

async fn create_response(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .respond_with(ResponseTemplate::new(201).set_body_json(
            json!({"key": "PROJ-101", "id": "101", "self": "http://example.invalid/issue/101"}),
        ))
        .expect(1)
        .mount(server)
        .await;
}

async fn no_writes(server: &MockServer) {
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn multiple_active_sprints_and_duplicate_exact_names_require_a_choice() {
    for input in ["active", "Sprint Alpha", "Alpha"] {
        let server = MockServer::start().await;
        boards(&server).await;
        sprints(&server, 1, vec![sprint(10, "Sprint Alpha")]).await;
        sprints(&server, 2, vec![sprint(20, "Sprint Alpha")]).await;
        let err = client(&server).resolve_sprint(input).await.unwrap_err();
        assert!(matches!(err, ApiError::InvalidInput(_)));
        let message = err.to_string();
        assert!(
            message.contains("Ambiguous sprint")
                && message.contains("10")
                && message.contains("20")
                && message.contains("--board"),
            "{message}"
        );
        assert!(
            !server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.url.path().contains("/board/3/"))
        );
    }
}

#[tokio::test]
async fn exact_name_wins_and_shared_sprints_are_deduplicated() {
    let server = MockServer::start().await;
    boards(&server).await;
    sprints(
        &server,
        1,
        vec![sprint(10, "Alpha"), sprint(20, "Alpha extended")],
    )
    .await;
    sprints(&server, 2, vec![sprint(10, "Alpha")]).await;
    assert_eq!(
        client(&server).resolve_sprint("alpha").await.unwrap().id,
        10
    );
    server.reset().await;
    boards(&server).await;
    sprints(&server, 1, vec![sprint(10, "Alpha")]).await;
    sprints(&server, 2, vec![sprint(10, "Alpha")]).await;
    assert_eq!(
        client(&server).resolve_sprint("active").await.unwrap().id,
        10
    );
}

#[tokio::test]
async fn project_scope_is_sent_to_jira_and_explicit_board_overrides_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/rest/agile/1.0/board"))
        .and(query_param("projectKeyOrId", "PROJ"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"isLast": true, "values": [{"id": 2, "name": "Project board", "type": "scrum"}]})))
        .expect(1).mount(&server).await;
    sprints(&server, 2, vec![sprint(20, "Project sprint")]).await;
    sprints(&server, 1, vec![sprint(10, "Explicit board sprint")]).await;
    get(
        &server,
        "/rest/agile/1.0/board/1",
        json!({"id":1,"name":"Explicit board","type":"scrum"}),
    )
    .await;
    assert_eq!(
        client(&server)
            .resolve_sprint_scoped("active", Some("PROJ"), None)
            .await
            .unwrap()
            .id,
        20
    );
    assert_eq!(
        client(&server)
            .resolve_sprint_scoped("active", Some("PROJ"), Some(1))
            .await
            .unwrap()
            .id,
        10
    );
}

#[tokio::test]
async fn ambiguous_matches_on_later_board_and_sprint_pages_are_not_ignored() {
    let server = MockServer::start().await;
    for (start, board) in [(0, 1), (1, 2)] {
        Mock::given(method("GET")).and(path("/rest/agile/1.0/board"))
            .and(query_param("startAt", start.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"isLast": start == 1, "startAt": start, "values": [{"id": board, "name": "Board", "type": "scrum"}]})))
            .expect(1).mount(&server).await;
    }
    sprints(&server, 1, vec![sprint(10, "Alpha")]).await;
    for (start, name) in [(0, "Unrelated"), (1, "Alpha")] {
        Mock::given(method("GET")).and(path("/rest/agile/1.0/board/2/sprint"))
            .and(query_param("startAt", start.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"isLast": start == 1, "startAt": start, "values": [sprint(20 + start, name)]})))
            .expect(1).mount(&server).await;
    }
    assert!(
        client(&server)
            .resolve_sprint("Alpha")
            .await
            .unwrap_err()
            .to_string()
            .contains("Ambiguous")
    );
}

#[tokio::test]
async fn numeric_sprint_checks_explicit_board_membership() {
    let server = MockServer::start().await;
    get(&server, "/rest/agile/1.0/sprint/10", sprint(10, "Alpha")).await;
    sprints(&server, 2, vec![sprint(20, "Beta")]).await;
    let err = client(&server)
        .resolve_sprint_scoped("10", None, Some(2))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not on board 2"));
    assert_eq!(
        client(&server)
            .resolve_sprint_scoped("10", Some("PROJ"), None)
            .await
            .unwrap()
            .id,
        10
    );
}

#[tokio::test]
async fn missing_closed_and_ambiguous_sprints_never_create_an_issue() {
    for input in ["999", "10", "active"] {
        let server = MockServer::start().await;
        get(
            &server,
            "/rest/agile/1.0/sprint/10",
            json!({"id": 10, "name": "Finished", "state": "closed"}),
        )
        .await;
        boards(&server).await;
        sprints(&server, 1, vec![sprint(10, "Alpha")]).await;
        sprints(&server, 2, vec![sprint(20, "Beta")]).await;
        let output = run(
            &server,
            &[
                "-o", "json", "issues", "create", "-p", "PROJ", "-s", "Example", "--sprint", input,
            ],
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_ne!(error["error"]["kind"], "partial_success");
        no_writes(&server).await;
    }
}

#[tokio::test]
async fn failed_sprint_move_returns_created_key_and_non_retryable_partial_success() {
    for status in [400, 429, 503] {
        let server = MockServer::start().await;
        get(&server, "/rest/agile/1.0/sprint/10", sprint(10, "Alpha")).await;
        create_response(&server).await;
        Mock::given(method("POST"))
            .and(path("/rest/agile/1.0/sprint/10/issue"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        let output = run(
            &server,
            &[
                "-o", "json", "--quiet", "issues", "create", "-p", "PROJ", "-s", "Example",
                "--sprint", "10",
            ],
        );
        assert_eq!(output.status.code(), Some(exit_codes::PARTIAL_SUCCESS));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["kind"], "partial_success");
        let details = &error["error"]["details"];
        assert_eq!(details["key"], "PROJ-101");
        assert_eq!(details["created"], true);
        assert_eq!(details["sprintMoved"], false);
        assert_eq!(details["sprintId"], 10);
        assert_eq!(
            details["recoveryCommand"],
            "jira issues move PROJ-101 --sprint 10"
        );
        assert_eq!(details["url"], format!("{}/browse/PROJ-101", server.uri()));
        assert!(!PARTIAL_SUCCESS.retryable);
        let requests = server.received_requests().await.unwrap();
        let lookup = requests
            .iter()
            .position(|r| r.url.path() == "/rest/agile/1.0/sprint/10")
            .unwrap();
        let create = requests
            .iter()
            .position(|r| r.method == "POST" && r.url.path() == "/rest/api/3/issue")
            .unwrap();
        assert!(lookup < create);
    }
}

#[tokio::test]
async fn text_partial_success_reports_created_issue_even_when_quiet() {
    let server = MockServer::start().await;
    get(&server, "/rest/agile/1.0/sprint/10", sprint(10, "Alpha")).await;
    create_response(&server).await;
    let output = run(
        &server,
        &[
            "-o", "text", "--quiet", "issues", "create", "-p", "PROJ", "-s", "Example", "--sprint",
            "10",
        ],
    );
    assert_eq!(output.status.code(), Some(8));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("Created PROJ-101") && error.contains("do not rerun issues create"));
    assert!(error.contains("jira issues move PROJ-101 --sprint 10"));
}

#[tokio::test]
async fn move_uses_actual_issue_project_and_create_honors_project_override() {
    for create in [false, true] {
        let server = MockServer::start().await;
        get(
            &server,
            "/rest/api/3/issue/PROJ-101",
            json!({"fields": {"project": {"key": "OTHER"}}}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/rest/agile/1.0/board"))
            .and(query_param("projectKeyOrId", "OTHER"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"isLast": true, "values": [{"id": 2, "name": "Board", "type": "scrum"}]}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        sprints(&server, 2, vec![sprint(20, "Alpha")]).await;
        Mock::given(method("POST"))
            .and(path("/rest/agile/1.0/sprint/20/issue"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let output = if create {
            create_response(&server).await;
            run(
                &server,
                &[
                    "issues",
                    "create",
                    "-p",
                    "PROJ",
                    "-s",
                    "Example",
                    "--sprint",
                    "active",
                    "--field",
                    "project={\"key\":\"OTHER\"}",
                ],
            )
        } else {
            run(
                &server,
                &["issues", "move", "PROJ-101", "--sprint", "active"],
            )
        };
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn board_flag_is_parsed_and_requires_a_sprint_on_create() {
    let server = MockServer::start().await;
    let output = run(
        &server,
        &[
            "issues", "create", "-p", "PROJ", "-s", "Example", "--board", "2",
        ],
    );
    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    assert!(server.received_requests().await.unwrap().is_empty());
    sprints(&server, 2, vec![sprint(20, "Alpha")]).await;
    get(
        &server,
        "/rest/agile/1.0/board/2",
        json!({"id":2,"name":"Team Scrum","type":"scrum"}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/rest/agile/1.0/sprint/20/issue"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let output = run(
        &server,
        &[
            "issues", "move", "PROJ-101", "--sprint", "active", "--board", "2",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().starts_with("/rest/api/3/issue/"))
    );
}

#[tokio::test]
async fn assignee_clear_sentinels_work_for_create_update_and_assign() {
    for sentinel in ["none", "unassign"] {
        let server = MockServer::start().await;
        create_response(&server).await;
        for endpoint in [
            "/rest/api/3/issue/PROJ-101",
            "/rest/api/3/issue/PROJ-101/assignee",
        ] {
            Mock::given(method("PUT"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }
        for args in [
            vec![
                "issues",
                "create",
                "-p",
                "PROJ",
                "-s",
                "Example",
                "--assignee",
                sentinel,
            ],
            vec!["issues", "update", "PROJ-101", "--assignee", sentinel],
            vec!["issues", "assign", "PROJ-101", "--assignee", sentinel],
        ] {
            let output = run(&server, &args);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        for request in server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method != "GET")
        {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            if request.url.path().ends_with("/assignee") {
                assert_eq!(body, json!({"accountId": null}));
            } else {
                assert!(body["fields"].get("assignee").unwrap().is_null());
            }
        }
    }
}

#[tokio::test]
async fn clear_epic_flag_updates_membership_and_conflicts_with_epic() {
    let server = MockServer::start().await;
    let output = run(
        &server,
        &[
            "issues",
            "update",
            "PROJ-101",
            "--clear-epic",
            "--epic",
            "PROJ-9",
        ],
    );
    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    assert!(server.received_requests().await.unwrap().is_empty());
    get(
        &server,
        "/rest/api/3/issue/PROJ-101",
        json!({"fields": {"issuetype": {"name": "Story"}}}),
    )
    .await;
    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-101"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let output = run(&server, &["issues", "update", "PROJ-101", "--clear-epic"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = server.received_requests().await.unwrap();
    let request = requests.iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        json!({"fields": {"parent": null}})
    );
}
