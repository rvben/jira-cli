use jira_cli::api::{ApiError, AuthType, IssueDraft, IssueUpdate, JiraClient};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer, version: u8) -> JiraClient {
    JiraClient::new(&server.uri(), "", "test-token", AuthType::Pat, version).unwrap()
}

fn draft() -> IssueDraft<'static> {
    IssueDraft {
        project_key: "PROJ",
        issue_type: "Story",
        summary: "A story",
        description: None,
        priority: None,
        labels: None,
        components: None,
        fix_versions: None,
        assignee: None,
        parent: None,
        epic: None,
    }
}

fn priorities() -> Value {
    json!({"name": "Priority", "allowedValues": [
        {"id": "10", "name": "1 - Blocker"},
        {"id": "20", "name": "2 - High"},
        {"id": "30", "name": "3 - Medium"}
    ]})
}

fn epic_field() -> Value {
    json!({"name": "Renamed epic relationship", "schema": {
        "type": "string", "custom": "com.pyxis.greenhopper.jira:gh-epic-link"
    }})
}

async fn get(server: &MockServer, endpoint: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn legacy(server: &MockServer, version: u8, fields: Value) {
    get(server, &format!("/rest/api/{version}/issue/createmeta"), json!({"projects": [{
        "key": "PROJ", "issuetypes": [{"id": "7", "name": "Story", "subtask": false, "fields": fields}]
    }]})).await;
}

async fn modern(server: &MockServer, version: u8, fields: Value) {
    get(
        server,
        &format!("/rest/api/{version}/issue/createmeta/PROJ/issuetypes"),
        json!({
            "issueTypes": [{"id": "7", "name": "Story", "subtask": false}], "total": 1
        }),
    )
    .await;
    let fields = fields
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, field)| {
            let mut field = field.clone();
            field["fieldId"] = json!(id);
            field
        })
        .collect::<Vec<_>>();
    get(
        server,
        &format!("/rest/api/{version}/issue/createmeta/PROJ/issuetypes/7"),
        json!({
            "total": fields.len(), "fields": fields
        }),
    )
    .await;
}

async fn target(server: &MockServer, version: u8, key: &str, issue_type: Value) {
    get(
        server,
        &format!("/rest/api/{version}/issue/{key}"),
        json!({"fields": {"issuetype": issue_type}}),
    )
    .await;
}

async fn allow_write(server: &MockServer, version: u8, update: bool) {
    Mock::given(method(if update { "PUT" } else { "POST" }))
        .and(path(if update {
            format!("/rest/api/{version}/issue/PROJ-1")
        } else {
            format!("/rest/api/{version}/issue")
        }))
        .respond_with(if update {
            ResponseTemplate::new(204)
        } else {
            ResponseTemplate::new(201).set_body_json(
                json!({"id": "1", "key": "PROJ-1", "self": "http://example.invalid/issue/1"}),
            )
        })
        .expect(1)
        .mount(server)
        .await;
}

async fn write_fields(server: &MockServer) -> Value {
    let writes = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method != "GET")
        .collect::<Vec<_>>();
    assert_eq!(
        writes.len(),
        1,
        "exactly one mutation, never retry a create"
    );
    serde_json::from_slice::<Value>(&writes[0].body).unwrap()["fields"].clone()
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
async fn server_epic_flag_and_parent_alias_use_discovered_custom_field() {
    for alias in [false, true] {
        let server = MockServer::start().await;
        legacy(
            &server,
            2,
            json!({"customfield_23456": epic_field(), "parent": {}}),
        )
        .await;
        target(&server, 2, "PROJ-9", json!({"name": "Epic"})).await;
        allow_write(&server, 2, false).await;
        let mut draft = draft();
        if alias {
            draft.parent = Some("PROJ-9");
        } else {
            draft.epic = Some("PROJ-9");
        }
        client(&server, 2).create_issue(&draft, &[]).await.unwrap();
        let fields = write_fields(&server).await;
        assert_eq!(fields["customfield_23456"], "PROJ-9");
        assert!(fields.get("parent").is_none());
        assert_eq!(fields["issuetype"], json!({"id": "7"}));
    }
}

#[tokio::test]
async fn native_epic_parent_works_on_both_api_versions_and_for_renamed_epics() {
    for version in [2, 3] {
        let server = MockServer::start().await;
        modern(&server, version, json!({"parent": {}})).await;
        target(
            &server,
            version,
            "PROJ-9",
            json!({"name": "Initiative", "hierarchyLevel": 1}),
        )
        .await;
        allow_write(&server, version, false).await;
        let mut draft = draft();
        draft.epic = Some("PROJ-9");
        client(&server, version)
            .create_issue(&draft, &[])
            .await
            .unwrap();
        assert_eq!(
            write_fields(&server).await["parent"],
            json!({"key": "PROJ-9"})
        );
    }
}

