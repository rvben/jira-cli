//! Parent and epic membership in issue output, strict `--fields`, and
//! `issues update --type`.

use super::*;
use serde_json::{Value, json};
use wiremock::matchers::{any, body_json, query_param};

const EPIC_LINK_SCHEMA: &str = "com.pyxis.greenhopper.jira:gh-epic-link";
const DETAIL_FIELDS: &str = "summary,status,assignee,reporter,priority,issuetype,parent,description,labels,components,fixVersions,versions,created,updated,comment,issuelinks";
const SEARCH_FIELDS: &str = "summary,status,assignee,priority,issuetype,parent,created,updated";

fn run_v2(server: &MockServer, args: &[&str]) -> std::process::Output {
    let dir = TempDir::new().unwrap();
    jira_cmd(&dir)
        .args(args)
        .env("JIRA_HOST", server.uri())
        .env("JIRA_EMAIL", "test@example.com")
        .env("JIRA_TOKEN", "test-token")
        .env("JIRA_API_VERSION", "2")
        .output()
        .unwrap()
}

fn stdout_json(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The error message from the JSON error envelope on stderr.
fn stderr(output: &std::process::Output) -> String {
    let envelope: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)));
    envelope["error"]["message"].as_str().unwrap().to_owned()
}

/// `value[name]` is present and null; a missing key would also index as null.
fn assert_null(value: &Value, name: &str) {
    match value.get(name) {
        Some(Value::Null) => {}
        other => panic!("expected {name}: null, got {other:?} in {value}"),
    }
}

fn issue(key: &str, issuetype: Value, parent: Option<Value>) -> Value {
    let mut fields = json!({
        "summary": format!("{key} summary"),
        "status": {"name": "To Do"},
        "issuetype": issuetype,
    });
    if let Some(parent) = parent {
        fields["parent"] = parent;
    }
    json!({"id": "1", "key": key, "fields": fields})
}

fn parent(key: &str, issuetype: Value) -> Value {
    json!({"id": "9", "key": key, "fields": {
        "summary": format!("{key} summary"),
        "status": {"name": "To Do"},
        "issuetype": issuetype,
    }})
}

