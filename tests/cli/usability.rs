use super::*;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, query_param};

fn parse_success(output: std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

async fn search_issues(server: &MockServer, count: usize) {
    let issues: Vec<_> = (1..=count)
        .map(|n| {
            let mut issue = full_issue();
            issue["key"] = json!(format!("PROJ-{n}"));
            issue
        })
        .collect();
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"issues":issues,"isLast":true})),
        )
        .mount(server)
        .await;
}

async fn transitions(server: &MockServer, key: &str, values: Value) {
    Mock::given(method("GET"))
        .and(path(format!("/rest/api/3/issue/{key}/transitions")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"transitions":values})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn empty_bulk_always_emits_a_summary_including_quiet_and_read_only() {
    let server = MockServer::start().await;
    search_issues(&server, 0).await;
    for command in ["bulk-assign", "bulk-transition"] {
        for dry_run in [false, true] {
            let mut args = vec!["issues", command, "--jql", "project = PROJ", "--quiet"];
            args.extend(if command == "bulk-assign" {
                ["--assignee", "none"]
            } else {
                ["--to", "Done"]
            });
            args.push(if dry_run { "--dry-run" } else { "--yes" });
            let output = if dry_run {
                run_jira_against_read_only(&server, &args)
            } else {
                run_jira_against(&server, &args)
            };
            let data = parse_success(output);
            assert_eq!(
                data,
                json!({"dryRun":dry_run,"total":0,"succeeded":0,"failed":0,"notAttempted":0,"ready":0,"issues":[]})
            );
            assert_json_keys_match_schema(&format!("issues {command}"), &data, &[]);
            if dry_run {
                assert_preview_schema(&format!("issues {command}"), &data);
            }
        }
    }
}

#[tokio::test]
async fn bulk_transition_preserves_success_after_a_later_lookup_failure() {
    let server = MockServer::start().await;
    search_issues(&server, 3).await;
    for key in ["PROJ-1", "PROJ-3"] {
        transitions(
            &server,
            key,
            json!([{"id":"21","name":"Start work","to":{"name":"In Progress"}}]),
        )
        .await;
        Mock::given(method("POST"))
            .and(path(format!("/rest/api/3/issue/{key}/transitions")))
            .and(body_json(json!({"transition":{"id":"21"}})))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    let output = run_jira_against(
        &server,
        &[
            "issues",
            "bulk-transition",
            "--jql",
            "project = PROJ",
            "--to",
            "in progress",
            "--yes",
        ],
    );
    assert_eq!(output.status.code(), Some(exit_codes::BULK_FAILURE));
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(data["succeeded"], 2);
    assert_eq!(data["failed"], 1);
    assert_eq!(data["issues"][1]["errorKind"], "not_found");
    assert_eq!(data["issues"][1]["retryable"], false);
    assert_json_keys_match_schema("issues bulk-transition", &data, &[]);
    let error = error_envelope(&String::from_utf8_lossy(&output.stderr));
    assert_eq!(error["error"]["kind"], "bulk_failure");
    assert_eq!(error["error"]["details"]["resultsStream"], "stdout");
}

#[tokio::test]
async fn bulk_assignment_failure_stops_on_session_errors_and_marks_remaining_issues() {
    for status in [400, 401, 403, 429, 503] {
        let server = MockServer::start().await;
        search_issues(&server, 3).await;
        Mock::given(method("PUT"))
            .and(path("/rest/api/3/issue/PROJ-1/assignee"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/api/3/issue/PROJ-2/assignee"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/api/3/issue/PROJ-3/assignee"))
            .respond_with(ResponseTemplate::new(204))
            .expect(if status == 400 { 1 } else { 0 })
            .mount(&server)
            .await;
        let output = run_jira_against(
            &server,
            &[
                "issues",
                "bulk-assign",
                "--jql",
                "project = PROJ",
                "--assignee",
                "none",
                "--yes",
                "--quiet",
            ],
        );
        assert_eq!(output.status.code(), Some(exit_codes::BULK_FAILURE));
        let data: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(data["succeeded"], if status == 400 { 2 } else { 1 });
        assert_eq!(data["failed"], 1);
        assert_eq!(data["notAttempted"], if status == 400 { 0 } else { 1 });
        assert_json_keys_match_schema("issues bulk-assign", &data, &[]);
        error_envelope(&String::from_utf8_lossy(&output.stderr));
    }
}

#[tokio::test]
async fn transition_matching_is_consistent_and_ambiguous_errors_are_one_json_document() {
    let server = MockServer::start().await;
    transitions(
        &server,
        "PROJ-1",
        json!([
            {"id":"21","name":"Start work","to":{"name":"In Progress"}},
            {"id":"22","name":"Resume","to":{"name":"In Progress"}},
            {"id":"23","name":"21","to":{"name":"Done"}}
        ]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .and(body_json(json!({"transition":{"id":"21"}})))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&server)
        .await;
    for input in ["21", "START WORK"] {
        let data = parse_success(run_jira_against(
            &server,
            &["issues", "transition", "PROJ-1", "--to", input],
        ));
        assert_eq!(data["id"], "21");
    }
    for (input, code, kind) in [
        ("In Progress", 2, "invalid_input"),
        ("missing", 4, "not_found"),
    ] {
        let output = run_jira_against(
            &server,
            &["issues", "transition", "PROJ-1", "--to", input, "--quiet"],
        );
        assert_eq!(output.status.code(), Some(code));
        assert!(output.stdout.is_empty());
        let envelope = error_envelope(&String::from_utf8_lossy(&output.stderr));
        assert_eq!(envelope["error"]["kind"], kind);
        assert_eq!(
            envelope["error"]["details"]["candidates"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            envelope["error"]["details"]["hint"],
            "jira issues list-transitions PROJ-1"
        );
    }
    search_issues(&server, 1).await;
    let failed = run_jira_against_read_only(
        &server,
        &[
            "issues",
            "bulk-transition",
            "--jql",
            "project = PROJ",
            "--to",
            "In Progress",
            "--dry-run",
        ],
    );
    assert_eq!(failed.status.code(), Some(9));
    let data: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(data["issues"][0]["errorKind"], "invalid_input");
    assert_json_keys_match_schema("issues bulk-transition", &data, &[]);
    let preview = parse_success(run_jira_against_read_only(
        &server,
        &[
            "issues",
            "bulk-transition",
            "--jql",
            "project = PROJ",
            "--to",
            "Start work",
            "--dry-run",
        ],
    ));
    assert_preview_schema("issues bulk-transition", &preview);
    assert_eq!(preview["ready"], 1);
    assert_eq!(preview["issues"][0]["transitionId"], "21");
    assert_json_keys_match_schema("issues bulk-transition", &preview, &[]);
}

#[tokio::test]
async fn destination_status_resolves_for_single_transition() {
    let server = MockServer::start().await;
    transitions(
        &server,
        "PROJ-1",
        json!([{"id":"21","name":"Start work","to":{"name":"In Progress"}}]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .and(body_json(json!({"transition":{"id":"21"}})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let result = parse_success(run_jira_against(
        &server,
        &["issues", "transition", "PROJ-1", "--to", "in progress"],
    ));
    assert_eq!(result["status"], "In Progress");
}

#[tokio::test]
async fn project_sprints_are_scoped_deduplicated_and_include_all_board_context() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/rest/agile/1.0/board")).and(query_param("projectKeyOrId","PROJ"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"isLast":true,"values":[
            {"id":2,"name":"Delivery","type":"scrum"}, {"id":3,"name":"Flow","type":"kanban"}, {"id":1,"name":"Team","type":"simple"}
        ]}))).expect(2).mount(&server).await;
    for id in [1, 2] {
        Mock::given(method("GET")).and(path(format!("/rest/agile/1.0/board/{id}/sprint")))
            .and(query_param("state","active,future"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"isLast":true,"values":[{"id":8,"name":"Iteration","state":"active","originBoardId":2}]})))
            .expect(1).mount(&server).await;
    }
    let result = parse_success(run_jira_against(
        &server,
        &[
            "sprints",
            "list",
            "--project",
            "PROJ",
            "--state",
            "active,future",
        ],
    ));
    assert_eq!(result["total"], 1);
    let dir = TempDir::new().unwrap();
    let schema = parse_success(
        jira_cmd(&dir)
            .args(["schema", "--command", "sprints list"])
            .output()
            .unwrap(),
    );
    assert!(
        jsonschema::validator_for(&schema["stdout_schema"])
            .unwrap()
            .is_valid(&result)
    );
    assert_eq!(result["sprints"][0]["boardId"], 2);
    assert_eq!(
        result["sprints"][0]["boards"],
        json!([{"id":1,"name":"Team"},{"id":2,"name":"Delivery"}])
    );
    assert_json_keys_match_schema("sprints list", &result["sprints"][0], &[]);
    let boards = parse_success(run_jira_against(&server, &["boards", "list", "-p", "PROJ"]));
    assert_eq!(boards["total"], 3);
    assert_json_keys_match_schema("boards list", &boards["boards"][0], &[]);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().contains("board/3/sprint"))
    );
}