#[tokio::test]
async fn cloud_prefers_native_parent_when_legacy_epic_link_is_also_advertised() {
    let server = MockServer::start().await;
    modern(
        &server,
        3,
        json!({"parent": {}, "customfield_23456": epic_field()}),
    )
    .await;
    target(&server, 3, "PROJ-9", json!({"name": "Epic"})).await;
    allow_write(&server, 3, false).await;
    let mut draft = draft();
    draft.epic = Some("PROJ-9");
    client(&server, 3).create_issue(&draft, &[]).await.unwrap();
    let fields = write_fields(&server).await;
    assert_eq!(fields["parent"]["key"], "PROJ-9");
    assert!(fields.get("customfield_23456").is_none());
}

#[tokio::test]
async fn unavailable_createmeta_falls_back_to_field_discovery_on_server() {
    let server = MockServer::start().await;
    get(
        &server,
        "/rest/api/2/field",
        json!([
            {"id": "customfield_45678", "name": "Epic Link", "custom": true},
            {"id": "parent", "name": "Parent", "custom": false}
        ]),
    )
    .await;
    target(&server, 2, "PROJ-9", json!({"name": "Epic"})).await;
    allow_write(&server, 2, false).await;
    let mut draft = draft();
    draft.epic = Some("PROJ-9");
    client(&server, 2).create_issue(&draft, &[]).await.unwrap();
    assert_eq!(write_fields(&server).await["customfield_45678"], "PROJ-9");
}

#[tokio::test]
async fn explicit_epic_rejects_non_epic_targets() {
    let server = MockServer::start().await;
    modern(&server, 3, json!({"parent": {}})).await;
    target(&server, 3, "PROJ-9", json!({"name": "Task"})).await;
    let mut draft = draft();
    draft.epic = Some("PROJ-9");
    let err = client(&server, 3)
        .create_issue(&draft, &[])
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::InvalidInput(_)));
    assert!(err.to_string().contains("not an Epic"));
    no_writes(&server).await;
}

#[tokio::test]
async fn subtask_cannot_be_linked_directly_to_epic() {
    let server = MockServer::start().await;
    get(
        &server,
        "/rest/api/2/issue/createmeta",
        json!({"projects": [{"key": "PROJ", "issuetypes": [
            {"id": "8", "name": "Small task", "subtask": true, "fields": {"parent": {}}}
        ]}]}),
    )
    .await;
    target(&server, 2, "PROJ-9", json!({"name": "Epic"})).await;
    let mut draft = draft();
    draft.issue_type = "Small task";
    draft.parent = Some("PROJ-9");
    let err = client(&server, 2)
        .create_issue(&draft, &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("--parent <STORY-OR-TASK>"));
    no_writes(&server).await;
}

#[tokio::test]
async fn epic_link_missing_ambiguous_or_conflicting_fails_before_write() {
    for (fields, custom, message) in [
        (json!({}), vec![], "Cannot resolve epic linkage"),
        (
            json!({"customfield_1": epic_field(), "customfield_2": epic_field()}),
            vec![],
            "Ambiguous Epic Link",
        ),
        (
            json!({"customfield_1": epic_field()}),
            vec![("customfield_1".into(), json!("PROJ-8"))],
            "conflicts",
        ),
        (
            json!({"customfield_1": epic_field()}),
            vec![("parent".into(), json!({"key": "PROJ-8"}))],
            "conflicts",
        ),
    ] {
        let server = MockServer::start().await;
        legacy(&server, 2, fields).await;
        target(&server, 2, "PROJ-9", json!({"name": "Epic"})).await;
        let mut draft = draft();
        draft.epic = Some("PROJ-9");
        let err = client(&server, 2)
            .create_issue(&draft, &custom)
            .await
            .unwrap_err();
        assert!(err.to_string().contains(message), "{err}");
        no_writes(&server).await;
    }
}

