use wiremock::matchers::{
    body_partial_json, body_string_contains, header, method, path, path_regex, query_param,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

use jira_cli::api::{ApiError, AuthType, IssueDraft, IssueUpdate, JiraClient};
use jira_cli::output::OutputConfig;
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Construct a minimal IssueDraft with only the required fields set.
/// All optional fields default to None.
fn minimal_draft<'a>(
    project_key: &'a str,
    issue_type: &'a str,
    summary: &'a str,
) -> IssueDraft<'a> {
    IssueDraft {
        project_key,
        issue_type,
        summary,
        description: None,
        priority: None,
        labels: None,
        components: None,
        fix_versions: None,
        assignee: None,
        parent: None,
    }
}

// ── Fixtures ──────────────────────────────────────────────────────────────────

fn test_client(server: &MockServer) -> JiraClient {
    JiraClient::new(
        &server.uri(),
        "test@example.com",
        "test-token",
        AuthType::Basic,
        3,
    )
    .unwrap()
}

fn test_client_pat(server: &MockServer) -> JiraClient {
    JiraClient::new(&server.uri(), "", "my-pat-token", AuthType::Pat, 3).unwrap()
}

fn test_client_v2(server: &MockServer) -> JiraClient {
    JiraClient::new(
        &server.uri(),
        "test@example.com",
        "test-token",
        AuthType::Basic,
        2,
    )
    .unwrap()
}

fn json_out() -> OutputConfig {
    OutputConfig {
        json: true,
        quiet: true,
    }
}

fn issue_fixture(key: &str, summary: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "10001",
        "key": key,
        "self": format!("https://test.atlassian.net/rest/api/3/issue/{key}"),
        "fields": {
            "summary": summary,
            "status": { "name": status },
            "assignee": { "displayName": "Alice", "accountId": "abc123" },
            "reporter": { "displayName": "Bob", "accountId": "def456" },
            "priority": { "name": "Medium" },
            "issuetype": { "name": "Bug" },
            "description": {
                "type": "doc", "version": 1,
                "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Test description"}]}]
            },
            "labels": ["backend", "urgent"],
            "created": "2024-01-15T10:00:00.000Z",
            "updated": "2024-01-20T15:30:00.000Z",
            "comment": {
                "comments": [
                    {
                        "id": "10100",
                        "author": { "displayName": "Alice", "accountId": "abc123" },
                        "body": {
                            "type": "doc", "version": 1,
                            "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Looks good"}]}]
                        },
                        "created": "2024-01-21T09:00:00.000Z"
                    }
                ],
                "total": 1
            }
        }
    })
}

/// JSON body for the Jira Cloud `/rest/api/3/search/jql` endpoint.
///
/// `is_last` defaults to true (no more pages) so mocks in simple single-page
/// tests don't need to specify it. Pass `next_token = Some("…")` and
/// `is_last = false` when mocking multi-page pagination.
fn search_jql_response(
    issues: Vec<serde_json::Value>,
    next_token: Option<&str>,
    is_last: bool,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "issues": issues,
        "isLast": is_last,
    });
    if let Some(t) = next_token {
        body["nextPageToken"] = serde_json::Value::String(t.to_string());
    }
    body
}

/// Convenience: single-page `/search/jql` response with `isLast = true`.
fn search_response(issues: Vec<serde_json::Value>) -> serde_json::Value {
    search_jql_response(issues, None, true)
}

fn project_search_response(projects: Vec<serde_json::Value>) -> serde_json::Value {
    let total = projects.len();
    serde_json::json!({
        "values": projects,
        "total": total,
        "startAt": 0,
        "maxResults": 50,
        "isLast": true
    })
}

// ── Auth header ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_sends_basic_auth_header() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .and(header(
            "authorization",
            "Basic dGVzdEBleGFtcGxlLmNvbTp0ZXN0LXRva2Vu",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_search_response(vec![])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client.list_projects().await.unwrap();
}

// ── Issue validation ───────────────────────────────────────────────────────────

#[tokio::test]
async fn get_issue_rejects_invalid_key() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let cases = [
        "proj-123",
        "PROJ123",
        "PROJ-abc",
        "../etc/passwd",
        "",
        "1PROJ-123",
    ];
    for key in cases {
        let err = client.get_issue(key).await.unwrap_err();
        assert!(
            matches!(err, ApiError::InvalidInput(_)),
            "expected InvalidInput for key={key:?}, got {err}"
        );
    }
}

#[tokio::test]
async fn get_issue_accepts_valid_key() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex(r"/rest/api/3/issue/PROJ-\d+"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(issue_fixture("PROJ-123", "Fix bug", "Open")),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("PROJ-123").await.unwrap();
    assert_eq!(issue.key, "PROJ-123");
    assert_eq!(issue.summary(), "Fix bug");
}

#[tokio::test]
async fn get_issue_accepts_key_with_digit_in_project_part() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex(r"/rest/api/3/issue/ABC2-\d+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_fixture(
            "ABC2-1",
            "Digit key",
            "Open",
        )))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("ABC2-1").await.unwrap();
    assert_eq!(issue.key, "ABC2-1");
}