async fn get_json(server: &MockServer, endpoint: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// Data Center field list: the real Epic Link plus an unrelated text field
/// that merely shares its name.
fn dc_fields() -> Value {
    json!([
        {"id": "summary", "name": "Summary", "custom": false, "schema": {"type": "string", "system": "summary"}},
        {"id": "customfield_10100", "name": "Epic Link", "custom": true,
         "schema": {"type": "any", "custom": EPIC_LINK_SCHEMA}},
        {"id": "customfield_10200", "name": "Epic Link", "custom": true,
         "schema": {"type": "string", "custom": "com.atlassian.jira.plugin.system.customfieldtypes:textfield"}}
    ])
}

// ── Reading parent and epic ───────────────────────────────────────────────────

#[tokio::test]
async fn cloud_reports_parent_and_derives_epic_from_the_parent_level() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(query_param("fields", DETAIL_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(full_issue()))
        .expect(2)
        .mount(&server)
        .await;
    // A renamed epic type is still an epic when Jira says it sits at level 1;
    // a Story parent is not an epic even with no name to go on.
    let issues = json!([
        issue(
            "PROJ-2",
            json!({"name": "Task", "hierarchyLevel": 0}),
            Some(parent(
                "PROJ-50",
                json!({"name": "Feature", "hierarchyLevel": 1})
            ))
        ),
        issue(
            "PROJ-3",
            json!({"name": "Sub-task", "hierarchyLevel": -1}),
            Some(parent(
                "PROJ-2",
                json!({"name": "Task", "hierarchyLevel": 0})
            ))
        ),
        issue("PROJ-4", json!({"name": "Task"}), None),
    ]);
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(body_partial_json(
            json!({"fields": SEARCH_FIELDS.split(',').collect::<Vec<_>>()}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"issues": issues, "isLast": true})),
        )
        .mount(&server)
        .await;
    // Cloud has no Epic Link field to look up.
    Mock::given(method("GET"))
        .and(path("/rest/api/3/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(0)
        .mount(&server)
        .await;

    let shown = stdout_json(&run_jira_against(
        &server,
        &["issues", "show", "PROJ-1", "--json"],
    ));
    assert_eq!(
        shown["parent"],
        json!({"key": "PROJ-100", "summary": "An epic", "type": "Epic"})
    );
    assert_eq!(shown["epic"], "PROJ-100");

    let text = run_jira_against(&server, &["issues", "show", "PROJ-1", "--output", "text"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Parent:     PROJ-100 (Epic)"), "{text}");
    assert!(text.contains("Epic:       PROJ-100"), "{text}");

    let found = stdout_json(&run_jira_against(
        &server,
        &["search", "project = PROJ", "--json"],
    ));
    let items = found["items"].as_array().unwrap();
    assert_eq!(items[0]["parent"]["key"], "PROJ-50");
    assert_eq!(items[0]["epic"], "PROJ-50");
    assert_eq!(items[1]["parent"]["key"], "PROJ-2");
    assert_eq!(items[1]["parent"]["type"], "Task");
    assert_null(&items[1], "epic");
    assert_null(&items[2], "parent");
    assert_null(&items[2], "epic");
}

#[tokio::test]
async fn data_center_reads_epic_from_the_epic_link_schema_field_only() {
    let server = MockServer::start().await;
    // One lookup serves every page of an `--all` listing.
    Mock::given(method("GET"))
        .and(path("/rest/api/2/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(dc_fields()))
        .expect(1)
        .mount(&server)
        .await;
    let fields = format!("{SEARCH_FIELDS},customfield_10100");
    let mut in_epic = issue("PROJ-1", json!({"name": "Story", "subtask": false}), None);
    in_epic["fields"]["customfield_10100"] = json!("PROJ-50");
    in_epic["fields"]["customfield_10200"] = json!("not an epic key");
    // A subtask directly under an epic: the epic is its parent, and Data
    // Center gives no reliable way to call that parent an epic.
    let mut subtask = issue(
        "PROJ-2",
        json!({"name": "Sub-task", "subtask": true}),
        Some(parent("PROJ-50", json!({"name": "Epic", "subtask": false}))),
    );
    subtask["fields"]["customfield_10100"] = Value::Null;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search"))
        .and(query_param("fields", fields.as_str()))
        .and(query_param("startAt", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [in_epic], "total": 2, "startAt": 0, "maxResults": 100
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search"))
        .and(query_param("fields", fields.as_str()))
        .and(query_param("startAt", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [subtask], "total": 2, "startAt": 1, "maxResults": 100
        })))
        .expect(1)
        .mount(&server)
        .await;

    let listed = stdout_json(&run_v2(&server, &["issues", "list", "--all", "--json"]));
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["epic"], "PROJ-50");
    assert_null(&items[0], "parent");
    assert_null(&items[1], "epic");
    assert_eq!(
        items[1]["parent"],
        json!({"key": "PROJ-50", "summary": "PROJ-50 summary", "type": "Epic"})
    );
}

#[tokio::test]
async fn data_center_show_requests_the_epic_link_field() {
    let server = MockServer::start().await;
    get_json(&server, "/rest/api/2/field", dc_fields()).await;
    let mut body = issue("PROJ-1", json!({"name": "Story", "subtask": false}), None);
    body["fields"]["customfield_10100"] = json!("PROJ-50");
    let fields = format!("{DETAIL_FIELDS},customfield_10100");
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/PROJ-1"))
        .and(query_param("fields", fields.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;

    let shown = stdout_json(&run_v2(&server, &["issues", "show", "PROJ-1", "--json"]));
    assert_eq!(shown["epic"], "PROJ-50");
    assert_null(&shown, "parent");
}

#[tokio::test]
async fn data_center_without_jira_software_has_no_epics() {
    let server = MockServer::start().await;
    get_json(
        &server,
        "/rest/api/2/field",
        json!([{"id": "customfield_10200", "name": "Epic Link", "custom": true,
                "schema": {"type": "string", "custom": "com.atlassian.jira.plugin.system.customfieldtypes:textfield"}}]),
    )
    .await;
    let mut body = issue("PROJ-1", json!({"name": "Task", "subtask": false}), None);
    body["fields"]["customfield_10200"] = json!("PROJ-50");
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/PROJ-1"))
        .and(query_param("fields", DETAIL_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;

    let shown = stdout_json(&run_v2(&server, &["issues", "show", "PROJ-1", "--json"]));
    assert_null(&shown, "epic");
}

/// Two genuine Epic Link fields (for example after an app migration) are read
/// together: one distinct epic across them is the answer, two are a conflict.
#[tokio::test]
async fn data_center_reads_every_epic_link_field_and_reports_conflicts() {
    let server = MockServer::start().await;
    get_json(
        &server,
        "/rest/api/2/field",
        json!([
            {"id": "customfield_1", "name": "Epic Link", "schema": {"type": "any", "custom": EPIC_LINK_SCHEMA}},
            {"id": "customfield_2", "name": "Old Epic", "schema": {"type": "any", "custom": EPIC_LINK_SCHEMA}}
        ]),
    )
    .await;
    let fields = format!("{DETAIL_FIELDS},customfield_1,customfield_2");
    for (key, first, second) in [
        ("PROJ-1", Value::Null, json!("PROJ-50")),
        ("PROJ-2", json!("PROJ-50"), json!("PROJ-50")),
        ("PROJ-3", json!("PROJ-50"), json!("PROJ-60")),
    ] {
        let mut body = issue(key, json!({"name": "Story", "subtask": false}), None);
        body["fields"]["customfield_1"] = first;
        body["fields"]["customfield_2"] = second;
        Mock::given(method("GET"))
            .and(path(format!("/rest/api/2/issue/{key}")))
            .and(query_param("fields", fields.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
    }

    for key in ["PROJ-1", "PROJ-2"] {
        let shown = stdout_json(&run_v2(&server, &["issues", "show", key, "--json"]));
        assert_eq!(shown["epic"], "PROJ-50", "{key}");
    }
    let output = run_v2(&server, &["issues", "show", "PROJ-3", "--json"]);
    assert!(!output.status.success());
    let err = stderr(&output);
    assert!(
        err.contains("PROJ-3 has conflicting Epic Link values"),
        "{err}"
    );
    assert!(err.contains("PROJ-50") && err.contains("PROJ-60"), "{err}");
}

/// A listing whose `--fields` leaves out `epic` neither looks the field up
/// nor asks Jira for it.
#[tokio::test]
async fn data_center_skips_the_epic_lookup_when_epic_is_not_selected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(dc_fields()))
        .expect(0)
        .mount(&server)
        .await;
    let body = issue("PROJ-1", json!({"name": "Story", "subtask": false}), None);
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search"))
        .and(query_param("fields", SEARCH_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [body], "total": 1, "startAt": 0, "maxResults": 50
        })))
        .expect(1)
        .mount(&server)
        .await;

    let listed = stdout_json(&run_v2(
        &server,
        &[
            "search",
            "project = PROJ",
            "--fields",
            "key,parent",
            "--json",
        ],
    ));
    assert_eq!(listed["items"], json!([{"key": "PROJ-1", "parent": null}]));
}

// ── --fields ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn fields_filter_can_select_parent_and_epic() {
    let server = MockServer::start().await;
    let issues = json!([issue(
        "PROJ-2",
        json!({"name": "Task", "hierarchyLevel": 0}),
        Some(parent(
            "PROJ-50",
            json!({"name": "Epic", "hierarchyLevel": 1})
        ))
    )]);
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"issues": issues, "isLast": true})),
        )
        .mount(&server)
        .await;

    let found = stdout_json(&run_jira_against(
        &server,
        &[
            "search",
            "project = PROJ",
            "--fields",
            "key,parent,epic",
            "--json",
        ],
    ));
    assert_eq!(
        found["items"][0],
        json!({
            "key": "PROJ-2",
            "parent": {"key": "PROJ-50", "summary": "PROJ-50 summary", "type": "Epic"},
            "epic": "PROJ-50",
        })
    );
}

/// Commands that never report the epic neither look it up nor trip over
/// conflicting Epic Link values.
#[tokio::test]
async fn data_center_commands_without_epic_output_skip_the_lookup() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(dc_fields()))
        .expect(0)
        .mount(&server)
        .await;
    let mut body = issue("PROJ-1", json!({"name": "Story", "subtask": false}), None);
    body["fields"]["comment"] = json!({"comments": [], "total": 0});
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/PROJ-1"))
        .and(query_param("fields", DETAIL_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_v2(&server, &["issues", "comments", "PROJ-1", "--json"]);
    stdout_json(&output);

    let listing = issue("PROJ-1", json!({"name": "Story", "subtask": false}), None);
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search"))
        .and(query_param("fields", SEARCH_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issues": [listing], "total": 1, "startAt": 0, "maxResults": 50
        })))
        .expect(2)
        .mount(&server)
        .await;
    for args in [
        &["issues", "list", "--output", "text"][..],
        &["search", "project = PROJ", "--output", "text"][..],
    ] {
        let output = run_v2(&server, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("PROJ-1"));
    }
}

