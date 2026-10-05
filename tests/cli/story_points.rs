//! Story points in issue output: discovery on Cloud and Data Center, the
//! difference between unknown and unestimated, conflicts, and a pinned field.

use super::*;
use serde_json::{Value, json};
use wiremock::matchers::query_param;

const JSW_SCHEMA: &str = "com.pyxis.greenhopper.jira:jsw-story-points";
const FLOAT_SCHEMA: &str = "com.atlassian.jira.plugin.system.customfieldtypes:float";
const DETAIL_FIELDS: &str = "summary,status,assignee,reporter,priority,issuetype,parent,description,labels,components,fixVersions,versions,created,updated,comment,issuelinks";
const SEARCH_FIELDS: &str = "summary,status,assignee,priority,issuetype,parent,created,updated";

fn run(server: &MockServer, api: u8, env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    let dir = TempDir::new().unwrap();
    let mut cmd = jira_cmd(&dir);
    cmd.args(args)
        .env("JIRA_HOST", server.uri())
        .env("JIRA_EMAIL", "test@example.com")
        .env("JIRA_TOKEN", "test-token")
        .env("JIRA_API_VERSION", api.to_string());
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

fn stdout_text(output: &std::process::Output) -> String {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The error message from the JSON error envelope on stderr.
fn error_message(output: &std::process::Output) -> String {
    let envelope: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)));
    envelope["error"]["message"].as_str().unwrap().to_owned()
}

fn issue(key: &str, extra: Value) -> Value {
    let mut fields = json!({
        "summary": format!("{key} summary"),
        "status": {"name": "To Do"},
        "issuetype": {"name": "Story"},
    });
    fields
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({"id": "1", "key": key, "fields": fields})
}

fn jsw_field(id: &str) -> Value {
    json!({"id": id, "name": "Story point estimate", "custom": true,
           "schema": {"type": "number", "custom": JSW_SCHEMA}})
}

fn float_field(id: &str, name: &str) -> Value {
    json!({"id": id, "name": name, "custom": true,
           "schema": {"type": "number", "custom": FLOAT_SCHEMA}})
}

async fn catalog(server: &MockServer, api: u8, fields: Value, expect: u64) {
    Mock::given(method("GET"))
        .and(path(format!("/rest/api/{api}/field")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fields))
        .expect(expect)
        .mount(server)
        .await;
}

/// A Cloud search that answers only when asked for exactly `fields`.
async fn cloud_search(server: &MockServer, fields: &[&str], issues: Value) {
    let mut requested: Vec<&str> = SEARCH_FIELDS.split(',').collect();
    requested.extend_from_slice(fields);
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(body_partial_json(json!({"fields": requested})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"issues": issues, "isLast": true})),
        )
        .mount(server)
        .await;
}

fn summary_of(output: &str, key: &str) -> String {
    output
        .lines()
        .find(|line| line.starts_with(key))
        .unwrap_or_else(|| panic!("no {key} row in:\n{output}"))
        .to_owned()
}

#[tokio::test]
async fn cloud_reads_the_jira_software_estimate_and_keeps_unestimated_distinct() {
    let server = MockServer::start().await;
    // Also a text field that only shares the name, which must not be read.
    catalog(
        &server,
        3,
        json!([
            jsw_field("customfield_10016"),
            {"id": "customfield_10500", "name": "Story Points", "custom": true,
             "schema": {"type": "string", "custom": "com.atlassian.jira.plugin.system.customfieldtypes:textfield"}}
        ]),
        2,
    )
    .await;
    cloud_search(
        &server,
        &["customfield_10016"],
        json!([
            issue("PROJ-1", json!({"customfield_10016": 5})),
            issue("PROJ-2", json!({"customfield_10016": null})),
            issue(
                "PROJ-3",
                json!({"customfield_10016": 0.5, "customfield_10500": "lots"})
            ),
        ]),
    )
    .await;

    let listed = stdout_json(&run(&server, 3, &[], &["issues", "list", "--json"]));
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items[0]["storyPoints"], json!(5));
    assert_eq!(items[1].get("storyPoints"), Some(&Value::Null));
    assert_eq!(items[2]["storyPoints"], json!(0.5));

    let table = stdout_text(&run(
        &server,
        3,
        &[],
        &["search", "project = PROJ", "--output", "text"],
    ));
    let header = table.lines().next().unwrap();
    assert!(header.contains("Points"), "{table}");
    assert!(
        summary_of(&table, "PROJ-1").contains(" 5 PROJ-1 summary"),
        "{table}"
    );
    assert!(
        summary_of(&table, "PROJ-2").contains(" - PROJ-2 summary"),
        "{table}"
    );
    assert!(
        summary_of(&table, "PROJ-3").contains(" 0.5 PROJ-3 summary"),
        "{table}"
    );
}