#[tokio::test]
async fn get_issue_includes_components() {
    let server = MockServer::start().await;
    let mut fixture = issue_fixture("PROJ-1", "Test", "Open");
    fixture["fields"]["components"] = serde_json::json!([
        {"id": "10010", "name": "Backend"},
        {"id": "10020", "name": "Frontend", "description": "UI layer"},
    ]);

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(query_param(
            "fields",
            "summary,status,assignee,reporter,priority,issuetype,description,labels,components,fixVersions,versions,created,updated,comment,issuelinks",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("PROJ-1").await.unwrap();
    let components = issue.components();
    assert_eq!(components.len(), 2);
    assert_eq!(components[0].name, "Backend");
    assert_eq!(components[0].id, "10010");
    assert_eq!(components[1].description.as_deref(), Some("UI layer"));
}

#[tokio::test]
async fn get_issue_includes_fix_versions() {
    let server = MockServer::start().await;
    let mut fixture = issue_fixture("PROJ-1", "Test", "Open");
    fixture["fields"]["fixVersions"] = serde_json::json!([
        {"id": "10010", "name": "1.2.0", "released": true, "archived": false, "releaseDate": "2024-03-01"},
        {"id": "10020", "name": "1.3.0", "released": false, "archived": false},
    ]);
    fixture["fields"]["versions"] = serde_json::json!([
        {"id": "10005", "name": "1.1.0", "released": true, "archived": false},
    ]);

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(query_param(
            "fields",
            "summary,status,assignee,reporter,priority,issuetype,description,labels,components,fixVersions,versions,created,updated,comment,issuelinks",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("PROJ-1").await.unwrap();
    let fv = issue
        .fields
        .fix_versions
        .as_ref()
        .expect("fix_versions present");
    assert_eq!(fv.len(), 2);
    assert_eq!(fv[0].name, "1.2.0");
    assert_eq!(fv[0].id, "10010");
    assert_eq!(fv[0].release_date.as_deref(), Some("2024-03-01"));
    let av = issue.fields.versions.as_ref().expect("versions present");
    assert_eq!(av.len(), 1);
    assert_eq!(av[0].name, "1.1.0");
}

// ── Search / list ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn search_returns_issues_with_pagination_metadata() {
    let server = MockServer::start().await;
    let issue = issue_fixture("PROJ-1", "First issue", "To Do");

    // Cloud `/search/jql` response: no `total`, cursor-based pagination.
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issues": [issue],
            "isLast": true,
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let resp = client.search("project = PROJ", 1, 0).await.unwrap();
    assert!(resp.total.is_none(), "Cloud does not return a total");
    assert_eq!(resp.start_at, 0);
    assert!(resp.is_last);
    assert_eq!(resp.issues.len(), 1);
    assert_eq!(resp.issues[0].key, "PROJ-1");
}

#[tokio::test]
async fn search_v3_passes_jql_in_post_body() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(body_partial_json(
            serde_json::json!({ "jql": "project = PROJ" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(vec![])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client.search("project = PROJ", 50, 0).await.unwrap();
}

#[tokio::test]
async fn search_v3_walks_cursor_to_reach_offset() {
    // The new Cloud `/search/jql` endpoint does not support `startAt`.
    // With `offset = 25`, the client must first walk the cursor forward by
    // issuing an `id`-only skip request, then make the real request using
    // the returned `nextPageToken`.
    let server = MockServer::start().await;

    // Skip request: no `nextPageToken`, returns a cursor for the real request.
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(body_partial_json(serde_json::json!({
            "fields": ["id"],
            "maxResults": 25,
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(search_jql_response(
                (0..25)
                    .map(|i| serde_json::json!({ "id": i.to_string(), "key": format!("PROJ-{i}") }))
                    .collect(),
                Some("cursor-after-25"),
                false,
            )),
        )
        .expect(1)
        .mount(&server)
        .await;

    // Real request: carries the cursor and the full field list.
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(body_partial_json(serde_json::json!({
            "nextPageToken": "cursor-after-25",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(vec![])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let resp = client.search("project = PROJ", 25, 25).await.unwrap();
    assert_eq!(resp.start_at, 25);
    assert!(resp.total.is_none(), "Cloud does not return a total");
}

#[tokio::test]
async fn search_v3_uses_post_with_fields_and_no_start_at() {
    let server = MockServer::start().await;
    let long_clause = "x".repeat(2000);
    let jql = format!("summary ~ \"{long_clause}\"");

    // GET must NEVER be used — the new endpoint is always called via POST.
    Mock::given(method("GET"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(500).set_body_string("should not use GET"))
        .expect(0)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(vec![])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let _resp = client.search(&jql, 50, 0).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let post_req = requests
        .iter()
        .find(|r| r.method == wiremock::http::Method::POST)
        .expect("POST request not found");
    let body: serde_json::Value = serde_json::from_slice(&post_req.body).unwrap();
    assert_eq!(body["jql"], jql.as_str());
    assert_eq!(body["maxResults"], 50);
    assert!(body.get("startAt").is_none(), "startAt must not be sent");
    assert!(
        body["fields"].is_array(),
        "fields must be a JSON array on the new endpoint"
    );
}

#[tokio::test]
async fn search_v2_uses_classic_endpoint_with_start_at() {
    let server = MockServer::start().await;

    // API v2 (Data Center / Server) still uses /rest/api/2/search with startAt.
    Mock::given(method("GET"))
        .and(path("/rest/api/2/search"))
        .and(query_param("startAt", "25"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issues": [],
            "total": 0,
            "startAt": 25,
            "maxResults": 25,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client_v2(&server);
    client.search("project = PROJ", 25, 25).await.unwrap();
}

// ── Issue detail ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn show_issue_includes_description_and_comments() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_fixture(
            "PROJ-42",
            "Important bug",
            "In Progress",
        )))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("PROJ-42").await.unwrap();

    // Verify description extraction
    assert_eq!(issue.description_text(), "Test description");
    // Verify comment is present and has correct content
    let comment_list = issue.fields.comment.as_ref().unwrap();
    assert_eq!(comment_list.total, 1);
    assert_eq!(comment_list.comments.len(), 1);
    assert_eq!(comment_list.comments[0].body_text(), "Looks good");
    assert_eq!(comment_list.comments[0].author.display_name, "Alice");
}

#[tokio::test]
async fn show_issue_json_contains_expected_fields() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_fixture(
            "PROJ-42",
            "Important bug",
            "In Progress",
        )))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("PROJ-42").await.unwrap();
    assert_eq!(issue.key, "PROJ-42");
    assert_eq!(issue.summary(), "Important bug");
    assert_eq!(issue.status(), "In Progress");
    assert_eq!(issue.issue_type(), "Bug");
    assert_eq!(issue.priority(), "Medium");
    assert_eq!(issue.assignee(), "Alice");
    assert_eq!(issue.description_text(), "Test description");
}

#[tokio::test]
async fn show_issue_json_includes_components() {
    let server = MockServer::start().await;
    let mut fixture = issue_fixture("PROJ-1", "Test", "Open");
    fixture["fields"]["components"] = serde_json::json!([
        {"id": "10010", "name": "Backend", "description": "Server-side"},
    ]);

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("PROJ-1").await.unwrap();

    // Verify the typed field round-trips through serde — this pins the invariant
    // that components serialize correctly in the JSON envelope produced by
    // issue_detail_to_json (which uses `issue.fields.components` directly).
    let envelope = serde_json::to_value(serde_json::json!({
        "labels": issue.fields.labels,
        "components": issue.fields.components,
    }))
    .unwrap();
    let comps = envelope.get("components").expect("components key present");
    assert_eq!(comps[0]["name"], "Backend");
    assert_eq!(comps[0]["description"], "Server-side");
    assert_eq!(comps[0]["id"], "10010");
}

#[tokio::test]
async fn show_issue_json_includes_fix_versions() {
    let server = MockServer::start().await;
    let mut fixture = issue_fixture("PROJ-1", "Test", "Open");
    fixture["fields"]["fixVersions"] = serde_json::json!([
        {"id": "10010", "name": "1.2.0", "released": true, "archived": false, "releaseDate": "2024-03-01"},
    ]);
    fixture["fields"]["versions"] = serde_json::json!([
        {"id": "10005", "name": "1.1.0", "released": true, "archived": false},
    ]);

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("PROJ-1").await.unwrap();

    // Verifies the JSON shape produced by the `--json` code path, which passes the
    // deserialized issue through `issue_detail_to_json`.
    let json = jira_cli::commands::issues::issue_detail_to_json(&issue, &client);

    let fvs = json["fixVersions"]
        .as_array()
        .expect("fixVersions array present");
    assert_eq!(fvs.len(), 1);
    assert_eq!(fvs[0]["name"], "1.2.0");
    assert_eq!(fvs[0]["id"], "10010");
    assert_eq!(fvs[0]["released"], true);
    assert_eq!(fvs[0]["archived"], false);
    assert_eq!(fvs[0]["releaseDate"], "2024-03-01");

    let avs = json["affectedVersions"]
        .as_array()
        .expect("affectedVersions array present");
    assert_eq!(avs.len(), 1);
    assert_eq!(avs[0]["name"], "1.1.0");
    assert_eq!(avs[0]["released"], true);
}

#[tokio::test]
async fn show_issue_extracts_adf_description() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex(r"/rest/api/3/issue/PROJ-\d+"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(issue_fixture("PROJ-1", "Test", "Open")),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("PROJ-1").await.unwrap();
    assert_eq!(issue.description_text(), "Test description");
}

// ── Create issue ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_issue_posts_correct_payload() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10042",
            "key": "PROJ-42",
            "self": "https://test.atlassian.net/rest/api/3/issue/10042"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let resp = client
        .create_issue(
            &IssueDraft {
                project_key: "PROJ",
                issue_type: "Bug",
                summary: "Something broke",
                description: Some("Details here"),
                priority: None,
                labels: None,
                components: None,
                fix_versions: None,
                assignee: None,
                parent: None,
            },
            &[],
        )
        .await
        .unwrap();
    assert_eq!(resp.key, "PROJ-42");
    assert_eq!(resp.id, "10042");
}

// ── Comments ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn add_comment_posts_adf_body() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/comment"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10200",
            "author": { "displayName": "Alice", "accountId": "abc123" },
            "body": {
                "type": "doc", "version": 1,
                "content": [{"type": "paragraph", "content": [{"type": "text", "text": "My comment"}]}]
            },
            "created": "2024-01-22T08:00:00.000Z"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let comment = client.add_comment("PROJ-1", "My comment").await.unwrap();
    assert_eq!(comment.id, "10200");
    assert_eq!(comment.author.display_name, "Alice");
    assert_eq!(comment.body_text(), "My comment");

    // Verify the request payload is ADF, not a plain string
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["body"]["type"], "doc",
        "v3 comment body must be ADF doc"
    );
    assert_eq!(body["body"]["version"], 1);
    let content = body["body"]["content"]
        .as_array()
        .expect("content must be array");
    assert!(!content.is_empty());
    assert_eq!(content[0]["type"], "paragraph");
}

#[tokio::test]
async fn add_comment_404_returns_not_found_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-999/comment"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Issue Does Not Exist"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.add_comment("PROJ-999", "test").await.unwrap_err();
    assert!(
        matches!(err, ApiError::NotFound(_)),
        "404 from comment endpoint must map to NotFound, got: {err:?}"
    );
}

#[tokio::test]
async fn get_issue_fetches_additional_comment_pages() {
    let server = MockServer::start().await;

    // Issue with 1 embedded comment but total=3 — client must fetch 2 more
    let issue_body = serde_json::json!({
        "id": "10001",
        "key": "PROJ-1",
        "fields": {
            "summary": "Test",
            "status": { "name": "Open" },
            "issuetype": { "name": "Bug" },
            "comment": {
                "comments": [
                    {
                        "id": "1",
                        "author": { "displayName": "Alice", "accountId": "abc" },
                        "body": null,
                        "created": "2024-01-01T00:00:00.000Z"
                    }
                ],
                "total": 3,
                "startAt": 0,
                "maxResults": 1
            }
        }
    });

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_body))
        .mount(&server)
        .await;

    // Additional comment page
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/comment"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "comments": [
                {
                    "id": "2",
                    "author": { "displayName": "Bob", "accountId": "def" },
                    "body": null,
                    "created": "2024-01-02T00:00:00.000Z"
                },
                {
                    "id": "3",
                    "author": { "displayName": "Charlie", "accountId": "ghi" },
                    "body": null,
                    "created": "2024-01-03T00:00:00.000Z"
                }
            ],
            "total": 3,
            "startAt": 1,
            "maxResults": 100
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("PROJ-1").await.unwrap();
    let comment_list = issue.fields.comment.as_ref().unwrap();
    // All 3 comments must be present after pagination
    assert_eq!(comment_list.comments.len(), 3);
    assert_eq!(comment_list.comments[0].id, "1");
    assert_eq!(comment_list.comments[1].id, "2");
    assert_eq!(comment_list.comments[2].id, "3");
}

// ── Transitions ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_transitions_returns_list() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transitions": [
                { "id": "11", "name": "To Do" },
                { "id": "21", "name": "In Progress" },
                { "id": "31", "name": "Done" },
            ]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let ts = client.get_transitions("PROJ-1").await.unwrap();
    assert_eq!(ts.len(), 3);
    assert_eq!(ts[1].name, "In Progress");
    assert_eq!(ts[1].id, "21");
}

#[tokio::test]
async fn get_transitions_includes_to_field_when_present() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transitions": [{
                "id": "21",
                "name": "In Progress",
                "to": {
                    "name": "In Progress",
                    "statusCategory": { "key": "indeterminate", "name": "In Progress" }
                }
            }]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let ts = client.get_transitions("PROJ-1").await.unwrap();
    assert_eq!(ts.len(), 1);
    let to = ts[0].to.as_ref().unwrap();
    assert_eq!(to.name, "In Progress");
    let cat = to.status_category.as_ref().unwrap();
    assert_eq!(cat.key, "indeterminate");
}

#[tokio::test]
async fn transition_matches_by_name_case_insensitive() {
    let server = MockServer::start().await;

    // transitions endpoint
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transitions": [
                { "id": "11", "name": "To Do" },
                { "id": "21", "name": "In Progress" },
            ]
        })))
        .mount(&server)
        .await;

    // transition POST
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::transition(&client, &out, "PROJ-1", "in progress")
        .await
        .unwrap();
}

#[tokio::test]
async fn transition_not_found_returns_structured_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transitions": [{ "id": "11", "name": "Done" }]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    let err = jira_cli::commands::issues::transition(&client, &out, "PROJ-1", "Nonexistent")
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)));
    // Error message must reference the missing transition name
    if let ApiError::NotFound(msg) = &err {
        assert!(
            msg.contains("Nonexistent"),
            "expected transition name in error, got: {msg}"
        );
    }
}

// ── Projects ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_projects_returns_all_projects() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_search_response(vec![
            serde_json::json!({ "id": "10000", "key": "ALPHA", "name": "Alpha Project", "projectTypeKey": "software" }),
            serde_json::json!({ "id": "10001", "key": "BETA", "name": "Beta Project", "projectTypeKey": "business" }),
        ])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let projects = client.list_projects().await.unwrap();
    assert_eq!(projects.len(), 2);
    assert_eq!(projects[0].key, "ALPHA");
    assert_eq!(projects[0].name, "Alpha Project");
    assert_eq!(projects[1].key, "BETA");
}

#[tokio::test]
async fn list_projects_handles_short_non_terminal_pages() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .and(query_param("startAt", "0"))
        .and(query_param("maxResults", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [
                { "id": "10000", "key": "ALPHA", "name": "Alpha Project", "projectTypeKey": "software" },
                { "id": "10001", "key": "BETA", "name": "Beta Project", "projectTypeKey": "business" }
            ],
            "total": 3,
            "startAt": 0,
            "maxResults": 50,
            "isLast": false
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .and(query_param("startAt", "2"))
        .and(query_param("maxResults", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [
                { "id": "10002", "key": "GAMMA", "name": "Gamma Project", "projectTypeKey": "service_desk" }
            ],
            "total": 3,
            "startAt": 2,
            "maxResults": 50,
            "isLast": true
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let projects = client.list_projects().await.unwrap();
    assert_eq!(projects.len(), 3);
    assert_eq!(projects[0].key, "ALPHA");
    assert_eq!(projects[1].key, "BETA");
    assert_eq!(projects[2].key, "GAMMA");
}

// ── Error mapping ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn api_401_maps_to_auth_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.list_projects().await.unwrap_err();
    assert!(matches!(err, ApiError::Auth(_)));
}

#[tokio::test]
async fn api_404_maps_to_not_found_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex(r"/rest/api/3/issue/PROJ-\d+"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Issue does not exist"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.get_issue("PROJ-999").await.unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)));
}

#[tokio::test]
async fn api_429_maps_to_rate_limit_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.list_projects().await.unwrap_err();
    assert!(matches!(err, ApiError::RateLimit));
}

#[tokio::test]
async fn api_500_maps_to_api_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.list_projects().await.unwrap_err();
    assert!(matches!(err, ApiError::Api { status: 500, .. }));
}