// ── issues update --type ──────────────────────────────────────────────────────

fn type_value(id: &str, name: &str, subtask: bool, level: Option<i64>) -> Value {
    let mut value = json!({"id": id, "name": name, "subtask": subtask});
    if let Some(level) = level {
        value["hierarchyLevel"] = json!(level);
    }
    value
}

fn cloud_types() -> Value {
    json!([
        type_value("1", "Task", false, Some(0)),
        type_value("2", "Story", false, Some(0)),
        type_value("3", "Sub-task", true, Some(-1)),
        type_value("4", "Epic", false, Some(1)),
    ])
}

async fn current_type(server: &MockServer, version: u8, key: &str, issuetype: Value) {
    Mock::given(method("GET"))
        .and(path(format!("/rest/api/{version}/issue/{key}")))
        .and(query_param("fields", "issuetype,project"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "1", "key": key,
            "fields": {"issuetype": issuetype, "project": {"id": "10000", "key": "PROJ"}}
        })))
        .mount(server)
        .await;
}

async fn editmeta(server: &MockServer, version: u8, key: &str, fields: Value) {
    get_json(
        server,
        &format!("/rest/api/{version}/issue/{key}/editmeta"),
        json!({"fields": fields}),
    )
    .await;
}

async fn cloud_editmeta_with_types(server: &MockServer) {
    editmeta(
        server,
        3,
        "PROJ-1",
        json!({"issuetype": {"name": "Issue Type", "allowedValues": cloud_types()}}),
    )
    .await;
}