#[tokio::test]
async fn explicit_board_uses_direct_lookup_and_validates_states_without_http() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id":1,"name":"Team","type":"scrum"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .and(query_param("state", "active,future"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"isLast":true,"values":[]})))
        .expect(1)
        .mount(&server)
        .await;
    parse_success(run_jira_against(
        &server,
        &[
            "sprints", "list", "--board", "1", "--state", "active", "--state", "future",
        ],
    ));
    let bad = run_jira_against(&server, &["sprints", "list", "--state", "bogus"]);
    assert_eq!(bad.status.code(), Some(2));
    error_envelope(&String::from_utf8_lossy(&bad.stderr));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn kanban_unknown_board_and_unknown_project_get_actionable_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/3"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id":3,"name":"Flow","type":"kanban"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"isLast":true,"values":[]})))
        .mount(&server)
        .await;
    for args in [
        vec!["sprints", "list", "--board", "3"],
        vec!["sprints", "list", "--board", "Unknown"],
        vec!["sprints", "list", "--project", "MISSING"],
    ] {
        let output = run_jira_against(&server, &args);
        assert!(!output.status.success());
        let error = error_envelope(&String::from_utf8_lossy(&output.stderr));
        if args[3] == "3" {
            assert!(
                error["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("kanban")
            );
        } else {
            assert_eq!(error["error"]["kind"], "not_found");
        }
    }
}