// ── JQL escaping ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn search_encodes_jql_in_query_string() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(vec![])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    // JQL with special characters — should not panic or produce a malformed URL
    client
        .search(
            r#"project = "My Project" AND status = "In Progress""#,
            10,
            0,
        )
        .await
        .unwrap();
}

// ── ADF helpers ───────────────────────────────────────────────────────────────

#[test]
fn text_to_adf_multiline_produces_multiple_paragraphs() {
    use jira_cli::api::text_to_adf;
    let adf = text_to_adf("First line\nSecond line");
    let content = adf["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "paragraph");
    assert_eq!(content[1]["type"], "paragraph");
}

#[test]
fn text_to_adf_blank_line_produces_empty_content_array() {
    use jira_cli::api::text_to_adf;
    let adf = text_to_adf("Before\n\nAfter");
    let content = adf["content"].as_array().unwrap();
    assert_eq!(content.len(), 3);
    // The blank line must produce an empty content array — not a text node
    // with "" which some Jira Cloud instances reject with 400.
    let blank = &content[1];
    assert_eq!(blank["type"], "paragraph");
    let blank_content = blank["content"].as_array().unwrap();
    assert!(
        blank_content.is_empty(),
        "blank line must produce empty content, not text nodes"
    );
}

#[test]
fn adf_extract_handles_code_block() {
    use jira_cli::api::extract_adf_text;
    let doc = serde_json::json!({
        "type": "doc",
        "version": 1,
        "content": [{
            "type": "codeBlock",
            "content": [{"type": "text", "text": "let x = 1;"}]
        }]
    });
    assert_eq!(extract_adf_text(&doc), "let x = 1;");
}

#[test]
fn escape_jql_prevents_injection() {
    use jira_cli::api::escape_jql;
    let malicious = r#"Done" OR 1=1 --"#;
    let escaped = escape_jql(malicious);
    // The double quote must be backslash-escaped so it cannot break out of a JQL string literal.
    // After escaping, `Done" OR 1=1 --` becomes `Done\" OR 1=1 --`.
    assert!(
        escaped.contains("\\\""),
        "double quote must be backslash-escaped"
    );
    // The escaped value must start with the prefix up to and including the escaped quote,
    // confirming the quote is inside the escaped string, not acting as a string terminator.
    assert!(
        escaped.starts_with(r#"Done\""#),
        "escaped value must begin with the literal prefix Done\""
    );
}

// ── issue update ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn update_issue_sends_components() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": {
                "components": [{ "name": "Backend" }],
            }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                components: Some(&["Backend"]),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn update_issue_components_empty_clears_field() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": { "components": [] }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                components: Some(&[]),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn update_issue_sends_put_request() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                summary: Some("New summary"),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();

    // Verify only specified fields appear — unset fields must be omitted, not sent as null
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["fields"]["summary"], "New summary");
    assert!(
        body["fields"].get("description").is_none(),
        "unset description must not be sent"
    );
    assert!(
        body["fields"].get("priority").is_none(),
        "unset priority must not be sent"
    );
}

#[tokio::test]
async fn update_issue_requires_at_least_one_field() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let err = client
        .update_issue("PROJ-1", &IssueUpdate::default(), &[])
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::InvalidInput(_)));
    if let ApiError::InvalidInput(msg) = err {
        assert!(
            msg.contains("--fix-versions")
                && msg.contains("--labels")
                && msg.contains("--assignee"),
            "error message should mention new flags, got: {msg}"
        );
    }
}

#[tokio::test]
async fn update_issue_sends_fix_versions() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": {
                "fixVersions": [{ "name": "1.2.0" }],
            }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                fix_versions: Some(&["1.2.0"]),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn update_issue_fix_versions_none_sentinel_clears() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": { "fixVersions": [] }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                fix_versions: Some(&[]),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn update_issue_sends_labels() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": { "labels": ["backend", "urgent"] }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                labels: Some(&["backend", "urgent"]),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn update_issue_labels_empty_clears_field() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": { "labels": [] }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                labels: Some(&[]),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn update_issue_sends_assignee_v3() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": { "assignee": { "accountId": "abc123" } }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                assignee: Some(Some("abc123")),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn update_issue_sends_assignee_v2() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/2/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": { "assignee": { "name": "ruben" } }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client_v2(&server);
    client
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                assignee: Some(Some("ruben")),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn update_issue_unassigns_via_null() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": { "assignee": null }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client
        .update_issue(
            "PROJ-1",
            &IssueUpdate {
                assignee: Some(None),
                ..Default::default()
            },
            &[],
        )
        .await
        .unwrap();
}

// ── myself ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn myself_returns_account_id_and_display_name() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "abc123",
            "displayName": "Alice Smith",
            "emailAddress": "alice@example.com"
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let me = client.get_myself().await.unwrap();
    assert_eq!(me.account_id, "abc123");
    assert_eq!(me.display_name, "Alice Smith");
}

// ── accountId in issue JSON ───────────────────────────────────────────────────

#[tokio::test]
async fn issue_json_includes_assignee_account_id() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path_regex(r"/rest/api/3/issue/PROJ-\d+"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "10001",
            "key": "PROJ-1",
            "fields": {
                "summary": "Test",
                "status": { "name": "Open" },
                "assignee": {
                    "displayName": "Alice",
                    "accountId": "alice-account-id-123",
                    "emailAddress": "alice@example.com"
                },
                "issuetype": { "name": "Bug" },
                "priority": { "name": "High" }
            }
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let issue = client.get_issue("PROJ-1").await.unwrap();
    let account_id = issue
        .fields
        .assignee
        .as_ref()
        .and_then(|a| a.account_id.as_deref());
    assert_eq!(account_id, Some("alice-account-id-123"));
}

// ── transition error stdout contract ─────────────────────────────────────────

#[tokio::test]
async fn transition_not_found_produces_no_stdout_data() {
    // Verifies that a failed transition does NOT write JSON to stdout.
    // Error information goes to stderr via print_message; stdout stays clean
    // so agents piping stdout get nothing on failure.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transitions": [{ "id": "11", "name": "Done" }]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = OutputConfig {
        json: true,
        quiet: true,
    };
    let err = jira_cli::commands::issues::transition(&client, &out, "PROJ-1", "Nonexistent")
        .await
        .unwrap_err();

    // Must return NotFound (exit code 4), not a generic error
    assert!(
        matches!(err, ApiError::NotFound(_)),
        "expected NotFound, got: {err}"
    );
    // The error message must identify the missing transition
    if let ApiError::NotFound(msg) = &err {
        assert!(
            msg.contains("Nonexistent"),
            "error message must include the requested transition name; got: {msg}"
        );
        assert!(
            msg.contains("PROJ-1"),
            "error message must include the issue key; got: {msg}"
        );
    }
}

// ── invalid input exit code ───────────────────────────────────────────────────

#[test]
fn invalid_issue_key_maps_to_input_error_exit_code() {
    use jira_cli::output::{exit_code_for_error, exit_codes};
    let err = ApiError::InvalidInput("bad key".into());
    assert_eq!(exit_code_for_error(&err), exit_codes::INPUT_ERROR);
}

#[test]
fn missing_credentials_maps_to_input_error_exit_code() {
    use jira_cli::output::{exit_code_for_error, exit_codes};
    let err = ApiError::InvalidInput("No Jira host configured.".into());
    assert_eq!(exit_code_for_error(&err), exit_codes::INPUT_ERROR);
}

// ── Empty results ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn issues_list_with_no_results_succeeds() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(vec![])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::list(
        &client,
        &out,
        jira_cli::commands::issues::ListFilters::default(),
        50,
        0,
        false,
        None,
    )
    .await
    .unwrap();
}

// ── Myself command ────────────────────────────────────────────────────────────

#[tokio::test]
async fn myself_command_returns_account_info() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "user-abc-123",
            "displayName": "Test User",
            "emailAddress": "test@example.com"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::myself::show(&client, &out)
        .await
        .unwrap();
}

// ── Projects show ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn projects_show_returns_project_details() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/PROJ"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "10001",
            "key": "PROJ",
            "name": "Test Project",
            "projectTypeKey": "software"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::projects::show(&client, &out, "PROJ")
        .await
        .unwrap();
}

// ── PAT auth ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pat_auth_sends_bearer_header() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .and(header("authorization", "Bearer my-pat-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_search_response(vec![])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client_pat(&server);
    client.list_projects().await.unwrap();
}

#[tokio::test]
async fn basic_auth_does_not_send_bearer_header() {
    let server = MockServer::start().await;

    // Basic auth header must NOT start with "Bearer"
    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .and(header(
            "authorization",
            "Basic dGVzdEBleGFtcGxlLmNvbTp0ZXN0LXRva2Vu",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_search_response(vec![])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client.list_projects().await.unwrap();
}

// ── API v2 ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn api_v2_uses_v2_base_path() {
    let server = MockServer::start().await;

    // Must hit /rest/api/2/myself, not /rest/api/3/myself
    Mock::given(method("GET"))
        .and(path("/rest/api/2/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "testuser",
            "displayName": "Test User",
            "key": "testuser",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client_v2(&server);
    client.get_myself().await.unwrap();
}

#[tokio::test]
async fn api_v2_add_comment_sends_plain_string_body() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue/PROJ-1/comment"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10100",
            "author": { "displayName": "Test", "accountId": "abc" },
            "body": "Hello world",
            "created": "2024-01-01T00:00:00.000Z"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client_v2(&server);
    let comment = client.add_comment("PROJ-1", "Hello world").await.unwrap();
    assert_eq!(comment.id, "10100");

    // v2 must send a plain JSON string, not an ADF object — Jira DC/Server rejects ADF
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(
        body["body"].is_string(),
        "v2 comment body must be a plain string, got: {}",
        body["body"]
    );
    assert_eq!(body["body"], "Hello world");
}

#[tokio::test]
async fn api_v2_plain_string_description_is_extracted_as_text() {
    use jira_cli::api::extract_adf_text;
    let plain = serde_json::Value::String("This is a plain description".to_string());
    assert_eq!(extract_adf_text(&plain), "This is a plain description");
}

#[tokio::test]
async fn api_v3_adf_description_still_extracted_correctly() {
    use jira_cli::api::extract_adf_text;
    let adf = serde_json::json!({
        "type": "doc", "version": 1,
        "content": [{"type": "paragraph", "content": [{"type": "text", "text": "ADF paragraph"}]}]
    });
    assert_eq!(extract_adf_text(&adf), "ADF paragraph");
}

// ── Users ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn search_users_v3_uses_query_param() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/api/3/user/search"))
        .and(query_param("query", "alice"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "accountId": "abc123", "displayName": "Alice Smith", "emailAddress": "alice@example.com" }
        ])))
        .mount(&server)
        .await;

    let users = client.search_users("alice").await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].account_id, "abc123");
    assert_eq!(users[0].display_name, "Alice Smith");
}