async fn expect_put(server: &MockServer, version: u8, body: Value, times: u64) {
    Mock::given(method("PUT"))
        .and(path(format!("/rest/api/{version}/issue/PROJ-1")))
        .and(body_json(body))
        .respond_with(ResponseTemplate::new(204))
        .expect(times)
        .mount(server)
        .await;
}

async fn forbid_put(server: &MockServer) {
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(server)
        .await;
}

#[tokio::test]
async fn update_type_switches_within_a_level_in_one_put() {
    let server = MockServer::start().await;
    cloud_editmeta_with_types(&server).await;
    current_type(
        &server,
        3,
        "PROJ-1",
        type_value("1", "Task", false, Some(0)),
    )
    .await;
    expect_put(&server, 3, json!({"fields": {"issuetype": {"id": "2"}}}), 1).await;

    let output = run_jira_against(&server, &["issues", "update", "PROJ-1", "--type", "story"]);
    assert_eq!(
        stdout_json(&output),
        json!({"key": "PROJ-1", "updated": true})
    );
}

#[tokio::test]
async fn update_type_refuses_subtask_and_level_changes_without_writing() {
    for (current, target) in [
        (type_value("3", "Sub-task", true, Some(-1)), "Story"),
        (type_value("1", "Task", false, Some(0)), "Sub-task"),
        (type_value("2", "Story", false, Some(0)), "Epic"),
    ] {
        let server = MockServer::start().await;
        cloud_editmeta_with_types(&server).await;
        current_type(&server, 3, "PROJ-1", current.clone()).await;
        forbid_put(&server).await;

        let output = run_jira_against(&server, &["issues", "update", "PROJ-1", "--type", target]);
        assert_eq!(
            output.status.code(),
            Some(exit_codes::INPUT_ERROR),
            "{current} -> {target}"
        );
        let err = stderr(&output);
        assert!(err.contains("not supported by the Jira edit API"), "{err}");
        assert!(err.contains("More > Move"), "{err}");
    }
}

