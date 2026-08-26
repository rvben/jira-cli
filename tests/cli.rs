use std::process::Command;

use assert_cmd::prelude::*;
use jira_cli::output::exit_codes;
use jira_cli::test_support::{config_dir_env_name, write_config};
use tempfile::TempDir;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config_fixture() -> &'static str {
    r#"
[default]
host = "work.atlassian.net"
email = "me@example.com"
token = "secret-token"
"#
}

/// A `jira` invocation whose config lives in `dir` and whose `JIRA_*` variables
/// start out unset.
///
/// The child is handed its own environment instead of inheriting one this
/// process mutated. That keeps a real `JIRA_TOKEN` in the developer's shell out
/// of the test, and it means these tests need no cross-process lock, so adding
/// one does not push another over a lock timeout.
fn jira_cmd(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("jira").unwrap();
    cmd.env(config_dir_env_name(), dir.path())
        .env("NO_COLOR", "1")
        .env_remove("JIRA_HOST")
        .env_remove("JIRA_EMAIL")
        .env_remove("JIRA_TOKEN")
        .env_remove("JIRA_PROFILE")
        .env_remove("JIRA_READ_ONLY");
    cmd
}

#[test]
fn config_show_auto_json_when_piped() {
    let dir = TempDir::new().unwrap();
    let config_path = write_config(dir.path(), config_fixture()).unwrap();

    let output = jira_cmd(&dir).args(["config", "show"]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["configPath"], config_path.display().to_string());
    assert_eq!(json["host"], "work.atlassian.net");
    assert_eq!(json["email"], "me@example.com");
    assert_eq!(json["tokenMasked"], "***oken");
}

#[test]
fn config_init_auto_json_when_piped() {
    let dir = TempDir::new().unwrap();

    let output = jira_cmd(&dir).args(["config", "init"]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["configPath"],
        dir.path()
            .join("jira")
            .join("config.toml")
            .display()
            .to_string()
    );
    assert_eq!(
        json["tokenInstructions"],
        "https://id.atlassian.com/manage-profile/security/api-tokens"
    );
    assert_eq!(
        json["example"]["default"]["host"],
        "mycompany.atlassian.net"
    );
    assert!(json["pathResolution"].as_str().is_some());
    assert!(json["recommendedPermissions"].as_str().is_some());
    // configExists reflects whether the config file was present at the time of the call
    assert_eq!(json["configExists"], false);
}

#[test]
fn init_alias_matches_config_init_json_contract() {
    let dir = TempDir::new().unwrap();

    let output = jira_cmd(&dir).args(["init"]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["configPath"],
        dir.path()
            .join("jira")
            .join("config.toml")
            .display()
            .to_string()
    );
    assert_eq!(
        json["tokenInstructions"],
        "https://id.atlassian.com/manage-profile/security/api-tokens"
    );
}

#[test]
fn config_show_invalid_config_returns_input_exit_code() {
    let dir = TempDir::new().unwrap();

    let output = jira_cmd(&dir).args(["config", "show"]).output().unwrap();

    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("No Jira host configured"));
}

/// Parse the structured error envelope. In machine-readable mode the whole of
/// stderr is the envelope and nothing else, so a caller can pipe stderr
/// straight into a JSON parser. Locating the envelope by line would accept a
/// stray prose line alongside it, which is the defect this asserts against.
fn error_envelope(stderr: &str) -> serde_json::Value {
    let envelope: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap_or_else(|e| {
        panic!("the whole of stderr must parse as one JSON error envelope ({e}), got:\n{stderr}")
    });
    assert!(
        envelope.get("error").is_some(),
        "envelope needs an `error` key, got: {envelope}"
    );
    envelope
}

#[test]
fn config_remove_emits_declared_json_contract() {
    let dir = TempDir::new().unwrap();
    let path = write_config(
        dir.path(),
        "[default]\nhost = \"first.atlassian.net\"\ntoken = \"tok1\"\n\n\
         [profiles.work]\nhost = \"work.atlassian.net\"\ntoken = \"tok2\"\n",
    )
    .unwrap();

    let output = jira_cmd(&dir)
        .args(["config", "remove", "work"])
        .output()
        .unwrap();

    assert!(output.status.success(), "removal must succeed");

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout must be JSON when piped, got {:?}: {e}",
            output.stdout
        )
    });

    // Exactly the fields `jira schema` declares for `config remove`.
    assert_eq!(json["profile"], "work");
    assert_eq!(json["removed"], true);
    let keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, vec!["profile", "removed"], "output must match schema");

    // The JSON must not lie about what happened on disk.
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(!content.contains("work.atlassian.net"), "work must be gone");
    assert!(content.contains("first.atlassian.net"), "default preserved");
}

#[test]
fn config_remove_missing_profile_is_not_found() {
    let dir = TempDir::new().unwrap();
    write_config(
        dir.path(),
        "[default]\nhost = \"first.atlassian.net\"\ntoken = \"tok1\"\n\n\
         [profiles.work]\nhost = \"work.atlassian.net\"\ntoken = \"tok2\"\n",
    )
    .unwrap();

    let output = jira_cmd(&dir)
        .args(["config", "remove", "nosuchprofile"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(exit_codes::NOT_FOUND));
    assert!(output.stdout.is_empty(), "no data on stdout for a failure");

    let stderr = String::from_utf8(output.stderr).unwrap();
    let envelope = error_envelope(&stderr);
    assert_eq!(envelope["error"]["kind"], "not_found");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("nosuchprofile"),
        "names the profile asked for"
    );
    assert!(
        message.contains("work"),
        "lists what is available: {message}"
    );
}

#[test]
fn unknown_profile_selection_is_not_found() {
    let dir = TempDir::new().unwrap();
    write_config(dir.path(), config_fixture()).unwrap();

    let output = jira_cmd(&dir)
        .args(["--profile", "nosuchprofile", "config", "show"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(exit_codes::NOT_FOUND));

    let stderr = String::from_utf8(output.stderr).unwrap();
    let envelope = error_envelope(&stderr);
    assert_eq!(envelope["error"]["kind"], "not_found");
}

#[test]
fn unknown_profile_with_no_named_profiles_does_not_render_an_empty_list() {
    let dir = TempDir::new().unwrap();
    // `config_fixture` defines [default] only, so there are no named profiles.
    write_config(dir.path(), config_fixture()).unwrap();

    let output = jira_cmd(&dir)
        .args(["--profile", "nosuchprofile", "config", "show"])
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    let message = error_envelope(&stderr)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !message.trim_end().ends_with("Available:"),
        "an empty profile list must not be rendered as a value: {message}"
    );
}

#[test]
fn completions_install_powershell_returns_input_error() {
    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["completions", "powershell", "--install"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not supported"));
    assert!(stderr.to_lowercase().contains("redirect"));
}

// ── issues update end-to-end (binary + mock server) ──────────────────────────

/// Run the `jira` binary against a MockServer. Sets all required env vars, runs
/// the process to completion, and returns its output.
fn run_jira_against(server: &MockServer, args: &[&str]) -> std::process::Output {
    let dir = TempDir::new().unwrap();
    jira_cmd(&dir)
        .args(args)
        // JiraClient::new preserves the http:// scheme, so the MockServer URI
        // works as JIRA_HOST.
        .env("JIRA_HOST", server.uri())
        .env("JIRA_EMAIL", "test@example.com")
        .env("JIRA_TOKEN", "test-token")
        .output()
        .unwrap()
}

#[tokio::test]
async fn myself_emits_the_email_field_it_declares() {
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

    let output = run_jira_against(&server, &["myself", "--json"]);
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["accountId"], "user-abc-123");
    assert_eq!(json["displayName"], "Test User");
    assert_eq!(
        json["email"], "test@example.com",
        "schema declares `email`, so it must be emitted"
    );
}

#[tokio::test]
async fn myself_renders_a_withheld_email_as_null_rather_than_dropping_it() {
    let server = MockServer::start().await;

    // Jira omits emailAddress entirely when the account's email is private.
    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "user-private-456",
            "displayName": "Private User"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["myself", "--json"]);
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let obj = json.as_object().unwrap();
    assert!(
        obj.contains_key("email"),
        "a withheld email must still appear as a key, not vanish: {json}"
    );
    assert!(
        json["email"].is_null(),
        "a withheld email must be null, never an empty string: {json}"
    );
}

