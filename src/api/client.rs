use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::ApiError;
use super::AuthType;
use super::types::*;

mod hierarchy;
mod metadata;
mod sprints;

fn parse_issue_sprints(
    extra: &serde_json::Map<String, serde_json::Value>,
    field_ids: &[String],
) -> Vec<IssueSprint> {
    let mut found = BTreeMap::new();
    for field_id in field_ids {
        let Some(values) = extra.get(field_id) else {
            continue;
        };
        let values = values
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_else(|| std::slice::from_ref(values));
        for value in values {
            let parsed = if let Some(object) = value.as_object() {
                let id = object
                    .get("id")
                    .and_then(|id| id.as_u64().or_else(|| id.as_str()?.parse().ok()));
                let name = object.get("name").and_then(|s| s.as_str());
                let state = object.get("state").and_then(|s| s.as_str());
                id.zip(name)
                    .zip(state)
                    .map(|((id, name), state)| IssueSprint {
                        id,
                        name: name.to_owned(),
                        state: state.to_ascii_lowercase(),
                    })
            } else if let Some(raw) = value.as_str() {
                let id = legacy_sprint_property(raw, "id=").and_then(|s| s.parse().ok());
                let name = raw.split_once("name=").map(|(_, tail)| {
                    [
                        ",goal=",
                        ",startDate=",
                        ",endDate=",
                        ",completeDate=",
                        ",sequence=",
                        "]",
                    ]
                    .iter()
                    .filter_map(|marker| tail.find(marker))
                    .min()
                    .map_or(tail, |end| &tail[..end])
                });
                let state = legacy_sprint_property(raw, "state=");
                id.zip(name)
                    .zip(state)
                    .map(|((id, name), state)| IssueSprint {
                        id,
                        name: name.to_owned(),
                        state: state.to_ascii_lowercase(),
                    })
            } else {
                None
            };
            if let Some(sprint) = parsed {
                found.insert(sprint.id, sprint);
            }
        }
    }
    found.into_values().collect()
}

fn legacy_sprint_property<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    raw.split_once(key)
        .map(|(_, tail)| tail.split([',', ']']).next().unwrap_or(tail).trim())
}

pub struct JiraClient {
    http: reqwest::Client,
    base_url: String,
    agile_base_url: String,
    site_url: String,
    host: String,
    api_version: u8,
    read_only: bool,
    /// How to replace a rejected token, attached to every 401; see
    /// `with_auth_remedy`.
    auth_remedy: Option<super::AuthRemedy>,
    /// Whether issue reads fill in `Issue::epic`; see `enable_epic_lookup`.
    epic_lookup: std::sync::atomic::AtomicBool,
    /// Data Center Epic Link field IDs, resolved at most once per client.
    epic_link_fields: tokio::sync::OnceCell<Vec<String>>,
}

const SEARCH_FIELDS: [&str; 8] = [
    "summary",
    "status",
    "assignee",
    "priority",
    "issuetype",
    "parent",
    "created",
    "updated",
];
const ISSUE_DETAIL_FIELDS: [&str; 16] = [
    "summary",
    "status",
    "assignee",
    "reporter",
    "priority",
    "issuetype",
    "parent",
    "description",
    "labels",
    "components",
    "fixVersions",
    "versions",
    "created",
    "updated",
    "comment",
    "issuelinks",
];
const SEARCH_GET_JQL_LIMIT: usize = 1500;

/// Max issues per page the Jira Cloud `/search/jql` endpoint will return when
/// any non-ID fields are requested. The server silently caps larger values,
/// so we paginate internally to fulfil larger caller-requested limits.
const SEARCH_JQL_MAX_PAGE: usize = 100;

/// Page size used when walking the cursor forward to simulate an offset on
/// Jira Cloud. Requests only `id` to stay cheap (allows up to 5000/page).
const SEARCH_JQL_SKIP_PAGE: usize = 1000;

/// Build a JSON array of name/ID options for components and versions.
fn named_option_array(items: &[&str]) -> serde_json::Value {
    serde_json::Value::Array(
        items
            .iter()
            .map(|name| metadata::unresolved_option(name))
            .collect(),
    )
}

impl JiraClient {
    pub fn new(
        host: &str,
        email: &str,
        token: &str,
        auth_type: AuthType,
        api_version: u8,
    ) -> Result<Self, ApiError> {
        Self::new_with_cloud(host, email, token, auth_type, api_version, None, "classic")
    }

    pub fn new_with_cloud(
        host: &str,
        email: &str,
        token: &str,
        auth_type: AuthType,
        api_version: u8,
        cloud_id: Option<&str>,
        token_kind: &str,
    ) -> Result<Self, ApiError> {
        // Determine the scheme. An explicit `http://` prefix is preserved as-is
        // (useful for local testing); everything else defaults to HTTPS.
        let (scheme, domain) = if host.starts_with("http://") {
            (
                "http",
                host.trim_start_matches("http://").trim_end_matches('/'),
            )
        } else {
            (
                "https",
                host.trim_start_matches("https://").trim_end_matches('/'),
            )
        };

        if domain.is_empty() {
            return Err(ApiError::Other("Host cannot be empty".into()));
        }

        let auth_value = match auth_type {
            AuthType::Basic => {
                let credentials = BASE64.encode(format!("{email}:{token}"));
                format!("Basic {credentials}")
            }
            AuthType::Pat => format!("Bearer {token}"),
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_value).map_err(|e| ApiError::Other(e.to_string()))?,
        );

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(ApiError::Http)?;