async fn create_metadata(server: &MockServer, fields: Value) {
    Mock::given(method("GET")).and(path("/rest/api/3/issue/createmeta/PROJ/issuetypes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"isLast":true,"issueTypes":[
            {"id":"100","name":"Story","hierarchyLevel":0}, {"id":"101","name":"Epic","hierarchyLevel":1}
        ]}))).mount(server).await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/createmeta/PROJ/issuetypes/100"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"isLast":true,"fields":fields})),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn create_meta_lists_types_and_reports_fields_defaults_and_epic_evidence() {
    let server = MockServer::start().await;
    create_metadata(&server,json!([
        {"fieldId":"summary","name":"Summary","required":true},
        {"fieldId":"priority","required":true,"hasDefaultValue":true,"defaultValue":{"id":"3"},"allowedValues":[{"id":"3","name":"3 - Medium"}]},
        {"fieldId":"components","allowedValues":[]}, {"fieldId":"parent","required":false}
    ])).await;
    let listing = parse_success(run_jira_against_read_only(
        &server,
        &["issues", "create-meta", "-p", "PROJ"],
    ));
    assert!(listing["issueType"].is_null());
    assert!(listing["fields"].is_null());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_json_keys_match_schema("issues create-meta", &listing, &[]);
    let selected = parse_success(run_jira_against_read_only(
        &server,
        &["issues", "create-meta", "-p", "PROJ", "-t", "story"],
    ));
    assert_eq!(selected["issueType"]["id"], "100");
    assert_eq!(selected["fields"]["summary"]["allowedValues"], Value::Null);
    assert_eq!(selected["fields"]["components"]["allowedValues"], json!([]));
    assert_eq!(
        selected["fields"]["priority"]["defaultValue"],
        json!({"id":"3"})
    );
    assert_eq!(selected["epic"]["status"], "available");
    assert_eq!(selected["epic"]["mechanism"], "parent");
    assert_json_keys_match_schema("issues create-meta", &selected, &[]);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
}

fn assert_preview_schema(command: &str, data: &Value) {
    let dir = TempDir::new().unwrap();
    let schema = parse_success(
        jira_cmd(&dir)
            .args(["schema", "--command", command])
            .output()
            .unwrap(),
    );
    assert_eq!(schema["x-dry-run"]["effects"], "read_only");
    if let Some(stdout_schema) = schema.get("stdout_schema") {
        let validator = jsonschema::validator_for(stdout_schema).unwrap();
        assert!(
            validator.is_valid(data),
            "Preview must satisfy complete stdout schema: {data}"
        );
    }
    assert_fields_match(
        command,
        schema["x-dry-run"]["output_fields"].as_array().unwrap(),
        data,
        &[],
    );
}