#[tokio::test]
async fn create_normalizes_priority_labels_prefixes_exact_names_and_ids() {
    for input in [
        "Medium",
        "med",
        "3 - Medium",
        "3 - mEdIuM",
        "30",
        " Medium ",
    ] {
        let server = MockServer::start().await;
        modern(&server, 3, json!({"priority": priorities()})).await;
        allow_write(&server, 3, false).await;
        let mut draft = draft();
        draft.priority = Some(input);
        draft.issue_type = "story";
        client(&server, 3).create_issue(&draft, &[]).await.unwrap();
        let fields = write_fields(&server).await;
        assert_eq!(fields["priority"], json!({"id": "30"}), "{input}");
        assert_eq!(fields["issuetype"], json!({"id": "7"}));
    }
}

#[tokio::test]
async fn invalid_and_ambiguous_priorities_list_project_choices() {
    for (input, ambiguous) in [
        ("Urgent", false),
        (" ", false),
        ("M", true),
        ("Medium", true),
    ] {
        let server = MockServer::start().await;
        let mut priority = priorities();
        priority["allowedValues"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "40", "name": "4 - Medium"}));
        modern(&server, 3, json!({"priority": priority})).await;
        let mut draft = draft();
        draft.priority = Some(input);
        let err = client(&server, 3)
            .create_issue(&draft, &[])
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains(if ambiguous {
                "Ambiguous priority"
            } else {
                "Invalid priority"
            }),
            "{message}"
        );
        assert!(
            message.contains("3 - Medium")
                && message.contains("4 - Medium")
                && message.contains("1 - Blocker")
        );
        no_writes(&server).await;
    }
}

#[tokio::test]
async fn exact_priority_name_wins_over_normalized_matches() {
    let server = MockServer::start().await;
    let mut priority = priorities();
    priority["allowedValues"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "40", "name": "Medium"}));
    legacy(&server, 2, json!({"priority": priority})).await;
    allow_write(&server, 2, false).await;
    let mut draft = draft();
    draft.priority = Some("Medium");
    client(&server, 2).create_issue(&draft, &[]).await.unwrap();
    assert_eq!(write_fields(&server).await["priority"], json!({"id": "40"}));
}

#[tokio::test]
async fn omitted_priority_preserves_default_and_raw_field_override_is_respected() {
    for override_field in [false, true] {
        let server = MockServer::start().await;
        modern(&server, 3, json!({})).await;
        allow_write(&server, 3, false).await;
        let mut draft = draft();
        let custom = if override_field {
            draft.priority = Some("ignored");
            vec![("priority".into(), json!({"id": "99"}))]
        } else {
            vec![]
        };
        client(&server, 3)
            .create_issue(&draft, &custom)
            .await
            .unwrap();
        let fields = write_fields(&server).await;
        if override_field {
            assert_eq!(fields["priority"], json!({"id": "99"}));
        } else {
            assert!(fields.get("priority").is_none());
        }
    }
}

#[tokio::test]
async fn priority_missing_from_screen_is_actionable() {
    let server = MockServer::start().await;
    modern(&server, 3, json!({})).await;
    let mut draft = draft();
    draft.priority = Some("Medium");
    let err = client(&server, 3)
        .create_issue(&draft, &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("omit --priority"));
    no_writes(&server).await;
}

#[tokio::test]
async fn invalid_issue_type_lists_available_types_without_posting() {
    let server = MockServer::start().await;
    modern(&server, 3, json!({})).await;
    let mut draft = draft();
    draft.issue_type = "Stroy";
    let err = client(&server, 3)
        .create_issue(&draft, &[])
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("Invalid issue type \"Stroy\"; valid: \"Story\"")
    );
    no_writes(&server).await;
}