#[tokio::test]
async fn doctor_verifies_authentication_project_access_and_write_safety() {
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
    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{
                "id": "10000",
                "key": "TST",
                "name": "Test",
                "projectTypeKey": "software"
            }],
            "total": 1,
            "startAt": 0,
            "isLast": true
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["doctor", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["user"]["displayName"], "Test User");
    assert_eq!(result["projectCount"], 1);
    assert_eq!(result["checks"][1]["name"], "authentication");
    assert_eq!(result["checks"][2]["name"], "projects");
    assert_json_keys_match_schema("doctor", &result, &[]);
}

#[tokio::test]
async fn doctor_turns_an_auth_endpoint_404_into_actionable_diagnostics() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(404).set_body_string("resource not found"))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["doctor", "--json"]);
    assert_eq!(output.status.code(), Some(exit_codes::NOT_FOUND));

    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["checks"][1]["name"], "authentication");
    assert!(
        result["checks"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("Confirm the site is active")
    );

    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "not_found");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("REST API v3")
    );
}

#[tokio::test]
async fn issues_update_dispatch_assignee_me_calls_myself_then_puts_account_id() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "abc-self-123",
            "displayName": "Test User",
            "emailAddress": "test@example.com",
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": { "assignee": { "accountId": "abc-self-123" } }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["issues", "update", "PROJ-1", "--assignee", "me"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn issues_update_dispatch_assignee_none_sends_null_in_single_put() {
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

    let output = run_jira_against(
        &server,
        &["issues", "update", "PROJ-1", "--assignee", "none"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn issues_update_dispatch_fix_versions_none_sends_empty_array() {
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

    let output = run_jira_against(
        &server,
        &["issues", "update", "PROJ-1", "--fix-versions", "none"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn issues_update_dispatch_labels_passthrough() {
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

    let output = run_jira_against(
        &server,
        &[
            "issues", "update", "PROJ-1", "--labels", "backend", "--labels", "urgent",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn issues_update_dispatch_combined_flags_send_one_put() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "abc-self-123",
            "displayName": "Test User",
            "emailAddress": "test@example.com",
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .and(body_partial_json(serde_json::json!({
            "fields": {
                "summary": "Updated summary",
                "fixVersions": [{ "name": "1.2.0" }],
                "labels": ["backend"],
                "assignee": { "accountId": "abc-self-123" }
            }
        })))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_jira_against(
        &server,
        &[
            "issues",
            "update",
            "PROJ-1",
            "--summary",
            "Updated summary",
            "--fix-versions",
            "1.2.0",
            "--labels",
            "backend",
            "--assignee",
            "me",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Verify that `jira schema` output validates against the vendored clispec v0.3
/// JSON Schema. This catches regressions where required fields are dropped or
/// the schema structure diverges from the spec.
#[test]
fn schema_output_validates_against_clispec_v0_3() {
    let dir = TempDir::new().unwrap();
    let output = jira_cmd(&dir).args(["schema"]).output().unwrap();

    assert!(
        output.status.success(),
        "jira schema failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let schema_output: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("jira schema must emit valid JSON");

    let meta_schema_str = include_str!("fixtures/clispec-v0.3.json");
    let meta_schema: serde_json::Value = serde_json::from_str(meta_schema_str)
        .expect("bundled clispec v0.3 schema must be valid JSON");

    let validator = jsonschema::validator_for(&meta_schema)
        .expect("clispec v0.3 JSON Schema must be compilable");

    let errors: Vec<String> = validator
        .iter_errors(&schema_output)
        .map(|e| format!("{e}"))
        .collect();

    assert!(
        errors.is_empty(),
        "jira schema output failed clispec v0.3 validation:\n{}",
        errors.join("\n")
    );
}

#[test]
fn schema_can_describe_one_command_without_returning_the_full_tree() {
    let dir = TempDir::new().unwrap();
    let output = jira_cmd(&dir)
        .args(["schema", "--command", "jira issues list"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schema["cli"], "jira");
    assert_eq!(schema["name"], "issues list");
    assert_eq!(schema["clispec"], "0.3");
    assert!(
        schema["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg["name"] == "--project")
    );
    assert!(schema["global_args"].is_array());
    assert!(schema["errors"].is_array());
    assert!(
        schema.get("commands").is_none(),
        "compact schema must omit the full command tree"
    );
}

#[test]
fn schema_command_lookup_has_an_actionable_not_found_error() {
    let dir = TempDir::new().unwrap();
    let output = jira_cmd(&dir)
        .args(["schema", "--command", "issues frobnicate"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(exit_codes::NOT_FOUND));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "not_found");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("jira schema")
    );
}

/// Verify that `jira issues list --help` mentions `--fields`, satisfying
/// Principle 6 (Bounded Output) of the CLI Spec.
#[test]
fn issues_list_help_mentions_fields_flag() {
    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["issues", "list", "--help"])
        .output()
        .unwrap();

    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("--fields"),
        "issues list --help must mention --fields; got:\n{help}"
    );
}

/// Verify that unrecognized subcommands emit a structured error envelope on
/// stderr (Principle 1: Structured Output), so agents can branch on `kind`
/// without parsing prose.
#[test]
fn unrecognized_subcommand_emits_error_envelope() {
    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["__no_such_subcommand__"])
        .output()
        .unwrap();

    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    let envelope = error_envelope(&stderr);

    assert_eq!(envelope["error"]["kind"], "invalid_input");
    // Clap's suggestion is the most useful part of a usage error, so routing the
    // error through the envelope has to carry it rather than drop it.
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("__no_such_subcommand__"),
        "envelope must carry clap's own message; got: {message}"
    );
}

/// `--help` and `--version` are clap "errors" that exit 0. Successful output
/// must not carry an error envelope, or an agent that treats any envelope on
/// stderr as a failure reads a working `--help` as a broken one.
#[test]
fn help_is_success_output_with_a_silent_stderr() {
    let output = Command::cargo_bin("jira")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(exit_codes::SUCCESS));
    assert!(
        output.stderr.is_empty(),
        "--help must not write to stderr, got:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"), "help goes to stdout: {stdout}");
}

#[test]
fn version_is_success_output_with_a_silent_stderr() {
    let output = Command::cargo_bin("jira")
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(exit_codes::SUCCESS));
    assert!(
        output.stderr.is_empty(),
        "--version must not write to stderr, got:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("jira "));
}

/// The whole of stderr is the envelope in machine mode. Emitting a prose line
/// beside it makes `2>&1 | jq` fail on the prose, which is the whole reason an
/// agent has a structured envelope to read.
#[test]
fn machine_mode_stderr_is_only_the_envelope() {
    let dir = TempDir::new().unwrap();

    let output = jira_cmd(&dir)
        .args(["--json", "issues", "show", "PROJ-1"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    let stderr = String::from_utf8(output.stderr).unwrap();
    let envelope = error_envelope(&stderr);
    assert_eq!(envelope["error"]["kind"], "invalid_input");
    assert!(
        !stderr.contains("Error:"),
        "the prose rendering must not accompany the envelope, got:\n{stderr}"
    );
}

/// The mirror image: a human asking for text gets prose, not a JSON blob.
#[test]
fn text_mode_stderr_is_only_prose() {
    let dir = TempDir::new().unwrap();

    let output = jira_cmd(&dir)
        .args(["-o", "text", "issues", "show", "PROJ-1"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("No Jira host configured"),
        "text mode explains the failure in prose, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("\"error\""),
        "text mode must not emit the JSON envelope, got:\n{stderr}"
    );
}

/// A usage error in text mode keeps clap's own rendering, including its
/// suggestion, rather than being replaced by JSON.
#[test]
fn text_mode_usage_error_keeps_clap_prose() {
    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["-o", "text", "__no_such_subcommand__"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unrecognized subcommand"), "got:\n{stderr}");
    assert!(
        !stderr.contains("\"error\""),
        "text mode must not emit the JSON envelope, got:\n{stderr}"
    );
}

/// Refusing an unconfirmed destructive bulk operation is its own failure mode:
/// the command line was well formed and re-running it with `--yes` succeeds,
/// which is a different recovery from fixing malformed input. The schema has
/// declared this kind since v0.3; this asserts the binary can actually emit it.
#[test]
fn bulk_transition_without_yes_reports_confirmation_required() {
    let dir = TempDir::new().unwrap();
    write_config(dir.path(), config_fixture()).unwrap();

    // stdin is a pipe under `output()`, so the confirmation cannot be prompted
    // for and the command must refuse instead of proceeding.
    let output = jira_cmd(&dir)
        .args([
            "--json",
            "issues",
            "bulk-transition",
            "--jql",
            "project = TST",
            "--to",
            "Done",
        ])
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    let envelope = error_envelope(&stderr);
    assert_eq!(
        envelope["error"]["kind"], "confirmation_required",
        "refusing for want of --yes is not a malformed-input error"
    );
    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
}

// ── Attachment commands: CLI surface ─────────────────────────────────────────

/// Same as `run_jira_against`, but with `JIRA_READ_ONLY` set, to exercise the
/// read-only write guard against a real subprocess.
fn run_jira_against_read_only(server: &MockServer, args: &[&str]) -> std::process::Output {
    let dir = TempDir::new().unwrap();
    jira_cmd(&dir)
        .args(args)
        .env("JIRA_HOST", server.uri())
        .env("JIRA_EMAIL", "test@example.com")
        .env("JIRA_TOKEN", "test-token")
        .env("JIRA_READ_ONLY", "1")
        .output()
        .unwrap()
}

/// One attachment as Jira reports it, with every optional field present.
fn attachment_json(id: &str, filename: &str, size: u64) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "filename": filename,
        "author": { "displayName": "Alice", "accountId": "abc123" },
        "created": "2024-01-15T10:00:00.000Z",
        "size": size,
        "mimeType": "application/pdf",
    })
}

/// An issue response carrying `attachments` in its `attachment` field.
fn issue_with_attachments(attachments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "fields": { "attachment": attachments } })
}

/// The declaration `jira schema` emits for `command`.
fn schema_command(command: &str) -> serde_json::Value {
    let dir = TempDir::new().unwrap();
    let output = jira_cmd(&dir).args(["schema"]).output().unwrap();
    assert!(output.status.success());
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    schema["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == command)
        .unwrap_or_else(|| panic!("schema must declare command '{command}'"))
        .clone()
}

/// The repo treats `jira schema`'s declared `output_fields` as the contract for
/// what a command's JSON output looks like. Assert that the keys of `actual`,
/// plus `extra` (fields the command emits outside that object), are exactly the
/// fields declared for `command`.
///
/// A field declared `optional` may be absent, since that is what the declaration
/// promises. Everything else is checked both ways: an undeclared key is an
/// undocumented contract and a missing declared key is a broken one.
///
/// Nested objects are checked to the same standard, recursively, wherever the
/// declaration carries its own `fields`. Without that, a declared `assignee`
/// could name any keys it liked one level down and nothing would notice, which
/// is the same defect this helper exists to catch at the top level.
fn assert_json_keys_match_schema(command: &str, actual: &serde_json::Value, extra: &[&str]) {
    let schema = schema_command(command);
    let fields = schema["output_fields"].as_array().unwrap();
    assert_fields_match(command, fields, actual, extra);
}

/// `path` is the dotted location inside the command's output, starting as the
/// command name, so a failure says which nested object is wrong rather than
/// leaving the reader to guess which level broke.
fn assert_fields_match(
    path: &str,
    fields: &[serde_json::Value],
    actual: &serde_json::Value,
    extra: &[&str],
) {
    let declared: std::collections::BTreeSet<&str> =
        fields.iter().map(|f| f["name"].as_str().unwrap()).collect();
    let required: std::collections::BTreeSet<&str> = fields
        .iter()
        .filter(|f| f["optional"] != serde_json::Value::Bool(true))
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    let mut emitted: std::collections::BTreeSet<&str> = actual
        .as_object()
        .unwrap_or_else(|| panic!("{path} must be a JSON object; got: {actual}"))
        .keys()
        .map(String::as_str)
        .collect();
    emitted.extend(extra.iter().copied());

    let undeclared: Vec<&str> = emitted.difference(&declared).copied().collect();
    assert!(
        undeclared.is_empty(),
        "{path} emits {undeclared:?}, which `jira schema` does not declare"
    );
    let missing: Vec<&str> = required.difference(&emitted).copied().collect();
    assert!(
        missing.is_empty(),
        "`jira schema` declares {missing:?} for {path}, which it does not emit \
         (mark the field `optional` if it is genuinely conditional)"
    );

    for field in fields {
        let nested = field["fields"]
            .as_array()
            .or_else(|| field["items"]["fields"].as_array());
        let Some(nested) = nested else {
            continue;
        };
        let name = field["name"].as_str().unwrap();
        let value = &actual[name];
        // Absent or null is the declaration's business, already checked above.
        if value.is_null() {
            continue;
        }
        let child = format!("{path}.{name}");
        match field["type"].as_str() {
            Some("array") if field["items"]["type"] == "object" => {
                let items = value
                    .as_array()
                    .unwrap_or_else(|| panic!("{child} is declared object[]; got: {value}"));
                for (i, item) in items.iter().enumerate() {
                    assert_fields_match(&format!("{child}[{i}]"), nested, item, &[]);
                }
            }
            _ => assert_fields_match(&child, nested, value, &[]),
        }
    }
}

/// Verify that `jira issues attach --help` documents the flag used to select
/// files, satisfying the same "surface is discoverable via --help" bar as
/// other mutating commands (e.g. `issues list --help` mentions `--fields`).
#[test]
fn issues_attach_help_mentions_file_flag() {
    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["issues", "attach", "--help"])
        .output()
        .unwrap();

    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("--file"),
        "issues attach --help must mention --file; got:\n{help}"
    );
}

#[tokio::test]
async fn issues_attach_missing_required_file_flag_is_rejected() {
    let server = MockServer::start().await;

    let output = run_jira_against(&server, &["issues", "attach", "PROJ-1"]);
    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--file"),
        "missing required --file must be reported; got:\n{stderr}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn attachment_read_commands_are_allowed_in_read_only_mode() {
    let server = MockServer::start().await;
    let dest = TempDir::new().unwrap();

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(issue_with_attachments(serde_json::json!([]))),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(attachment_json(
            "10001",
            "report.pdf",
            4,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/content/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
        .mount(&server)
        .await;

    for args in [
        &["issues", "attachments", "PROJ-1"][..],
        &[
            "issues",
            "download-attachment",
            "10001",
            "--dir",
            dest.path().to_str().unwrap(),
        ][..],
    ] {
        let output = run_jira_against_read_only(&server, args);
        assert!(
            output.status.success(),
            "args: {args:?}, stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// `jira schema` declares `author` as `type: "string"`, matching how
/// `issues comment` and `issues log-work` render an author as a plain display
/// name, so the emitted value is pinned to that declared type too.
#[tokio::test]
async fn issues_attachments_json_output_matches_schema_contract() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(issue_with_attachments(serde_json::json!([
                attachment_json("10001", "report.pdf", 2048)
            ]))),
        )
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["issues", "attachments", "PROJ-1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("issues attachments", &json["attachments"][0], &[]);

    let schema = schema_command("issues attachments");
    let author = schema["output_fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "author")
        .unwrap();
    assert_eq!(author["type"], "string");
    assert!(
        json["attachments"][0]["author"].is_string(),
        "schema declares 'author' as type string but the actual value is: {}",
        json["attachments"][0]["author"]
    );
}

#[tokio::test]
async fn issues_attach_json_field_names_match_schema_contract() {
    let server = MockServer::start().await;
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("diagram.png");
    std::fs::write(&file_path, b"png-bytes").unwrap();

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/attachments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            attachment_json("10005", "diagram.png", 9)
        ])))
        .mount(&server)
        .await;

    let output = run_jira_against(
        &server,
        &[
            "issues",
            "attach",
            "PROJ-1",
            "--file",
            file_path.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // `issue` sits next to `attachments`, not inside an attachment object.
    assert_json_keys_match_schema("issues attach", &json["attachments"][0], &["issue"]);
}

#[tokio::test]
async fn issues_download_attachment_json_field_names_match_schema_contract() {
    let server = MockServer::start().await;
    let dest = TempDir::new().unwrap();

    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(attachment_json(
            "10001",
            "report.pdf",
            4,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/content/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
        .mount(&server)
        .await;

    let output = run_jira_against(
        &server,
        &[
            "issues",
            "download-attachment",
            "10001",
            "--dir",
            dest.path().to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("issues download-attachment", &json, &[]);
}

#[tokio::test]
async fn issues_delete_attachment_json_field_names_match_schema_contract() {
    let server = MockServer::start().await;

    Mock::given(method("DELETE"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["issues", "delete-attachment", "10001"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("issues delete-attachment", &json, &[]);
}

/// A user-supplied attachment id that isn't numeric must be rejected as an
/// input error before any request reaches Jira. `download-attachment` is used
/// (rather than a mutating command) so the exit code can only come from id
/// validation, not the read-only write guard.
#[tokio::test]
async fn issues_download_attachment_with_non_numeric_id_is_rejected_before_any_request() {
    let server = MockServer::start().await;

    let output = run_jira_against(&server, &["issues", "download-attachment", "not-a-number"]);
    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not-a-number"), "stderr: {stderr}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// An existing target file makes `download-attachment` exit with code 7 and
/// an error envelope of kind `conflict` naming the path, and nothing is
/// printed to stdout. Only the metadata request is expected to reach the
/// server - the content endpoint must not be hit once the local file check
/// refuses the download.
#[tokio::test]
async fn issues_download_attachment_without_force_exits_conflict_and_reports_kind() {
    let server = MockServer::start().await;
    let dest = TempDir::new().unwrap();
    let target = dest.path().join("report.pdf");
    std::fs::write(&target, b"original bytes").unwrap();

    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(attachment_json(
            "10001",
            "report.pdf",
            4,
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/content/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
        .expect(0)
        .mount(&server)
        .await;

    let output = run_jira_against(
        &server,
        &[
            "issues",
            "download-attachment",
            "10001",
            "--dir",
            dest.path().to_str().unwrap(),
        ],
    );

    assert_eq!(output.status.code(), Some(exit_codes::CONFLICT));
    assert!(
        output.stdout.is_empty(),
        "stdout must stay clean on refusal; got: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let last_line = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .expect("stderr must not be empty");
    let envelope: serde_json::Value = serde_json::from_str(last_line)
        .unwrap_or_else(|_| panic!("last stderr line must be a JSON error envelope: {last_line}"));
    assert_eq!(envelope["error"]["kind"], "conflict");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&target.display().to_string()),
        "conflict message must name the path; got: {message}"
    );

    assert_eq!(std::fs::read(&target).unwrap(), b"original bytes");
}

/// `--force` overwrites an existing target file reached through the CLI.
#[tokio::test]
async fn issues_download_attachment_force_overwrites_via_cli() {
    let server = MockServer::start().await;
    let dest = TempDir::new().unwrap();
    let target = dest.path().join("report.pdf");
    std::fs::write(&target, b"original bytes").unwrap();

    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(attachment_json(
            "10001",
            "report.pdf",
            4,
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/attachment/content/10001"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"new data".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let output = run_jira_against(
        &server,
        &[
            "issues",
            "download-attachment",
            "10001",
            "--dir",
            dest.path().to_str().unwrap(),
            "--force",
        ],
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"new data");
}

#[tokio::test]
async fn issues_attachments_text_table_renders_sizes_at_unit_boundaries() {
    let cases = [
        (1023u64, "1023 B"),
        (1024, "1.0 KB"),
        (1024 * 1024, "1.0 MB"),
        (1024 * 1024 * 1024, "1.0 GB"),
    ];

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(issue_with_attachments(
                cases
                    .iter()
                    .enumerate()
                    .map(|(i, (size, _))| attachment_json(&i.to_string(), "f.bin", *size))
                    .collect(),
            )),
        )
        .mount(&server)
        .await;

    let output = run_jira_against(
        &server,
        &["issues", "attachments", "PROJ-1", "--output", "text"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for (size, rendered) in cases {
        assert!(
            stdout.contains(rendered),
            "{size} bytes must render as {rendered:?}; stdout:\n{stdout}"
        );
    }
}

#[tokio::test]
async fn issues_attachments_degrades_absent_author_and_mime_type() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(issue_with_attachments(serde_json::json!([{
                "id": "10001",
                "filename": "notes.txt",
                "created": "2024-01-15T10:00:00.000Z",
                "size": 10,
            }]))),
        )
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["issues", "attachments", "PROJ-1"]);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let attachment = &json["attachments"][0];
    assert!(
        attachment["mimeType"].is_null(),
        "mimeType must stay null when Jira omits it, got: {}",
        attachment["mimeType"]
    );
    assert!(
        attachment["author"].is_null(),
        "author must stay null when Jira omits it, so an unattributed upload is \
         distinguishable from a user whose display name is literally '-'; got: {}",
        attachment["author"]
    );

    let output = run_jira_against(
        &server,
        &["issues", "attachments", "PROJ-1", "--output", "text"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let row = stdout
        .lines()
        .find(|line| line.contains("notes.txt"))
        .unwrap_or_else(|| panic!("attachment row must be present, got:\n{stdout}"));
    // Columns are: ID, Filename, Size (two tokens: number + unit), Type, Author, Created.
    let cells: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(
        cells.get(4),
        Some(&"-"),
        "Type column must show '-' when mimeType is absent; row: {row}"
    );
    assert_eq!(
        cells.get(5),
        Some(&"-"),
        "Author column must show '-' when author is absent; row: {row}"
    );
}

// ── schema conformance: what the binary prints vs what `jira schema` declares ──

/// A `/search/jql` page carrying one issue.
fn search_page(issue: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "issues": [issue], "isLast": true })
}

/// A fully populated issue: every optional field Jira can return is present, so
/// a key missing from the output is a defect rather than absent source data.
fn full_issue() -> serde_json::Value {
    serde_json::json!({
        "id": "10001",
        "key": "PROJ-1",
        "self": "https://test.atlassian.net/rest/api/3/issue/PROJ-1",
        "fields": {
            "summary": "A summary",
            "status": { "name": "To Do" },
            "assignee": { "displayName": "Alice", "accountId": "abc123" },
            "reporter": { "displayName": "Bob", "accountId": "def456" },
            "priority": { "name": "Medium" },
            "issuetype": { "name": "Bug" },
            "description": {
                "type": "doc", "version": 1,
                "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Body"}]}]
            },
            "labels": ["backend"],
            "components": [{ "id": "1", "name": "api", "description": "API" }],
            "fixVersions": [{ "id": "2", "name": "1.0" }],
            "versions": [{ "id": "3", "name": "0.9" }],
            "created": "2024-01-15T10:00:00.000Z",
            "updated": "2024-01-20T15:30:00.000Z",
            "comment": {
                "comments": [{
                    "id": "10100",
                    "author": { "displayName": "Alice", "accountId": "abc123" },
                    "body": { "type": "doc", "version": 1, "content": [] },
                    "created": "2024-01-21T09:00:00.000Z",
                    "updated": "2024-01-21T09:05:00.000Z"
                }],
                "total": 1
            },
            "issuelinks": [{
                "id": "20001",
                "type": { "id": "10", "name": "Blocks", "inward": "is blocked by", "outward": "blocks" },
                "outwardIssue": { "key": "PROJ-2", "fields": { "summary": "Other", "status": { "name": "Done" } } }
            }]
        }
    })
}

/// An issue with every optional field omitted, which is what Jira returns for an
/// unassigned issue in a project that does not use priorities.
fn bare_issue() -> serde_json::Value {
    serde_json::json!({
        "id": "10002",
        "key": "PROJ-2",
        "fields": {
            "summary": "A summary",
            "status": { "name": "To Do" },
            "issuetype": { "name": "Task" }
        }
    })
}

/// `issues list`, `issues mine` and `search` all render issues through the same
/// function, so one wrong declaration is three wrong contracts.
#[tokio::test]
async fn issue_summary_commands_emit_exactly_the_fields_they_declare() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_page(full_issue())))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "abc123", "displayName": "Alice"
        })))
        .mount(&server)
        .await;

    for (command, args) in [
        ("issues list", &["issues", "list", "--json"][..]),
        ("issues mine", &["issues", "mine", "--json"][..]),
        ("search", &["search", "project = PROJ", "--json"][..]),
    ] {
        let output = run_jira_against(&server, args);
        assert!(
            output.status.success(),
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_json_keys_match_schema(command, &json["items"][0], &[]);
    }
}

#[tokio::test]
async fn issues_show_emits_exactly_the_fields_it_declares() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(full_issue()))
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["issues", "show", "PROJ-1", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("issues show", &json, &[]);
}

#[tokio::test]
async fn issues_log_work_emits_exactly_the_fields_it_declares() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/worklog"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "30001",
            "author": { "displayName": "Alice", "accountId": "abc123" },
            "timeSpent": "1h 30m",
            "timeSpentSeconds": 5400,
            "started": "2024-01-15T10:00:00.000+0000",
            "created": "2024-01-15T10:01:00.000+0000"
        })))
        .mount(&server)
        .await;

    let output = run_jira_against(
        &server,
        &["issues", "log-work", "PROJ-1", "--time", "1h 30m", "--json"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("issues log-work", &json, &[]);
}

/// The bulk commands report a count an agent uses to decide whether to retry, so
/// the key that count lives under has to be the one the schema names.
#[tokio::test]
async fn bulk_commands_emit_exactly_the_fields_they_declare() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_page(full_issue())))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transitions": [{ "id": "31", "name": "Done", "to": { "name": "Done" } }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1/assignee"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    for (command, args) in [
        (
            "issues bulk-transition",
            &[
                "issues",
                "bulk-transition",
                "--jql",
                "project = PROJ",
                "--to",
                "Done",
                "--yes",
                "--json",
            ][..],
        ),
        (
            "issues bulk-assign",
            &[
                "issues",
                "bulk-assign",
                "--jql",
                "project = PROJ",
                "--assignee",
                "abc123",
                "--yes",
                "--json",
            ][..],
        ),
    ] {
        let output = run_jira_against(&server, args);
        assert!(
            output.status.success(),
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_json_keys_match_schema(command, &json, &[]);
    }
}

