//! `--fields` accepts only the names a listing can output.

use super::*;
use serde_json::Value;
use wiremock::matchers::any;

/// The error message from the JSON error envelope on stderr.
fn stderr(output: &std::process::Output) -> String {
    let envelope: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stderr)));
    envelope["error"]["message"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn unknown_fields_names_fail_before_any_request() {
    for args in [
        &[
            "search",
            "project = PROJ",
            "--fields",
            "key,parnet",
            "--json",
        ][..],
        &["issues", "list", "--fields", "parnet", "--json"][..],
        &["issues", "mine", "--fields", "key, bogus ,parent", "--json"][..],
    ] {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let output = run_jira_against(&server, args);
        assert_eq!(
            output.status.code(),
            Some(exit_codes::INPUT_ERROR),
            "{args:?}"
        );
        let err = stderr(&output);
        assert!(err.contains("Unknown --fields name(s)"), "{err}");
        assert!(
            err.contains("key, id, url, summary"),
            "valid names must be listed: {err}"
        );
        assert!(
            !err.contains("\"key\""),
            "a valid name is not reported as unknown: {err}"
        );
    }
}

/// An empty filter would otherwise read as "no filter" and print everything.
#[tokio::test]
async fn empty_fields_filters_are_rejected() {
    for value in ["", " , ,"] {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let output = run_jira_against(&server, &["issues", "list", "--fields", value, "--json"]);
        assert_eq!(
            output.status.code(),
            Some(exit_codes::INPUT_ERROR),
            "{value:?}"
        );
        assert!(stderr(&output).contains("--fields needs at least one name"));
    }
}
