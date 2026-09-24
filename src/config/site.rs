//! Resolving the Jira site a user types or pastes during `jira init`, and
//! identifying whether it runs Jira Cloud or Data Center.

use std::time::Duration;

use serde::Deserialize;

const CLOUD_SUFFIX: &str = ".atlassian.net";

/// Upper bound on `serverInfo` requests for one address.
const MAX_PROBES: usize = 5;

/// Jira deployment behind a site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Deployment {
    Cloud,
    /// Data Center, or its end-of-life predecessor Server. Both serve REST API v2
    /// and accept personal access tokens as bearer tokens.
    DataCenter,
}

/// What a site reported about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Detected {
    pub deployment: Deployment,
    pub version: Option<String>,
    /// The host to store in the profile. Usually the site as entered; for a Cloud
    /// site on a custom domain, the `*.atlassian.net` address it reports; and the
    /// bare origin when only the origin answered.
    pub site: String,
}

impl Detected {
    pub fn describe(&self) -> String {
        match (self.deployment, &self.version) {
            (Deployment::Cloud, _) => "Jira Cloud".to_owned(),
            (Deployment::DataCenter, Some(version)) => format!("Jira Data Center {version}"),
            (Deployment::DataCenter, None) => "Jira Data Center".to_owned(),
        }
    }
}

/// A typed or pasted Jira address, normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Site {
    /// `host[:port][/context-path]` with Jira's own routes removed: the best
    /// guess at the site root, and the form a profile stores.
    pub host: String,
    /// The same address with its whole path kept, for the rare install whose
    /// context path shares a name with a Jira route, such as `/issues`.
    pub as_entered: String,
}

/// Normalize a typed or pasted Jira address into the host form a profile stores:
/// `host[:port][/context-path]`, prefixed with `http://` only when that scheme
/// was given explicitly.
///
/// Accepts a bare Cloud subdomain (`acme`), a host, or any link copied from the
/// browser, such as an issue or board URL. Jira's own routes (`/browse/...`,
/// `/secure/...`, `*.jspa`) are dropped so only a Data Center context path such
/// as `/jira` remains; Cloud sites never have one.
pub(crate) fn normalize_site(input: &str) -> Result<Site, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("enter the address of your Jira site".into());
    }
    if input.contains(char::is_whitespace) {
        return Err(format!("`{input}` contains spaces; enter a single address"));
    }
    let has_scheme = input.contains("://");
    let with_scheme = if has_scheme {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let url = reqwest::Url::parse(&with_scheme)
        .map_err(|error| format!("`{input}` is not a valid address: {error}"))?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "`{input}` uses `{scheme}://`; Jira is reached over https:// (or http://)"
        ));
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| format!("`{input}` has no host name"))?;

    // A single word is a Cloud subdomain, the part of the address people know.
    let host = if !has_scheme && !host.contains(['.', ':', '[']) && host != "localhost" {
        format!("{host}{CLOUD_SUFFIX}")
    } else {
        host.to_owned()
    };

    let (context, full_path) = if is_cloud_host(&host) {
        (String::new(), String::new())
    } else {
        (context_path(url.path()), full_path(url.path()))
    };
    let authority = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    };
    // Cloud is served only over HTTPS; keeping `http://` would send the first
    // authenticated request, token included, in cleartext before any redirect.
    let prefix = if scheme == "http" && !is_cloud_host(&authority) {
        "http://"
    } else {
        ""
    };
    Ok(Site {
        host: format!("{prefix}{authority}{context}"),
        as_entered: format!("{prefix}{authority}{full_path}"),
    })
}

/// True when a stored host names a `*.atlassian.net` Cloud site.
pub(crate) fn is_cloud_host(site: &str) -> bool {
    let without_scheme = site
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let authority = without_scheme.split('/').next().unwrap_or_default();
    let host = authority.split(':').next().unwrap_or_default();
    host.to_ascii_lowercase().ends_with(CLOUD_SUFFIX)
}