#[tokio::test]
async fn create_preview_matches_real_payload_and_plans_both_steps_without_writes() {
    let server = MockServer::start().await;
    create_metadata(
        &server,
        json!([
            {"fieldId":"summary","required":true},
            {"fieldId":"priority","allowedValues":[{"id":"3","name":"3 - Medium"}]},
            {"fieldId":"parent"}
        ]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-9"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"fields":{"issuetype":{"name":"Epic"}}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/sprint/8"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":8,"name":"Iteration","state":"active"})),
        )
        .mount(&server)
        .await;
    let args = [
        "issues",
        "create",
        "-p",
        "PROJ",
        "-t",
        "story",
        "-s",
        "Example",
        "--priority",
        "Medium",
        "--epic",
        "PROJ-9",
        "--sprint",
        "8",
        "--field",
        "customfield_12000=5",
    ];
    let mut preview_args = args.to_vec();
    preview_args.push("--dry-run");
    let preview = parse_success(run_jira_against_read_only(&server, &preview_args));
    assert_eq!(preview["fields"]["priority"], json!({"id":"3"}));
    assert_eq!(preview["fields"]["parent"], json!({"key":"PROJ-9"}));
    assert_eq!(preview["fields"]["customfield_12000"], 5);
    assert_eq!(preview["steps"], json!(["create_issue", "move_to_sprint"]));
    assert!(preview["key"].is_null());
    assert_preview_schema("issues create", &preview);
    let before = server.received_requests().await.unwrap();
    assert!(before.iter().all(|r| r.method == "GET"));
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .and(body_json(json!({"fields":preview["fields"]})))
        .respond_with(ResponseTemplate::new(201).set_body_json(
            json!({"id":"1","key":"PROJ-1","self":"http://example.invalid/issue/1"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/agile/1.0/sprint/8/issue"))
        .and(body_json(json!({"issues":["PROJ-1"]})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    parse_success(run_jira_against(&server, &args));
    let after = server.received_requests().await.unwrap();
    assert_eq!(
        after.iter().filter(|r| r.method == "GET").count(),
        2 * before.len()
    );
}

#[tokio::test]
async fn update_and_move_previews_are_read_only_and_match_real_write_inputs() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/rest/api/3/issue/PROJ-1/editmeta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"fields":{"priority":{"allowedValues":[{"id":"3","name":"3 - Medium"}]},"labels":{}}})))
        .mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"fields":{"project":{"key":"PROJ"}}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/sprint/8"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":8,"name":"Iteration","state":"future"})),
        )
        .mount(&server)
        .await;
    let update_args = [
        "issues",
        "update",
        "PROJ-1",
        "--priority",
        "Medium",
        "--labels",
        "none",
        "--assignee",
        "none",
    ];
    let mut dry = update_args.to_vec();
    dry.push("--dry-run");
    let preview = parse_success(run_jira_against_read_only(&server, &dry));
    assert_eq!(
        preview["fields"],
        json!({"priority":{"id":"3"},"labels":[],"assignee":null})
    );
    assert_preview_schema("issues update", &preview);
    let move_args = ["issues", "move", "PROJ-1", "--sprint", "8"];
    let mut dry_move = move_args.to_vec();
    dry_move.push("--dry-run");
    let move_preview = parse_success(run_jira_against_read_only(&server, &dry_move));
    assert_preview_schema("issues move", &move_preview);
    assert_eq!(move_preview["steps"], json!(["move_to_sprint"]));
    assert_eq!(move_preview["sprint"]["id"], 8);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_json(json!({"fields":preview["fields"]})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/agile/1.0/sprint/8/issue"))
        .and(body_json(json!({"issues":["PROJ-1"]})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    parse_success(run_jira_against(&server, &update_args));
    parse_success(run_jira_against(&server, &move_args));
}

#[tokio::test]
async fn update_sprint_only_and_combined_partial_success_keep_the_issue_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/sprint/8"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":8,"name":"Iteration","state":"active"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"fields":{"project":{"key":"PROJ"}}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/editmeta"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"fields":{"summary":{},"description":{}}})),
        )
        .mount(&server)
        .await;
    let preview = parse_success(run_jira_against(
        &server,
        &["issues", "update", "PROJ-1", "--sprint", "8", "--dry-run"],
    ));
    assert_eq!(preview["steps"], json!(["move_to_sprint"]));
    assert_preview_schema("issues update", &preview);
    let preview = parse_success(run_jira_against(
        &server,
        &[
            "issues",
            "update",
            "PROJ-1",
            "-s",
            "New title",
            "-d",
            "Body",
            "--sprint",
            "8",
            "--dry-run",
        ],
    ));
    assert_eq!(preview["steps"], json!(["update_issue", "move_to_sprint"]));
    assert_eq!(preview["fields"]["summary"], "New title");
    assert_preview_schema("issues update", &preview);
    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/agile/1.0/sprint/8/issue"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"errorMessages":["Move rejected"]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = run_jira_against(
        &server,
        &[
            "issues",
            "update",
            "PROJ-1",
            "-s",
            "New title",
            "-d",
            "Body",
            "--sprint",
            "8",
            "--json",
        ],
    );
    assert_eq!(result.status.code(), Some(8));
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "partial_success");
    assert_eq!(error["error"]["details"]["key"], "PROJ-1");
    assert_eq!(error["error"]["details"]["updated"], true);
    assert_eq!(error["error"]["details"]["created"], false);
    assert_eq!(
        error["error"]["details"]["recoveryCommand"],
        "jira issues move PROJ-1 --sprint 8"
    );
}