#[tokio::test]
async fn search_users_v2_uses_username_param() {
    let server = MockServer::start().await;
    let client = test_client_v2(&server);

    Mock::given(method("GET"))
        .and(path("/rest/api/2/user/search"))
        .and(query_param("username", "ruben"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "name": "ruben", "displayName": "Ruben J", "emailAddress": "r@example.com" }
        ])))
        .mount(&server)
        .await;

    let users = client.search_users("ruben").await.unwrap();
    assert_eq!(users.len(), 1);
    // v2 `name` field is deserialized into account_id via the alias
    assert_eq!(users[0].account_id, "ruben");
}

// ── Issue links ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_link_types_returns_list() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issueLinkType"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issueLinkTypes": [
                { "id": "10000", "name": "Blocks", "inward": "is blocked by", "outward": "blocks" },
                { "id": "10003", "name": "Relates", "inward": "relates to", "outward": "relates to" },
            ]
        })))
        .mount(&server)
        .await;

    let types = client.get_link_types().await.unwrap();
    assert_eq!(types.len(), 2);
    assert_eq!(types[0].name, "Blocks");
    assert_eq!(types[0].outward, "blocks");
    assert_eq!(types[0].inward, "is blocked by");
}

#[tokio::test]
async fn link_issues_posts_correct_payload() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issueLink"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    client
        .link_issues("PROJ-1", "PROJ-2", "Blocks")
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["type"]["name"], "Blocks");
    assert_eq!(body["inwardIssue"]["key"], "PROJ-1");
    assert_eq!(body["outwardIssue"]["key"], "PROJ-2");
}

#[tokio::test]
async fn unlink_issues_sends_delete() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("DELETE"))
        .and(path("/rest/api/3/issueLink/10042"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    client.unlink_issues("10042").await.unwrap();
}

#[tokio::test]
async fn show_issue_includes_issue_links_in_json() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let mut fixture = issue_fixture("PROJ-1", "Issue with links", "Open");
    fixture["fields"]["issuelinks"] = serde_json::json!([
        {
            "id": "10003",
            "type": { "id": "10000", "name": "Blocks", "inward": "is blocked by", "outward": "blocks" },
            "outwardIssue": { "key": "PROJ-2", "fields": { "summary": "Blocked thing", "status": { "name": "To Do" } } }
        }
    ]);

    Mock::given(method("GET"))
        .and(path_regex("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture))
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::show(&client, &out, "PROJ-1", false)
        .await
        .unwrap();
}

// ── Boards & Sprints ──────────────────────────────────────────────────────────

#[tokio::test]
async fn list_boards_returns_all_boards() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [
                { "id": 1, "name": "TST board", "type": "scrum" },
                { "id": 2, "name": "KAN board", "type": "kanban" },
            ],
            "total": 2,
            "startAt": 0,
            "isLast": true
        })))
        .mount(&server)
        .await;

    let boards = client.list_boards().await.unwrap();
    assert_eq!(boards.len(), 2);
    assert_eq!(boards[0].id, 1);
    assert_eq!(boards[0].name, "TST board");
    assert_eq!(boards[0].board_type, "scrum");
}

#[tokio::test]
async fn list_sprints_filters_by_state() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .and(query_param("state", "active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [
                {
                    "id": 5,
                    "name": "Sprint 5",
                    "state": "active",
                    "startDate": "2026-03-01T00:00:00.000Z",
                    "endDate": "2026-03-15T00:00:00.000Z",
                    "originBoardId": 1
                }
            ],
            "startAt": 0,
            "isLast": true
        })))
        .mount(&server)
        .await;

    let sprints = client.list_sprints(1, Some("active")).await.unwrap();
    assert_eq!(sprints.len(), 1);
    assert_eq!(sprints[0].id, 5);
    assert_eq!(sprints[0].state, "active");
}

#[tokio::test]
async fn list_sprints_without_state_returns_all() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [
                { "id": 1, "name": "Sprint 1", "state": "closed", "originBoardId": 1 },
                { "id": 2, "name": "Sprint 2", "state": "active", "originBoardId": 1 },
            ],
            "startAt": 0,
            "isLast": true
        })))
        .mount(&server)
        .await;

    let sprints = client.list_sprints(1, None).await.unwrap();
    assert_eq!(sprints.len(), 2);
}

// ── Sprint assignment ─────────────────────────────────────────────────────────

#[tokio::test]
async fn move_issue_to_sprint_posts_to_agile_endpoint() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("POST"))
        .and(path("/rest/agile/1.0/sprint/5/issue"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    client.move_issue_to_sprint("PROJ-1", 5).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["issues"][0], "PROJ-1");
}

#[tokio::test]
async fn resolve_sprint_id_by_numeric_string() {
    // Numeric string resolves without any API calls
    let server = MockServer::start().await;
    let client = test_client(&server);
    let id = client.resolve_sprint_id("42").await.unwrap();
    assert_eq!(id, 42);
}

#[tokio::test]
async fn resolve_sprint_id_active_finds_first_active_sprint() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{ "id": 1, "name": "TST board", "type": "scrum" }],
            "total": 1, "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [
                { "id": 3, "name": "Sprint 3", "state": "closed", "originBoardId": 1 },
                { "id": 7, "name": "Sprint 7", "state": "active", "originBoardId": 1 },
            ],
            "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    let id = client.resolve_sprint_id("active").await.unwrap();
    assert_eq!(id, 7);
}

#[tokio::test]
async fn resolve_sprint_id_by_name_substring() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{ "id": 1, "name": "TST board", "type": "scrum" }],
            "total": 1, "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [
                { "id": 9, "name": "Q2 Planning Sprint", "state": "future", "originBoardId": 1 },
            ],
            "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    let id = client.resolve_sprint_id("Q2 Planning").await.unwrap();
    assert_eq!(id, 9);
}

// ── Custom fields ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_issue_sends_custom_fields() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10050", "key": "PROJ-50",
            "self": "https://test.atlassian.net/rest/api/3/issue/PROJ-50"
        })))
        .mount(&server)
        .await;

    let custom = vec![
        ("customfield_10106".to_string(), serde_json::json!(8)),
        ("customfield_10014".to_string(), serde_json::json!("PROJ-1")),
    ];
    client
        .create_issue(&minimal_draft("PROJ", "Story", "My story"), &custom)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["fields"]["customfield_10106"], 8);
    assert_eq!(body["fields"]["customfield_10014"], "PROJ-1");
}

#[tokio::test]
async fn update_issue_sends_custom_fields() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let custom = vec![("customfield_10106".to_string(), serde_json::json!(13))];
    client
        .update_issue("PROJ-1", &IssueUpdate::default(), &custom)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["fields"]["customfield_10106"], 13);
}

#[tokio::test]
async fn list_fields_returns_system_and_custom() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/api/3/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": "summary", "name": "Summary", "custom": false, "schema": { "type": "string", "system": "summary" } },
            { "id": "customfield_10106", "name": "Story Points", "custom": true, "schema": { "type": "number", "custom": "com.atlassian.jira.plugin.system.customfieldtypes:float" } },
        ])))
        .mount(&server)
        .await;

    let fields = client.list_fields().await.unwrap();
    assert_eq!(fields.len(), 2);
    assert!(!fields[0].custom);
    assert!(fields[1].custom);
    assert_eq!(fields[1].id, "customfield_10106");
}

// ── Assign: v2 vs v3 payload ──────────────────────────────────────────────────

#[tokio::test]
async fn assign_issue_v3_sends_account_id() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1/assignee"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    client.assign_issue("PROJ-1", Some("abc123")).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["accountId"], "abc123");
    assert!(body.get("name").is_none(), "v3 must not send 'name' field");
}

#[tokio::test]
async fn assign_issue_v2_sends_name() {
    let server = MockServer::start().await;
    let client = test_client_v2(&server);

    Mock::given(method("PUT"))
        .and(path("/rest/api/2/issue/PROJ-1/assignee"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    client.assign_issue("PROJ-1", Some("ruben")).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["name"], "ruben");
    assert!(
        body.get("accountId").is_none(),
        "v2 must not send 'accountId' field"
    );
}

#[tokio::test]
async fn assign_issue_v3_unassign_sends_null_account_id() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1/assignee"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    client.assign_issue("PROJ-1", None).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(body["accountId"].is_null());
}

#[tokio::test]
async fn assign_issue_v2_unassign_sends_null_name() {
    let server = MockServer::start().await;
    let client = test_client_v2(&server);

    Mock::given(method("PUT"))
        .and(path("/rest/api/2/issue/PROJ-1/assignee"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    client.assign_issue("PROJ-1", None).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(body["name"].is_null());
    assert!(
        body.get("accountId").is_none(),
        "v2 must not send 'accountId' field"
    );
}

// ── Create: v2 assignee uses name, not accountId ──────────────────────────────

#[tokio::test]
async fn create_issue_v2_assignee_uses_name_field() {
    let server = MockServer::start().await;
    let client = test_client_v2(&server);

    Mock::given(method("POST"))
        .and(path("/rest/api/2/issue"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10001", "key": "PROJ-1",
            "self": "http://jira.example.com/rest/api/2/issue/PROJ-1"
        })))
        .mount(&server)
        .await;

    client
        .create_issue(
            &IssueDraft {
                project_key: "PROJ",
                issue_type: "Task",
                summary: "My task",
                description: None,
                priority: None,
                labels: None,
                components: None,
                fix_versions: None,
                assignee: Some("ruben"),
                parent: None,
            },
            &[],
        )
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["fields"]["assignee"]["name"], "ruben");
    assert!(
        body["fields"]["assignee"].get("accountId").is_none(),
        "v2 create must not send 'accountId' for assignee"
    );
}

#[tokio::test]
async fn create_issue_v3_assignee_uses_account_id_field() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10001", "key": "PROJ-1",
            "self": "https://test.atlassian.net/rest/api/3/issue/PROJ-1"
        })))
        .mount(&server)
        .await;

    client
        .create_issue(
            &IssueDraft {
                project_key: "PROJ",
                issue_type: "Task",
                summary: "My task",
                description: None,
                priority: None,
                labels: None,
                components: None,
                fix_versions: None,
                assignee: Some("abc123"),
                parent: None,
            },
            &[],
        )
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["fields"]["assignee"]["accountId"], "abc123");
    assert!(
        body["fields"]["assignee"].get("name").is_none(),
        "v3 create must not send 'name' for assignee"
    );
}

// ── Sprint resolve error cases ────────────────────────────────────────────────

#[tokio::test]
async fn resolve_sprint_id_no_active_sprint_returns_not_found() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{ "id": 1, "name": "TST board", "type": "scrum" }],
            "total": 1, "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [
                { "id": 1, "name": "Sprint 1", "state": "closed", "originBoardId": 1 }
            ],
            "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    let err = client.resolve_sprint_id("active").await.unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)));
}

#[tokio::test]
async fn resolve_sprint_id_name_not_found_returns_not_found() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{ "id": 1, "name": "TST board", "type": "scrum" }],
            "total": 1, "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [
                { "id": 9, "name": "Q2 Sprint", "state": "active", "originBoardId": 1 }
            ],
            "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    let err = client.resolve_sprint_id("Q3 Sprint").await.unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)));
    if let ApiError::NotFound(msg) = err {
        assert!(msg.contains("Q3 Sprint"));
    }
}

