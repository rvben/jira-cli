//! `sprints show`: story point and issue totals counted the way the board's
//! sprint views count them, with the board's own estimation field.

use super::*;
use serde_json::{Value, json};
use wiremock::matchers::query_param;

const BASE_FIELDS: &str = "summary,status,issuetype,assignee,priority";

fn run(server: &MockServer, env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    let dir = TempDir::new().unwrap();
    let mut cmd = jira_cmd(&dir);
    cmd.args(args)
        .env("JIRA_HOST", server.uri())
        .env("JIRA_EMAIL", "test@example.com")
        .env("JIRA_TOKEN", "test-token");
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.output().unwrap()
}

fn stdout_json(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn error_message(output: &std::process::Output) -> String {
    assert!(!output.status.success(), "expected failure");
    let envelope: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)));
    envelope["error"]["message"].as_str().unwrap().to_owned()
}

fn status(name: &str, category: &str) -> Value {
    json!({"name": name, "statusCategory": {"key": category, "name": name}})
}

/// An issue as the Agile sprint listing returns it, with `extra` fields merged in.
fn issue(key: &str, status: Value, issue_type: &str, subtask: bool, extra: Value) -> Value {
    let mut fields = json!({
        "summary": format!("{key} summary"),
        "status": status,
        "issuetype": {"name": issue_type, "subtask": subtask},
        "assignee": null,
        "priority": {"name": "Medium"},
    });
    fields
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({"id": format!("1{}", key.len()), "key": key, "fields": fields})
}

async fn mount_sprint(server: &MockServer, origin_board: Option<u64>) {
    let mut sprint = json!({
        "id": 7, "name": "Sprint 7", "state": "active",
        "startDate": "2026-09-28T08:00:00.000Z", "endDate": "2026-10-12T08:00:00.000Z",
        "goal": "Ship the importer",
    });
    if let Some(board) = origin_board {
        sprint["originBoardId"] = json!(board);
    }
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/sprint/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sprint))
        .mount(server)
        .await;
}

async fn mount_board(server: &MockServer, id: u64, estimation: Option<Value>) {
    Mock::given(method("GET"))
        .and(path(format!("/rest/agile/1.0/board/{id}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": id, "name": format!("Board {id}"), "type": "scrum"})),
        )
        .mount(server)
        .await;
    let mut config = json!({"id": id, "name": format!("Board {id}")});
    if let Some(estimation) = estimation {
        config["estimation"] = estimation;
    }
    Mock::given(method("GET"))
        .and(path(format!("/rest/agile/1.0/board/{id}/configuration")))
        .respond_with(ResponseTemplate::new(200).set_body_json(config))
        .mount(server)
        .await;
}

/// The sprint's issues on `board`, answering only a request for exactly `fields`.
async fn mount_issues(server: &MockServer, board: u64, fields: &str, issues: Value) {
    let total = issues.as_array().unwrap().len();
    Mock::given(method("GET"))
        .and(path(format!(
            "/rest/agile/1.0/board/{board}/sprint/7/issue"
        )))
        .and(query_param("fields", fields))
        .and(query_param("startAt", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"startAt": 0, "maxResults": 100, "total": total, "issues": issues}),
        ))
        .expect(1)
        .mount(server)
        .await;
}

/// Field discovery must not run when the board names its estimation field.
async fn forbid_field_catalog(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/rest/api/3/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(0)
        .mount(server)
        .await;
}

fn story_point_estimate() -> Value {
    json!({"type": "field", "field": {"fieldId": "customfield_10016", "displayName": "Story point estimate"}})
}

/// A sprint mixing every status category, an unestimated issue, a subtask and
/// fractional estimates. The site also carries a second points field the board
/// does not estimate with; its values must not leak into the totals.
fn mixed_sprint() -> Value {
    json!([
        issue(
            "A-1",
            status("To Do", "new"),
            "Story",
            false,
            json!({"customfield_10016": 3, "customfield_10002": 40})
        ),
        issue(
            "A-6",
            status("Review", "indeterminate"),
            "Story",
            false,
            json!({"customfield_10016": 0.1})
        ),
        issue(
            "A-7",
            status("Review", "indeterminate"),
            "Story",
            false,
            json!({"customfield_10016": 0.2})
        ),
        issue(
            "A-2",
            status("Done", "done"),
            "Story",
            false,
            json!({"customfield_10016": 5})
        ),
        issue(
            "A-3",
            status("Done", "done"),
            "Story",
            false,
            json!({"customfield_10016": 8})
        ),
        issue(
            "A-4",
            status("To Do", "new"),
            "Bug",
            false,
            json!({"customfield_10016": null})
        ),
        issue(
            "A-5",
            status("Done", "done"),
            "Sub-task",
            true,
            json!({"customfield_10016": 2})
        ),
    ])
}