#[tokio::test]
async fn sprint_only_update_moves_without_a_field_write() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/sprint/8"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":8,"name":"Iteration","state":"active"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/agile/1.0/sprint/8/issue"))
        .and(body_json(json!({"issues":["PROJ-1"]})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let result = parse_success(run_jira_against(
        &server,
        &["issues", "update", "PROJ-1", "--sprint", "8"],
    ));
    assert_eq!(result["updated"], true);
    assert_eq!(result["sprintId"], 8);
    assert_json_keys_match_schema("issues update", &result, &[]);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method != "PUT")
    );
}

#[tokio::test]
async fn previews_reject_missing_targets_and_preserve_metadata_error_kinds() {
    for status in [401, 429, 500] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/createmeta/PROJ/issuetypes"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        for args in [
            vec!["issues", "create-meta", "-p", "PROJ"],
            vec![
                "issues",
                "create",
                "-p",
                "PROJ",
                "-s",
                "Example",
                "--dry-run",
            ],
        ] {
            let output = run_jira_against(&server, &args);
            assert!(!output.status.success());
            let error = error_envelope(&String::from_utf8_lossy(&output.stderr));
            assert_eq!(
                error["error"]["kind"],
                match status {
                    401 => "auth",
                    429 => "rate_limit",
                    _ => "api_error",
                }
            );
        }
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/createmeta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"projects":[]})))
        .mount(&server)
        .await;
    for args in [
        vec!["issues", "create-meta", "-p", "MISSING"],
        vec![
            "issues",
            "update",
            "PROJ-999",
            "--summary",
            "Example",
            "--dry-run",
        ],
        vec![
            "issues",
            "create",
            "-p",
            "MISSING",
            "-s",
            "Example",
            "--dry-run",
        ],
    ] {
        let output = run_jira_against(&server, &args);
        assert_eq!(output.status.code(), Some(4));
        error_envelope(&String::from_utf8_lossy(&output.stderr));
    }
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
async fn unavailable_field_metadata_is_reported_without_claiming_full_validation() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/createmeta/PROJ/issuetypes"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"isLast":true,"issueTypes":[{"id":"100","name":"Story"}]})),
        )
        .mount(&server)
        .await;
    let data = parse_success(run_jira_against(
        &server,
        &["issues", "create-meta", "-p", "PROJ", "-t", "Story"],
    ));
    assert!(data["fields"].is_null());
    assert_eq!(data["epic"]["status"], "unavailable");
    assert!(!data["warnings"].as_array().unwrap().is_empty());
    let preview = parse_success(run_jira_against(
        &server,
        &[
            "issues",
            "create",
            "-p",
            "PROJ",
            "-t",
            "Story",
            "-s",
            "Example",
            "--dry-run",
        ],
    ));
    assert_eq!(
        preview["metadata"],
        json!({"issueTypes":true,"fields":false})
    );
    assert_preview_schema("issues create", &preview);
}