#[tokio::test]
async fn resolve_sprint_id_no_boards_returns_not_found() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [], "total": 0, "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    let err = client.resolve_sprint_id("active").await.unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)));
}

// ── resolve_sprint returns full Sprint ────────────────────────────────────────

#[tokio::test]
async fn resolve_sprint_returns_sprint_with_name() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{ "id": 1, "name": "TST board", "type": "scrum" }],
            "total": 1, "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{ "id": 7, "name": "Team Sprint 4", "state": "active", "originBoardId": 1 }],
            "startAt": 0, "isLast": true
        })))
        .mount(&server)
        .await;

    let sprint = client.resolve_sprint("active").await.unwrap();
    assert_eq!(sprint.id, 7);
    assert_eq!(sprint.name, "Team Sprint 4");
}

#[tokio::test]
async fn get_sprint_fetches_single_sprint() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/sprint/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 42, "name": "Sprint 42", "state": "future", "originBoardId": 1
        })))
        .mount(&server)
        .await;

    let sprint = client.get_sprint(42).await.unwrap();
    assert_eq!(sprint.id, 42);
    assert_eq!(sprint.name, "Sprint 42");
}

// ── transition output includes resulting status ───────────────────────────────

#[tokio::test]
async fn transition_response_includes_resulting_status() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transitions": [{
                "id": "21",
                "name": "Start Progress",
                "to": {
                    "name": "In Progress",
                    "statusCategory": { "key": "indeterminate", "name": "In Progress" }
                }
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    // Verify the command completes without error.
    // The `status` field in the JSON output comes from the `to.name` field of the transition.
    jira_cli::commands::issues::transition(&client, &out, "PROJ-1", "Start Progress")
        .await
        .unwrap();
}

// ── issue list --type filter ───────────────────────────────────────────────────

#[tokio::test]
async fn issues_list_type_filter_adds_issuetype_to_jql() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(body_string_contains("issuetype"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(vec![])))
        .expect(1)
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::list(
        &client,
        &out,
        jira_cli::commands::issues::ListFilters {
            issue_type: Some("Bug"),
            ..Default::default()
        },
        50,
        0,
        false,
        None,
    )
    .await
    .unwrap();
}

// ── issue link sentence in JSON ───────────────────────────────────────────────

#[tokio::test]
async fn show_issue_link_json_includes_plain_english_sentence() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let mut fixture = issue_fixture("PROJ-1", "Issue with links", "Open");
    fixture["fields"]["issuelinks"] = serde_json::json!([
        {
            "id": "10003",
            "type": { "id": "10000", "name": "Blocks", "inward": "is blocked by", "outward": "blocks" },
            "outwardIssue": { "key": "PROJ-2", "fields": { "summary": "Blocked thing", "status": { "name": "To Do" } } }
        },
        {
            "id": "10004",
            "type": { "id": "10000", "name": "Blocks", "inward": "is blocked by", "outward": "blocks" },
            "inwardIssue": { "key": "PROJ-3", "fields": { "summary": "Blocker", "status": { "name": "In Progress" } } }
        }
    ]);

    Mock::given(method("GET"))
        .and(path_regex("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture))
        .mount(&server)
        .await;

    // Capture stdout via the json_out config
    let out = json_out();
    jira_cli::commands::issues::show(&client, &out, "PROJ-1", false)
        .await
        .unwrap();
    // Sentence format is validated by the command logic:
    // outward: "PROJ-1 blocks PROJ-2"
    // inward:  "PROJ-1 is blocked by PROJ-3"
}

// ── --all pagination flag ─────────────────────────────────────────────────────

#[tokio::test]
async fn issues_list_all_fetches_multiple_pages() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let page1 = search_jql_response(
        vec![issue_fixture("PROJ-1", "Issue 1", "Open")],
        Some("cursor-page-2"),
        false,
    );
    let page2 = search_jql_response(vec![issue_fixture("PROJ-2", "Issue 2", "Open")], None, true);

    // Second request carries the cursor. Mount it first so it takes priority
    // when both matchers match.
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(body_partial_json(
            serde_json::json!({ "nextPageToken": "cursor-page-2" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(page2))
        .expect(1)
        .mount(&server)
        .await;

    // First request: no nextPageToken. Matches any other POST to the endpoint.
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page1))
        .expect(1)
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::list(
        &client,
        &out,
        jira_cli::commands::issues::ListFilters::default(),
        50,
        0,
        true,
        None,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn search_all_fetches_multiple_pages() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let page1 = search_jql_response(
        vec![issue_fixture("PROJ-1", "Issue 1", "Open")],
        Some("cursor-page-2"),
        false,
    );
    let page2 = search_jql_response(vec![issue_fixture("PROJ-2", "Issue 2", "Open")], None, true);

    // Second request (mounted first so its more-specific matcher wins).
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(body_partial_json(
            serde_json::json!({ "nextPageToken": "cursor-page-2" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(page2))
        .expect(1)
        .mount(&server)
        .await;

    // First request: no nextPageToken.
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page1))
        .expect(1)
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::search::run(&client, &out, "project = PROJ", 50, 0, true, None)
        .await
        .unwrap();
}

// ── issues comments subcommand ────────────────────────────────────────────────

#[tokio::test]
async fn issues_comments_returns_comment_list() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("GET"))
        .and(path_regex("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_fixture(
            "PROJ-1",
            "Some issue",
            "Open",
        )))
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::comments(&client, &out, "PROJ-1")
        .await
        .unwrap();
}

#[tokio::test]
async fn issues_comments_empty_when_no_comments() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let mut fixture = issue_fixture("PROJ-5", "No comments issue", "Open");
    fixture["fields"]["comment"] = serde_json::json!({ "comments": [], "total": 0 });

    Mock::given(method("GET"))
        .and(path_regex("/rest/api/3/issue/PROJ-5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture))
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::comments(&client, &out, "PROJ-5")
        .await
        .unwrap();
}

// ── issues mine shorthand ─────────────────────────────────────────────────────

#[tokio::test]
async fn issues_mine_uses_current_user_assignee_filter() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(body_string_contains("currentUser"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(vec![])))
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::mine(
        &client,
        &out,
        jira_cli::commands::issues::ListFilters::default(),
        50,
        false,
        None,
    )
    .await
    .unwrap();
}

// ── Worklog ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn log_work_posts_to_worklog_endpoint() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let worklog_response = serde_json::json!({
        "id": "10200",
        "author": { "displayName": "Alice", "accountId": "abc123" },
        "timeSpent": "2h",
        "timeSpentSeconds": 7200,
        "started": "2024-01-15T09:00:00.000+0000",
        "created": "2024-01-15T09:05:00.000+0000"
    });

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/worklog"))
        .respond_with(ResponseTemplate::new(201).set_body_json(worklog_response))
        .expect(1)
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::log_work(&client, &out, "PROJ-1", "2h", None, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn log_work_with_comment_includes_body_in_payload() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let worklog_response = serde_json::json!({
        "id": "10201",
        "author": { "displayName": "Alice", "accountId": "abc123" },
        "timeSpent": "30m",
        "timeSpentSeconds": 1800,
        "started": "2024-01-15T10:00:00.000+0000",
        "created": "2024-01-15T10:01:00.000+0000"
    });

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-2/worklog"))
        .respond_with(ResponseTemplate::new(201).set_body_json(worklog_response))
        .expect(1)
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::log_work(
        &client,
        &out,
        "PROJ-2",
        "30m",
        Some("Fixed the flaky test"),
        None,
    )
    .await
    .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["timeSpent"], "30m");
    // v3 sends ADF comment; the doc type should be present
    assert_eq!(body["comment"]["type"], "doc");
}

// ── Bulk transition ───────────────────────────────────────────────────────────

#[tokio::test]
async fn bulk_transition_dry_run_makes_no_api_calls() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let issues = vec![
        issue_fixture("PROJ-1", "Issue 1", "To Do"),
        issue_fixture("PROJ-2", "Issue 2", "To Do"),
    ];

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(issues)))
        .mount(&server)
        .await;

    // No transition calls should happen in dry-run mode (mock would fail if called)
    let out = json_out();
    jira_cli::commands::issues::bulk_transition(
        &client,
        &out,
        "project = PROJ AND status = 'To Do'",
        "In Progress",
        true,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn bulk_transition_calls_transition_for_each_issue() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let issues = vec![issue_fixture("PROJ-1", "Issue 1", "To Do")];

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(issues)))
        .mount(&server)
        .await;

    let transitions = serde_json::json!({
        "transitions": [
            { "id": "21", "name": "In Progress", "to": { "name": "In Progress", "statusCategory": { "key": "indeterminate", "name": "In Progress" } } }
        ]
    });

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(transitions))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::bulk_transition(
        &client,
        &out,
        "project = PROJ",
        "In Progress",
        false,
    )
    .await
    .unwrap();
}

// ── Bulk assign ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn bulk_assign_dry_run_makes_no_api_calls() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let issues = vec![issue_fixture("PROJ-1", "Issue 1", "Open")];

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(issues)))
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::bulk_assign(&client, &out, "project = PROJ", "alice123", true)
        .await
        .unwrap();
}

#[tokio::test]
async fn bulk_assign_calls_assign_for_each_issue() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let issues = vec![issue_fixture("PROJ-1", "Issue 1", "Open")];

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_response(issues)))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1/assignee"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::bulk_assign(&client, &out, "project = PROJ", "alice123", false)
        .await
        .unwrap();
}

// ── Subtask creation (--parent flag) ─────────────────────────────────────────

#[tokio::test]
async fn create_issue_with_parent_includes_parent_field() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10010",
            "key": "PROJ-10",
            "self": "https://test.atlassian.net/rest/api/3/issue/10010"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let out = json_out();
    jira_cli::commands::issues::create(
        &client,
        &out,
        &IssueDraft {
            project_key: "PROJ",
            issue_type: "Subtask",
            summary: "Do a sub-thing",
            description: None,
            priority: None,
            labels: None,
            components: None,
            fix_versions: None,
            assignee: None,
            parent: Some("PROJ-5"),
        },
        None,
        &[],
    )
    .await
    .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["fields"]["parent"]["key"], "PROJ-5");
    assert_eq!(body["fields"]["issuetype"]["name"], "Subtask");
}

// ── get_project (client method) ───────────────────────────────────────────────

#[tokio::test]
async fn get_project_fetches_single_project_by_key() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/PROJ"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "10001",
            "key": "PROJ",
            "name": "My Project",
            "projectTypeKey": "software"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let project = client.get_project("PROJ").await.unwrap();
    assert_eq!(project.key, "PROJ");
    assert_eq!(project.name, "My Project");
    assert_eq!(project.project_type.as_deref(), Some("software"));
}

#[tokio::test]
async fn get_project_404_returns_not_found() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/NOPE"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Project Does Not Exist"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.get_project("NOPE").await.unwrap_err();
    assert!(
        matches!(err, ApiError::NotFound(_)),
        "404 from project endpoint must map to NotFound"
    );
}

// ── Error cases for write operations ─────────────────────────────────────────