/// Data Center reports no hierarchy levels, so the subtask flag alone has to
/// stop a Sub-task -> Story edit, in either direction.
#[tokio::test]
async fn data_center_refuses_subtask_conversions_without_writing() {
    for (current, target) in [
        (
            json!({"id": "3", "name": "Sub-task", "subtask": true}),
            "Story",
        ),
        (
            json!({"id": "2", "name": "Story", "subtask": false}),
            "Sub-task",
        ),
    ] {
        let server = MockServer::start().await;
        server_info(
            &server,
            json!({"version": "10.3.1", "versionNumbers": [10, 3, 1]}),
        )
        .await;
        editmeta(
            &server,
            2,
            "PROJ-1",
            json!({"issuetype": {"allowedValues": [
                {"id": "2", "name": "Story", "subtask": false},
                {"id": "3", "name": "Sub-task", "subtask": true}
            ]}}),
        )
        .await;
        current_type(&server, 2, "PROJ-1", current.clone()).await;
        forbid_put(&server).await;

        let output = run_v2(&server, &["issues", "update", "PROJ-1", "--type", target]);
        assert_eq!(
            output.status.code(),
            Some(exit_codes::INPUT_ERROR),
            "{current} -> {target}"
        );
        assert!(stderr(&output).contains("not supported by the Jira edit API"));
    }
}

#[tokio::test]
async fn update_type_trusts_reported_classification_over_names() {
    // A standard type that happens to be named "Sub-task".
    let server = MockServer::start().await;
    editmeta(
        &server,
        3,
        "PROJ-1",
        json!({"issuetype": {"allowedValues": [
            type_value("7", "Sub-task", false, Some(0)),
            type_value("2", "Story", false, Some(0)),
        ]}}),
    )
    .await;
    current_type(
        &server,
        3,
        "PROJ-1",
        type_value("7", "Sub-task", false, Some(0)),
    )
    .await;
    expect_put(&server, 3, json!({"fields": {"issuetype": {"id": "2"}}}), 1).await;

    let output = run_jira_against(&server, &["issues", "update", "PROJ-1", "--type", "Story"]);
    stdout_json(&output);
}

#[tokio::test]
async fn update_type_rejects_unknown_same_and_conflicting_types() {
    let server = MockServer::start().await;
    cloud_editmeta_with_types(&server).await;
    current_type(
        &server,
        3,
        "PROJ-1",
        type_value("1", "Task", false, Some(0)),
    )
    .await;
    forbid_put(&server).await;

    let output = run_jira_against(&server, &["issues", "update", "PROJ-1", "--type", "Bug"]);
    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    let err = stderr(&output);
    assert!(err.contains("Invalid issue type \"Bug\""), "{err}");
    assert!(
        err.contains("\"Story\" (ID 2)"),
        "choices must be listed: {err}"
    );

    let output = run_jira_against(&server, &["issues", "update", "PROJ-1", "--type", "1"]);
    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    assert!(stderr(&output).contains("already of issue type \"Task\""));

    let output = run_jira_against(
        &server,
        &[
            "issues",
            "update",
            "PROJ-1",
            "--type",
            "Story",
            "--field",
            "issuetype={\"id\":\"2\"}",
        ],
    );
    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    assert!(stderr(&output).contains("--type conflicts with --field issuetype"));
}

#[tokio::test]
async fn update_type_falls_back_to_project_types_when_editmeta_omits_them() {
    let server = MockServer::start().await;
    editmeta(
        &server,
        3,
        "PROJ-1",
        json!({"summary": {"name": "Summary"}}),
    )
    .await;
    current_type(
        &server,
        3,
        "PROJ-1",
        type_value("1", "Task", false, Some(0)),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/10000"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "10000", "key": "PROJ", "issueTypes": cloud_types()})),
        )
        .expect(1)
        .mount(&server)
        .await;
    // Creatable types are not the question here, and need another permission.
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/createmeta/PROJ/issuetypes"))
        .respond_with(ResponseTemplate::new(403))
        .expect(0)
        .mount(&server)
        .await;
    expect_put(&server, 3, json!({"fields": {"issuetype": {"id": "2"}}}), 1).await;

    stdout_json(&run_jira_against(
        &server,
        &["issues", "update", "PROJ-1", "--type", "Story"],
    ));
}