#[test]
fn schema_arg_types_constraints_enums_defaults_and_previews_match_the_parser() {
    let dir = TempDir::new().unwrap();
    let schema = parse_success(jira_cmd(&dir).args(["schema"]).output().unwrap());
    let command = |name: &str| {
        schema["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == name)
            .unwrap()
    };
    assert_eq!(command("issues transition")["effects"], "non_idempotent");
    assert_eq!(
        command("issues bulk-transition")["effects"],
        "non_idempotent"
    );
    let argument = |name: &str, arg: &str| {
        command(name)["args"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == arg)
            .unwrap()
    };
    assert_eq!(argument("issues create", "--board")["type"], "integer");
    assert_eq!(
        argument("issues create", "--board")["requires"],
        json!(["--sprint"])
    );
    assert_eq!(
        argument("issues create", "--epic")["conflicts_with"],
        json!(["--parent"])
    );
    assert_eq!(
        argument("issues update", "--epic")["conflicts_with"],
        json!(["--clear-epic"])
    );
    assert_eq!(argument("issues move", "--dry-run")["default"], false);
    assert_eq!(argument("issues move", "--board")["type"], "integer");
    assert_eq!(
        argument("completions", "shell")["enum"],
        json!(["bash", "elvish", "fish", "powershell", "zsh"])
    );
    assert_eq!(
        argument("sprints list", "--state")["enum"],
        json!(["active", "closed", "future", "all"])
    );
    assert_eq!(argument("sprints list", "--state")["type"], "string[]");
    assert_eq!(
        argument("sprints list", "--state")["default"],
        json!(["active"])
    );
    for args in [
        vec![
            "issues", "create", "-p", "PROJ", "-s", "Example", "--board", "1",
        ],
        vec![
            "issues",
            "update",
            "PROJ-1",
            "--epic",
            "PROJ-9",
            "--clear-epic",
        ],
    ] {
        let output = jira_cmd(&dir).args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            error_envelope(&String::from_utf8_lossy(&output.stderr))["error"]["kind"],
            "invalid_input"
        );
    }
    for name in [
        "issues create",
        "issues update",
        "issues move",
        "issues bulk-assign",
        "issues bulk-transition",
    ] {
        assert_eq!(command(name)["x-dry-run"]["effects"], "read_only");
        assert!(
            !command(name)["x-dry-run"]["output_fields"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn data_center_create_meta_handles_epic_links_ambiguity_and_type_hierarchy() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/rest/api/2/issue/createmeta"))
        .and(query_param("projectKeys", "PROJ"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"projects":[{"key":"PROJ","issuetypes":[
            {"id":"100","name":"Story","fields":{"customfield_12001":{"name":"Epic Link","schema":{"custom":"com.pyxis.greenhopper.jira:gh-epic-link"}}}},
            {"id":"101","name":"Epic","fields":{}},
            {"id":"102","name":"Bug","fields":{"customfield_12001":{"name":"Epic Link"},"customfield_12002":{"name":"Epic Link"}}}
        ]}]}))).mount(&server).await;
    for (kind, status, field) in [
        ("Story", "available", Some("customfield_12001")),
        ("Epic", "not_applicable", None),
        ("Bug", "ambiguous", None),
    ] {
        let dir = TempDir::new().unwrap();
        let output = jira_cmd(&dir)
            .env("JIRA_HOST", server.uri())
            .env("JIRA_TOKEN", "test-token")
            .env("JIRA_EMAIL", "test@example.com")
            .env("JIRA_API_VERSION", "2")
            .args(["issues", "create-meta", "-p", "PROJ", "-t", kind])
            .output()
            .unwrap();
        let result = parse_success(output);
        assert_eq!(result["epic"]["status"], status);
        assert_eq!(result["epic"]["field"], json!(field));
        assert_json_keys_match_schema("issues create-meta", &result, &[]);
    }
}

#[tokio::test]
async fn create_metadata_and_bulk_previews_are_readable_in_text_mode() {
    let server = MockServer::start().await;
    create_metadata(&server,json!([{"fieldId":"priority","name":"Priority","required":true,"hasDefaultValue":true,"defaultValue":{"id":"3","name":"3 - Medium"},"allowedValues":[{"id":"3","name":"3 - Medium"}]}])).await;
    let result = run_jira_against(
        &server,
        &[
            "issues",
            "create-meta",
            "-p",
            "PROJ",
            "-t",
            "Story",
            "-o",
            "text",
        ],
    );
    assert!(result.status.success());
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains("Priority [priority]: required, default provided"));
    assert!(text.contains("Allowed: 3 - Medium (3)"));
    search_issues(&server, 1).await;
    let output = run_jira_against_read_only(
        &server,
        &[
            "issues",
            "bulk-assign",
            "--jql",
            "project = PROJ",
            "--assignee",
            "none",
            "--dry-run",
            "-o",
            "text",
            "--quiet",
        ],
    );
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("PROJ-1  would assign none")
    );
}