#[tokio::test]
async fn data_center_reads_a_number_field_named_story_points() {
    let server = MockServer::start().await;
    let fields = json!([
        float_field("customfield_10002", "Story Points"),
        float_field("customfield_10003", "Story Points Remaining"),
    ]);
    catalog(&server, 2, fields, 3).await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search"))
        .and(query_param("fields", format!("{SEARCH_FIELDS},customfield_10002")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [issue("PROJ-1", json!({"customfield_10002": 3.5, "customfield_10003": 1.0}))],
            "total": 1, "startAt": 0, "maxResults": 50
        })))
        .mount(&server)
        .await;
    let mut detail = issue("PROJ-1", json!({"customfield_10002": 3.5}));
    detail["fields"]["comment"] = json!({"comments": [], "total": 0});
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/PROJ-1"))
        .and(query_param(
            "fields",
            format!("{DETAIL_FIELDS},customfield_10002"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(detail))
        .mount(&server)
        .await;

    let listed = stdout_json(&run(&server, 2, &[], &["issues", "list", "--json"]));
    assert_eq!(listed["items"][0]["storyPoints"], json!(3.5));

    let shown = stdout_json(&run(
        &server,
        2,
        &[],
        &["issues", "show", "PROJ-1", "--json"],
    ));
    assert_eq!(shown["storyPoints"], json!(3.5));
    assert_eq!(shown["warnings"], json!([]));

    let text = stdout_text(&run(
        &server,
        2,
        &[],
        &["issues", "show", "PROJ-1", "--output", "text"],
    ));
    assert!(text.contains("  Points:     3.5\n"), "{text}");
}

/// Without a story points field nothing is known about estimates, which must
/// not read as every issue being unestimated.
#[tokio::test]
async fn a_site_without_a_story_points_field_reports_nothing_rather_than_null() {
    let server = MockServer::start().await;
    catalog(
        &server,
        3,
        json!([float_field("customfield_10003", "Budget")]),
        4,
    )
    .await;
    cloud_search(&server, &[], json!([issue("PROJ-1", json!({}))])).await;

    let listed = stdout_json(&run(&server, 3, &[], &["issues", "list", "--json"]));
    assert!(listed["items"][0].get("storyPoints").is_none(), "{listed}");

    let table = stdout_text(&run(
        &server,
        3,
        &[],
        &["issues", "list", "--output", "text"],
    ));
    assert!(!table.contains("Points"), "{table}");

    // `--fields` filters JSON only, so a text listing has not asked for story
    // points by naming any field.
    let text_with_fields = stdout_text(&run(
        &server,
        3,
        &[],
        &["issues", "list", "--output", "text", "--fields", "key"],
    ));
    assert!(text_with_fields.contains("PROJ-1"), "{text_with_fields}");

    let asked = run(
        &server,
        3,
        &[],
        &["issues", "list", "--json", "--fields", "key,storyPoints"],
    );
    assert_eq!(asked.status.code(), Some(2));
    let message = error_message(&asked);
    assert!(message.contains("no story points field"), "{message}");
    assert!(message.contains("JIRA_STORY_POINTS_FIELD"), "{message}");
}

/// A site estimating with both fields cannot be read as one estimate when they
/// disagree; pinning one field settles it without a catalog request.
#[tokio::test]
async fn conflicting_fields_are_an_error_until_one_is_pinned() {
    let server = MockServer::start().await;
    catalog(
        &server,
        3,
        json!([
            jsw_field("customfield_10016"),
            float_field("customfield_10028", "Story Points")
        ]),
        1,
    )
    .await;
    let both = json!({"customfield_10016": 3, "customfield_10028": 5});
    cloud_search(
        &server,
        &["customfield_10016", "customfield_10028"],
        json!([issue("PROJ-1", both.clone())]),
    )
    .await;
    cloud_search(
        &server,
        &["customfield_10028"],
        json!([issue("PROJ-1", both)]),
    )
    .await;

    let conflict = run(&server, 3, &[], &["issues", "list", "--json"]);
    assert!(!conflict.status.success());
    let message = error_message(&conflict);
    assert!(
        message.contains("PROJ-1 has conflicting story points"),
        "{message}"
    );
    assert!(message.contains("3 in customfield_10016"), "{message}");
    assert!(message.contains("5 in customfield_10028"), "{message}");

    let pinned = stdout_json(&run(
        &server,
        3,
        &[("JIRA_STORY_POINTS_FIELD", "customfield_10028")],
        &["issues", "list", "--json"],
    ));
    assert_eq!(pinned["items"][0]["storyPoints"], json!(5));
}

#[tokio::test]
async fn a_pinned_field_that_is_not_a_field_id_is_refused_before_any_request() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let output = run(
        &server,
        3,
        &[("JIRA_STORY_POINTS_FIELD", "Story Points")],
        &["issues", "list", "--json"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(error_message(&output).contains("customfield_10016"));
}

/// A failed field discovery costs a listing its story points, said on stderr,
/// but not the listing itself; only output that asked for them by name fails.
#[tokio::test]
async fn failed_field_discovery_degrades_listings_and_warns_on_show() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/field"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    cloud_search(
        &server,
        &[],
        json!([issue("PROJ-1", json!({"customfield_10016": 5}))]),
    )
    .await;
    cloud_search(
        &server,
        &["customfield_10016"],
        json!([issue("PROJ-1", json!({"customfield_10016": 5}))]),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(query_param("fields", DETAIL_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue("PROJ-1", json!({}))))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(query_param(
            "fields",
            format!("{DETAIL_FIELDS},customfield_10016"),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(issue("PROJ-1", json!({"customfield_10016": 5}))),
        )
        .mount(&server)
        .await;

    let table = run(&server, 3, &[], &["issues", "list", "--output", "text"]);
    let stdout = stdout_text(&table);
    assert!(stdout.contains("PROJ-1"), "{stdout}");
    assert!(!stdout.contains("Points"), "{stdout}");
    let stderr = String::from_utf8_lossy(&table.stderr);
    assert!(
        stderr.contains("Warning: story point data unavailable: field discovery failed"),
        "{stderr}"
    );

    let quiet = run(
        &server,
        3,
        &[],
        &["issues", "list", "--output", "text", "--quiet"],
    );
    assert!(stdout_text(&quiet).contains("PROJ-1"));
    assert!(
        quiet.stderr.is_empty(),
        "--quiet must silence the warning: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );

    let text_with_fields = run(
        &server,
        3,
        &[],
        &["issues", "list", "--output", "text", "--fields", "key"],
    );
    assert!(stdout_text(&text_with_fields).contains("PROJ-1"));

    let asked = run(
        &server,
        3,
        &[],
        &["issues", "list", "--json", "--fields", "storyPoints"],
    );
    assert!(!asked.status.success());

    let shown = stdout_json(&run(
        &server,
        3,
        &[],
        &["issues", "show", "PROJ-1", "--json"],
    ));
    assert!(shown.get("storyPoints").is_none(), "{shown}");
    let warning = shown["warnings"][0].as_str().unwrap();
    assert!(
        warning.starts_with("Sprint and story point data unavailable: field discovery failed"),
        "{warning}"
    );

    // A pinned field needs no catalog, so only Sprint data is missing.
    let pin = [("JIRA_STORY_POINTS_FIELD", "customfield_10016")];
    let shown = stdout_json(&run(
        &server,
        3,
        &pin,
        &["issues", "show", "PROJ-1", "--json"],
    ));
    assert_eq!(shown["storyPoints"], json!(5));
    let warning = shown["warnings"][0].as_str().unwrap();
    assert!(
        warning.starts_with("Sprint data unavailable: field discovery failed"),
        "{warning}"
    );
    let listed = stdout_json(&run(&server, 3, &pin, &["issues", "list", "--json"]));
    assert_eq!(listed["items"][0]["storyPoints"], json!(5));
}