#[tokio::test]
async fn update_type_dry_run_shows_the_type_and_writes_nothing() {
    let server = MockServer::start().await;
    cloud_editmeta_with_types(&server).await;
    current_type(
        &server,
        3,
        "PROJ-1",
        type_value("1", "Task", false, Some(0)),
    )
    .await;
    forbid_put(&server).await;

    let output = run_jira_against(
        &server,
        &[
            "issues",
            "update",
            "PROJ-1",
            "--type",
            "Story",
            "--dry-run",
            "--json",
        ],
    );
    let preview = stdout_json(&output);
    assert_eq!(preview["dryRun"], true);
    assert_eq!(
        preview["fields"],
        json!({"issuetype": {"id": "2"}}),
        "{preview}"
    );
}

#[tokio::test]
async fn update_type_is_blocked_in_read_only_mode() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let output =
        run_jira_against_read_only(&server, &["issues", "update", "PROJ-1", "--type", "Story"]);
    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    assert!(stderr(&output).contains("read-only"));
}

#[tokio::test]
async fn update_type_explains_a_jira_refusal() {
    let server = MockServer::start().await;
    cloud_editmeta_with_types(&server).await;
    current_type(
        &server,
        3,
        "PROJ-1",
        type_value("1", "Task", false, Some(0)),
    )
    .await;
    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "errorMessages": [],
            "errors": {"issuetype": "The issue type selected is invalid."}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["issues", "update", "PROJ-1", "--type", "Story"]);
    assert!(!output.status.success());
    let err = stderr(&output);
    assert!(err.contains("API error 400"), "{err}");
    assert!(err.contains("fields: issuetype"), "{err}");
    assert!(
        err.contains("share a workflow and field configuration"),
        "{err}"
    );
}

/// The type-change hint belongs to refusals about the type, not to every 400
/// of a write that happens to include `--type`.
#[tokio::test]
async fn update_type_leaves_unrelated_refusals_unannotated() {
    let server = MockServer::start().await;
    cloud_editmeta_with_types(&server).await;
    current_type(
        &server,
        3,
        "PROJ-1",
        type_value("1", "Task", false, Some(0)),
    )
    .await;
    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "errorMessages": [],
            "errors": {"summary": "Summary is too long."}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_jira_against(
        &server,
        &[
            "issues",
            "update",
            "PROJ-1",
            "--type",
            "Story",
            "--summary",
            "x",
        ],
    );
    assert!(!output.status.success());
    let err = stderr(&output);
    assert!(err.contains("fields: summary"), "{err}");
    assert!(!err.contains("workflow"), "{err}");
}

async fn server_info(server: &MockServer, info: Value) {
    get_json(server, "/rest/api/2/serverInfo", info).await;
}

#[tokio::test]
async fn update_type_refuses_data_center_releases_that_skip_workflow_checks() {
    for info in [
        json!({"version": "9.4.2", "versionNumbers": [9, 4, 2]}),
        json!({"version": "8.20.30", "versionNumbers": [8, 20, 30]}),
        json!({"version": "9.10.0-SNAPSHOT"}),
    ] {
        let server = MockServer::start().await;
        server_info(&server, info.clone()).await;
        editmeta(&server, 2, "PROJ-1", json!({})).await;
        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/PROJ-1"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        forbid_put(&server).await;

        let output = run_v2(&server, &["issues", "update", "PROJ-1", "--type", "Story"]);
        assert_eq!(
            output.status.code(),
            Some(exit_codes::INPUT_ERROR),
            "{info}"
        );
        let err = stderr(&output);
        assert!(err.contains("9.10.0"), "{err}");
        assert!(err.contains("More > Move"), "{err}");
    }
}

