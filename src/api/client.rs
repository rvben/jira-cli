use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::ApiError;
use super::AuthType;
use super::types::*;

pub struct JiraClient {
    http: reqwest::Client,
    base_url: String,
    agile_base_url: String,
    site_url: String,
    host: String,
    api_version: u8,
}

const SEARCH_FIELDS: [&str; 7] = [
    "summary",
    "status",
    "assignee",
    "priority",
    "issuetype",
    "created",
    "updated",
];
const SEARCH_GET_JQL_LIMIT: usize = 1500;

/// Max issues per page the Jira Cloud `/search/jql` endpoint will return when
/// any non-ID fields are requested. The server silently caps larger values,
/// so we paginate internally to fulfil larger caller-requested limits.
const SEARCH_JQL_MAX_PAGE: usize = 100;

/// Page size used when walking the cursor forward to simulate an offset on
/// Jira Cloud. Requests only `id` to stay cheap (allows up to 5000/page).
const SEARCH_JQL_SKIP_PAGE: usize = 1000;

/// Build a `[{"name": x}, ...]` JSON array used by Jira for components, versions, etc.
fn name_object_array(items: &[&str]) -> serde_json::Value {
    serde_json::Value::Array(
        items
            .iter()
            .map(|name| serde_json::json!({ "name": name }))
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
        let base_url = format!("{site_url}/rest/api/{api_version}");
        let agile_base_url = format!("{site_url}/rest/agile/1.0");

        Ok(Self {
            http,
            base_url,
            agile_base_url,
            site_url,
            host: domain.to_string(),
            api_version,
        })
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

    fn map_status(status: u16, body: String) -> ApiError {
        let message = summarize_error_body(status, &body);
        match status {
            401 | 403 => ApiError::Auth(message),
            404 => ApiError::NotFound(message),
            429 => ApiError::RateLimit,
            _ => ApiError::Api { status, message },
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let url = format!("{}/{path}", self.base_url);
        let resp = self.http.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::map_status(status.as_u16(), body));
        }
        resp.json::<T>().await.map_err(ApiError::Http)
    }

    async fn agile_get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let url = format!("{}/{path}", self.agile_base_url);
        let resp = self.http.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::map_status(status.as_u16(), body));
        }
        resp.json::<T>().await.map_err(ApiError::Http)
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, ApiError> {
        let url = format!("{}/{path}", self.base_url);
        let resp = self.http.post(&url).json(body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(Self::map_status(status.as_u16(), body_text));
        }
        resp.json::<T>().await.map_err(ApiError::Http)
    }

    async fn post_empty_response(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let url = format!("{}/{path}", self.base_url);
        let resp = self.http.post(&url).json(body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(Self::map_status(status.as_u16(), body_text));
        }
        Ok(())
    }

    async fn put_empty_response(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let url = format!("{}/{path}", self.base_url);
        let resp = self.http.put(&url).json(body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(Self::map_status(status.as_u16(), body_text));
        }
        Ok(())
    }

    /// Send a request whose response body is not JSON, mapping a non-success
    /// status to an `ApiError` like the helpers above.
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, ApiError> {
        let resp = request.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::map_status(status.as_u16(), body));
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
    /// `/rest/api/3/search/jql` endpoint — the original `/search` was retired
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
        let fields = SEARCH_FIELDS.join(",");
        let encoded_jql = percent_encode(jql);
        #[derive(serde::Deserialize)]
        struct RawV2 {
            issues: Vec<Issue>,
            #[serde(default)]
            total: usize,
            #[serde(rename = "startAt", default)]
            start_at: usize,
            #[serde(rename = "maxResults", default)]
            max_results: usize,
        }
        let raw: RawV2 = if encoded_jql.len() <= SEARCH_GET_JQL_LIMIT {
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
                    "fields": SEARCH_FIELDS,
                }),
            )
            .await?
        };
        let is_last = raw.start_at + raw.issues.len() >= raw.total;
        Ok(SearchResponse {
            issues: raw.issues,
            total: Some(raw.total),
            start_at: raw.start_at,
            max_results: raw.max_results,
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
        validate_issue_key(key)?;
        let fields = "summary,status,assignee,reporter,priority,issuetype,description,labels,components,fixVersions,versions,created,updated,comment,issuelinks";
        let path = format!("issue/{key}?fields={fields}");
        let mut issue: Issue = self.get(&path).await?;

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

    /// Create a new issue.
    pub async fn create_issue(
        &self,
        draft: &IssueDraft<'_>,
        custom_fields: &[(String, serde_json::Value)],
    ) -> Result<CreateIssueResponse, ApiError> {
        let mut fields = serde_json::json!({
            "project": { "key": draft.project_key },
            "issuetype": { "name": draft.issue_type },
            "summary": draft.summary,
        });

        if let Some(desc) = draft.description {
            fields["description"] = self.make_body(desc);
        }
        if let Some(p) = draft.priority {
            fields["priority"] = serde_json::json!({ "name": p });
        }
        if let Some(lbls) = draft.labels
            && !lbls.is_empty()
        {
            fields["labels"] = serde_json::json!(lbls);
        }
        if let Some(comps) = draft.components
            && !comps.is_empty()
        {
            fields["components"] = name_object_array(comps);
        }
        if let Some(fvs) = draft.fix_versions
            && !fvs.is_empty()
        {
            fields["fixVersions"] = name_object_array(fvs);
        }
        if let Some(id) = draft.assignee {
            fields["assignee"] = self.assignee_payload(id);
        }
        if let Some(parent_key) = draft.parent {
            fields["parent"] = serde_json::json!({ "key": parent_key });
        }
        for (key, value) in custom_fields {
            fields[key] = value.clone();
        }

        self.post("issue", &serde_json::json!({ "fields": fields }))
            .await
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
        validate_issue_key(key)?;
        let mut fields = serde_json::Map::new();
        if let Some(s) = update.summary {
            fields.insert("summary".into(), serde_json::Value::String(s.into()));
        }
        if let Some(d) = update.description {
            fields.insert("description".into(), self.make_body(d));
        }
        if let Some(p) = update.priority {
            fields.insert("priority".into(), serde_json::json!({ "name": p }));
        }
        if let Some(comps) = update.components {
            fields.insert("components".into(), name_object_array(comps));
        }
        if let Some(fvs) = update.fix_versions {
            fields.insert("fixVersions".into(), name_object_array(fvs));
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
        if fields.is_empty() {
            return Err(ApiError::InvalidInput(
                "At least one field (--summary, --description, --priority, --components, --fix-versions, --labels, --assignee, or --field) is required"
                    .into(),
            ));
        }
        self.put_empty_response(
            &format!("issue/{key}"),
            &serde_json::json!({ "fields": fields }),
        )
        .await
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
        validate_issue_key(from_key)?;
        validate_issue_key(to_key)?;
        let payload = serde_json::json!({
            "type": { "name": link_type },
            "inwardIssue": { "key": from_key },
            "outwardIssue": { "key": to_key },
        });
        let url = format!("{}/issueLink", self.base_url);
        let resp = self.http.post(&url).json(&payload).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::map_status(status.as_u16(), body));
        }
        Ok(())
    }

    /// Remove an issue link by its ID.
    pub async fn unlink_issues(&self, link_id: &str) -> Result<(), ApiError> {
        let url = format!("{}/issueLink/{link_id}", self.base_url);
        let resp = self.http.delete(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::map_status(status.as_u16(), body));
        }
        Ok(())
    }

    // ── Boards & Sprints ──────────────────────────────────────────────────────

    /// List all boards, fetching all pages.
    pub async fn list_boards(&self) -> Result<Vec<Board>, ApiError> {
        let mut all = Vec::new();
        let mut start_at = 0usize;
        const PAGE: usize = 50;
        loop {
            let path = format!("board?startAt={start_at}&maxResults={PAGE}");
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
            let state_param = state.map(|s| format!("&state={s}")).unwrap_or_default();
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
        self.get(&format!("project/{key}")).await
    }

    /// List all components for a project.
    ///
    /// Returns a flat array on both Jira Cloud (API v3) and DC/Server (API v2)
    /// — the `project/{key}/components` endpoint is not paginated.
    pub async fn list_components(&self, project_key: &str) -> Result<Vec<Component>, ApiError> {
        self.get::<Vec<Component>>(&format!("project/{project_key}/components"))
            .await
    }

    /// List all versions for a project.
    ///
    /// Returns a flat array on both Jira Cloud (API v3) and DC/Server (API v2)
    /// — the `project/{key}/versions` endpoint is not paginated.
    pub async fn list_versions(&self, project_key: &str) -> Result<Vec<Version>, ApiError> {
        self.get::<Vec<Version>>(&format!("project/{project_key}/versions"))
            .await
    }

    // ── Fields ────────────────────────────────────────────────────────────────

    /// List all available fields (system and custom).
    pub async fn list_fields(&self) -> Result<Vec<Field>, ApiError> {
        self.get::<Vec<Field>>("field").await
    }

    /// Move an issue to a sprint.
    ///
    /// Uses the Agile REST API which is version-independent.
    pub async fn move_issue_to_sprint(
        &self,
        issue_key: &str,
        sprint_id: u64,
    ) -> Result<(), ApiError> {
        validate_issue_key(issue_key)?;
        let url = format!("{}/sprint/{sprint_id}/issue", self.agile_base_url);
        let payload = serde_json::json!({ "issues": [issue_key] });
        let resp = self.http.post(&url).json(&payload).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::map_status(status.as_u16(), body));
        }
        Ok(())
    }

    /// Fetch a single sprint by numeric ID.
    pub async fn get_sprint(&self, sprint_id: u64) -> Result<Sprint, ApiError> {
        self.agile_get::<Sprint>(&format!("sprint/{sprint_id}"))
            .await
    }

    /// Resolve a sprint specifier to a `Sprint`.
    ///
    /// Accepts:
    /// - A numeric string: fetches the sprint by ID to confirm it exists and get the name
    /// - `"active"`: returns the first active sprint found across all boards
    /// - Any other string: matched case-insensitively as a substring of sprint names
    pub async fn resolve_sprint(&self, specifier: &str) -> Result<Sprint, ApiError> {
        if let Ok(id) = specifier.parse::<u64>() {
            return self.get_sprint(id).await;
        }

        let boards = self.list_boards().await?;
        if boards.is_empty() {
            return Err(ApiError::NotFound("No boards found".into()));
        }

        let target_state = if specifier.eq_ignore_ascii_case("active") {
            Some("active")
        } else {
            None
        };

        for board in &boards {
            let sprints = self.list_sprints(board.id, target_state).await?;
            for sprint in sprints {
                if specifier.eq_ignore_ascii_case("active") {
                    if sprint.state == "active" {
                        return Ok(sprint);
                    }
                } else if sprint
                    .name
                    .to_lowercase()
                    .contains(&specifier.to_lowercase())
                {
                    return Ok(sprint);
                }
            }
        }

        Err(ApiError::NotFound(format!(
            "No sprint found matching '{specifier}'"
        )))
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
        401 | 403 => "request unauthorized".into(),
        404 => "resource not found".into(),
        429 => "rate limited by Jira".into(),
        400..=499 => format!("request failed with status {status}"),
        _ => format!("Jira request failed with status {status}"),
    }
}

fn should_include_raw_error_body() -> bool {
    matches!(
        std::env::var("JIRA_DEBUG_HTTP").ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    )
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JiraErrorPayload {
    #[serde(default)]
    error_messages: Vec<String>,
    #[serde(default)]
    errors: BTreeMap<String, String>,
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
}