#[tokio::test]
async fn totals_sum_the_board_estimation_field_by_status_category() {
    let server = MockServer::start().await;
    mount_sprint(&server, Some(12)).await;
    mount_board(&server, 12, Some(story_point_estimate())).await;
    mount_issues(
        &server,
        12,
        &format!("{BASE_FIELDS},customfield_10016"),
        mixed_sprint(),
    )
    .await;
    forbid_field_catalog(&server).await;

    let json = stdout_json(&run(&server, &[], &["sprints", "show", "7", "--json"]));

    assert_json_keys_match_schema("sprints show", &json, &[]);
    assert_eq!(json["board"], json!({"id": 12, "name": "Board 12"}));
    assert_eq!(json["goal"], "Ship the importer");
    assert_eq!(
        json["estimation"],
        json!({"source": "board", "fieldIds": ["customfield_10016"], "fieldName": "Story point estimate"})
    );
    // 0.1 + 0.2 sums to 0.30000000000000004 in floating point.
    assert_eq!(
        json["totals"]["points"],
        json!({"total": 16.3, "toDo": 3, "inProgress": 0.3, "done": 13, "uncategorized": 0})
    );
    assert_eq!(
        json["totals"]["issues"],
        json!({"total": 6, "toDo": 2, "inProgress": 2, "done": 2, "uncategorized": 0})
    );
    assert_eq!(json["totals"]["estimated"], 5);
    assert_eq!(json["totals"]["unestimated"], 1);
    assert_eq!(json["totals"]["subtasksExcluded"], 1);
    assert_eq!(json["warnings"], json!([]));

    let issues = json["issues"].as_array().unwrap();
    assert_eq!(
        issues.len(),
        7,
        "subtasks are listed even though not counted"
    );
    assert_eq!(issues[0]["storyPoints"], 3);
    assert_eq!(issues[0]["statusCategory"], "new");
    assert_eq!(issues[5]["key"], "A-4");
    assert_eq!(issues[5]["storyPoints"], Value::Null);
    assert_eq!(issues[6]["subtask"], true);
    assert_eq!(issues[6]["storyPoints"], 2);
}

#[tokio::test]
async fn text_view_states_the_totals_and_the_excluded_subtasks() {
    let server = MockServer::start().await;
    mount_sprint(&server, Some(12)).await;
    mount_board(&server, 12, Some(story_point_estimate())).await;
    mount_issues(
        &server,
        12,
        &format!("{BASE_FIELDS},customfield_10016"),
        mixed_sprint(),
    )
    .await;

    let output = run(&server, &[], &["--output", "text", "sprints", "show", "7"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(
            "Points:   16.3 total, 3 to do, 0.3 in progress, 13 done (Story point estimate)"
        ),
        "{text}"
    );
    assert!(
        text.contains("Issues:   6 total, 2 to do, 2 in progress, 2 done, 1 unestimated"),
        "{text}"
    );
    assert!(text.contains("Subtasks: 1 listed below"), "{text}");
    assert!(text.contains("Goal:     Ship the importer"), "{text}");
    assert!(text.contains("A-5"), "{text}");
}

#[tokio::test]
async fn a_board_estimating_with_time_reports_no_point_totals() {
    let server = MockServer::start().await;
    mount_sprint(&server, Some(12)).await;
    mount_board(
        &server,
        12,
        Some(json!({"type": "field", "field": {"fieldId": "timeoriginalestimate", "displayName": "Original Time Estimate"}})),
    )
    .await;
    mount_issues(
        &server,
        12,
        BASE_FIELDS,
        json!([issue(
            "A-1",
            status("Done", "done"),
            "Story",
            false,
            json!({})
        )]),
    )
    .await;
    forbid_field_catalog(&server).await;

    let json = stdout_json(&run(&server, &[], &["sprints", "show", "7", "--json"]));

    assert_json_keys_match_schema("sprints show", &json, &[]);
    assert_eq!(json["totals"]["points"], Value::Null);
    assert_eq!(json["totals"]["estimated"], Value::Null);
    assert_eq!(json["totals"]["unestimated"], Value::Null);
    assert_eq!(json["totals"]["issues"]["done"], 1);
    assert_eq!(json["estimation"]["fieldIds"], json!([]));
    assert_eq!(json["estimation"]["fieldName"], "Original Time Estimate");
    assert!(
        json["issues"][0].get("storyPoints").is_none(),
        "unavailable points must not read as unestimated: {}",
        json["issues"][0]
    );
    assert_eq!(
        json["warnings"],
        json!([
            "Board 12 \"Board 12\" estimates with Original Time Estimate, not story points; story point totals are unavailable"
        ])
    );
}

#[tokio::test]
async fn a_board_estimating_by_issue_count_reports_no_point_totals() {
    let server = MockServer::start().await;
    mount_sprint(&server, Some(12)).await;
    mount_board(&server, 12, Some(json!({"type": "none"}))).await;
    mount_issues(
        &server,
        12,
        BASE_FIELDS,
        json!([issue(
            "A-1",
            status("To Do", "new"),
            "Story",
            false,
            json!({})
        )]),
    )
    .await;

    let json = stdout_json(&run(&server, &[], &["sprints", "show", "7", "--json"]));

    assert_eq!(json["totals"]["points"], Value::Null);
    assert_eq!(json["estimation"]["fieldName"], Value::Null);
    assert_eq!(
        json["warnings"],
        json!([
            "Board 12 \"Board 12\" estimates by issue count, not story points; story point totals are unavailable"
        ])
    );
}

#[tokio::test]
async fn without_a_board_estimation_the_profile_pin_stands_in_and_says_so() {
    let server = MockServer::start().await;
    mount_sprint(&server, Some(12)).await;
    mount_board(&server, 12, None).await;
    mount_issues(
        &server,
        12,
        &format!("{BASE_FIELDS},customfield_10002"),
        json!([issue(
            "A-1",
            status("To Do", "new"),
            "Story",
            false,
            json!({"customfield_10002": 13})
        )]),
    )
    .await;
    forbid_field_catalog(&server).await;

    let json = stdout_json(&run(
        &server,
        &[("JIRA_STORY_POINTS_FIELD", "customfield_10002")],
        &["sprints", "show", "7", "--json"],
    ));

    assert_eq!(
        json["estimation"],
        json!({"source": "profile", "fieldIds": ["customfield_10002"], "fieldName": null})
    );
    assert_eq!(json["totals"]["points"]["total"], 13);
    assert_eq!(
        json["warnings"],
        json!([
            "Board 12 \"Board 12\" reports no estimation statistic; story points are read from customfield_10002"
        ])
    );
}

#[tokio::test]
async fn without_a_board_estimation_discovered_fields_stand_in() {
    let server = MockServer::start().await;
    mount_sprint(&server, Some(12)).await;
    mount_board(&server, 12, None).await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": "customfield_10016", "name": "Story point estimate", "custom": true,
            "schema": {"type": "number", "custom": "com.pyxis.greenhopper.jira:jsw-story-points"}
        }])))
        .expect(1)
        .mount(&server)
        .await;
    mount_issues(
        &server,
        12,
        &format!("{BASE_FIELDS},customfield_10016"),
        json!([issue(
            "A-1",
            status("Done", "done"),
            "Story",
            false,
            json!({"customfield_10016": 5})
        )]),
    )
    .await;

    let json = stdout_json(&run(&server, &[], &["sprints", "show", "7", "--json"]));

    assert_eq!(json["estimation"]["source"], "fieldCatalog");
    assert_eq!(json["totals"]["points"]["done"], 5);
}