/// On Data Center there are no hierarchy levels and an Epic is a standard
/// type, so Epic -> Story is an ordinary edit. Adding the result to an epic in
/// the same write must be judged against Story, the type it is becoming.
#[tokio::test]
async fn update_type_with_epic_checks_the_target_type() {
    let server = MockServer::start().await;
    server_info(
        &server,
        json!({"version": "9.12.4", "versionNumbers": [9, 12, 4]}),
    )
    .await;
    editmeta(
        &server,
        2,
        "PROJ-1",
        json!({
            "issuetype": {"allowedValues": [
                {"id": "5", "name": "Epic", "subtask": false},
                {"id": "2", "name": "Story", "subtask": false}
            ]},
            "customfield_10100": {"name": "Epic Link", "schema": {"type": "any", "custom": EPIC_LINK_SCHEMA}}
        }),
    )
    .await;
    current_type(
        &server,
        2,
        "PROJ-1",
        json!({"id": "5", "name": "Epic", "subtask": false}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/PROJ-9"))
        .and(query_param("fields", "issuetype"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "fields": {"issuetype": {"id": "5", "name": "Epic", "subtask": false}}
        })))
        .mount(&server)
        .await;
    // The current type is not what the epic check looks at.
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/PROJ-1"))
        .and(query_param("fields", "issuetype"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    expect_put(
        &server,
        2,
        json!({"fields": {"issuetype": {"id": "2"}, "customfield_10100": "PROJ-9"}}),
        1,
    )
    .await;

    let output = run_v2(
        &server,
        &[
            "issues", "update", "PROJ-1", "--type", "Story", "--epic", "PROJ-9",
        ],
    );
    stdout_json(&output);
}

/// A standard type that is named like a subtask can still join an epic: the
/// reported classification, not the name, decides.
#[tokio::test]
async fn update_type_with_epic_trusts_the_reported_target_classification() {
    let server = MockServer::start().await;
    server_info(
        &server,
        json!({"version": "9.12.4", "versionNumbers": [9, 12, 4]}),
    )
    .await;
    editmeta(
        &server,
        2,
        "PROJ-1",
        json!({
            "issuetype": {"allowedValues": [
                {"id": "1", "name": "Task", "subtask": false},
                {"id": "7", "name": "Sub-task", "subtask": false}
            ]},
            "customfield_10100": {"name": "Epic Link", "schema": {"type": "any", "custom": EPIC_LINK_SCHEMA}}
        }),
    )
    .await;
    current_type(
        &server,
        2,
        "PROJ-1",
        json!({"id": "1", "name": "Task", "subtask": false}),
    )
    .await;
    get_json(
        &server,
        "/rest/api/2/issue/PROJ-9",
        json!({"fields": {"issuetype": {"id": "5", "name": "Epic", "subtask": false}}}),
    )
    .await;
    expect_put(
        &server,
        2,
        json!({"fields": {"issuetype": {"id": "7"}, "customfield_10100": "PROJ-9"}}),
        1,
    )
    .await;

    let output = run_v2(
        &server,
        &[
            "issues", "update", "PROJ-1", "--type", "7", "--epic", "PROJ-9",
        ],
    );
    stdout_json(&output);
}

/// Cloud reports a hierarchy level for every type, so a target without one
/// cannot be shown to sit at the issue's level.
#[tokio::test]
async fn update_type_refuses_a_cloud_type_without_a_hierarchy_level() {
    let server = MockServer::start().await;
    editmeta(
        &server,
        3,
        "PROJ-1",
        json!({"issuetype": {"allowedValues": [
            type_value("1", "Task", false, Some(0)),
            type_value("4", "Epic", false, None),
        ]}}),
    )
    .await;
    current_type(
        &server,
        3,
        "PROJ-1",
        type_value("1", "Task", false, Some(0)),
    )
    .await;
    forbid_put(&server).await;

    for extra in [&[][..], &["--dry-run"][..]] {
        let mut args = vec!["issues", "update", "PROJ-1", "--type", "Epic"];
        args.extend_from_slice(extra);
        let output = run_jira_against(&server, &args);
        assert_eq!(
            output.status.code(),
            Some(exit_codes::INPUT_ERROR),
            "{args:?}"
        );
        assert!(stderr(&output).contains("did not report the hierarchy level"));
    }
}