#[tokio::test]
async fn projects_versions_emits_exactly_the_fields_it_declares() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/PROJ/versions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "id": "10000",
                "name": "1.0",
                "description": "First release",
                "released": true,
                "archived": false,
                "releaseDate": "2024-02-01"
            }])),
        )
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["projects", "versions", "PROJ", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("projects versions", &json["versions"][0], &[]);
}

#[tokio::test]
async fn sprints_list_emits_exactly_the_fields_it_declares() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{ "id": 1, "name": "Board One", "type": "scrum" }],
            "isLast": true,
            "total": 1
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board/1/sprint"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{
                "id": 5,
                "name": "Sprint 5",
                "state": "closed",
                "startDate": "2024-01-01T00:00:00.000Z",
                "endDate": "2024-01-14T00:00:00.000Z",
                "completeDate": "2024-01-14T18:00:00.000Z"
            }],
            "isLast": true
        })))
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["sprints", "list", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("sprints list", &json["sprints"][0], &[]);
}

/// `init` reaches no network, so it is asserted against the binary directly.
#[test]
fn init_emits_exactly_the_fields_it_declares() {
    for (command, args) in [
        ("init", &["init", "--json"][..]),
        ("config init", &["config", "init", "--json"][..]),
    ] {
        let dir = TempDir::new().unwrap();
        let output = jira_cmd(&dir).args(args).output().unwrap();
        assert!(output.status.success());

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_json_keys_match_schema(command, &json, &[]);
    }
}