#[tokio::test]
async fn metadata_paginates_dc_types_and_fields_before_selecting() {
    let server = MockServer::start().await;
    for (start, item) in [
        (0, json!({"id": "5", "name": "Task"})),
        (1, json!({"id": "7", "name": "Story"})),
    ] {
        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/createmeta/PROJ/issuetypes"))
            .and(query_param("startAt", start.to_string()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"startAt": start, "total": 2, "values": [item]})),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    for (start, item) in [
        (0, json!({"fieldId": "summary", "name": "Summary"})),
        (1, {
            let mut p = priorities();
            p["fieldId"] = json!("priority");
            p
        }),
    ] {
        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/createmeta/PROJ/issuetypes/7"))
            .and(query_param("startAt", start.to_string()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"startAt": start, "isLast": start == 1, "values": [item]}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    allow_write(&server, 2, false).await;
    let mut draft = draft();
    draft.issue_type = "7";
    draft.priority = Some("Medium");
    client(&server, 2).create_issue(&draft, &[]).await.unwrap();
    assert_eq!(write_fields(&server).await["priority"]["id"], "30");
}

#[tokio::test]
async fn update_uses_issue_editmeta_for_priority_and_epic() {
    for version in [2, 3] {
        let server = MockServer::start().await;
        let link = if version == 2 {
            "customfield_98765"
        } else {
            "parent"
        };
        get(
            &server,
            &format!("/rest/api/{version}/issue/PROJ-1/editmeta"),
            json!({"fields": {
                "priority": priorities(), link: if version == 2 {epic_field()} else {json!({})}
            }}),
        )
        .await;
        target(&server, version, "PROJ-1", json!({"name": "Story"})).await;
        target(&server, version, "PROJ-9", json!({"name": "Epic"})).await;
        allow_write(&server, version, true).await;
        client(&server, version)
            .update_issue(
                "PROJ-1",
                &IssueUpdate {
                    priority: Some("Medium"),
                    epic: Some("PROJ-9"),
                    ..Default::default()
                },
                &[],
            )
            .await
            .unwrap();
        let fields = write_fields(&server).await;
        assert_eq!(fields["priority"], json!({"id": "30"}));
        assert_eq!(
            fields[link],
            if version == 2 {
                json!("PROJ-9")
            } else {
                json!({"key": "PROJ-9"})
            }
        );
    }
}

#[tokio::test]
async fn update_invalid_priority_and_epic_self_link_do_not_write() {
    let server = MockServer::start().await;
    get(
        &server,
        "/rest/api/3/issue/PROJ-1/editmeta",
        json!({"fields": {"priority": priorities()}}),
    )
    .await;
    for update in [
        IssueUpdate {
            priority: Some("Urgent"),
            ..Default::default()
        },
        IssueUpdate {
            epic: Some("PROJ-1"),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            client(&server, 3)
                .update_issue("PROJ-1", &update, &[])
                .await,
            Err(ApiError::InvalidInput(_))
        ));
    }
    no_writes(&server).await;
}

#[tokio::test]
async fn unavailable_metadata_preserves_exact_priority_and_adds_400_guidance() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"errors": {"priority": "invalid"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut draft = draft();
    draft.priority = Some("Medium");
    let err = client(&server, 2)
        .create_issue(&draft, &[])
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Api { status: 400, .. }));
    assert!(err.to_string().contains("omit --priority"));
    assert_eq!(
        write_fields(&server).await["priority"],
        json!({"name": "Medium"})
    );
}

#[tokio::test]
async fn server_400_retains_status_and_supplies_known_priority_options() {
    let server = MockServer::start().await;
    modern(&server, 3, json!({"priority": priorities()})).await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"errors": {"priority": "changed configuration"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut draft = draft();
    draft.priority = Some("Medium");
    let err = client(&server, 3)
        .create_issue(&draft, &[])
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Api { status: 400, .. }));
    assert!(err.to_string().contains("3 - Medium"));
    write_fields(&server).await;
}