/// Keep the leading path segments that precede Jira's own routes.
fn context_path(path: &str) -> String {
    join_path(
        path.split('/')
            .filter(|segment| !segment.is_empty())
            .take_while(|segment| !is_jira_route(segment)),
    )
}

fn full_path(path: &str) -> String {
    join_path(path.split('/').filter(|segment| !segment.is_empty()))
}

fn join_path<'a>(segments: impl Iterator<Item = &'a str>) -> String {
    segments.map(|segment| format!("/{segment}")).collect()
}

fn is_jira_route(segment: &str) -> bool {
    const ROUTES: &[&str] = &[
        "browse",
        "secure",
        "projects",
        "issues",
        "rest",
        "plugins",
        "servicedesk",
        "dashboards",
        // Cloud's product UI, which a custom domain can carry under `/jira`.
        "software",
        "core",
        "your-work",
        "for-you",
    ];
    let segment = segment.to_ascii_lowercase();
    ROUTES.contains(&segment.as_str())
        || segment.ends_with(".jspa")
        || segment.ends_with(".jsp")
        || segment.ends_with(".action")
}

fn site_url(site: &str) -> String {
    let site = site.trim_end_matches('/');
    if site.starts_with("http://") || site.starts_with("https://") {
        site.to_owned()
    } else {
        format!("https://{site}")
    }
}

/// The site, then each shorter path prefix down to the bare origin.
fn candidate_sites(site: &str) -> Vec<String> {
    let (prefix, rest) = match site.strip_prefix("http://") {
        Some(rest) => ("http://", rest),
        None => ("", site.trim_start_matches("https://")),
    };
    let mut parts = rest.split('/').filter(|part| !part.is_empty());
    let Some(authority) = parts.next() else {
        return vec![site.to_owned()];
    };
    let segments: Vec<&str> = parts.collect();
    (0..=segments.len())
        .rev()
        .map(|len| {
            let path: String = segments[..len]
                .iter()
                .map(|segment| format!("/{segment}"))
                .collect();
            format!("{prefix}{authority}{path}")
        })
        .collect()
}

/// Where to look for `serverInfo`, at most [`MAX_PROBES`] addresses: the
/// route-stripped address first, then every prefix of the address as entered,
/// shallowest first. Context paths are short, so the origin and one- or
/// two-segment prefixes come before deep ones. The as-entered prefixes also
/// cover a context path named like a Jira route (`/issues`), and a pasted link
/// with trailing segments that are neither routes nor part of the context path.
fn probe_order(site: &Site) -> Vec<String> {
    let mut order = vec![site.host.clone()];
    for candidate in candidate_sites(&site.as_entered).into_iter().rev() {
        if !order.contains(&candidate) {
            order.push(candidate);
        }
    }
    order.truncate(MAX_PROBES);
    order
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerInfo {
    deployment_type: Option<String>,
    version: Option<String>,
    base_url: Option<String>,
}

/// Ask the site what it runs, through Jira's unauthenticated `serverInfo`
/// endpoint. Cloud reports `deploymentType: "Cloud"`; Data Center reports
/// `"DataCenter"` or, on some releases, `"Server"`.
///
/// Addresses are tried in [`probe_order`]. The error reported is the one for
/// the first address tried.
pub(crate) async fn detect(site: &Site) -> Result<Detected, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;

    let mut first_error = None;
    for candidate in probe_order(site) {
        match probe(&client, &candidate).await {
            Ok(detected) => return Ok(detected),
            Err(Probe::Unreachable(error)) => return Err(first_error.unwrap_or(error)),
            Err(Probe::NotJira(error)) => {
                first_error.get_or_insert(error);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| format!("{} did not answer as a Jira site", site.host)))
}

enum Probe {
    /// The host did not answer at all; retrying another path cannot help.
    Unreachable(String),
    /// The host answered, but not as a Jira site.
    NotJira(String),
}

async fn probe(client: &reqwest::Client, site: &str) -> Result<Detected, Probe> {
    let url = format!("{}/rest/api/2/serverInfo", site_url(site));
    let response = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| Probe::Unreachable(describe_request_error(site, &error)))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Probe::NotJira(format!(
            "{site} did not answer as a Jira site (HTTP {status}); check the address"
        )));
    }
    let info: ServerInfo = response.json().await.map_err(|_| {
        Probe::NotJira(format!(
            "{site} answered with a web page instead of Jira server information; check the address, or a single sign-on page may be in front of it"
        ))
    })?;

    let deployment = match info.deployment_type.as_deref() {
        Some(kind) if kind.eq_ignore_ascii_case("cloud") => Deployment::Cloud,
        Some(_) => Deployment::DataCenter,
        None if info.version.is_some() => Deployment::DataCenter,
        None => {
            return Err(Probe::NotJira(format!(
                "{site} answered, but did not say which Jira deployment it runs"
            )));
        }
    };

    let reported_cloud_host = info
        .base_url
        .as_deref()
        .and_then(|base| normalize_site(base).ok())
        .map(|reported| reported.host)
        .filter(|host| is_cloud_host(host));
    let site = match (deployment, reported_cloud_host) {
        (Deployment::Cloud, Some(host)) => host,
        _ => site.to_owned(),
    };
    let version = match deployment {
        Deployment::Cloud => None,
        Deployment::DataCenter => info.version,
    };
    Ok(Detected {
        deployment,
        version,
        site,
    })
}