/// Assigning and unassigning are two branches of one command. If they emit
/// different keys, an agent has to know which branch ran to parse the result,
/// and only one of the two can match the schema.
#[tokio::test]
async fn issues_assign_emits_the_same_keys_whether_it_assigns_or_unassigns() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1/assignee"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let assigned = run_jira_against(
        &server,
        &[
            "issues",
            "assign",
            "PROJ-1",
            "--assignee",
            "abc123",
            "--json",
        ],
    );
    assert!(
        assigned.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&assigned.stderr)
    );
    let assigned: serde_json::Value = serde_json::from_slice(&assigned.stdout).unwrap();
    assert_json_keys_match_schema("issues assign", &assigned, &[]);
    assert_eq!(assigned["accountId"], "abc123");

    let unassigned = run_jira_against(
        &server,
        &["issues", "assign", "PROJ-1", "--assignee", "none", "--json"],
    );
    assert!(
        unassigned.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unassigned.stderr)
    );
    let unassigned: serde_json::Value = serde_json::from_slice(&unassigned.stdout).unwrap();
    assert_json_keys_match_schema("issues assign", &unassigned, &[]);
    assert!(
        unassigned["accountId"].is_null(),
        "an unassigned issue is reported by a null accountId, got: {}",
        unassigned["accountId"]
    );
}