#[tokio::test]
async fn metadata_auth_rate_limit_and_server_errors_are_never_ignored() {
    for status in [401, 403, 429, 500] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/createmeta/PROJ/issuetypes"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        let err = client(&server, 3)
            .create_issue(&draft(), &[])
            .await
            .unwrap_err();
        assert!(matches!(
            (status, err),
            (401 | 403, ApiError::Auth(_))
                | (429, ApiError::RateLimit)
                | (500, ApiError::Api { status: 500, .. })
        ));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn stalled_metadata_pagination_does_not_validate_partial_options() {
    let server = MockServer::start().await;
    get(
        &server,
        "/rest/api/3/issue/createmeta/PROJ/issuetypes",
        json!({
            "startAt": 0, "total": 2, "issueTypes": [{"id": "7", "name": "Story"}]
        }),
    )
    .await;
    let err = client(&server, 3)
        .create_issue(&draft(), &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("pagination did not advance"));
    no_writes(&server).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn metadata_absence_still_allows_explicit_priority_and_type_ids() {
    let server = MockServer::start().await;
    allow_write(&server, 2, false).await;
    let mut draft = draft();
    draft.priority = Some("30");
    draft.issue_type = "7";
    client(&server, 2).create_issue(&draft, &[]).await.unwrap();
    let fields = write_fields(&server).await;
    assert_eq!(fields["priority"], json!({"id": "30"}));
    assert_eq!(fields["issuetype"], json!({"id": "7"}));
}

#[tokio::test]
async fn epic_only_update_and_unavailable_editmeta_remain_usable() {
    let server = MockServer::start().await;
    target(&server, 3, "PROJ-9", json!({"name": "Epic"})).await;
    target(&server, 3, "PROJ-1", json!({"name": "Story"})).await;
    allow_write(&server, 3, true).await;
    client(&server, 3)
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                epic: Some("PROJ-9"),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        write_fields(&server).await,
        json!({"parent": {"key": "PROJ-9"}})
    );
}

#[tokio::test]
async fn epic_400_adds_guidance_and_does_not_retry() {
    let server = MockServer::start().await;
    legacy(&server, 2, json!({"customfield_23456": epic_field()})).await;
    target(&server, 2, "PROJ-9", json!({"name": "Epic"})).await;
    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"errors": {"issuetype": "invalid"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut draft = draft();
    draft.parent = Some("PROJ-9");
    let err = client(&server, 2)
        .create_issue(&draft, &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("--epic PROJ-9"));
    assert!(err.to_string().contains("create/edit screen"));
    write_fields(&server).await;
}

#[tokio::test]
async fn raw_project_override_controls_metadata_selection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/2/issue/createmeta"))
        .and(query_param("projectIds", "12345"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"projects": [{
            "id": "12345", "key": "OTHER", "issuetypes": [{"id": "7", "name": "Story", "fields": {"priority": priorities()}}]
        }]}))).expect(1).mount(&server).await;
    allow_write(&server, 2, false).await;
    let mut draft = draft();
    draft.priority = Some("Medium");
    client(&server, 2)
        .create_issue(&draft, &[("project".into(), json!({"id": "12345"}))])
        .await
        .unwrap();
    let fields = write_fields(&server).await;
    assert_eq!(fields["project"], json!({"id": "12345"}));
    assert_eq!(fields["priority"], json!({"id": "30"}));
    assert!(
        !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path().contains("/PROJ/"))
    );
}

#[tokio::test]
async fn cloud_native_parent_rejects_a_competing_legacy_epic_override() {
    let server = MockServer::start().await;
    modern(
        &server,
        3,
        json!({"parent": {}, "customfield_23456": epic_field()}),
    )
    .await;
    target(&server, 3, "PROJ-9", json!({"name": "Epic"})).await;
    let mut draft = draft();
    draft.epic = Some("PROJ-9");
    let err = client(&server, 3)
        .create_issue(&draft, &[("customfield_23456".into(), json!("PROJ-8"))])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("conflicts"));
    no_writes(&server).await;
}

async fn edit_metadata(server: &MockServer, version: u8, fields: Value) {
    get(
        server,
        &format!("/rest/api/{version}/issue/PROJ-1/editmeta"),
        json!({"fields": fields}),
    )
    .await;
}

fn named_arrays() -> Value {
    json!({
        "components": {"name": "Components", "allowedValues": [
            {"id": "11", "name": "Backend"}, {"id": "12", "name": "Backend API"}, {"id": "13", "name": "Frontend"}
        ]},
        "fixVersions": {"name": "Fix versions", "allowedValues": [
            {"id": "21", "name": "1.2"}, {"id": "22", "name": "2.0 Preview"}
        ]}
    })
}

#[tokio::test]
async fn component_and_version_names_ids_and_unique_prefixes_resolve_on_create_and_update() {
    for update in [false, true] {
        for (component, version, expected_component, expected_version) in [
            ("backend", "1.2", "11", "21"),
            ("13", "22", "13", "22"),
            ("front", "2.0", "13", "22"),
        ] {
            let server = MockServer::start().await;
            if update {
                edit_metadata(&server, 3, named_arrays()).await;
            } else {
                modern(&server, 3, named_arrays()).await;
            }
            allow_write(&server, 3, update).await;
            let components = [component];
            let versions = [version];
            if update {
                client(&server, 3)
                    .update_issue(
                        "PROJ-1",
                        &IssueUpdate {
                            components: Some(&components),
                            fix_versions: Some(&versions),
                            ..Default::default()
                        },
                        &[],
                    )
                    .await
                    .unwrap();
            } else {
                let mut draft = draft();
                draft.components = Some(&components);
                draft.fix_versions = Some(&versions);
                client(&server, 3).create_issue(&draft, &[]).await.unwrap();
            }
            let fields = write_fields(&server).await;
            assert_eq!(fields["components"], json!([{"id": expected_component}]));
            assert_eq!(fields["fixVersions"], json!([{"id": expected_version}]));
        }
    }
}