        let site_url = format!("{scheme}://{domain}");
        let api_origin = if token_kind == "scoped" {
            let cloud_id = cloud_id
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    ApiError::InvalidInput("scoped token requires a Jira Cloud ID".into())
                })?;
            format!("https://api.atlassian.com/ex/jira/{cloud_id}")
        } else {
            site_url.clone()
        };
        let base_url = format!("{api_origin}/rest/api/{api_version}");
        let agile_base_url = format!("{api_origin}/rest/agile/1.0");

        Ok(Self {
            http,
            base_url,
            agile_base_url,
            site_url,
            host: domain.to_string(),
            api_version,
            read_only: false,
            auth_remedy: None,
            epic_lookup: std::sync::atomic::AtomicBool::new(false),
            epic_link_fields: tokio::sync::OnceCell::new(),
        })
    }

    /// Advice every 401 from this client carries, naming where the token came
    /// from and how to replace it. Set once here so each reporting path, per-item
    /// bulk results included, publishes the same guidance.
    pub fn with_auth_remedy(mut self, remedy: super::AuthRemedy) -> Self {
        self.auth_remedy = Some(remedy);
        self
    }

    /// Enforce the CLI's read-only policy at every HTTP write boundary.
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    fn ensure_writable(&self) -> Result<(), ApiError> {
        if self.read_only {
            Err(ApiError::InvalidInput(
                "read-only mode is enabled; Jira writes are blocked".into(),
            ))
        } else {
            Ok(())
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn api_version(&self) -> u8 {
        self.api_version
    }

    pub fn browse_base_url(&self) -> &str {
        &self.site_url
    }

    pub fn browse_url(&self, issue_key: &str) -> String {
        format!("{}/browse/{issue_key}", self.browse_base_url())
    }

    fn map_status(&self, status: u16, body: String) -> ApiError {
        let message = summarize_error_body(status, &body);
        match status {
            401 => {
                let missing_scope = super::scopes::is_scope_mismatch(&body);
                let remedy = match (&self.auth_remedy, missing_scope) {
                    (Some(remedy), true) => Some(remedy.missing_scope.clone()),
                    (Some(remedy), false) => Some(remedy.rejected.clone()),
                    (None, true) => Some(super::DEFAULT_MISSING_SCOPE_REMEDY.to_owned()),
                    (None, false) => None,
                };
                ApiError::Auth { message, remedy }
            }
            403 => ApiError::Forbidden(message),
            404 => ApiError::NotFound(message),
            409 => ApiError::Conflict(message),
            429 => ApiError::RateLimit,
            _ => ApiError::Api { status, message },
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let url = format!("{}/{path}", self.base_url);
        self.send(self.http.get(&url))
            .await?
            .json()
            .await
            .map_err(ApiError::Http)
    }

    async fn agile_get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let url = format!("{}/{path}", self.agile_base_url);
        self.send(self.http.get(&url))
            .await?
            .json()
            .await
            .map_err(ApiError::Http)
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, ApiError> {
        let url = format!("{}/{path}", self.base_url);
        self.send(self.http.post(&url).json(body))
            .await?
            .json()
            .await
            .map_err(ApiError::Http)
    }

    async fn post_empty_response(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let url = format!("{}/{path}", self.base_url);
        self.send(self.http.post(&url).json(body)).await?;
        Ok(())
    }

    async fn put_empty_response(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let url = format!("{}/{path}", self.base_url);
        self.send(self.http.put(&url).json(body)).await?;
        Ok(())
    }

    /// All requests pass through this boundary. Only the explicitly enumerated
    /// search POSTs are reads; other non-GET/HEAD requests require write access.
    async fn send(&self, builder: reqwest::RequestBuilder) -> Result<reqwest::Response, ApiError> {
        let request = builder.build()?;
        let method = request.method();
        let read_post = method == reqwest::Method::POST
            && ["search", "search/jql"].iter().any(|path| {
                // Compare canonical URLs: configured hosts may include an
                // uppercase name or a default port that reqwest normalizes.
                reqwest::Url::parse(&format!("{}/{path}", self.base_url))
                    .is_ok_and(|allowed| &allowed == request.url())
            });
        if !matches!(*method, reqwest::Method::GET | reqwest::Method::HEAD) && !read_post {
            self.ensure_writable()?;
        }
        let resp = self.http.execute(request).await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(self.map_status(status.as_u16(), body));
        }
        Ok(resp)
    }

    // ── Issues ────────────────────────────────────────────────────────────────

    /// Search issues using JQL.
    ///
    /// On API v2 (Jira Data Center / Server) this uses the classic
    /// `/rest/api/2/search` endpoint with offset-based pagination.
    ///
    /// On API v3 (Jira Cloud) this uses the replacement
    /// `/rest/api/3/search/jql` endpoint - the original `/search` was retired
    /// on 2025-10-31 and returns 410 Gone. The new endpoint only supports
    /// cursor-based pagination and does not return an exact total, so we
    /// simulate the `start_at` offset by walking the cursor forward.
    pub async fn search(
        &self,
        jql: &str,
        max_results: usize,
        start_at: usize,
    ) -> Result<SearchResponse, ApiError> {
        if self.api_version >= 3 {
            self.search_jql_v3(jql, max_results, start_at).await
        } else {
            self.search_v2(jql, max_results, start_at).await
        }
    }

    async fn search_v2(
        &self,
        jql: &str,
        max_results: usize,
        start_at: usize,
    ) -> Result<SearchResponse, ApiError> {
        let field_list = self.issue_fields(&SEARCH_FIELDS).await?;
        let fields = field_list.join(",");
        let encoded_jql = percent_encode(jql);
        // Every counter is optional because a response that omits one must stay
        // distinguishable from one reporting zero. Defaulting `total` to 0 made
        // `is_last` below true for any page, silently ending `--all` after the
        // first one while the JSON reported no results next to the results.
        #[derive(serde::Deserialize)]
        struct RawV2 {
            issues: Vec<Issue>,
            total: Option<usize>,
            #[serde(rename = "startAt")]
            start_at: Option<usize>,
            #[serde(rename = "maxResults")]
            max_results: Option<usize>,
        }
        let mut raw: RawV2 = if encoded_jql.len() <= SEARCH_GET_JQL_LIMIT {
            let path = format!(
                "search?jql={encoded_jql}&maxResults={max_results}&startAt={start_at}&fields={fields}"
            );
            self.get(&path).await?
        } else {
            self.post(
                "search",
                &serde_json::json!({
                    "jql": jql,
                    "maxResults": max_results,
                    "startAt": start_at,
                    "fields": field_list,
                }),
            )
            .await?
        };
        self.fill_epics(&mut raw.issues).await?;
        // An absent offset or page size is reported as what was asked for, which
        // is true of the request even when the server does not echo it back.
        let echoed_start_at = raw.start_at.unwrap_or(start_at);
        let is_last = match raw.total {
            Some(total) => echoed_start_at + raw.issues.len() >= total,
            // Without a total, a page shorter than the one requested is the only
            // reliable end-of-results signal.
            None => raw.issues.len() < max_results,
        };
        Ok(SearchResponse {
            issues: raw.issues,
            total: raw.total,
            start_at: echoed_start_at,
            max_results: raw.max_results.unwrap_or(max_results),
            is_last,
        })
    }

    /// Fetch a single page from the Jira Cloud `/search/jql` endpoint with
    /// the full field list populated on each issue.
    ///
    /// Always uses POST: it handles long JQL without URL-length limits and
    /// accepts `fields` as a JSON array (GET requires repeated query params).
    async fn search_jql_page(
        &self,
        jql: &str,
        page_size: usize,
        next_token: Option<&str>,
    ) -> Result<SearchJqlPage, ApiError> {
        let mut body = serde_json::json!({
            "jql": jql,
            "maxResults": page_size,
            "fields": SEARCH_FIELDS,
        });
        if let Some(t) = next_token {
            body["nextPageToken"] = serde_json::Value::String(t.to_string());
        }
        self.post("search/jql", &body).await
    }

    /// Fetch a `/search/jql` page requesting only the `id` field.
    ///
    /// Used to cheaply walk the cursor forward when simulating an offset.
    /// Issues in the response lack a `fields` sub-object, so they are
    /// deserialized as raw JSON values rather than full `Issue`s.
    async fn search_jql_skip_page(
        &self,
        jql: &str,
        page_size: usize,
        next_token: Option<&str>,
    ) -> Result<SearchJqlSkipPage, ApiError> {
        let mut body = serde_json::json!({
            "jql": jql,
            "maxResults": page_size,
            "fields": ["id"],
        });
        if let Some(t) = next_token {
            body["nextPageToken"] = serde_json::Value::String(t.to_string());
        }
        self.post("search/jql", &body).await
    }

    async fn search_jql_v3(
        &self,
        jql: &str,
        max_results: usize,
        start_at: usize,
    ) -> Result<SearchResponse, ApiError> {
        // Walk the cursor forward to simulate `start_at`. The `/search/jql`
        // endpoint only supports sequential cursor pagination, so arbitrary
        // offsets require fetching and discarding earlier pages. Request
        // `id`-only to keep skip-pages cheap.
        let mut next_token: Option<String> = None;
        let mut skipped = 0usize;
        while skipped < start_at {
            let want = (start_at - skipped).min(SEARCH_JQL_SKIP_PAGE);
            let page = self
                .search_jql_skip_page(jql, want, next_token.as_deref())
                .await?;
            let got = page.issues.len();
            skipped += got;
            if got == 0 || page.is_last {
                // Offset is past the end of the result set.
                return Ok(SearchResponse {
                    issues: Vec::new(),
                    total: None,
                    start_at,
                    max_results: 0,
                    is_last: true,
                });
            }
            next_token = page.next_page_token;
            if next_token.is_none() {
                // Server reported more pages but returned no cursor; treat as end
                // rather than silently restarting from page 0 on the next iteration.
                return Ok(SearchResponse {
                    issues: Vec::new(),
                    total: None,
                    start_at,
                    max_results: 0,
                    is_last: true,
                });
            }
        }

        // Collect up to `max_results` issues, paging internally to honour
        // the server's per-page cap when fields are requested.
        let mut collected: Vec<Issue> = Vec::new();
        let mut is_last = false;
        while collected.len() < max_results {
            let remaining = max_results - collected.len();
            let want = remaining.min(SEARCH_JQL_MAX_PAGE);
            let page = self
                .search_jql_page(jql, want, next_token.as_deref())
                .await?;
            let got = page.issues.len();
            collected.extend(page.issues);
            if page.is_last || got == 0 {
                is_last = true;
                break;
            }
            next_token = page.next_page_token;
            if next_token.is_none() {
                is_last = true;
                break;
            }
        }

        self.fill_epics(&mut collected).await?;
        let returned = collected.len();
        Ok(SearchResponse {
            issues: collected,
            // Cloud `/search/jql` does not return an exact total.
            total: None,
            start_at,
            max_results: returned,
            is_last,
        })
    }

    /// Fetch a single issue by key (e.g. `PROJ-123`), including all comments.
    ///
    /// Jira embeds only the first page of comments in the issue response. When
    /// the embedded page is incomplete, additional requests are made to fetch
    /// the remaining comments.
    pub async fn get_issue(&self, key: &str) -> Result<Issue, ApiError> {
        self.get_issue_with_sprint_fields(key, &[], true).await
    }

    /// Read sprint values for `issues show` without adding a field lookup to
    /// every issue read. Jira assigns the Sprint custom field a site-specific ID.
    pub async fn get_issue_with_sprints(&self, key: &str) -> Result<Issue, ApiError> {
        self.get_issue_with_sprints_diagnostic(key)
            .await
            .map(|(issue, _)| issue)
    }

    /// Like `get_issue_with_sprints`, but includes any field discovery warning.
    /// The CLI prints the warning so an empty sprint list is not misleading.
    pub async fn get_issue_with_sprints_diagnostic(
        &self,
        key: &str,
    ) -> Result<(Issue, Option<String>), ApiError> {
        validate_issue_key(key)?;
        const SPRINT_SCHEMA: &str = "com.pyxis.greenhopper.jira:gh-sprint";
        let (ids, warning): (Vec<String>, Option<String>) = match self.list_fields().await {
            Ok(fields) => (
                fields
                    .into_iter()
                    .filter(|f| {
                        f.schema
                            .as_ref()
                            .is_some_and(|s| s.custom.as_deref() == Some(SPRINT_SCHEMA))
                    })
                    .map(|f| f.id)
                    .collect(),
                None,
            ),
            Err(error) => (
                Vec::new(),
                Some(format!(
                    "Sprint{} data unavailable: field discovery failed: {error}",
                    if self.api_version < 3 {
                        " and epic"
                    } else {
                        ""
                    }
                )),
            ),
        };
        // Data Center uses the same catalog for Epic Link. Skip that lookup
        // for this read only when the catalog already failed.
        let include_epic_lookup = warning.is_none() || self.api_version >= 3;
        let issue = self
            .get_issue_with_sprint_fields(key, &ids, include_epic_lookup)
            .await?;
        Ok((issue, warning))
    }

    async fn get_issue_with_sprint_fields(
        &self,
        key: &str,
        sprint_fields: &[String],
        include_epic_lookup: bool,
    ) -> Result<Issue, ApiError> {
        validate_issue_key(key)?;
        let mut fields = if include_epic_lookup {
            self.issue_fields(&ISSUE_DETAIL_FIELDS).await?
        } else {
            ISSUE_DETAIL_FIELDS
                .iter()
                .map(|f| (*f).to_owned())
                .collect()
        };
        fields.extend(sprint_fields.iter().cloned());
        let fields = fields.join(",");
        let path = format!("issue/{key}?fields={fields}");
        let mut issue: Issue = self.get(&path).await?;
        if include_epic_lookup {
            self.fill_epics(std::slice::from_mut(&mut issue)).await?;
        }
        issue.sprints = parse_issue_sprints(&issue.fields.extra, sprint_fields);

        // Fetch remaining comment pages if the embedded page is incomplete
        if let Some(ref mut comment_list) = issue.fields.comment
            && comment_list.total > comment_list.comments.len()
        {
            let mut start_at = comment_list.comments.len();
            while comment_list.comments.len() < comment_list.total {
                let page: CommentList = self
                    .get(&format!(
                        "issue/{key}/comment?startAt={start_at}&maxResults=100"
                    ))
                    .await?;
                if page.comments.is_empty() {
                    break;
                }
                start_at += page.comments.len();
                comment_list.comments.extend(page.comments);
            }
        }

        Ok(issue)
    }

    /// Create a new issue using the same prepared fields as a preview.
    pub async fn create_issue(
        &self,
        draft: &IssueDraft<'_>,
        custom_fields: &[(String, serde_json::Value)],
    ) -> Result<CreateIssueResponse, ApiError> {
        let (fields, meta) = self.prepare_create_payload(draft, custom_fields).await?;
        self.post("issue", &serde_json::json!({ "fields": fields }))
            .await
            .map_err(|err| metadata::create_error(err, meta.as_ref(), draft))
    }

    /// Resolve and validate a create request without sending a write.
    pub async fn preview_create_issue(
        &self,
        draft: &IssueDraft<'_>,
        custom_fields: &[(String, serde_json::Value)],
    ) -> Result<serde_json::Value, ApiError> {
        let (fields, meta) = self.prepare_create_payload(draft, custom_fields).await?;
        if meta.is_none() {
            let project = fields["project"]["key"]
                .as_str()
                .or_else(|| fields["project"]["id"].as_str())
                .expect("validated project");
            self.get_project(project).await?;
        }
        Ok(write_preview(
            fields,
            Some(meta.is_some()),
            meta.as_ref().is_some_and(|m| m.fields.is_some()),
        ))
    }

    async fn prepare_create_payload(
        &self,
        draft: &IssueDraft<'_>,
        custom_fields: &[(String, serde_json::Value)],
    ) -> Result<(serde_json::Value, Option<metadata::CreateMetadata>), ApiError> {
        let mut fields = serde_json::json!({
            "project": { "key": draft.project_key },
            "issuetype": metadata::unresolved_option(draft.issue_type),
            "summary": draft.summary,
        });

        if let Some(desc) = draft.description {
            fields["description"] = self.make_body(desc);
        }
        if let Some(p) = draft.priority {
            fields["priority"] = metadata::unresolved_option(p);
        }
        if let Some(lbls) = draft.labels
            && !lbls.is_empty()
        {
            fields["labels"] = serde_json::json!(lbls);
        }
        if let Some(comps) = draft.components
            && !comps.is_empty()
        {
            fields["components"] = named_option_array(comps);
        }
        if let Some(fvs) = draft.fix_versions
            && !fvs.is_empty()
        {
            fields["fixVersions"] = named_option_array(fvs);
        }
        if let Some(assignee) = draft.assignee {
            fields["assignee"] = assignee
                .map(|id| self.assignee_payload(id))
                .unwrap_or(serde_json::Value::Null);
        }
        for (key, value) in custom_fields {
            fields[key] = value.clone();
        }
        let meta = self
            .prepare_create(&mut fields, draft, custom_fields)
            .await?;
        Ok((fields, meta))
    }

    /// Log work on an issue.
    ///
    /// `time_spent` uses Jira duration format (e.g. `2h 30m`, `1d`, `30m`).
    /// `started` is an ISO-8601 datetime string; when `None` the server uses now.
    pub async fn log_work(
        &self,
        key: &str,
        time_spent: &str,
        comment: Option<&str>,
        started: Option<&str>,
    ) -> Result<WorklogEntry, ApiError> {
        validate_issue_key(key)?;
        let mut payload = serde_json::json!({ "timeSpent": time_spent });
        if let Some(c) = comment {
            payload["comment"] = self.make_body(c);
        }
        if let Some(s) = started {
            payload["started"] = serde_json::Value::String(s.to_string());
        }
        self.post(&format!("issue/{key}/worklog"), &payload).await
    }

    /// Add a comment to an issue.
    pub async fn add_comment(&self, key: &str, body: &str) -> Result<Comment, ApiError> {
        validate_issue_key(key)?;
        let payload = serde_json::json!({ "body": self.make_body(body) });
        self.post(&format!("issue/{key}/comment"), &payload).await
    }

    /// List available transitions for an issue.
    pub async fn get_transitions(&self, key: &str) -> Result<Vec<Transition>, ApiError> {
        validate_issue_key(key)?;
        let resp: TransitionsResponse = self.get(&format!("issue/{key}/transitions")).await?;
        Ok(resp.transitions)
    }

    /// Execute a transition by transition ID.
    pub async fn do_transition(&self, key: &str, transition_id: &str) -> Result<(), ApiError> {
        validate_issue_key(key)?;
        let payload = serde_json::json!({ "transition": { "id": transition_id } });
        self.post_empty_response(&format!("issue/{key}/transitions"), &payload)
            .await
    }

    /// Assign an issue to a user, or unassign with `None`.
    ///
    /// API v3 (Jira Cloud) identifies users by `accountId`.
    /// API v2 (Jira Data Center / Server) identifies users by `name` (username).
    pub async fn assign_issue(&self, key: &str, account_id: Option<&str>) -> Result<(), ApiError> {
        validate_issue_key(key)?;
        let payload = match account_id {
            Some(id) => self.assignee_payload(id),
            None => {
                if self.api_version >= 3 {
                    serde_json::json!({ "accountId": null })
                } else {
                    serde_json::json!({ "name": null })
                }
            }
        };
        self.put_empty_response(&format!("issue/{key}/assignee"), &payload)
            .await
    }

    /// Build the assignee payload for the current API version.
    ///
    /// API v3 uses `accountId`; API v2 uses `name` (username).
    fn assignee_payload(&self, id: &str) -> serde_json::Value {
        if self.api_version >= 3 {
            serde_json::json!({ "accountId": id })
        } else {
            serde_json::json!({ "name": id })
        }
    }

    /// Get the currently authenticated user.
    pub async fn get_myself(&self) -> Result<Myself, ApiError> {
        self.get("myself").await
    }

    /// Update issue fields.
    ///
    /// All fields in `update` are optional. `components`, `fix_versions`, and `labels`
    /// are three-state: `None` leaves the field untouched, `Some(&[])` clears it,
    /// `Some(&[..])` replaces it. `assignee` is also three-state:
    /// `None` = untouched, `Some(None)` = unassign, `Some(Some(id))` = set.
    pub async fn update_issue(
        &self,
        key: &str,
        update: &IssueUpdate<'_>,
        custom_fields: &[(String, serde_json::Value)],
    ) -> Result<(), ApiError> {
        let (fields, meta) = self
            .prepare_update_payload(key, update, custom_fields)
            .await?;
        self.put_empty_response(
            &format!("issue/{key}"),
            &serde_json::json!({"fields": fields}),
        )
        .await
        .map_err(|err| {
            // A refused type change is explained as such, not as an epic
            // linking problem.
            let type_refused = update.issue_type.is_some()
                && matches!(&err, ApiError::Api { status: 400, message } if message.contains("issuetype"));
            if !type_refused {
                return metadata::write_error(err, meta.as_ref(), update.priority, update.epic);
            }
            match metadata::write_error(err, meta.as_ref(), update.priority, None) {
                ApiError::Api { status, mut message } => {
                    message.push_str("; Jira only changes an issue type in place when both types share a workflow and field configuration, otherwise use More > Move in the Jira web UI");
                    ApiError::Api { status, message }
                }
                err => err,
            }
        })
    }

    /// Resolve and validate an update request without sending a write.
    pub async fn preview_update_issue(
        &self,
        key: &str,
        update: &IssueUpdate<'_>,
        custom_fields: &[(String, serde_json::Value)],
    ) -> Result<serde_json::Value, ApiError> {
        let (fields, meta) = self
            .prepare_update_payload(key, update, custom_fields)
            .await?;
        if meta.is_none() {
            self.issue_type_for_link(key).await?;
        }
        Ok(write_preview(fields, None, meta.is_some()))
    }

    async fn prepare_update_payload(
        &self,
        key: &str,
        update: &IssueUpdate<'_>,
        custom_fields: &[(String, serde_json::Value)],
    ) -> Result<(serde_json::Value, Option<metadata::Fields>), ApiError> {
        validate_issue_key(key)?;
        let mut fields = serde_json::Map::new();
        if let Some(s) = update.summary {
            fields.insert("summary".into(), serde_json::Value::String(s.into()));
        }
        if let Some(d) = update.description {
            fields.insert("description".into(), self.make_body(d));
        }
        if let Some(p) = update.priority {
            fields.insert("priority".into(), metadata::unresolved_option(p));
        }
        if let Some(comps) = update.components {
            fields.insert("components".into(), named_option_array(comps));
        }
        if let Some(fvs) = update.fix_versions {
            fields.insert("fixVersions".into(), named_option_array(fvs));
        }
        if let Some(lbls) = update.labels {
            fields.insert("labels".into(), serde_json::json!(lbls));
        }
        if let Some(assignee_choice) = update.assignee {
            let payload = match assignee_choice {
                None => serde_json::Value::Null,
                Some(id) => self.assignee_payload(id),
            };
            fields.insert("assignee".into(), payload);
        }
        for (k, value) in custom_fields {
            fields.insert(k.clone(), value.clone());
        }
        if fields.is_empty()
            && update.issue_type.is_none()
            && update.epic.is_none()
            && !update.clear_epic
        {
            return Err(ApiError::InvalidInput(
                "At least one field (--summary, --description, --priority, --type, --epic, --clear-epic, --components, --fix-versions, --labels, --assignee, or --field) is required"
                    .into(),
            ));
        }
        let mut fields = serde_json::Value::Object(fields);
        let meta = self
            .prepare_update(key, &mut fields, update, custom_fields)
            .await?;
        Ok((fields, meta))
    }

    /// Build the appropriate body value for a description or comment field.
    ///
    /// API v3 (Jira Cloud) requires Atlassian Document Format (ADF). API v2
    /// (Jira Data Center / Server) accepts plain strings.
    fn make_body(&self, text: &str) -> serde_json::Value {
        if self.api_version >= 3 {
            text_to_adf(text)
        } else {
            serde_json::Value::String(text.to_string())
        }
    }

    // ── Attachments ───────────────────────────────────────────────────────────

    /// List the attachments on an issue.
    pub async fn list_attachments(&self, key: &str) -> Result<Vec<Attachment>, ApiError> {
        validate_issue_key(key)?;
        #[derive(serde::Deserialize)]
        struct Wrapper {
            fields: AttachmentField,
        }
        #[derive(serde::Deserialize)]
        struct AttachmentField {
            #[serde(default)]
            attachment: Vec<Attachment>,
        }
        let w: Wrapper = self.get(&format!("issue/{key}?fields=attachment")).await?;
        Ok(w.fields.attachment)
    }

    /// Upload one or more local files to an issue.
    ///
    /// Jira rejects this endpoint unless the `X-Atlassian-Token: no-check`
    /// header is present (XSRF protection) and every file is sent under the
    /// multipart field name `file`.
    pub async fn upload_attachments(
        &self,
        key: &str,
        paths: &[PathBuf],
    ) -> Result<Vec<Attachment>, ApiError> {
        self.ensure_writable()?;
        validate_issue_key(key)?;

        let mut form = reqwest::multipart::Form::new();
        for path in paths {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    ApiError::InvalidInput(format!(
                        "'{}' does not name a file to upload",
                        path.display()
                    ))
                })?
                .to_string();
            let content_type = mime_guess::from_path(&file_name).first_or_octet_stream();
            let data = std::fs::read(path)
                .map_err(|e| ApiError::Other(format!("cannot read {}: {e}", path.display())))?;
            let part = reqwest::multipart::Part::bytes(data)
                .file_name(file_name)
                .mime_str(content_type.as_ref())
                .map_err(|e| ApiError::Other(e.to_string()))?;
            form = form.part("file", part);
        }

        let url = format!("{}/issue/{key}/attachments", self.base_url);
        let request = self
            .http
            .post(&url)
            .header("X-Atlassian-Token", "no-check")
            .multipart(form);
        let resp = self.send(request).await?;
        resp.json::<Vec<Attachment>>().await.map_err(ApiError::Http)
    }

    /// Fetch the metadata of a single attachment.
    pub async fn get_attachment(&self, id: &str) -> Result<Attachment, ApiError> {
        validate_attachment_id(id)?;
        self.get(&format!("attachment/{id}")).await
    }

    /// Download the binary content of an attachment.
    ///
    /// Jira answers with a redirect to the storage backend; reqwest follows it
    /// and drops the `Authorization` header when the target is another origin.
    pub async fn download_attachment(&self, id: &str) -> Result<Vec<u8>, ApiError> {
        validate_attachment_id(id)?;
        let url = format!("{}/attachment/content/{id}", self.base_url);
        let resp = self.send(self.http.get(&url)).await?;
        let bytes = resp.bytes().await.map_err(ApiError::Http)?;
        Ok(bytes.to_vec())
    }

    /// Delete an attachment by its ID.
    pub async fn delete_attachment(&self, id: &str) -> Result<(), ApiError> {
        self.ensure_writable()?;
        validate_attachment_id(id)?;
        let url = format!("{}/attachment/{id}", self.base_url);
        self.send(self.http.delete(&url)).await?;
        Ok(())
    }

    // ── Users ─────────────────────────────────────────────────────────────────

    /// Search for users matching a query string.
    ///
    /// API v2: uses `username` parameter. API v3: uses `query` parameter.
    pub async fn search_users(&self, query: &str) -> Result<Vec<User>, ApiError> {
        let encoded = percent_encode(query);
        let param = if self.api_version >= 3 {
            "query"
        } else {
            "username"
        };
        let path = format!("user/search?{param}={encoded}&maxResults=50");
        self.get::<Vec<User>>(&path).await
    }

    // ── Issue links ───────────────────────────────────────────────────────────

    /// List available issue link types.
    pub async fn get_link_types(&self) -> Result<Vec<IssueLinkType>, ApiError> {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(rename = "issueLinkTypes")]
            types: Vec<IssueLinkType>,
        }
        let w: Wrapper = self.get("issueLinkType").await?;
        Ok(w.types)
    }

    /// Link two issues.
    ///
    /// `link_type` is the name of the link type (e.g. "Blocks", "Duplicate").
    /// The direction follows the link type's `outward` description:
    /// `from_key` outward-links to `to_key`.
    pub async fn link_issues(
        &self,
        from_key: &str,
        to_key: &str,
        link_type: &str,
    ) -> Result<(), ApiError> {
        self.ensure_writable()?;
        validate_issue_key(from_key)?;
        validate_issue_key(to_key)?;
        let payload = serde_json::json!({
            "type": { "name": link_type },
            "inwardIssue": { "key": from_key },
            "outwardIssue": { "key": to_key },
        });
        let url = format!("{}/issueLink", self.base_url);
        self.send(self.http.post(&url).json(&payload)).await?;
        Ok(())
    }

    /// Remove an issue link by its ID.
    pub async fn unlink_issues(&self, link_id: &str) -> Result<(), ApiError> {
        self.ensure_writable()?;
        let url = format!("{}/issueLink/{link_id}", self.base_url);
        self.send(self.http.delete(&url)).await?;
        Ok(())
    }

    // ── Boards & Sprints ──────────────────────────────────────────────────────

    /// List all boards, fetching all pages.
    pub async fn list_boards(&self) -> Result<Vec<Board>, ApiError> {
        self.list_boards_for_project(None).await
    }

    pub async fn list_boards_for_project(
        &self,
        project: Option<&str>,
    ) -> Result<Vec<Board>, ApiError> {
        let mut all = Vec::new();
        let mut start_at = 0usize;
        const PAGE: usize = 50;
        loop {
            let project_param = project
                .map(|p| format!("&projectKeyOrId={}", percent_encode(p)))
                .unwrap_or_default();
            let path = format!("board?startAt={start_at}&maxResults={PAGE}{project_param}");
            let page: BoardSearchResponse = self.agile_get(&path).await?;
            let received = page.values.len();
            all.extend(page.values);
            if page.is_last || received == 0 {
                break;
            }
            start_at += received;
        }
        Ok(all)
    }

    /// Fetch one board without scanning unrelated boards.
    pub async fn get_board(&self, id: u64) -> Result<Board, ApiError> {
        self.agile_get(&format!("board/{id}")).await
    }

    /// Discovery may include nonstandard board types. Skip only Jira's explicit
    /// unsupported-sprints response, never authentication or unrelated errors.
    pub(crate) async fn list_sprints_for_discovery(
        &self,
        board_id: u64,
        state: Option<&str>,
    ) -> Result<Option<Vec<Sprint>>, ApiError> {
        match self.list_sprints(board_id, state).await {
            Ok(sprints) => Ok(Some(sprints)),
            Err(ApiError::Api {
                status: 400,
                ref message,
            }) if {
                let message = message.to_ascii_lowercase();
                message.contains("does not support sprints")
                    || message.contains("doesn't support sprints")
                    || message.contains("doesn’t support sprints")
            } =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// List sprints for a board, optionally filtered by state.
    ///
    /// `state` can be "active", "closed", "future", or `None` for all.
    pub async fn list_sprints(
        &self,
        board_id: u64,
        state: Option<&str>,
    ) -> Result<Vec<Sprint>, ApiError> {
        let mut all = Vec::new();
        let mut start_at = 0usize;
        const PAGE: usize = 50;
        loop {
            let state_param = state
                .map(|s| format!("&state={}", percent_encode(s)))
                .unwrap_or_default();
            let path = format!(
                "board/{board_id}/sprint?startAt={start_at}&maxResults={PAGE}{state_param}"
            );
            let page: SprintSearchResponse = self.agile_get(&path).await?;
            let received = page.values.len();
            all.extend(page.values);
            if page.is_last || received == 0 {
                break;
            }
            start_at += received;
        }
        Ok(all)
    }

    // ── Projects ──────────────────────────────────────────────────────────────

    /// List all accessible projects.
    ///
    /// API v3 (Jira Cloud) uses the paginated `project/search` endpoint.
    /// API v2 (Jira Data Center / Server) uses the simpler `project` endpoint
    /// that returns all results in a single flat array.
    pub async fn list_projects(&self) -> Result<Vec<Project>, ApiError> {
        if self.api_version < 3 {
            return self.get::<Vec<Project>>("project").await;
        }

        let mut all: Vec<Project> = Vec::new();
        let mut start_at: usize = 0;
        const PAGE: usize = 50;

        loop {
            let path = format!("project/search?startAt={start_at}&maxResults={PAGE}&orderBy=key");
            let page: ProjectSearchResponse = self.get(&path).await?;
            let page_start = page.start_at;
            let received = page.values.len();
            let total = page.total;
            all.extend(page.values);

            if page.is_last || all.len() >= total {
                break;
            }

            if received == 0 {
                return Err(ApiError::Other(
                    "Project pagination returned an empty non-terminal page".into(),
                ));
            }

            start_at = page_start.saturating_add(received);
        }

        Ok(all)
    }

    /// Fetch a single project by key.
    pub async fn get_project(&self, key: &str) -> Result<Project, ApiError> {
        self.get(&format!("project/{}", metadata::encode_segment(key)))
            .await
    }

    /// List all components for a project.
    ///
    /// Returns a flat array on both Jira Cloud (API v3) and DC/Server (API v2)
    /// - the `project/{key}/components` endpoint is not paginated.
    pub async fn list_components(&self, project_key: &str) -> Result<Vec<Component>, ApiError> {
        self.get::<Vec<Component>>(&format!("project/{project_key}/components"))
            .await
    }

    /// List all versions for a project.
    ///
    /// Returns a flat array on both Jira Cloud (API v3) and DC/Server (API v2)
    /// - the `project/{key}/versions` endpoint is not paginated.
    pub async fn list_versions(&self, project_key: &str) -> Result<Vec<Version>, ApiError> {
        self.get::<Vec<Version>>(&format!("project/{project_key}/versions"))
            .await
    }

    // ── Fields ────────────────────────────────────────────────────────────────

    /// List all available fields (system and custom).
    pub async fn list_fields(&self) -> Result<Vec<Field>, ApiError> {
        self.get::<Vec<Field>>("field").await
    }

    /// Catch a known sprint-move denial before a separate issue-field write.
    /// Older Jira installations may not expose this permission; in that case
    /// the move endpoint remains authoritative and partial-success recovery applies.
    pub async fn preflight_sprint_move_permission(&self, issue_key: &str) -> Result<(), ApiError> {
        validate_issue_key(issue_key)?;
        let permission = if self.api_version >= 3 {
            "SCHEDULE_ISSUES"
        } else {
            "SCHEDULE_ISSUE"
        };
        let path = format!(
            "mypermissions?permissions={permission}&issueKey={}",
            percent_encode(issue_key)
        );
        let response: serde_json::Value = match self.get(&path).await {
            Ok(value) => value,
            Err(_) => return Ok(()),
        };
        if response["permissions"][permission]["havePermission"].as_bool() == Some(false) {
            return Err(ApiError::Forbidden(format!(
                "Schedule Issues permission is required to move {issue_key} to a sprint"
            )));
        }
        Ok(())
    }

    /// Move an issue to a sprint.
    ///
    /// Uses the Agile REST API which is version-independent.
    pub async fn move_issue_to_sprint(
        &self,
        issue_key: &str,
        sprint_id: u64,
    ) -> Result<(), ApiError> {
        self.ensure_writable()?;
        validate_issue_key(issue_key)?;
        let url = format!("{}/sprint/{sprint_id}/issue", self.agile_base_url);
        let payload = serde_json::json!({ "issues": [issue_key] });
        self.send(self.http.post(&url).json(&payload)).await?;
        Ok(())
    }

    /// Fetch a single sprint by numeric ID.
    pub async fn get_sprint(&self, sprint_id: u64) -> Result<Sprint, ApiError> {
        self.agile_get::<Sprint>(&format!("sprint/{sprint_id}"))
            .await
    }

    /// Resolve a sprint specifier to its numeric ID.
    ///
    /// See [`resolve_sprint`] for accepted specifier formats.
    pub async fn resolve_sprint_id(&self, specifier: &str) -> Result<u64, ApiError> {
        if let Ok(id) = specifier.parse::<u64>() {
            return Ok(id);
        }
        self.resolve_sprint(specifier).await.map(|s| s.id)
    }
}

/// Validate that a key matches the `[A-Z][A-Z0-9]*-[0-9]+` format
/// before using it in a URL path.
///
/// Jira project keys start with an uppercase letter and may contain further
/// uppercase letters or digits (e.g. `ABC2-123` is valid).
fn validate_issue_key(key: &str) -> Result<(), ApiError> {
    let mut parts = key.splitn(2, '-');
    let project = parts.next().unwrap_or("");
    let number = parts.next().unwrap_or("");

    let valid = !project.is_empty()
        && !number.is_empty()
        && project
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
        && project
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && number.chars().all(|c| c.is_ascii_digit());

    if valid {
        Ok(())
    } else {
        Err(ApiError::InvalidInput(format!(
            "Invalid issue key '{key}'. Expected format: PROJECT-123"
        )))
    }
}

/// Validate that an attachment ID is numeric before using it in a URL path.
fn validate_attachment_id(id: &str) -> Result<(), ApiError> {
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ApiError::InvalidInput(format!(
            "Invalid attachment ID '{id}'. Expected a number."
        )))
    }
}