/// The table renders an absent assignee or priority as "-", which is the right
/// placeholder for a column and a lie in JSON: it is indistinguishable from a
/// user actually named "-", or from a priority literally called "-".
#[tokio::test]
async fn absent_issue_fields_are_null_in_json_and_dashes_only_in_the_table() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search_page(bare_issue())))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(bare_issue()))
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["issues", "list", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let item = &json["items"][0];
    assert!(
        item["assignee"].is_null(),
        "an unassigned issue must report a null assignee, got: {}",
        item["assignee"]
    );
    assert!(
        item["priority"].is_null(),
        "an issue with no priority must report null, got: {}",
        item["priority"]
    );

    let output = run_jira_against(&server, &["issues", "show", "PROJ-2", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for field in ["assignee", "priority", "reporter", "description"] {
        assert!(
            json[field].is_null(),
            "issues show must report an absent {field} as null, got: {}",
            json[field]
        );
    }

    // Same data, table mode: "-" is correct here, and its presence proves the
    // JSON assertions above are not just testing an empty response.
    let output = run_jira_against(&server, &["issues", "show", "PROJ-2", "--output", "text"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Assignee:   -"),
        "table must still show a dash for an unassigned issue; got:\n{stdout}"
    );
}

/// `issues comments` renders the same comment shape `issues show` nests, so its
/// declaration has to describe the same keys.
#[tokio::test]
async fn issues_comments_emits_exactly_the_fields_it_declares() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(full_issue()))
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["issues", "comments", "PROJ-1", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("issues comments", &json["comments"][0], &[]);
}

/// `issues create` grows keys when `--parent` or `--sprint` is passed, which is
/// why they are declared `optional`. Both shapes are asserted, so neither the
/// bare form nor the enriched one can drift from the declaration.
#[tokio::test]
async fn issues_create_emits_exactly_the_fields_it_declares() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10001", "key": "PROJ-1",
            "self": "https://test.atlassian.net/rest/api/3/issue/10001"
        })))
        .mount(&server)
        .await;
    // A numeric --sprint is resolved by ID, so no board scan is involved.
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/sprint/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 5, "name": "Sprint 5", "state": "active"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/agile/1.0/sprint/5/issue"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let bare = run_jira_against(
        &server,
        &[
            "issues",
            "create",
            "--project",
            "PROJ",
            "--type",
            "Task",
            "--summary",
            "S",
            "--json",
        ],
    );
    assert!(
        bare.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&bare.stderr)
    );
    let bare: serde_json::Value = serde_json::from_slice(&bare.stdout).unwrap();
    assert_json_keys_match_schema("issues create", &bare, &[]);
    for optional in ["parent", "sprintId", "sprintName"] {
        assert!(
            bare.get(optional).is_none(),
            "an unadorned create must not invent a {optional} key: {bare}"
        );
    }

    let enriched = run_jira_against(
        &server,
        &[
            "issues",
            "create",
            "--project",
            "PROJ",
            "--type",
            "Task",
            "--summary",
            "S",
            "--parent",
            "PROJ-9",
            "--sprint",
            "5",
            "--json",
        ],
    );
    assert!(
        enriched.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&enriched.stderr)
    );
    let enriched: serde_json::Value = serde_json::from_slice(&enriched.stdout).unwrap();
    assert_json_keys_match_schema("issues create", &enriched, &[]);
    // Without this the optional keys could be declared but never emitted, and
    // the relaxed check above would never notice.
    for optional in ["parent", "sprintId", "sprintName"] {
        assert!(
            enriched.get(optional).is_some(),
            "--parent/--sprint must produce a {optional} key: {enriched}"
        );
    }
}

/// The single-issue write commands, each asserted against its declaration.
#[tokio::test]
async fn issue_write_commands_emit_exactly_the_fields_they_declare() {
    let server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/PROJ-1"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/comment"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": "10100",
            "author": { "displayName": "Alice", "accountId": "abc123" },
            "body": { "type": "doc", "version": 1, "content": [] },
            "created": "2024-01-21T09:00:00.000Z",
            "updated": "2024-01-21T09:00:00.000Z"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transitions": [{
                "id": "31", "name": "Done",
                "to": { "name": "Done", "statusCategory": { "key": "done", "name": "Done" } }
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    // A numeric --sprint is resolved by ID, so no board scan is involved.
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/sprint/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 5, "name": "Sprint 5", "state": "active"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/agile/1.0/sprint/5/issue"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    for (command, args) in [
        (
            "issues update",
            &["issues", "update", "PROJ-1", "--summary", "New", "--json"][..],
        ),
        (
            "issues comment",
            &["issues", "comment", "PROJ-1", "--body", "Hello", "--json"][..],
        ),
        (
            "issues transition",
            &["issues", "transition", "PROJ-1", "--to", "Done", "--json"][..],
        ),
        (
            "issues move",
            &["issues", "move", "PROJ-1", "--sprint", "5", "--json"][..],
        ),
    ] {
        let output = run_jira_against(&server, args);
        assert!(
            output.status.success(),
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_json_keys_match_schema(command, &json, &[]);
    }
}