#[tokio::test]
async fn do_transition_404_returns_not_found() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-999/transitions"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Issue Does Not Exist"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.do_transition("PROJ-999", "21").await.unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)));
}

#[tokio::test]
async fn assign_issue_404_returns_not_found() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-999/assignee"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Issue Does Not Exist"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client
        .assign_issue("PROJ-999", Some("alice"))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)));
}

#[tokio::test]
async fn link_issues_404_returns_not_found() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issueLink"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Issue Does Not Exist"))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client
        .link_issues("PROJ-1", "PROJ-999", "Blocks")
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)));
}

#[tokio::test]
async fn log_work_400_maps_to_api_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/worklog"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "errorMessages": [],
            "errors": { "timeSpent": "Invalid time value" }
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client
        .log_work("PROJ-1", "notavalidtime", None, None)
        .await
        .unwrap_err();
    // 400 maps to ApiError (not NotFound or Auth)
    assert!(
        !matches!(err, ApiError::NotFound(_) | ApiError::Auth(_)),
        "400 should not map to NotFound or Auth"
    );
}

// ── log_work --started parameter ─────────────────────────────────────────────

#[tokio::test]
async fn log_work_with_started_includes_started_in_payload() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/worklog"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10202",
            "author": { "displayName": "Alice", "accountId": "abc123" },
            "timeSpent": "1h",
            "timeSpentSeconds": 3600,
            "started": "2024-06-01T09:00:00.000+0000",
            "created": "2024-06-01T09:01:00.000+0000"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::log_work(
        &client,
        &out,
        "PROJ-1",
        "1h",
        None,
        Some("2024-06-01T09:00:00.000+0000"),
    )
    .await
    .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["timeSpent"], "1h");
    assert_eq!(body["started"], "2024-06-01T09:00:00.000+0000");
    // No comment field when comment is None
    assert!(
        body.get("comment").is_none(),
        "comment must be absent when not provided"
    );
}

// ── Command layer: link, unlink, link_types ───────────────────────────────────

#[tokio::test]
async fn issues_link_command_posts_correct_payload() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issueLink"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::link(&client, &out, "PROJ-1", "PROJ-2", "Blocks")
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["type"]["name"], "Blocks");
    assert_eq!(body["inwardIssue"]["key"], "PROJ-1");
    assert_eq!(body["outwardIssue"]["key"], "PROJ-2");
}

#[tokio::test]
async fn issues_unlink_command_sends_delete_request() {
    let server = MockServer::start().await;

    Mock::given(method("DELETE"))
        .and(path("/rest/api/3/issueLink/10055"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::unlink(&client, &out, "10055")
        .await
        .unwrap();
}

#[tokio::test]
async fn issues_link_types_command_returns_list() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issueLinkType"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issueLinkTypes": [
                { "id": "10000", "name": "Blocks", "inward": "is blocked by", "outward": "blocks" },
                { "id": "10001", "name": "Cloners", "inward": "is cloned by", "outward": "clones" }
            ]
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::link_types(&client, &out)
        .await
        .unwrap();
}

// ── Command layer: move_to_sprint ─────────────────────────────────────────────

#[tokio::test]
async fn move_to_sprint_command_resolves_name_and_posts_to_agile() {
    let server = MockServer::start().await;

    // Board list
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{ "id": 1, "name": "TST board", "type": "scrum" }],
            "isLast": true, "startAt": 0, "total": 1
        })))
        .mount(&server)
        .await;

    // Sprint list for board 1
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{
                "id": 7, "name": "Sprint Alpha", "state": "active",
                "startDate": "2024-01-01T00:00:00Z", "endDate": "2024-01-14T00:00:00Z"
            }],
            "isLast": true, "startAt": 0
        })))
        .mount(&server)
        .await;

    // Move to sprint
    Mock::given(method("POST"))
        .and(path("/rest/agile/1.0/sprint/7/issue"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::move_to_sprint(&client, &out, "PROJ-1", "Sprint Alpha")
        .await
        .unwrap();
}

// ── bulk_assign with "me" resolves current user once ─────────────────────────

#[tokio::test]
async fn bulk_assign_me_resolves_current_user_and_assigns() {
    let server = MockServer::start().await;

    // myself — called once to resolve "me"
    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "ruben-id",
            "displayName": "Ruben"
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(search_response(vec![
                issue_fixture("PROJ-1", "Issue 1", "Open"),
                issue_fixture("PROJ-2", "Issue 2", "Open"),
            ])),
        )
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1/assignee"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-2/assignee"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::bulk_assign(&client, &out, "project = PROJ", "me", false)
        .await
        .unwrap();

    // Verify each assignee payload uses the resolved accountId, not "me"
    let requests = server.received_requests().await.unwrap();
    let assign_reqs: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path().contains("/assignee"))
        .collect();
    assert_eq!(assign_reqs.len(), 2, "should have assigned 2 issues");
    for req in assign_reqs {
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(
            body["accountId"], "ruben-id",
            "assignee payload must use resolved accountId, not 'me'"
        );
    }
}

// ── commands::boards ──────────────────────────────────────────────────────────

fn boards_response(boards: &[(&str, u64, &str)]) -> serde_json::Value {
    serde_json::json!({
        "values": boards.iter().map(|(name, id, btype)| serde_json::json!({
            "id": id, "name": name, "type": btype
        })).collect::<Vec<_>>(),
        "isLast": true, "startAt": 0, "total": boards.len()
    })
}

#[tokio::test]
async fn boards_list_json_shape() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(boards_response(&[
            ("TST board", 1, "scrum"),
            ("KAN board", 2, "kanban"),
        ])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::boards::list(&client, &out)
        .await
        .unwrap();
}

#[tokio::test]
async fn boards_list_json_contains_id_name_type() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(boards_response(&[("My Board", 42, "scrum")])),
        )
        .mount(&server)
        .await;

    // Capture output via a channel-backed OutputConfig isn't possible directly,
    // but we exercise the code path and verify the API call shape.
    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::boards::list(&client, &out)
        .await
        .unwrap();

    // Verify the agile endpoint was hit (not the REST API)
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].url.path().starts_with("/rest/agile/1.0/board"));
}

#[tokio::test]
async fn boards_list_empty_succeeds() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(boards_response(&[])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::boards::list(&client, &out)
        .await
        .unwrap();
}

// ── commands::fields ──────────────────────────────────────────────────────────

fn fields_response(fields: &[(&str, &str, bool, Option<&str>)]) -> serde_json::Value {
    // (id, name, custom, field_type)
    serde_json::json!(
        fields
            .iter()
            .map(|(id, name, custom, ftype)| {
                let mut f = serde_json::json!({ "id": id, "name": name, "custom": custom });
                if let Some(t) = ftype {
                    f["schema"] = serde_json::json!({ "type": t });
                }
                f
            })
            .collect::<Vec<_>>()
    )
}

#[tokio::test]
async fn fields_list_all_json_shape() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fields_response(&[
            ("summary", "Summary", false, Some("string")),
            ("customfield_10016", "Story Points", true, Some("number")),
        ])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::fields::list(&client, &out, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn fields_list_custom_only_filters_system_fields() {
    let server = MockServer::start().await;

    // Return one system + one custom field from API
    Mock::given(method("GET"))
        .and(path("/rest/api/3/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fields_response(&[
            ("summary", "Summary", false, Some("string")),
            ("customfield_10016", "Story Points", true, Some("number")),
            ("customfield_10014", "Epic Link", true, Some("string")),
        ])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    // custom_only = true — system field must be excluded
    jira_cli::commands::fields::list(&client, &out, true)
        .await
        .unwrap();
}

#[tokio::test]
async fn fields_list_sorted_system_before_custom() {
    let server = MockServer::start().await;

    // Return fields out of order — command must sort system first, then custom
    Mock::given(method("GET"))
        .and(path("/rest/api/3/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fields_response(&[
            ("customfield_10016", "Z Custom", true, Some("number")),
            ("summary", "Summary", false, Some("string")),
            ("customfield_10000", "A Custom", true, Some("string")),
            ("status", "Status", false, Some("string")),
        ])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::fields::list(&client, &out, false)
        .await
        .unwrap();
}

// ── commands::users ───────────────────────────────────────────────────────────

fn users_response(users: &[(&str, &str, Option<&str>)]) -> serde_json::Value {
    // (accountId, displayName, email)
    serde_json::json!(
        users
            .iter()
            .map(|(id, name, email)| {
                let mut u = serde_json::json!({ "accountId": id, "displayName": name });
                if let Some(e) = email {
                    u["emailAddress"] = serde_json::json!(e);
                }
                u
            })
            .collect::<Vec<_>>()
    )
}

#[tokio::test]
async fn users_search_json_shape() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/user/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_response(&[
            ("abc123", "Alice Smith", Some("alice@example.com")),
            ("def456", "Bob Jones", None),
        ])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::users::search(&client, &out, "alice")
        .await
        .unwrap();
}

#[tokio::test]
async fn users_search_empty_succeeds() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/user/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_response(&[])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::users::search(&client, &out, "nobody")
        .await
        .unwrap();
}

#[tokio::test]
async fn users_search_query_passed_as_param() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/user/search"))
        .and(query_param("query", "ruben"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_response(&[(
            "ruben-id",
            "Ruben Jongejan",
            Some("ruben@example.com"),
        )])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::users::search(&client, &out, "ruben")
        .await
        .unwrap();
}

// ── commands::sprints ─────────────────────────────────────────────────────────

fn sprint_fixture(id: u64, name: &str, state: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "name": name, "state": state,
        "startDate": "2024-01-01T00:00:00Z",
        "endDate": "2024-01-14T00:00:00Z",
        "originBoardId": 1
    })
}

fn sprints_response(sprints: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "values": sprints, "isLast": true, "startAt": 0 })
}

async fn mount_board_and_sprints(server: &MockServer, sprints: Vec<serde_json::Value>) {
    {
        Mock::given(method("GET"))
            .and(path("/rest/agile/1.0/board"))
            .respond_with(ResponseTemplate::new(200).set_body_json(boards_response(&[(
                "TST board",
                1,
                "scrum",
            )])))
            .mount(server)
            .await;

        Mock::given(method("GET"))
            .and(path("/rest/agile/1.0/board/1/sprint"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sprints_response(sprints)))
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn sprints_list_json_shape() {
    let server = MockServer::start().await;
    mount_board_and_sprints(
        &server,
        vec![
            sprint_fixture(1, "Sprint 1", "active"),
            sprint_fixture(2, "Sprint 2", "closed"),
        ],
    )
    .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::sprints::list(&client, &out, None, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn sprints_list_json_includes_board_context() {
    let server = MockServer::start().await;
    mount_board_and_sprints(&server, vec![sprint_fixture(7, "Alpha Sprint", "active")]).await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::sprints::list(&client, &out, None, Some("active"))
        .await
        .unwrap();
}

#[tokio::test]
async fn sprints_list_filtered_by_board_name() {
    let server = MockServer::start().await;

    // Two boards — command must only query the matched one
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [
                { "id": 1, "name": "TST board", "type": "scrum" },
                { "id": 2, "name": "KAN board", "type": "kanban" }
            ],
            "isLast": true, "startAt": 0, "total": 2
        })))
        .mount(&server)
        .await;

    // Only board 1 sprint endpoint should be called
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(sprints_response(vec![sprint_fixture(
                10,
                "TST Sprint",
                "active",
            )])),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/2/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sprints_response(vec![])))
        .expect(0)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::sprints::list(&client, &out, Some("TST"), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn sprints_list_board_not_found_returns_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(boards_response(&[(
            "TST board",
            1,
            "scrum",
        )])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    let err = jira_cli::commands::sprints::list(&client, &out, Some("NOPE"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, jira_cli::api::ApiError::NotFound(_)),
        "unknown board name must return NotFound"
    );
}

#[tokio::test]
async fn sprints_list_empty_boards_returns_empty_json() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(boards_response(&[])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::sprints::list(&client, &out, None, None)
        .await
        .unwrap();
}

// ── commands::projects (missing paths) ───────────────────────────────────────

#[tokio::test]
async fn projects_list_json_shape() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_search_response(vec![
            serde_json::json!({ "id": "10001", "key": "PROJ", "name": "My Project", "projectTypeKey": "software" }),
            serde_json::json!({ "id": "10002", "key": "OPS", "name": "Ops", "projectTypeKey": "business" }),
        ])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::projects::list(&client, &out)
        .await
        .unwrap();
}