#[tokio::test]
async fn an_explicit_board_counts_the_sprint_as_that_board_shows_it() {
    let server = MockServer::start().await;
    mount_sprint(&server, Some(12)).await;
    // The sprint must be on board 30 for --board 30 to be accepted.
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/30/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [{"id": 7, "name": "Sprint 7", "state": "active", "originBoardId": 12}],
            "isLast": true
        })))
        .mount(&server)
        .await;
    mount_board(&server, 30, Some(story_point_estimate())).await;
    mount_issues(
        &server,
        30,
        &format!("{BASE_FIELDS},customfield_10016"),
        json!([issue(
            "A-1",
            status("To Do", "new"),
            "Story",
            false,
            json!({"customfield_10016": 2})
        )]),
    )
    .await;

    let json = stdout_json(&run(
        &server,
        &[],
        &["sprints", "show", "7", "--board", "30", "--json"],
    ));

    assert_eq!(json["board"]["id"], 30);
    assert_eq!(json["totals"]["points"]["total"], 2);
}

#[tokio::test]
async fn a_sprint_without_an_origin_board_needs_one_named() {
    let server = MockServer::start().await;
    mount_sprint(&server, None).await;

    let output = run(&server, &[], &["sprints", "show", "7", "--json"]);

    assert_eq!(
        error_message(&output),
        "Invalid input: Sprint 7 \"Sprint 7\" reports no origin board; pass --board <ID>"
    );
}

#[tokio::test]
async fn every_page_of_the_sprint_is_counted() {
    let server = MockServer::start().await;
    mount_sprint(&server, Some(12)).await;
    mount_board(&server, 12, Some(story_point_estimate())).await;
    let fields = format!("{BASE_FIELDS},customfield_10016");
    for (start, keys) in [("0", ["A-1", "A-2"].as_slice()), ("2", ["A-3"].as_slice())] {
        let issues: Vec<Value> = keys
            .iter()
            .map(|key| {
                issue(
                    key,
                    status("To Do", "new"),
                    "Story",
                    false,
                    json!({"customfield_10016": 1}),
                )
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/rest/agile/1.0/board/12/sprint/7/issue"))
            .and(query_param("fields", fields.as_str()))
            .and(query_param("startAt", start))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"startAt": start.parse::<u64>().unwrap(), "maxResults": 2, "total": 3, "issues": issues}),
            ))
            .expect(1)
            .mount(&server)
            .await;
    }

    let json = stdout_json(&run(&server, &[], &["sprints", "show", "7", "--json"]));

    assert_eq!(json["totals"]["points"]["total"], 3);
    assert_eq!(json["issues"].as_array().unwrap().len(), 3);
}