#[test]
fn every_preview_contract_has_a_conformance_test() {
    let dir = TempDir::new().unwrap();
    let schema = parse_success(jira_cmd(&dir).args(["schema"]).output().unwrap());
    let declared: std::collections::BTreeSet<_> = schema["commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|command| command.get("x-dry-run").is_some())
        .map(|command| command["name"].as_str().unwrap())
        .collect();
    let checked = [
        "issues create",
        "issues update",
        "issues move",
        "issues bulk-assign",
        "issues bulk-transition",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        declared, checked,
        "Every preview must have a test against x-dry-run.output_fields"
    );
}

#[tokio::test]
async fn bulk_transition_abort_distinguishes_failed_lookups_from_uncertain_writes() {
    for phase in ["lookup", "write"] {
        let server = MockServer::start().await;
        search_issues(&server, 3).await;
        transitions(
            &server,
            "PROJ-1",
            json!([{"id":"21","name":"Start","to":{"name":"In Progress"}}]),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/PROJ-1/transitions"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        if phase == "lookup" {
            Mock::given(method("GET"))
                .and(path("/rest/api/3/issue/PROJ-2/transitions"))
                .respond_with(ResponseTemplate::new(429))
                .expect(1)
                .mount(&server)
                .await;
        } else {
            transitions(
                &server,
                "PROJ-2",
                json!([{"id":"21","name":"Start","to":{"name":"In Progress"}}]),
            )
            .await;
            Mock::given(method("POST"))
                .and(path("/rest/api/3/issue/PROJ-2/transitions"))
                .respond_with(ResponseTemplate::new(503))
                .expect(1)
                .mount(&server)
                .await;
        }
        let output = run_jira_against(
            &server,
            &[
                "issues",
                "bulk-transition",
                "--jql",
                "project = PROJ",
                "--to",
                "Start",
                "--yes",
            ],
        );
        assert_eq!(output.status.code(), Some(9));
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["succeeded"], 1);
        assert_eq!(result["failed"], 1);
        assert_eq!(result["notAttempted"], 1);
        assert_eq!(result["issues"][1]["phase"], phase);
        assert_eq!(
            result["issues"][1]["outcome"],
            if phase == "write" {
                "unknown"
            } else {
                "failed"
            }
        );
        assert_eq!(result["issues"][0]["destinationStatus"], "In Progress");
        assert_json_keys_match_schema("issues bulk-transition", &result, &[]);
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| !r.url.path().contains("PROJ-3"))
        );
    }
}

#[tokio::test]
async fn mixed_bulk_preview_counts_valid_and_invalid_issues_without_writes() {
    let server = MockServer::start().await;
    search_issues(&server, 2).await;
    transitions(
        &server,
        "PROJ-1",
        json!([{"id":"21","name":"Start","to":{"name":"In Progress"}}]),
    )
    .await;
    transitions(
        &server,
        "PROJ-2",
        json!([{"id":"31","name":"Close","to":{"name":"Done"}}]),
    )
    .await;
    let result = run_jira_against_read_only(
        &server,
        &[
            "issues",
            "bulk-transition",
            "--jql",
            "project = PROJ",
            "--to",
            "Start",
            "--dry-run",
        ],
    );
    assert_eq!(result.status.code(), Some(9));
    let preview: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(preview["total"], 2);
    assert_eq!(preview["ready"], 1);
    assert_eq!(preview["failed"], 1);
    assert_eq!(preview["succeeded"], 0);
    assert_preview_schema("issues bulk-transition", &preview);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET" || r.url.path() == "/rest/api/3/search/jql")
    );
}

#[tokio::test]
async fn discovery_and_move_agree_on_mixed_board_types_and_skip_only_unsupported_sprints() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .and(query_param("projectKeyOrId", "PROJ"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"isLast":true,"values":[
                {"id":1,"name":"Team without sprints","type":"simple"},
                {"id":2,"name":"Team Scrum","type":"scrum"},
                {"id":3,"name":"Team Kanban","type":"kanban"},
                {"id":4,"name":"Team with sprints","type":"simple"}
            ]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"errorMessages":["The board does not support sprints"]})),
        )
        .mount(&server)
        .await;
    for (id, sprint, name) in [(2, 8, "Current"), (4, 9, "Next")] {
        Mock::given(method("GET"))
            .and(path(format!("/rest/agile/1.0/board/{id}/sprint")))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"isLast":true,"values":[{"id":sprint,"name":name,"state":"active"}]}),
            ))
            .mount(&server)
            .await;
    }
    let listed = parse_success(run_jira_against(
        &server,
        &["sprints", "list", "-p", "PROJ", "--board", "Team"],
    ));
    assert_eq!(listed["total"], 2);
    assert_eq!(listed["warnings"].as_array().unwrap().len(), 2);
    assert!(listed["warnings"][0].as_str().unwrap().contains("Board 1"));
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"fields":{"project":{"key":"PROJ"}}})),
        )
        .mount(&server)
        .await;
    let moved = parse_success(run_jira_against_read_only(
        &server,
        &["issues", "move", "PROJ-1", "--sprint", "Next", "--dry-run"],
    ));
    assert_eq!(moved["sprint"]["id"], 9);
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().contains("board/3/sprint"))
    );
}