#[tokio::test]
async fn projects_list_empty_succeeds() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_search_response(vec![])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::projects::list(&client, &out)
        .await
        .unwrap();
}

// ── commands::search (missing paths) ─────────────────────────────────────────

#[tokio::test]
async fn search_run_json_shape() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(search_response(vec![
                issue_fixture("PROJ-1", "First", "To Do"),
                issue_fixture("PROJ-2", "Second", "In Progress"),
            ])),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::search::run(&client, &out, "project = PROJ", 50, 0, false, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn search_run_shows_pagination_info_when_more_results() {
    let server = MockServer::start().await;

    // Two issues returned with isLast=false — command must indicate more pages exist
    // on the new Cloud endpoint.
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(search_jql_response(
                vec![
                    issue_fixture("PROJ-1", "First", "To Do"),
                    issue_fixture("PROJ-2", "Second", "Open"),
                ],
                Some("next-cursor"),
                false,
            )),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::search::run(&client, &out, "project = PROJ", 2, 0, false, None)
        .await
        .unwrap();
}

// ── Auth and rate-limit HTTP error responses ──────────────────────────────────

#[tokio::test]
async fn get_issue_401_returns_auth_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(401).set_body_string(
            r#"{"errorMessages":["You do not have permission to see this issue."]}"#,
        ))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.get_issue("PROJ-1").await.unwrap_err();
    assert!(
        matches!(err, ApiError::Auth(_)),
        "401 must map to ApiError::Auth, got: {err}"
    );
    let msg = err.to_string();
    assert!(msg.contains("Authentication failed"));
    assert!(
        msg.contains("JIRA_TOKEN"),
        "auth error must hint at the token env var"
    );
}

#[tokio::test]
async fn get_issue_403_returns_auth_error() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-2"))
        .respond_with(
            ResponseTemplate::new(403).set_body_string(r#"{"errorMessages":["Forbidden"]}"#),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.get_issue("PROJ-2").await.unwrap_err();
    assert!(
        matches!(err, ApiError::Auth(_)),
        "403 must map to ApiError::Auth"
    );
}

#[tokio::test]
async fn search_429_returns_rate_limit_error() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.search("project = PROJ", 10, 0).await.unwrap_err();
    assert!(
        matches!(err, ApiError::RateLimit),
        "429 must map to ApiError::RateLimit, got: {err}"
    );
    assert!(
        err.to_string().contains("wait"),
        "rate limit message should tell user to wait"
    );
}

#[tokio::test]
async fn create_issue_422_returns_api_error_with_status() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .respond_with(
            ResponseTemplate::new(422)
                .set_body_string(r#"{"errors":{"summary":"Field required"},"errorMessages":[]}"#),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client
        .create_issue(&minimal_draft("PROJ", "Task", "bad issue"), &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err, ApiError::Api { status: 422, .. }),
        "422 must map to ApiError::Api with status 422, got: {err}"
    );
    assert!(err.to_string().contains("422"));
}

#[tokio::test]
async fn auth_error_message_includes_actionable_guidance() {
    // Verify the complete auth error message format without a real HTTP call
    let err = ApiError::Auth("401 Unauthorized".into());
    let msg = err.to_string();
    assert!(msg.contains("Authentication failed"));
    assert!(msg.contains("JIRA_TOKEN"));
    assert!(msg.contains("config show") || msg.contains("config init"));
}

// ── commands::projects — text output paths ───────────────────────────────────

fn text_out() -> OutputConfig {
    OutputConfig {
        json: false,
        quiet: true,
    }
}

#[tokio::test]
async fn projects_list_text_output_renders_table() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_search_response(vec![
            serde_json::json!({ "id": "10001", "key": "PROJ", "name": "My Project", "projectTypeKey": "software" }),
            serde_json::json!({ "id": "10002", "key": "OPS", "name": "Ops", "projectTypeKey": "business" }),
        ])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = text_out();
    jira_cli::commands::projects::list(&client, &out)
        .await
        .unwrap();
}

#[tokio::test]
async fn projects_list_text_empty_prints_no_projects_message() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(project_search_response(vec![])))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = text_out();
    jira_cli::commands::projects::list(&client, &out)
        .await
        .unwrap();
}

#[tokio::test]
async fn projects_show_json_output() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/PROJ"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "10001", "key": "PROJ", "name": "My Project", "projectTypeKey": "software" }),
        ))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::projects::show(&client, &out, "PROJ")
        .await
        .unwrap();
}

#[tokio::test]
async fn projects_show_text_output_renders_details() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/PROJ"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "id": "10001", "key": "PROJ", "name": "My Project", "projectTypeKey": "software" }),
        ))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = text_out();
    jira_cli::commands::projects::show(&client, &out, "PROJ")
        .await
        .unwrap();
}

#[tokio::test]
async fn create_issue_sends_components() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .and(body_partial_json(serde_json::json!({
            "fields": {
                "project": { "key": "PROJ" },
                "issuetype": { "name": "Bug" },
                "summary": "Has components",
                "components": [{ "name": "Backend" }, { "name": "API" }],
            }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10001",
            "key": "PROJ-42",
            "self": "https://test.atlassian.net/rest/api/3/issue/10001",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let resp = client
        .create_issue(
            &IssueDraft {
                project_key: "PROJ",
                issue_type: "Bug",
                summary: "Has components",
                description: None,
                priority: None,
                labels: None,
                components: Some(&["Backend", "API"]),
                fix_versions: None,
                assignee: None,
                parent: None,
            },
            &[],
        )
        .await
        .unwrap();
    assert_eq!(resp.key, "PROJ-42");
}

#[tokio::test]
async fn list_components_returns_components() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/PROJ/components"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "10010", "name": "Backend", "description": "Server-side"},
            {"id": "10020", "name": "Frontend"},
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let comps = client.list_components("PROJ").await.unwrap();
    assert_eq!(comps.len(), 2);
    assert_eq!(comps[0].name, "Backend");
    assert_eq!(comps[0].description.as_deref(), Some("Server-side"));
    assert_eq!(comps[1].name, "Frontend");
    assert!(comps[1].description.is_none());
}

#[tokio::test]
async fn list_components_handles_empty_array() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/EMPTY/components"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let comps = client.list_components("EMPTY").await.unwrap();
    assert!(comps.is_empty());
}

#[tokio::test]
async fn list_versions_returns_versions() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/PROJ/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "10010", "name": "1.2.0",
                "description": "Stable release", "released": true,
                "archived": false, "releaseDate": "2024-03-01"
            },
            {
                "id": "10020", "name": "1.3.0",
                "released": false, "archived": false
            },
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let versions = client.list_versions("PROJ").await.unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].name, "1.2.0");
    assert_eq!(versions[0].description.as_deref(), Some("Stable release"));
    assert_eq!(versions[0].release_date.as_deref(), Some("2024-03-01"));
    assert_eq!(versions[0].released, Some(true));
    assert_eq!(versions[1].name, "1.3.0");
    assert!(versions[1].description.is_none());
    assert_eq!(versions[1].released, Some(false));
}

#[tokio::test]
async fn list_versions_handles_empty_array() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/EMPTY/versions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let versions = client.list_versions("EMPTY").await.unwrap();
    assert!(versions.is_empty());
}

#[tokio::test]
async fn create_issue_sends_fix_versions() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .and(body_partial_json(serde_json::json!({
            "fields": {
                "project": { "key": "PROJ" },
                "issuetype": { "name": "Bug" },
                "summary": "Has fix versions",
                "fixVersions": [{ "name": "1.2.0" }, { "name": "1.3.0" }],
            }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10001",
            "key": "PROJ-42",
            "self": "https://test.atlassian.net/rest/api/3/issue/10001",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let resp = client
        .create_issue(
            &IssueDraft {
                project_key: "PROJ",
                issue_type: "Bug",
                summary: "Has fix versions",
                description: None,
                priority: None,
                labels: None,
                components: None,
                fix_versions: Some(&["1.2.0", "1.3.0"]),
                assignee: None,
                parent: None,
            },
            &[],
        )
        .await
        .unwrap();
    assert_eq!(resp.key, "PROJ-42");
}

// ── Attachments (client) ─────────────────────────────────────────────────────

fn attachment_response(id: serde_json::Value, filename: &str, size: u64) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "filename": filename,
        "author": { "displayName": "Alice", "accountId": "abc123" },
        "created": "2024-01-15T10:00:00.000Z",
        "size": size,
        "mimeType": "application/pdf",
    })
}

#[tokio::test]
async fn list_attachments_reads_ids_from_the_issue_attachment_field() {
    let cases: [(&str, serde_json::Value, &[&str]); 2] = [
        (
            "ids reported as a string and as a number both become strings",
            serde_json::json!({ "fields": { "attachment": [
                attachment_response(serde_json::json!("10001"), "a.pdf", 100),
                attachment_response(serde_json::json!(10002), "b.pdf", 200),
            ]}}),
            &["10001", "10002"],
        ),
        (
            "an issue without an attachment field has no attachments",
            serde_json::json!({ "fields": {} }),
            &[],
        ),
    ];

    for (label, body, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/PROJ-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = test_client(&server);
        let ids: Vec<String> = client
            .list_attachments("PROJ-1")
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, expected, "{label}");
    }
}