#[tokio::test]
async fn invalid_or_ambiguous_components_and_versions_list_choices_without_writing() {
    for update in [false, true] {
        for (field, input, choice) in [
            ("components", "Back", "Backend API"),
            ("components", "Missing", "Frontend"),
            ("fixVersions", "Preview", "2.0 Preview"),
        ] {
            let server = MockServer::start().await;
            if update {
                edit_metadata(&server, 3, named_arrays()).await;
            } else {
                modern(&server, 3, named_arrays()).await;
            }
            let inputs = [input];
            let components = (field == "components").then_some(inputs.as_slice());
            let fix_versions = (field == "fixVersions").then_some(inputs.as_slice());
            let err = if update {
                client(&server, 3)
                    .update_issue(
                        "PROJ-1",
                        &IssueUpdate {
                            components,
                            fix_versions,
                            ..Default::default()
                        },
                        &[],
                    )
                    .await
                    .unwrap_err()
            } else {
                let mut draft = draft();
                draft.components = components;
                draft.fix_versions = fix_versions;
                client(&server, 3)
                    .create_issue(&draft, &[])
                    .await
                    .unwrap_err()
            };
            let message = err.to_string();
            assert!(matches!(err, ApiError::InvalidInput(_)));
            assert!(
                message.contains(field) && message.contains(choice) && message.contains("valid:"),
                "{message}"
            );
            no_writes(&server).await;
        }
    }
}

#[tokio::test]
async fn arrays_allow_raw_overrides_optional_clears_and_unavailable_metadata_fallback() {
    for metadata in [true, false] {
        let server = MockServer::start().await;
        if metadata {
            edit_metadata(&server, 3, named_arrays()).await;
        }
        allow_write(&server, 3, true).await;
        client(&server, 3)
            .update_issue(
                "PROJ-1",
                &IssueUpdate {
                    components: Some(&["Invalid"]),
                    fix_versions: Some(&[]),
                    ..Default::default()
                },
                &[("components".into(), json!([{"id": "99"}]))],
            )
            .await
            .unwrap();
        let fields = write_fields(&server).await;
        assert_eq!(fields["components"], json!([{"id": "99"}]));
        assert_eq!(fields["fixVersions"], json!([]));
    }
}

#[tokio::test]
async fn array_fields_missing_from_screen_fail_before_writing() {
    let server = MockServer::start().await;
    modern(&server, 3, json!({})).await;
    let mut draft = draft();
    draft.components = Some(&["Backend"]);
    let err = client(&server, 3)
        .create_issue(&draft, &[])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("omit --components"));
    no_writes(&server).await;
}

#[tokio::test]
async fn required_create_fields_show_names_ids_and_choices() {
    for value in [
        None,
        Some(Value::Null),
        Some(json!("  ")),
        Some(json!([])),
        Some(json!({})),
    ] {
        let server = MockServer::start().await;
        modern(
            &server,
            3,
            json!({"customfield_34567": {
                "name": "Release track", "required": true,
                "allowedValues": [{"id": "1", "name": "Stable"}]
            }}),
        )
        .await;
        let custom = value
            .map(|v| vec![("customfield_34567".into(), v)])
            .unwrap_or_default();
        let err = client(&server, 3)
            .create_issue(&draft(), &custom)
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("Release track")
                && message.contains("customfield_34567")
                && message.contains("Stable")
                && message.contains("--field"),
            "{message}"
        );
        no_writes(&server).await;
    }
}

#[tokio::test]
async fn required_fields_accept_server_defaults_explicit_values_false_and_zero() {
    let server = MockServer::start().await;
    modern(
        &server,
        3,
        json!({
            "priority": {"required": true, "hasDefaultValue": true},
            "customfield_34567": {"required": true, "defaultValue": "Stable"},
            "customfield_34568": {"required": true},
            "customfield_34569": {"required": true},
            "customfield_34570": {"required": true}
        }),
    )
    .await;
    allow_write(&server, 3, false).await;
    client(&server, 3)
        .create_issue(
            &draft(),
            &[
                ("customfield_34568".into(), json!(false)),
                ("customfield_34569".into(), json!(0)),
                ("customfield_34570".into(), json!({"id": "1"})),
            ],
        )
        .await
        .unwrap();
    let fields = write_fields(&server).await;
    assert!(fields.get("priority").is_none());
    assert_eq!(fields["customfield_34568"], false);
    assert_eq!(fields["customfield_34569"], 0);
}