#[tokio::test]
async fn unrelated_board_api_failures_are_not_hidden_as_unsupported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"isLast":true,"values":[{"id":1,"name":"Team","type":"simple"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"errorMessages":["Invalid state filter"]})),
        )
        .mount(&server)
        .await;
    let result = run_jira_against(&server, &["sprints", "list"]);
    assert_eq!(result.status.code(), Some(5));
    assert_eq!(
        error_envelope(&String::from_utf8_lossy(&result.stderr))["error"]["kind"],
        "api_error"
    );
}

#[tokio::test]
async fn server_contraction_is_skipped_but_explicit_board_names_the_problem() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"isLast":true,"values":[
                {"id":1,"name":"Flow","type":"simple"},
                {"id":2,"name":"Delivery","type":"scrum"}
            ]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"errorMessages":["The board doesn't support sprints."]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id":1,"name":"Flow","type":"simple"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/2/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"isLast":true,"values":[{"id":8,"name":"Current","state":"active"}]}),
        ))
        .mount(&server)
        .await;
    let listed = parse_success(run_jira_against(&server, &["sprints", "list"]));
    assert_eq!(listed["sprints"][0]["id"], 8);
    assert!(listed["warnings"][0].as_str().unwrap().contains("Flow"));
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"fields":{"project":{"key":"PROJ"}}})),
        )
        .mount(&server)
        .await;
    let preview = parse_success(run_jira_against(
        &server,
        &[
            "issues",
            "move",
            "PROJ-1",
            "--sprint",
            "active",
            "--dry-run",
        ],
    ));
    assert_eq!(preview["sprint"]["id"], 8);
    let explicit = run_jira_against(&server, &["sprints", "list", "--board", "1"]);
    let message = String::from_utf8_lossy(&explicit.stderr);
    assert!(
        message.contains("1") && message.contains("Flow") && message.contains("simple"),
        "{message}"
    );
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
async fn epic_discovery_and_write_prefer_schema_match_over_a_similarly_named_field() {
    let server = MockServer::start().await;
    create_metadata(&server,json!([
        {"fieldId":"customfield_12001","name":"Epic Link","schema":{"custom":"com.pyxis.greenhopper.jira:gh-epic-link"}},
        {"fieldId":"customfield_12002","name":"Epic Link","schema":{"custom":"unrelated"}}
    ])).await;
    let discovered = parse_success(run_jira_against(
        &server,
        &["issues", "create-meta", "-p", "PROJ", "-t", "Story"],
    ));
    assert_eq!(discovered["epic"]["status"], "available");
    assert_eq!(discovered["epic"]["field"], "customfield_12001");
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-9"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"fields":{"issuetype":{"name":"Epic"}}})),
        )
        .mount(&server)
        .await;
    let preview = parse_success(run_jira_against_read_only(
        &server,
        &[
            "issues",
            "create",
            "-p",
            "PROJ",
            "-t",
            "Story",
            "-s",
            "Example",
            "--epic",
            "PROJ-9",
            "--dry-run",
        ],
    ));
    assert_eq!(preview["fields"]["customfield_12001"], "PROJ-9");
    assert!(preview["fields"].get("customfield_12002").is_none());
}

#[tokio::test]
async fn unsupported_metadata_and_absent_legacy_fields_are_distinct_from_missing_projects() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/PROJ"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id":"1","key":"PROJ","name":"Example"})),
        )
        .mount(&server)
        .await;
    let output = run_jira_against(&server, &["issues", "create-meta", "-p", "PROJ"]);
    assert_eq!(output.status.code(), Some(4));
    let error = error_envelope(&String::from_utf8_lossy(&output.stderr));
    assert_eq!(error["error"]["details"]["reason"], "unsupported");
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/createmeta/PROJ/issuetypes"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"isLast":true,"issueTypes":[{"id":"100","name":"Story"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/createmeta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"projects":[]})))
        .mount(&server)
        .await;
    let preview = parse_success(run_jira_against_read_only(
        &server,
        &[
            "issues",
            "create",
            "-p",
            "PROJ",
            "-t",
            "Story",
            "-s",
            "Example",
            "--dry-run",
        ],
    ));
    assert_eq!(preview["metadata"]["fields"], false);
    assert!(!preview["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn schema_omits_clap_generated_help_commands_and_flags() {
    let dir = TempDir::new().unwrap();
    let schema = parse_success(jira_cmd(&dir).args(["schema"]).output().unwrap());
    for command in schema["commands"].as_array().unwrap() {
        assert!(
            !command["name"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .any(|word| word == "help")
        );
        for arg in command["args"].as_array().unwrap() {
            assert!(arg["name"] != "--help" && arg["name"] != "--version");
        }
    }
}