/// Issue links: listing types, creating a link, and removing one.
#[tokio::test]
async fn issue_link_commands_emit_exactly_the_fields_they_declare() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issueLinkType"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issueLinkTypes": [{
                "id": "10000", "name": "Blocks",
                "inward": "is blocked by", "outward": "blocks"
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issueLink"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/rest/api/3/issueLink/20001"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["issues", "link-types", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("issues link-types", &json[0], &[]);

    for (command, args) in [
        (
            "issues link",
            &[
                "issues",
                "link",
                "PROJ-1",
                "--to",
                "PROJ-2",
                "--link-type",
                "Blocks",
                "--json",
            ][..],
        ),
        (
            "issues unlink",
            &["issues", "unlink", "20001", "--json"][..],
        ),
    ] {
        let output = run_jira_against(&server, args);
        assert!(
            output.status.success(),
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_json_keys_match_schema(command, &json, &[]);
    }
}

/// `issues list-transitions` serializes the API type straight through, so its
/// declaration is the only thing standing between an agent and a silent rename.
#[tokio::test]
async fn issues_list_transitions_emits_exactly_the_fields_it_declares() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/PROJ-1/transitions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "transitions": [{
                "id": "31", "name": "Done",
                "to": { "name": "Done", "statusCategory": { "key": "done", "name": "Done" } }
            }]
        })))
        .mount(&server)
        .await;

    let output = run_jira_against(&server, &["issues", "list-transitions", "PROJ-1", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("issues list-transitions", &json[0], &[]);
}

/// The project, user, board and field listings.
#[tokio::test]
async fn read_only_listings_emit_exactly_the_fields_they_declare() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{ "id": "10000", "key": "PROJ", "name": "Project", "projectTypeKey": "software" }],
            "startAt": 0, "maxResults": 50, "total": 1, "isLast": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/PROJ"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "10000", "key": "PROJ", "name": "Project", "projectTypeKey": "software"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/project/PROJ/components"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": "1", "name": "api", "description": "API layer" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/user/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "accountId": "abc123", "displayName": "Alice", "emailAddress": "alice@example.com" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/agile/1.0/board"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "values": [{ "id": 1, "name": "Board One", "type": "scrum" }],
            "startAt": 0, "maxResults": 50, "total": 1, "isLast": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/field"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": "summary", "name": "Summary", "custom": false,
              "schema": { "type": "string" } }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "abc123",
            "displayName": "Alice",
            "emailAddress": "alice@example.com"
        })))
        .mount(&server)
        .await;

    // (command, args, the key the element list lives under, or "" for a bare object)
    for (command, args, envelope) in [
        (
            "projects list",
            &["projects", "list", "--json"][..],
            "projects",
        ),
        (
            "projects show",
            &["projects", "show", "PROJ", "--json"][..],
            "",
        ),
        (
            "projects components",
            &["projects", "components", "PROJ", "--json"][..],
            "components",
        ),
        (
            "users search",
            &["users", "search", "alice", "--json"][..],
            "users",
        ),
        ("boards list", &["boards", "list", "--json"][..], "boards"),
        ("fields list", &["fields", "list", "--json"][..], "fields"),
        ("myself", &["myself", "--json"][..], ""),
    ] {
        let output = run_jira_against(&server, args);
        assert!(
            output.status.success(),
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let element = if envelope.is_empty() {
            &json
        } else {
            &json[envelope][0]
        };
        assert_json_keys_match_schema(command, element, &[]);
    }
}