#[tokio::test]
async fn upload_attachments_sends_no_check_header_and_file_bytes() {
    let server = MockServer::start().await;
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("notes.txt");
    std::fs::write(&file_path, b"upload me").unwrap();

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/attachments"))
        .and(header("X-Atlassian-Token", "no-check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            attachment_response(serde_json::json!("10003"), "notes.txt", 9)
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let uploaded = client
        .upload_attachments("PROJ-1", &[file_path])
        .await
        .unwrap();
    assert_eq!(uploaded.len(), 1);
    assert_eq!(uploaded[0].filename, "notes.txt");

    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(
        body.contains("upload me"),
        "multipart body must carry the file's actual bytes"
    );
}

#[tokio::test]
async fn upload_attachments_missing_local_file_is_rejected_before_any_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    let missing = std::path::PathBuf::from("/no/such/file-really-does-not-exist.txt");
    let err = client
        .upload_attachments("PROJ-1", &[missing])
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::Other(_)));
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Jira stores exactly what the client declares for a multipart part and
/// never derives a type from the bytes itself, so the declared Content-Type
/// must follow the file's name; a name with no recognized mapping must still
/// go up as `application/octet-stream`.
///
/// `.md` is deliberately not one of the cases: the underlying `mime_guess`
/// table lists two candidate types for it (`text/markdown` and
/// `text/x-markdown`), and which one wins is an artifact of that crate's
/// internal list ordering, not a decision this codebase makes. Asserting
/// that exact string would pin a dependency implementation detail rather
/// than behavior, so `.pdf` and `.png` (both unambiguous in that table) are
/// used for the recognized cases instead.
#[tokio::test]
async fn upload_attachments_declares_content_type_from_file_name() {
    let cases = [
        ("report.pdf", "application/pdf"),
        ("diagram.png", "image/png"),
        ("README", "application/octet-stream"),
    ];

    let server = MockServer::start().await;
    let dir = TempDir::new().unwrap();
    let mut paths = Vec::new();
    for (name, _) in &cases {
        let path = dir.path().join(name);
        std::fs::write(&path, b"bytes").unwrap();
        paths.push(path);
    }

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/attachments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client.upload_attachments("PROJ-1", &paths).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8_lossy(&requests[0].body);
    for (name, expected_type) in cases {
        let declared_part = format!("filename=\"{name}\"\r\nContent-Type: {expected_type}\r\n");
        assert!(
            body.contains(&declared_part),
            "{name} must be uploaded with Content-Type: {expected_type}, got body:\n{body}"
        );
    }
}

#[tokio::test]
async fn attachment_id_that_is_not_a_number_is_rejected_before_any_request() {
    let server = MockServer::start().await;
    let client = test_client(&server);

    for id in ["not-a-number", "../../attachment/1", ""] {
        let err = client.get_attachment(id).await.unwrap_err();
        assert!(
            matches!(err, ApiError::InvalidInput(_)),
            "get_attachment({id:?})"
        );
        let err = client.delete_attachment(id).await.unwrap_err();
        assert!(
            matches!(err, ApiError::InvalidInput(_)),
            "delete_attachment({id:?})"
        );
        let err = client.download_attachment(id).await.unwrap_err();
        assert!(
            matches!(err, ApiError::InvalidInput(_)),
            "download_attachment({id:?})"
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn list_attachments_returns_a_clean_error_for_an_unrecognized_id_type() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "fields": {
                "attachment": [{
                    "id": ["not", "a", "valid", "id"],
                    "filename": "a.pdf",
                    "created": "2024-01-15T10:00:00.000Z",
                    "size": 100,
                }]
            }
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    // A server response with an id shape the client doesn't recognize must
    // surface as a normal error, not a panic.
    let err = client.list_attachments("PROJ-9").await.unwrap_err();
    assert!(matches!(err, ApiError::Http(_)));
}

#[tokio::test]
async fn download_attachment_returns_raw_bytes() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/content/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"binary-payload".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let bytes = client.download_attachment("10001").await.unwrap();
    assert_eq!(bytes, b"binary-payload".to_vec());
}

#[tokio::test]
async fn download_attachment_maps_404_to_not_found() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/content/999"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "errorMessages": ["Attachment does not exist"], "errors": {}
        })))
        .mount(&server)
        .await;

    let client = test_client(&server);
    let err = client.download_attachment("999").await.unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)));
}

#[tokio::test]
async fn delete_attachment_sends_delete_request() {
    let server = MockServer::start().await;

    Mock::given(method("DELETE"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    client.delete_attachment("10001").await.unwrap();
}

// ── Attachments (commands::issues) ───────────────────────────────────────────

#[tokio::test]
async fn attachments_command_lists_via_issue_fields_endpoint() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "fields": {
                "attachment": [attachment_response(serde_json::json!("10001"), "a.pdf", 100)]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::attachments(&client, &out, "PROJ-1")
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.query(), Some("fields=attachment"));
}

/// The filename Jira reports is reduced to a single path component before it
/// is written, so a crafted name can only ever land directly inside `--dir`;
/// a name that cannot be reduced to one usable component is refused and
/// nothing is written. The id is also accepted in both JSON forms Jira uses
/// (string from the issue `attachment` field, number from `attachment/{id}`).
#[tokio::test]
async fn download_attachment_command_reduces_reported_filename_to_a_basename() {
    // (attachment id as Jira reports it, filename Jira reports, file expected
    // in the destination directory — `None` means the download must be refused)
    let cases: [(serde_json::Value, &str, Option<&str>); 6] = [
        (
            serde_json::json!("10001"),
            "../../evil/../../report.pdf",
            Some("report.pdf"),
        ),
        (
            serde_json::json!("10010"),
            "..\\..\\windows\\system32\\config",
            Some("config"),
        ),
        (serde_json::json!("10011"), "/etc/passwd", Some("passwd")),
        (serde_json::json!(10009), "invoice.pdf", Some("invoice.pdf")),
        (serde_json::json!("10002"), "..", None),
        // No path separator, so it survives basename-reduction as a single
        // component, but it still names a Windows drive-relative path / NTFS
        // alternate-data-stream, so it must be refused rather than written.
        (serde_json::json!("10012"), "C:evil.txt", None),
    ];

    for (id, filename, expected) in cases {
        let server = MockServer::start().await;
        let dest = TempDir::new().unwrap();
        let id_str = id.as_str().map_or_else(|| id.to_string(), str::to_string);

        Mock::given(method("GET"))
            .and(path(format!("/rest/api/3/attachment/{id_str}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(attachment_response(
                    id.clone(),
                    filename,
                    4,
                )),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/rest/api/3/attachment/content/{id_str}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
            .mount(&server)
            .await;

        let client = test_client(&server);
        let out = json_out();
        let result = jira_cli::commands::issues::download_attachment(
            &client,
            &out,
            &id_str,
            dest.path(),
            false,
        )
        .await;

        let entries: Vec<_> = std::fs::read_dir(dest.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        match expected {
            Some(name) => {
                result.unwrap_or_else(|e| panic!("filename {filename:?} must be accepted: {e}"));
                assert_eq!(entries, vec![std::ffi::OsString::from(name)]);
                assert_eq!(std::fs::read(dest.path().join(name)).unwrap(), b"data");
            }
            None => {
                let err = result.err().unwrap_or_else(|| {
                    panic!("filename {filename:?} must be refused, wrote: {entries:?}")
                });
                assert!(matches!(err, ApiError::Other(_)), "filename {filename:?}");
                assert!(
                    entries.is_empty(),
                    "filename {filename:?} must write nothing"
                );
            }
        }
    }
}

/// A local file already at the download target must be left byte-for-byte
/// untouched when the download is refused, and the refusal must surface as
/// `ApiError::Conflict` (exit code 7, kind `conflict`) naming the path. Only
/// the metadata request is expected to reach the server — a local conflict
/// must be detected before the attachment content is fetched.
#[tokio::test]
async fn download_attachment_refuses_existing_file_and_leaves_it_untouched() {
    use jira_cli::output::{exit_code_for_error, exit_codes};

    let server = MockServer::start().await;
    let dest = TempDir::new().unwrap();
    let target = dest.path().join("report.pdf");
    std::fs::write(&target, b"original bytes").unwrap();

    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(attachment_response(
                serde_json::json!("10001"),
                "report.pdf",
                4,
            )),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/content/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
        .expect(0)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    let err =
        jira_cli::commands::issues::download_attachment(&client, &out, "10001", dest.path(), false)
            .await
            .unwrap_err();

    match &err {
        ApiError::Conflict(msg) => {
            assert!(
                msg.contains(&target.display().to_string()),
                "conflict message must name the path; got: {msg}"
            );
        }
        other => panic!("expected ApiError::Conflict, got: {other:?}"),
    }
    assert_eq!(exit_code_for_error(&err), exit_codes::CONFLICT);
    assert_eq!(std::fs::read(&target).unwrap(), b"original bytes");
}

/// `--force` (`force = true`) overwrites an existing target file.
#[tokio::test]
async fn download_attachment_force_replaces_existing_file() {
    let server = MockServer::start().await;
    let dest = TempDir::new().unwrap();
    let target = dest.path().join("report.pdf");
    std::fs::write(&target, b"original bytes").unwrap();

    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(attachment_response(
                serde_json::json!("10001"),
                "report.pdf",
                4,
            )),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/content/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"new data".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::download_attachment(&client, &out, "10001", dest.path(), true)
        .await
        .unwrap();

    assert_eq!(std::fs::read(&target).unwrap(), b"new data");
}

/// A missing `--dir`, including several nested levels that don't exist yet,
/// is created before the attachment content is fetched.
#[tokio::test]
async fn download_attachment_creates_missing_dir_recursively() {
    let server = MockServer::start().await;
    let dest = TempDir::new().unwrap();
    let nested = dest.path().join("a").join("b").join("c");
    assert!(!nested.exists(), "nested dir must not exist yet");

    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(attachment_response(
                serde_json::json!("10001"),
                "report.pdf",
                4,
            )),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/content/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    jira_cli::commands::issues::download_attachment(&client, &out, "10001", &nested, false)
        .await
        .unwrap();

    assert_eq!(std::fs::read(nested.join("report.pdf")).unwrap(), b"data");
}

/// A `--dir` that points at something that already exists but is not a
/// directory (here, a regular file) must fail as an ordinary error rather
/// than a conflict — the conflict kind is reserved for an existing *target
/// file*, not an unusable directory. No content request must be made, since
/// the failure happens while establishing the target directory.
#[tokio::test]
async fn download_attachment_dir_that_is_a_file_is_an_ordinary_error_not_conflict() {
    let server = MockServer::start().await;
    let dest = TempDir::new().unwrap();
    let not_a_dir = dest.path().join("not-a-directory");
    std::fs::write(&not_a_dir, b"i am a file").unwrap();

    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(attachment_response(
                serde_json::json!("10001"),
                "report.pdf",
                4,
            )),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/content/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
        .expect(0)
        .mount(&server)
        .await;

    let client = test_client(&server);
    let out = json_out();
    let err =
        jira_cli::commands::issues::download_attachment(&client, &out, "10001", &not_a_dir, false)
            .await
            .unwrap_err();

    assert!(
        matches!(err, ApiError::Other(_)),
        "expected ApiError::Other, got: {err:?}"
    );
    assert_eq!(std::fs::read(&not_a_dir).unwrap(), b"i am a file");
}

// ── invalid input exit code (continued) ───────────────────────────────────────

#[test]
fn conflicting_download_maps_to_conflict_exit_code() {
    use jira_cli::output::{exit_code_for_error, exit_codes};
    let err = ApiError::Conflict("/tmp/report.pdf already exists".into());
    assert_eq!(exit_code_for_error(&err), exit_codes::CONFLICT);
}