#[tokio::test]
async fn required_update_fields_are_untouched_unless_explicitly_cleared() {
    for clear in [false, true] {
        let server = MockServer::start().await;
        edit_metadata(
            &server,
            3,
            json!({"assignee": {"name": "Assignee", "required": true}}),
        )
        .await;
        let update = IssueUpdate {
            summary: Some("New title"),
            assignee: clear.then_some(None),
            ..Default::default()
        };
        if clear {
            let err = client(&server, 3)
                .update_issue("PROJ-1", &update, &[])
                .await
                .unwrap_err();
            assert!(err.to_string().contains("Assignee"));
            no_writes(&server).await;
        } else {
            allow_write(&server, 3, true).await;
            client(&server, 3)
                .update_issue("PROJ-1", &update, &[])
                .await
                .unwrap();
            assert_eq!(write_fields(&server).await, json!({"summary": "New title"}));
        }
    }
}

async fn hierarchy_metadata(server: &MockServer) {
    get(server, "/rest/api/3/issue/createmeta", json!({"projects": [{"key": "PROJ", "issuetypes": [
        {"id": "7", "name": "Story", "subtask": false, "hierarchyLevel": 0, "fields": {"parent": {}}},
        {"id": "8", "name": "Work item", "subtask": true, "hierarchyLevel": -1, "fields": {"parent": {"required": true}}},
        {"id": "9", "name": "Initiative", "hierarchyLevel": 2, "fields": {"parent": {}}}
    ]}]})).await;
}

#[tokio::test]
async fn standard_parent_requires_actual_project_subtask_type_and_subtasks_require_parent() {
    for parent in [Some("PROJ-9"), None] {
        let server = MockServer::start().await;
        hierarchy_metadata(&server).await;
        target(
            &server,
            3,
            "PROJ-9",
            json!({"name": "Story", "hierarchyLevel": 0}),
        )
        .await;
        let mut draft = draft();
        draft.parent = parent;
        draft.issue_type = if parent.is_some() {
            "Story"
        } else {
            "Work item"
        };
        let err = client(&server, 3)
            .create_issue(&draft, &[])
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("Work item") && message.contains("--parent"),
            "{message}"
        );
        if parent.is_some() {
            assert!(message.contains("--type") && message.contains("ID 8"));
        }
        no_writes(&server).await;
    }
}

#[tokio::test]
async fn renamed_subtask_and_custom_hierarchy_children_accept_compatible_parents() {
    for (child, parent) in [
        ("Work item", json!({"name": "Task", "hierarchyLevel": 0})),
        ("Work item", json!({"name": "Task"})),
        (
            "Initiative",
            json!({"name": "Portfolio", "hierarchyLevel": 3}),
        ),
    ] {
        let server = MockServer::start().await;
        hierarchy_metadata(&server).await;
        target(&server, 3, "PROJ-9", parent).await;
        allow_write(&server, 3, false).await;
        let mut draft = draft();
        draft.issue_type = child;
        draft.parent = Some("PROJ-9");
        client(&server, 3).create_issue(&draft, &[]).await.unwrap();
        assert_eq!(
            write_fields(&server).await["parent"],
            json!({"key": "PROJ-9"})
        );
    }
}

#[tokio::test]
async fn subtask_parents_and_skipped_hierarchy_levels_are_rejected() {
    for parent in [
        json!({"name": "Work item", "subtask": true}),
        json!({"name": "Portfolio", "hierarchyLevel": 3}),
    ] {
        let server = MockServer::start().await;
        hierarchy_metadata(&server).await;
        target(&server, 3, "PROJ-9", parent).await;
        let mut draft = draft();
        draft.parent = Some("PROJ-9");
        assert!(matches!(
            client(&server, 3)
                .create_issue(&draft, &[])
                .await
                .unwrap_err(),
            ApiError::InvalidInput(_)
        ));
        no_writes(&server).await;
    }
}

#[tokio::test]
async fn clear_epic_resolves_custom_and_native_fields_on_both_api_versions() {
    for version in [2, 3] {
        let server = MockServer::start().await;
        edit_metadata(
            &server,
            version,
            json!({"parent": {}, "customfield_23456": epic_field()}),
        )
        .await;
        target(
            &server,
            version,
            "PROJ-1",
            json!({"name": "Story", "hierarchyLevel": 0}),
        )
        .await;
        allow_write(&server, version, true).await;
        client(&server, version)
            .update_issue(
                "PROJ-1",
                &IssueUpdate {
                    clear_epic: true,
                    ..Default::default()
                },
                &[],
            )
            .await
            .unwrap();
        let expected = if version == 2 {
            json!({"customfield_23456": null})
        } else {
            json!({"parent": null})
        };
        assert_eq!(write_fields(&server).await, expected);
    }
}