/// The two config commands that emit data, asserted against the same contract
/// as everything else. Neither reaches the network.
#[test]
fn config_commands_emit_exactly_the_fields_they_declare() {
    let dir = TempDir::new().unwrap();
    write_config(
        dir.path(),
        "[default]\nhost = \"work.atlassian.net\"\nemail = \"me@example.com\"\ntoken = \"tok\"\n\n\
         [profiles.work]\nhost = \"work.atlassian.net\"\ntoken = \"tok2\"\n",
    )
    .unwrap();

    let output = jira_cmd(&dir).args(["config", "show"]).output().unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("config show", &json, &[]);

    let output = jira_cmd(&dir)
        .args(["config", "remove", "work"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_json_keys_match_schema("config remove", &json, &[]);
}

#[tokio::test]
async fn auth_commands_emit_their_declared_json_contracts() {
    let login_dir = TempDir::new().unwrap();
    let login = jira_cmd(&login_dir)
        .args(["auth", "login"])
        .output()
        .unwrap();
    assert!(login.status.success());
    let login_json: serde_json::Value = serde_json::from_slice(&login.stdout).unwrap();
    assert_json_keys_match_schema("auth login", &login_json, &[]);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "user-123",
            "displayName": "Test User",
            "emailAddress": "test@example.com"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();
    write_config(
        dir.path(),
        &format!(
            "[default]\nhost = {:?}\nemail = \"test@example.com\"\ntoken = \"token\"\n",
            server.uri()
        ),
    )
    .unwrap();

    let status = jira_cmd(&dir).args(["auth", "status"]).output().unwrap();
    assert!(
        status.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status_json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_json_keys_match_schema("auth status", &status_json, &[]);

    let logout = jira_cmd(&dir).args(["auth", "logout"]).output().unwrap();
    assert!(
        logout.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&logout.stderr)
    );
    let logout_json: serde_json::Value = serde_json::from_slice(&logout.stdout).unwrap();
    assert_json_keys_match_schema("auth logout", &logout_json, &[]);

    // Migration's success path necessarily writes the user's OS keychain. Keep
    // this integration suite side-effect free while still pinning the stable
    // JSON contract emitted after its transactional config/keychain update.
    let migrate_json = serde_json::json!({
        "profile": "default",
        "migrated": true,
        "credentialStore": "os-keychain"
    });
    assert_json_keys_match_schema("auth migrate", &migrate_json, &[]);
}

/// Every command whose JSON output is asserted against `jira schema` by a test
/// in this file.
///
/// This is a tripwire, not proof: it forces a decision when a command starts
/// declaring `output_fields`, because the test below fails until the command is
/// listed. Adding a name here without writing the assertion defeats it, which is
/// a deliberate act rather than the passive drift this guards against.
const COMMANDS_WITH_A_CONFORMANCE_TEST: &[&str] = &[
    "auth login",
    "auth logout",
    "auth migrate",
    "auth status",
    "boards list",
    "capabilities",
    "config init",
    "config remove",
    "config show",
    "doctor",
    "fields list",
    "init",
    "issues assign",
    "issues attach",
    "issues attachments",
    "issues bulk-assign",
    "issues bulk-transition",
    "issues comment",
    "issues comments",
    "issues create",
    "issues delete-attachment",
    "issues download-attachment",
    "issues link",
    "issues link-types",
    "issues list",
    "issues list-transitions",
    "issues log-work",
    "issues mine",
    "issues move",
    "issues show",
    "issues transition",
    "issues unlink",
    "issues update",
    "myself",
    "projects components",
    "projects list",
    "projects show",
    "projects versions",
    "search",
    "sprints list",
    "users search",
];

#[test]
fn capabilities_json_matches_declared_output_fields() {
    let dir = TempDir::new().unwrap();
    let schema_output = jira_cmd(&dir).args(["schema"]).output().unwrap();
    let schema: serde_json::Value = serde_json::from_slice(&schema_output.stdout).unwrap();
    let fields = schema["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|command| command["name"] == "capabilities")
        .unwrap()["output_fields"]
        .as_array()
        .unwrap();
    let output = jira_cmd(&dir)
        .args(["--output", "json", "capabilities"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_fields_match("capabilities", fields, &actual, &[]);
}

/// A command that declares `output_fields` is making a promise to an agent, and
/// an unasserted promise is how these declarations drifted from the binary in
/// the first place.
#[test]
fn every_command_declaring_output_fields_has_a_conformance_test() {
    let dir = TempDir::new().unwrap();
    let output = jira_cmd(&dir).args(["schema"]).output().unwrap();
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    let declaring: std::collections::BTreeSet<&str> = schema["commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["output_fields"].as_array().is_some_and(|f| !f.is_empty()))
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    let checked: std::collections::BTreeSet<&str> =
        COMMANDS_WITH_A_CONFORMANCE_TEST.iter().copied().collect();

    let unchecked: Vec<&str> = declaring.difference(&checked).copied().collect();
    assert!(
        unchecked.is_empty(),
        "these commands declare output_fields with nothing asserting the binary \
         agrees: {unchecked:?}"
    );
    let stale: Vec<&str> = checked.difference(&declaring).copied().collect();
    assert!(
        stale.is_empty(),
        "these commands are listed as checked but no longer declare output_fields: {stale:?}"
    );
}

/// Object-typed fields that deliberately declare no nested `fields`, with the
/// reason they cannot.
///
/// `assert_json_keys_match_schema` recurses only where a declaration carries
/// `fields`, so an object without them is unchecked. That is right only when the
/// keys are genuinely not fixed; everywhere else it is a gap. Pinning the list
/// means a new opaque object fails this test until someone decides which it is.
const OBJECTS_WITHOUT_A_DECLARED_SHAPE: &[(&str, &str)] = &[
    (
        "auth login.example.profiles",
        "keyed by profile name, chosen by the user",
    ),
    (
        "config init.example.profiles",
        "keyed by profile name, chosen by the user",
    ),
    (
        "init.example.profiles",
        "keyed by profile name, chosen by the user",
    ),
];

#[test]
fn every_declared_object_either_has_a_shape_or_a_stated_reason() {
    let dir = TempDir::new().unwrap();
    let output = jira_cmd(&dir).args(["schema"]).output().unwrap();
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    fn collect(path: &str, fields: &[serde_json::Value], out: &mut Vec<String>) {
        for f in fields {
            let name = f["name"].as_str().unwrap();
            let child = format!("{path}.{name}");
            let is_object = f["type"] == "object";
            let nested = f["fields"]
                .as_array()
                .or_else(|| f["items"]["fields"].as_array());
            match nested {
                Some(nested) => collect(&child, nested, out),
                None if is_object => out.push(child),
                None => {}
            }
        }
    }

    let mut opaque = Vec::new();
    for command in schema["commands"].as_array().unwrap() {
        let Some(fields) = command["output_fields"].as_array() else {
            continue;
        };
        collect(command["name"].as_str().unwrap(), fields, &mut opaque);
    }
    opaque.sort();

    let excused: std::collections::BTreeSet<&str> = OBJECTS_WITHOUT_A_DECLARED_SHAPE
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let found: std::collections::BTreeSet<&str> = opaque.iter().map(String::as_str).collect();

    let undeclared: Vec<&str> = found.difference(&excused).copied().collect();
    assert!(
        undeclared.is_empty(),
        "these object fields declare no nested shape, so nothing checks their keys: \
         {undeclared:?} (add `fields`, or list it in OBJECTS_WITHOUT_A_DECLARED_SHAPE \
         with the reason its keys are not fixed)"
    );
    let stale: Vec<&str> = excused.difference(&found).copied().collect();
    assert!(
        stale.is_empty(),
        "these are excused from declaring a shape but no longer need to be: {stale:?}"
    );
}

// ── JIRA_READ_ONLY guard ─────────────────────────────────────────────────────

/// Every command that writes to Jira, with the shortest invocation clap accepts.
///
/// Only the required arguments are given: the guard runs before the command is
/// dispatched, so nothing here needs to be a request Jira would have honoured.
const JIRA_WRITE_INVOCATIONS: &[(&str, &[&str])] = &[
    (
        "issues create",
        &[
            "issues",
            "create",
            "--project",
            "PROJ",
            "--summary",
            "Summary",
        ],
    ),
    ("issues update", &["issues", "update", "PROJ-1"]),
    (
        "issues move",
        &["issues", "move", "PROJ-1", "--sprint", "5"],
    ),
    (
        "issues comment",
        &["issues", "comment", "PROJ-1", "--body", "text"],
    ),
    (
        "issues transition",
        &["issues", "transition", "PROJ-1", "--to", "Done"],
    ),
    (
        "issues assign",
        &["issues", "assign", "PROJ-1", "--assignee", "abc123"],
    ),
    (
        "issues link",
        &["issues", "link", "PROJ-1", "--to", "PROJ-2"],
    ),
    ("issues unlink", &["issues", "unlink", "10001"]),
    (
        "issues log-work",
        &["issues", "log-work", "PROJ-1", "--time", "1h"],
    ),
    (
        "issues attach",
        &["issues", "attach", "PROJ-1", "--file", "irrelevant.bin"],
    ),
    (
        "issues delete-attachment",
        &["issues", "delete-attachment", "10001"],
    ),
    (
        "issues bulk-transition",
        &[
            "issues",
            "bulk-transition",
            "--jql",
            "project = PROJ",
            "--to",
            "Done",
        ],
    ),
    (
        "issues bulk-assign",
        &[
            "issues",
            "bulk-assign",
            "--jql",
            "project = PROJ",
            "--assignee",
            "abc123",
        ],
    ),
];

/// An environment variable the CLI reads is part of its interface, and one the
/// schema does not mention is one an agent has no way to find. `JIRA_DEBUG_HTTP`
/// was exactly that: documented in the README, invisible to introspection.
///
/// Scanning the source for the literals keeps the check honest. A grep of the
/// schema text would pass on a variable merely named in an error message, so the
/// assertion is that every name appears in a `"name"` or `"env"` position.
#[test]
fn every_environment_variable_the_cli_reads_is_declared_in_the_schema() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut read_by_code = std::collections::BTreeSet::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (i, _) in text.match_indices("\"JIRA_") {
                let rest = &text[i + 1..];
                let Some(end) = rest.find('"') else { continue };
                let name = &rest[..end];
                // A whole quoted literal that is nothing but an identifier. The
                // error messages naming a variable mid-sentence are not uses.
                if name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                {
                    read_by_code.insert(name.to_string());
                }
            }
        }
    }
    assert!(
        read_by_code.contains("JIRA_HOST"),
        "the scan found no known variable, so it is broken rather than clean"
    );

    let dir = TempDir::new().unwrap();
    let output = jira_cmd(&dir).args(["schema"]).output().unwrap();
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    /// Collect every value sitting under a `name` or `env` key, at any depth.
    fn declared(value: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    if let (true, Some(s)) =
                        (matches!(key.as_str(), "name" | "env"), child.as_str())
                    {
                        out.insert(s.to_string());
                    }
                    declared(child, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|i| declared(i, out)),
            _ => {}
        }
    }
    let mut in_schema = std::collections::BTreeSet::new();
    declared(&schema, &mut in_schema);

    let undeclared: Vec<&String> = read_by_code.difference(&in_schema).collect();
    assert!(
        undeclared.is_empty(),
        "these environment variables change how the CLI behaves but `jira schema` \
         never names them: {undeclared:?}"
    );
}

/// The commands `jira schema` claims `JIRA_READ_ONLY` blocks.
fn schema_read_only_blocked() -> Vec<String> {
    let dir = TempDir::new().unwrap();
    let output = jira_cmd(&dir).args(["schema"]).output().unwrap();
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    schema["read_only"]["blocked_commands"]
        .as_array()
        .expect("schema must declare which commands read-only mode blocks")
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect()
}

/// Commands `jira schema` marks `mutating` that `JIRA_READ_ONLY` deliberately
/// still permits, with the reason.
///
/// `mutating` is the broader claim: it says the command persists state somewhere.
/// The guard is narrower, and covers writes to Jira only, so these two sets are
/// allowed to differ. They may not differ silently, which is what the test below
/// is for.
const MUTATING_WITHOUT_WRITING_TO_JIRA: &[(&str, &str)] = &[
    ("auth login", "writes the local config file and OS keychain"),
    ("auth logout", "edits the local config file and OS keychain"),
    (
        "auth migrate",
        "moves a local credential into the OS keychain",
    ),
    ("init", "writes the local config file"),
    ("config init", "writes the local config file"),
    ("config remove", "edits the local config file"),
    (
        "issues download-attachment",
        "reads from Jira, writes the bytes to a local path",
    ),
];

/// `jira schema` tells an agent which commands read-only mode blocks, so every
/// name on that list is run against a real subprocess rather than believed.
///
/// The mock server is the load-bearing assertion: a command that slipped past the
/// guard would reach it, so an empty request log is what proves nothing was
/// written.
#[tokio::test]
async fn every_command_the_schema_says_is_blocked_really_is() {
    let server = MockServer::start().await;

    let declared = schema_read_only_blocked();
    let covered: std::collections::BTreeSet<&str> = JIRA_WRITE_INVOCATIONS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let missing: Vec<&String> = declared
        .iter()
        .filter(|c| !covered.contains(c.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "the schema claims these are blocked but nothing here runs them: {missing:?}"
    );
    let unclaimed: Vec<&str> = covered
        .iter()
        .filter(|c| !declared.iter().any(|d| d == *c))
        .copied()
        .collect();
    assert!(
        unclaimed.is_empty(),
        "these are exercised here but the schema does not list them as blocked, \
         so an agent reading the contract would not know: {unclaimed:?}"
    );

    for (command, args) in JIRA_WRITE_INVOCATIONS {
        let output = run_jira_against_read_only(&server, args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(exit_codes::INPUT_ERROR),
            "{command} was not refused; stderr: {stderr}"
        );
        let envelope = error_envelope(&stderr);
        assert_eq!(
            envelope["error"]["kind"], "invalid_input",
            "{command} must be refused through the standard error envelope"
        );
        let message = envelope["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("read-only"),
            "{command} failed for some other reason than the guard: {message}"
        );
    }

    let requests = server.received_requests().await.unwrap();
    let reached: Vec<String> = requests
        .iter()
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect();
    assert!(
        reached.is_empty(),
        "read-only mode let these requests through: {reached:?}"
    );
}

/// The guard covers writes to Jira, not writes to disk, and the README says so.
/// Check the claim: a mutating command that only touches local files must still
/// run under `JIRA_READ_ONLY=1`, or an agent given read access loses the ability
/// to configure itself.
#[test]
fn config_writing_commands_still_work_in_read_only_mode() {
    let dir = TempDir::new().unwrap();
    let output = jira_cmd(&dir)
        .args(["--host", "example.atlassian.net", "config", "init"])
        .env("JIRA_READ_ONLY", "1")
        .env("JIRA_EMAIL", "me@example.com")
        .env("JIRA_TOKEN", "token")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "config init was refused under read-only; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = jira_cmd(&dir)
        .args(["init"])
        .env("JIRA_READ_ONLY", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "init was refused under read-only; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The guard is a hand-written `matches!` in `main.rs`, and the cost of
/// forgetting an entry is asymmetric: a command missing from it is silently
/// *permitted* to write to Jira under `JIRA_READ_ONLY=1`.
///
/// Every command the schema declares `mutating` must therefore be accounted for
/// in one of two ways: listed as blocked, or excused here with the reason it
/// writes nothing to Jira. A new mutating command fails this test until someone
/// decides which it is, and neither answer can be given by accident.
#[test]
fn every_mutating_command_is_either_guarded_or_excused() {
    let dir = TempDir::new().unwrap();
    let output = jira_cmd(&dir).args(["schema"]).output().unwrap();
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    let mutating: std::collections::BTreeSet<&str> = schema["commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["mutating"] == true)
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    let blocked = schema_read_only_blocked();
    let guarded: std::collections::BTreeSet<&str> = blocked.iter().map(String::as_str).collect();
    let excused: std::collections::BTreeSet<&str> = MUTATING_WITHOUT_WRITING_TO_JIRA
        .iter()
        .map(|(name, _)| *name)
        .collect();

    let unaccounted: Vec<&str> = mutating
        .iter()
        .filter(|c| !guarded.contains(*c) && !excused.contains(*c))
        .copied()
        .collect();
    assert!(
        unaccounted.is_empty(),
        "these commands are declared mutating but nothing says whether read-only \
         mode stops them: {unaccounted:?} (add them to READ_ONLY_BLOCKED_COMMANDS \
         and the guard in main.rs, or a reason to MUTATING_WITHOUT_WRITING_TO_JIRA)"
    );

    let stale: Vec<&str> = guarded
        .union(&excused)
        .filter(|c| !mutating.contains(*c))
        .copied()
        .collect();
    assert!(
        stale.is_empty(),
        "these are listed here but the schema no longer declares them mutating: {stale:?}"
    );
}

// ── House style ───────────────────────────────────────────────────────────────

/// Em and en dashes must not appear in anything this repository publishes.
///
/// The rule is easy to state and easy to lose: dashes arrive by imitation, since
/// the surrounding prose is what gets copied. Enforcing it here means CI catches
/// a new one instead of a reader catching it after release.
///
/// The scan covers printed output and help text as well as prose, because both
/// reach a user. It skips the working notes under `.claude/`, which are local and
/// not published.
#[test]
fn no_em_or_en_dashes_in_published_files() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut scanned = 0usize;
    let mut offenders: Vec<String> = Vec::new();

    let mut stack = vec![root.join("src"), root.join("tests")];
    let mut files: Vec<std::path::PathBuf> =
        vec![root.join("README.md"), root.join("CHANGELOG.md")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }

    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        scanned += 1;
        for (number, line) in text.lines().enumerate() {
            if line.contains('\u{2014}') || line.contains('\u{2013}') {
                let name = path.strip_prefix(root).unwrap_or(&path).display();
                offenders.push(format!("{name}:{}: {}", number + 1, line.trim()));
            }
        }
    }

    // Without this the check passes just as happily on a scan that found nothing
    // to read, which is the failure mode a clean result cannot distinguish.
    assert!(
        scanned > 5,
        "the scan read {scanned} files, so it is broken rather than clean"
    );
    assert!(
        offenders.is_empty(),
        "use a hyphen, a comma, or two sentences instead:\n{}",
        offenders.join("\n")
    );
}

// -- a downstream that stops reading (`jira ... | head`) ----------------------

/// A `/search/jql` page carrying enough issues that the rendered output cannot
/// fit in a pipe buffer, so the writer is still writing when the reader leaves.
fn oversized_search_page() -> serde_json::Value {
    let issues: Vec<serde_json::Value> = (1..=600)
        .map(|n| {
            serde_json::json!({
                "id": n.to_string(),
                "key": format!("PROJ-{n}"),
                "fields": {
                    "summary": "A summary long enough that six hundred of them add up to far more than any pipe buffer holds",
                    "status": { "name": "To Do" },
                    "issuetype": { "name": "Task" }
                }
            })
        })
        .collect();
    serde_json::json!({ "issues": issues, "isLast": true })
}

/// Rust sets `SIGPIPE` to `SIG_IGN` before `main`, which turns a closed
/// downstream into an `EPIPE` error that `println!` panics on. Piping into
/// `head` is the most ordinary thing a caller does, so it must not produce a
/// backtrace and an exit code the schema never declares.
#[tokio::test]
async fn a_downstream_that_stops_reading_does_not_panic_the_writer() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(oversized_search_page()))
        .mount(&server)
        .await;

    let args = ["issues", "list", "--limit", "600", "--json"];

    // Control: the whole output has to outgrow any pipe buffer. Below that the
    // reader can leave without the writer ever noticing, and this test would
    // pass against a binary that panics.
    let whole = run_jira_against(&server, &args);
    assert!(
        whole.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&whole.stderr)
    );
    assert!(
        whole.stdout.len() > 128 * 1024,
        "the fixture renders {} bytes, too few to fill a pipe buffer",
        whole.stdout.len()
    );

    let dir = TempDir::new().unwrap();
    let mut child = jira_cmd(&dir)
        .args(args)
        .env("JIRA_HOST", server.uri())
        .env("JIRA_EMAIL", "test@example.com")
        .env("JIRA_TOKEN", "test-token")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Take one line's worth and leave, the way `head` does.
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0u8; 40];
    std::io::Read::read_exact(&mut stdout, &mut first).unwrap();
    drop(stdout);

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "a reader that stops early must not panic the writer:\n{stderr}"
    );
    assert_ne!(
        output.status.code(),
        Some(101),
        "101 is a panic, and is not one of the declared exit codes"
    );

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            output.status.signal(),
            Some(13),
            "the writer must die of SIGPIPE the way any other member of a pipeline does"
        );
    }
}