/// Percent-encode a string for use in a URL query parameter.
///
/// Uses `%20` for spaces (not `+`) per standard URL encoding.
fn percent_encode(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len() * 2);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            b => encoded.push_str(&format!("%{b:02X}")),
        }
    }
    encoded
}

/// Truncate an API error body when explicitly debugging HTTP failures.
fn truncate_error_body(body: &str) -> String {
    const MAX: usize = 200;
    if body.chars().count() <= MAX {
        body.to_string()
    } else {
        let truncated: String = body.chars().take(MAX).collect();
        format!("{truncated}… (truncated)")
    }
}

fn summarize_error_body(status: u16, body: &str) -> String {
    if should_include_raw_error_body() && !body.trim().is_empty() {
        return truncate_error_body(body);
    }

    if let Some(message) = summarize_json_error_body(body) {
        return message;
    }

    default_status_message(status)
}

fn summarize_json_error_body(body: &str) -> Option<String> {
    let parsed: JiraErrorPayload = serde_json::from_str(body).ok()?;
    let mut parts = Vec::new();

    if !parsed.error_messages.is_empty() {
        parts.push(format_error_messages(&parsed.error_messages));
    } else if let Some(message) = parsed.message.as_deref().filter(|m| !m.trim().is_empty()) {
        parts.push(truncate_message(message.trim()));
    }

    if !parsed.errors.is_empty() {
        let fields = parsed.errors.keys().take(5).cloned().collect::<Vec<_>>();
        parts.push(format!(
            "validation errors for fields: {}",
            fields.join(", ")
        ));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

/// Maximum number of Jira `errorMessages` entries to surface inline before
/// collapsing the remainder into a `(+N more)` suffix.
const MAX_ERROR_MESSAGES_SHOWN: usize = 3;

/// Maximum character length of each individual message, so a single
/// pathological Jira response cannot dominate the user-visible error line.
const MAX_ERROR_MESSAGE_LEN: usize = 240;

fn format_error_messages(messages: &[String]) -> String {
    let shown: Vec<String> = messages
        .iter()
        .take(MAX_ERROR_MESSAGES_SHOWN)
        .map(|m| truncate_message(m.trim()))
        .collect();
    let joined = shown.join(" | ");
    let remaining = messages.len().saturating_sub(MAX_ERROR_MESSAGES_SHOWN);
    if remaining > 0 {
        format!("{joined} (+{remaining} more)")
    } else {
        joined
    }
}

fn truncate_message(msg: &str) -> String {
    if msg.chars().count() <= MAX_ERROR_MESSAGE_LEN {
        msg.to_string()
    } else {
        let truncated: String = msg.chars().take(MAX_ERROR_MESSAGE_LEN).collect();
        format!("{truncated}…")
    }
}

fn default_status_message(status: u16) -> String {
    match status {
        401 => "credentials rejected".into(),
        403 => "request forbidden".into(),
        404 => "resource not found".into(),
        409 => "conflicts with the current state of the resource".into(),
        429 => "rate limited by Jira".into(),
        400..=499 => format!("request failed with status {status}"),
        _ => format!("Jira request failed with status {status}"),
    }
}

fn should_include_raw_error_body() -> bool {
    std::env::var("JIRA_DEBUG_HTTP").is_ok_and(|value| crate::config::is_truthy(&value))
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JiraErrorPayload {
    #[serde(default)]
    error_messages: Vec<String>,
    /// The Cloud API gateway reports its own refusals, a missing token scope
    /// among them, as a single `message` rather than `errorMessages`.
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    errors: BTreeMap<String, String>,
}

fn write_preview(
    fields: serde_json::Value,
    issue_types: Option<bool>,
    field_metadata: bool,
) -> serde_json::Value {
    let mut warnings = vec![
        "Jira may apply additional workflow, permission, or plugin validators when the write is submitted.",
    ];
    if issue_types == Some(false) {
        warnings.push("Issue type metadata is unavailable; the issue type could not be normalized or validated.");
    }
    if !field_metadata {
        warnings.push("Field metadata is unavailable; required fields and allowed values could not be fully validated.");
    }
    serde_json::json!({"fields":fields, "metadata":{"issueTypes":issue_types, "fields":field_metadata}, "warnings":warnings})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_spaces_use_percent_20() {
        assert_eq!(percent_encode("project = FOO"), "project%20%3D%20FOO");
    }

    #[test]
    fn percent_encode_complex_jql() {
        let jql = r#"project = "MY PROJECT""#;
        let encoded = percent_encode(jql);
        assert!(encoded.contains("project"));
        assert!(!encoded.contains('"'));
        assert!(!encoded.contains(' '));
    }

    #[test]
    fn validate_issue_key_valid() {
        assert!(validate_issue_key("PROJ-123").is_ok());
        assert!(validate_issue_key("ABC-1").is_ok());
        assert!(validate_issue_key("MYPROJECT-9999").is_ok());
        // Digits are allowed in the project key after the initial letter
        assert!(validate_issue_key("ABC2-123").is_ok());
        assert!(validate_issue_key("P1-1").is_ok());
    }

    #[test]
    fn validate_issue_key_invalid() {
        assert!(validate_issue_key("proj-123").is_err()); // lowercase
        assert!(validate_issue_key("PROJ123").is_err()); // no dash
        assert!(validate_issue_key("PROJ-abc").is_err()); // non-numeric suffix
        assert!(validate_issue_key("../etc/passwd").is_err());
        assert!(validate_issue_key("").is_err());
        assert!(validate_issue_key("1PROJ-123").is_err()); // starts with digit
    }

    #[test]
    fn truncate_error_body_short() {
        let body = "short error";
        assert_eq!(truncate_error_body(body), body);
    }

    #[test]
    fn truncate_error_body_long() {
        let body = "x".repeat(300);
        let result = truncate_error_body(&body);
        assert!(result.len() < body.len());
        assert!(result.ends_with("(truncated)"));
    }

    #[test]
    fn summarize_json_error_body_surfaces_messages_and_redacts_field_values() {
        let body = serde_json::json!({
            "errorMessages": ["JQL validation failed"],
            "errors": {
                "summary": "Summary must not contain secret project name",
                "description": "Description cannot include api token"
            }
        })
        .to_string();

        let message = summarize_error_body(400, &body);
        // errorMessages are server-provided strings, safe to surface in full.
        assert!(message.contains("JQL validation failed"));
        // `errors` keys (field names) are safe; their values may echo user
        // input and must stay redacted.
        assert!(message.contains("summary"));
        assert!(message.contains("description"));
        assert!(!message.contains("secret project name"));
        assert!(!message.contains("api token"));
    }

    #[test]
    fn summarize_json_error_body_reports_retired_api() {
        // Real payload shape returned by Atlassian after CHANGE-2046.
        let body = serde_json::json!({
            "errorMessages": [
                "The requested API has been removed. Please migrate to the /rest/api/3/search/jql API."
            ],
            "errors": {}
        })
        .to_string();

        let message = summarize_error_body(410, &body);
        assert!(message.contains("The requested API has been removed"));
        assert!(message.contains("/rest/api/3/search/jql"));
    }

    #[test]
    fn summarize_json_error_body_joins_multiple_messages() {
        let body = serde_json::json!({
            "errorMessages": ["first problem", "second problem"],
            "errors": {}
        })
        .to_string();

        let message = summarize_error_body(400, &body);
        assert!(message.contains("first problem"));
        assert!(message.contains("second problem"));
        assert!(message.contains(" | "));
    }

    #[test]
    fn summarize_json_error_body_collapses_overflow_messages() {
        let body = serde_json::json!({
            "errorMessages": ["a", "b", "c", "d", "e"],
            "errors": {}
        })
        .to_string();

        let message = summarize_error_body(400, &body);
        assert!(message.contains("(+2 more)"));
    }

    #[test]
    fn summarize_json_error_body_truncates_oversized_message() {
        let huge = "x".repeat(1000);
        let body = serde_json::json!({
            "errorMessages": [huge],
            "errors": {}
        })
        .to_string();

        let message = summarize_error_body(400, &body);
        assert!(message.chars().count() < 500);
        assert!(message.contains('…'));
    }

    #[test]
    fn browse_url_preserves_explicit_http_hosts() {
        let client = JiraClient::new(
            "http://localhost:8080",
            "me@example.com",
            "token",
            AuthType::Basic,
            3,
        )
        .unwrap();
        assert_eq!(
            client.browse_url("PROJ-1"),
            "http://localhost:8080/browse/PROJ-1"
        );
    }

    #[test]
    fn new_with_pat_auth_does_not_require_email() {
        let client = JiraClient::new(
            "https://jira.example.com",
            "",
            "my-pat-token",
            AuthType::Pat,
            3,
        );
        assert!(client.is_ok());
    }

    #[test]
    fn new_with_api_v2_uses_v2_base_url() {
        let client = JiraClient::new(
            "https://jira.example.com",
            "me@example.com",
            "token",
            AuthType::Basic,
            2,
        )
        .unwrap();
        assert_eq!(client.api_version(), 2);
    }

    #[test]
    fn scoped_cloud_token_uses_gateway_but_keeps_site_links() {
        let client = JiraClient::new_with_cloud(
            "acme.atlassian.net",
            "me@example.com",
            "token",
            AuthType::Basic,
            3,
            Some("cloud-123"),
            "scoped",
        )
        .unwrap();

        assert_eq!(
            client.base_url,
            "https://api.atlassian.com/ex/jira/cloud-123/rest/api/3"
        );
        assert_eq!(
            client.agile_base_url,
            "https://api.atlassian.com/ex/jira/cloud-123/rest/agile/1.0"
        );
        assert_eq!(
            client.browse_url("PROJ-1"),
            "https://acme.atlassian.net/browse/PROJ-1"
        );
    }

    #[test]
    fn scoped_cloud_token_requires_cloud_id() {
        let error = JiraClient::new_with_cloud(
            "acme.atlassian.net",
            "me@example.com",
            "token",
            AuthType::Basic,
            3,
            None,
            "scoped",
        )
        .err()
        .expect("scoped credentials without a Cloud ID must fail");
        assert!(error.to_string().contains("Cloud ID"));
    }
}