#[tokio::test]
async fn clear_epic_rejects_required_relationships_incompatible_types_and_conflicting_overrides() {
    for scenario in ["required", "subtask", "epic", "raw", "both"] {
        let server = MockServer::start().await;
        edit_metadata(&server, 3, json!({"parent": {"required": scenario == "required"}, "customfield_23456": epic_field()})).await;
        let issue_type = match scenario {
            "subtask" => json!({"name": "Work item", "subtask": true}),
            "epic" => json!({"name": "Epic"}),
            _ => json!({"name": "Story"}),
        };
        target(&server, 3, "PROJ-1", issue_type).await;
        let custom = if scenario == "raw" {
            vec![("customfield_23456".into(), json!("PROJ-9"))]
        } else {
            vec![]
        };
        let err = client(&server, 3)
            .update_issue(
                "PROJ-1",
                &IssueUpdate {
                    clear_epic: true,
                    epic: (scenario == "both").then_some("PROJ-9"),
                    ..Default::default()
                },
                &custom,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::InvalidInput(_)), "{err}");
        no_writes(&server).await;
    }
}

#[tokio::test]
async fn create_assignee_can_be_omitted_cleared_or_set_on_cloud_and_server() {
    for version in [2, 3] {
        for assignee in [None, Some(None), Some(Some("test-user-id"))] {
            let server = MockServer::start().await;
            allow_write(&server, version, false).await;
            let mut draft = draft();
            draft.assignee = assignee;
            client(&server, version)
                .create_issue(&draft, &[])
                .await
                .unwrap();
            let fields = write_fields(&server).await;
            match assignee {
                None => assert!(fields.get("assignee").is_none()),
                Some(None) => assert!(fields.get("assignee").unwrap().is_null()),
                Some(Some(id)) => assert_eq!(
                    fields["assignee"],
                    if version == 2 {
                        json!({"name": id})
                    } else {
                        json!({"accountId": id})
                    }
                ),
            }
        }
    }
}

#[tokio::test]
async fn server_array_validation_errors_keep_field_specific_choices() {
    for field in ["components", "fixVersions", "customfield_34567"] {
        let server = MockServer::start().await;
        let mut meta = named_arrays();
        meta["parent"] = json!({});
        modern(&server, 3, meta).await;
        target(&server, 3, "PROJ-9", json!({"name": "Epic"})).await;
        let mut draft = draft();
        draft.epic = Some("PROJ-9");
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"errors": {field: "Rejected"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let err = client(&server, 3)
            .create_issue(&draft, &[(field.into(), json!("raw"))])
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(matches!(err, ApiError::Api { status: 400, .. }));
        if field == "customfield_34567" {
            assert!(!message.contains("Epic"));
        } else {
            let option = if field == "components" {
                "Backend"
            } else {
                "2.0 Preview"
            };
            assert!(
                message.contains(option) && message.contains(&format!("valid {field}")),
                "{message}"
            );
        }
    }
}

#[tokio::test]
async fn required_descriptions_reject_blank_text_on_cloud_and_server() {
    for version in [2, 3] {
        for update in [false, true] {
            let server = MockServer::start().await;
            let meta = json!({"description": {"name": "Description", "required": true}});
            if update {
                edit_metadata(&server, version, meta).await;
            } else {
                modern(&server, version, meta).await;
            }
            let err = if update {
                client(&server, version)
                    .update_issue(
                        "PROJ-1",
                        &IssueUpdate {
                            description: Some(" \n "),
                            ..Default::default()
                        },
                        &[],
                    )
                    .await
                    .unwrap_err()
            } else {
                let mut draft = draft();
                draft.description = Some(" \n ");
                client(&server, version)
                    .create_issue(&draft, &[])
                    .await
                    .unwrap_err()
            };
            assert!(err.to_string().contains("Description"), "{err}");
            no_writes(&server).await;
        }
    }
}

#[tokio::test]
async fn required_descriptions_accept_nontext_rich_content_and_raw_overrides() {
    let server = MockServer::start().await;
    modern(&server, 3, json!({"description": {"required": true}})).await;
    allow_write(&server, 3, false).await;
    let mut draft = draft();
    draft.description = Some("");
    let description = json!({"version": 1, "type": "doc", "content": [
        {"type": "mediaGroup", "content": [{"type": "media", "attrs": {"id": "example-media", "type": "file", "collection": ""}}]}
    ]});
    client(&server, 3)
        .create_issue(&draft, &[("description".into(), description.clone())])
        .await
        .unwrap();
    assert_eq!(write_fields(&server).await["description"], description);
}