fn describe_request_error(site: &str, error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return format!("{site} did not respond in time");
    }
    if error.is_connect() {
        return format!("could not connect to {site}; check the address and your network or VPN");
    }
    let mut message = format!("could not reach {site}: {error}");
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn bare_word_is_a_cloud_subdomain() {
        assert_eq!(normalize_site("acme").unwrap().host, "acme.atlassian.net");
        assert_eq!(
            normalize_site("  Acme  ").unwrap().host,
            "acme.atlassian.net"
        );
    }

    #[test]
    fn cloud_links_reduce_to_the_site() {
        for input in [
            "acme.atlassian.net",
            "https://acme.atlassian.net",
            "https://acme.atlassian.net/",
            "https://acme.atlassian.net/browse/ABC-123",
            "https://acme.atlassian.net/jira/software/projects/ABC/boards/1?selectedIssue=ABC-2",
            "acme.atlassian.net/jira/your-work",
        ] {
            assert_eq!(
                normalize_site(input).unwrap().host,
                "acme.atlassian.net",
                "{input}"
            );
        }
    }

    #[test]
    fn data_center_links_keep_the_context_path() {
        let cases = [
            ("jira.corp.example", "jira.corp.example"),
            (
                "https://jira.corp.example/browse/OPS-1",
                "jira.corp.example",
            ),
            (
                "https://issues.example.org/jira/browse/KAFKA-1",
                "issues.example.org/jira",
            ),
            (
                "https://issues.example.org/jira/secure/Dashboard.jspa",
                "issues.example.org/jira",
            ),
            (
                "issues.example.org/tools/jira/projects/ABC/issues",
                "issues.example.org/tools/jira",
            ),
            (
                "https://jira.corp.example:8443/browse/X-1",
                "jira.corp.example:8443",
            ),
            ("https://jira.corp.example:443/", "jira.corp.example"),
            (
                "https://jira.corp.example/jira/software/projects/ABC/boards/1",
                "jira.corp.example/jira",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_site(input).unwrap().host, expected, "{input}");
        }
    }

    #[test]
    fn the_address_as_entered_keeps_its_whole_path() {
        let site = normalize_site("https://jira.corp.example/issues/browse/OPS-1?x=1").unwrap();
        assert_eq!(site.host, "jira.corp.example");
        assert_eq!(site.as_entered, "jira.corp.example/issues/browse/OPS-1");
        let cloud = normalize_site("https://acme.atlassian.net/browse/ABC-1").unwrap();
        assert_eq!(cloud.as_entered, "acme.atlassian.net");
    }

    #[test]
    fn cloud_is_always_https() {
        let site = normalize_site("http://acme.atlassian.net/browse/ABC-1").unwrap();
        assert_eq!(site.host, "acme.atlassian.net");
        assert_eq!(site.as_entered, "acme.atlassian.net");
    }

    #[test]
    fn explicit_http_is_kept() {
        assert_eq!(
            normalize_site("http://localhost:8080/jira/browse/X-1")
                .unwrap()
                .host,
            "http://localhost:8080/jira"
        );
        assert_eq!(
            normalize_site("localhost:8080").unwrap().host,
            "localhost:8080"
        );
        assert_eq!(
            normalize_site("http://192.0.2.10:8080").unwrap().host,
            "http://192.0.2.10:8080"
        );
    }

    #[test]
    fn unusable_input_is_rejected_with_a_reason() {
        assert!(normalize_site("   ").is_err());
        assert!(
            normalize_site("ftp://jira.corp.example")
                .unwrap_err()
                .contains("ftp://")
        );
        assert!(normalize_site("https://").is_err());
        assert!(
            normalize_site("acme corp")
                .unwrap_err()
                .contains("contains spaces")
        );
    }

    #[test]
    fn cloud_host_is_recognised_with_or_without_scheme() {
        assert!(is_cloud_host("acme.atlassian.net"));
        assert!(is_cloud_host("https://ACME.atlassian.net/"));
        assert!(!is_cloud_host("jira.corp.example"));
        assert!(!is_cloud_host("atlassian.net.corp.example"));
    }

    #[test]
    fn candidates_run_from_the_full_path_down_to_the_origin() {
        assert_eq!(
            candidate_sites("jira.corp.example/tools/jira"),
            [
                "jira.corp.example/tools/jira",
                "jira.corp.example/tools",
                "jira.corp.example",
            ]
        );
        assert_eq!(
            candidate_sites("http://localhost:8080/jira"),
            ["http://localhost:8080/jira", "http://localhost:8080"]
        );
        assert_eq!(candidate_sites("jira.corp.example"), ["jira.corp.example"]);
    }

    async fn server_info(server: &MockServer, route: &str, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn detects_data_center_from_server_deployment_type() {
        let server = MockServer::start().await;
        server_info(
            &server,
            "/rest/api/2/serverInfo",
            serde_json::json!({"deploymentType": "Server", "version": "10.3.25"}),
        )
        .await;

        let detected = detect(&normalize_site(&server.uri()).unwrap())
            .await
            .unwrap();
        assert_eq!(detected.deployment, Deployment::DataCenter);
        assert_eq!(detected.describe(), "Jira Data Center 10.3.25");
        assert_eq!(detected.site, server.uri());
    }

    #[tokio::test]
    async fn detects_data_center_from_datacenter_deployment_type() {
        let server = MockServer::start().await;
        server_info(
            &server,
            "/jira/rest/api/2/serverInfo",
            serde_json::json!({"deploymentType": "DataCenter", "version": "9.12.0"}),
        )
        .await;

        let site = format!("{}/jira", server.uri());
        let detected = detect(&normalize_site(&site).unwrap()).await.unwrap();
        assert_eq!(detected.deployment, Deployment::DataCenter);
        assert_eq!(detected.site, site);
    }

    #[tokio::test]
    async fn cloud_on_a_custom_domain_resolves_to_its_atlassian_net_site() {
        let server = MockServer::start().await;
        server_info(
            &server,
            "/rest/api/2/serverInfo",
            serde_json::json!({
                "deploymentType": "Cloud",
                "version": "1001.0.0-SNAPSHOT",
                "baseUrl": "https://acme.atlassian.net"
            }),
        )
        .await;

        let detected = detect(&normalize_site(&server.uri()).unwrap())
            .await
            .unwrap();
        assert_eq!(detected.deployment, Deployment::Cloud);
        assert_eq!(detected.describe(), "Jira Cloud");
        assert_eq!(detected.site, "acme.atlassian.net");
    }

    #[tokio::test]
    async fn trailing_segments_fall_back_to_the_context_path() {
        let server = MockServer::start().await;
        server_info(
            &server,
            "/jira/rest/api/2/serverInfo",
            serde_json::json!({"deploymentType": "DataCenter", "version": "9.12.0"}),
        )
        .await;

        let detected = detect(&normalize_site(&format!("{}/jira/wiki/x", server.uri())).unwrap())
            .await
            .unwrap();
        assert_eq!(detected.site, format!("{}/jira", server.uri()));
    }

    #[tokio::test]
    async fn a_context_path_named_like_a_jira_route_is_found() {
        let server = MockServer::start().await;
        server_info(
            &server,
            "/issues/rest/api/2/serverInfo",
            serde_json::json!({"deploymentType": "DataCenter", "version": "9.12.0"}),
        )
        .await;

        let entered = format!("{}/issues/browse/OPS-1", server.uri());
        let site = normalize_site(&entered).unwrap();
        assert_eq!(site.host, server.uri());
        let detected = detect(&site).await.unwrap();
        assert_eq!(detected.site, format!("{}/issues", server.uri()));
    }

    #[test]
    fn probe_order_tries_the_stripped_address_then_shallow_prefixes() {
        let site = normalize_site("https://jira.corp.example/tools/browse/X-1").unwrap();
        assert_eq!(
            probe_order(&site),
            [
                "jira.corp.example/tools",
                "jira.corp.example",
                "jira.corp.example/tools/browse",
                "jira.corp.example/tools/browse/X-1",
            ]
        );
    }

    #[test]
    fn probe_order_is_bounded_and_keeps_shallow_context_paths() {
        let site = normalize_site("https://jira.corp.example/jira/a/b/c/d/e").unwrap();
        assert_eq!(
            probe_order(&site),
            [
                "jira.corp.example/jira/a/b/c/d/e",
                "jira.corp.example",
                "jira.corp.example/jira",
                "jira.corp.example/jira/a",
                "jira.corp.example/jira/a/b",
            ]
        );
    }

    #[tokio::test]
    async fn a_deep_board_link_finds_the_context_path() {
        let server = MockServer::start().await;
        server_info(
            &server,
            "/jira/rest/api/2/serverInfo",
            serde_json::json!({"deploymentType": "DataCenter", "version": "9.12.0"}),
        )
        .await;

        let entered = format!("{}/jira/software/projects/ABC/boards/1", server.uri());
        let detected = detect(&normalize_site(&entered).unwrap()).await.unwrap();
        assert_eq!(detected.site, format!("{}/jira", server.uri()));
    }

    #[tokio::test]
    async fn a_path_that_is_not_a_context_path_falls_back_to_the_origin() {
        let server = MockServer::start().await;
        server_info(
            &server,
            "/rest/api/2/serverInfo",
            serde_json::json!({"deploymentType": "DataCenter", "version": "9.12.0"}),
        )
        .await;

        let detected = detect(&normalize_site(&format!("{}/wiki", server.uri())).unwrap())
            .await
            .unwrap();
        assert_eq!(detected.site, server.uri());
    }

    #[tokio::test]
    async fn a_site_that_is_not_jira_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let error = detect(&normalize_site(&server.uri()).unwrap())
            .await
            .unwrap_err();
        assert!(error.contains("HTTP 404"), "{error}");
    }

    #[tokio::test]
    async fn a_login_page_instead_of_json_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>Sign in</html>"))
            .mount(&server)
            .await;
        let error = detect(&normalize_site(&server.uri()).unwrap())
            .await
            .unwrap_err();
        assert!(error.contains("single sign-on"), "{error}");
    }

    #[tokio::test]
    async fn an_unreachable_site_is_an_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let error = detect(&normalize_site(&format!("http://{addr}")).unwrap())
            .await
            .unwrap_err();
        assert!(error.contains("could not connect"), "{error}");
    }
}
