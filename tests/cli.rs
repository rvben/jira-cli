use std::process::Command;

use assert_cmd::prelude::*;
use jira_cli::output::exit_codes;
use jira_cli::test_support::{EnvVarGuard, ProcessEnvLock, set_config_dir_env, write_config};
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

#[test]
fn config_show_auto_json_when_piped() {
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    let config_path = write_config(dir.path(), config_fixture()).unwrap();
    let _config_dir = set_config_dir_env(dir.path());
    let _host = EnvVarGuard::unset("JIRA_HOST");
    let _email = EnvVarGuard::unset("JIRA_EMAIL");
    let _token = EnvVarGuard::unset("JIRA_TOKEN");
    let _profile = EnvVarGuard::unset("JIRA_PROFILE");

    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["config", "show"])
        .output()
        .unwrap();

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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    let _config_dir = set_config_dir_env(dir.path());

    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["config", "init"])
        .output()
        .unwrap();

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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    let _config_dir = set_config_dir_env(dir.path());

    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["init"])
        .output()
        .unwrap();

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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    let _config_dir = set_config_dir_env(dir.path());
    let _host = EnvVarGuard::unset("JIRA_HOST");
    let _email = EnvVarGuard::unset("JIRA_EMAIL");
    let _token = EnvVarGuard::unset("JIRA_TOKEN");
    let _profile = EnvVarGuard::unset("JIRA_PROFILE");

    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["config", "show"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(exit_codes::INPUT_ERROR));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("No Jira host configured"));
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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    // All `_`-prefixed guards below MUST remain in scope past `.output()`.
    // They are RAII guards that restore the prior environment on drop, and
    // their drop point is the end of this function — moving any of them
    // (or the `Command::output()` call) into a separate statement would
    // release the env vars before the child process inherits them.
    let _config_dir = set_config_dir_env(dir.path());
    // JiraClient::new preserves the http:// scheme, so the MockServer URI works as JIRA_HOST.
    let host = server.uri();
    let _host = EnvVarGuard::set("JIRA_HOST", &host);
    let _email = EnvVarGuard::set("JIRA_EMAIL", "test@example.com");
    let _token = EnvVarGuard::set("JIRA_TOKEN", "test-token");
    let _profile = EnvVarGuard::unset("JIRA_PROFILE");
    Command::cargo_bin("jira")
        .unwrap()
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
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

/// Verify that `jira schema` output validates against the vendored clispec v0.2
/// JSON Schema. This catches regressions where required fields are dropped or
/// the schema structure diverges from the spec.
#[test]
fn schema_output_validates_against_clispec_v0_2() {
    use jira_cli::test_support::{ProcessEnvLock, unset_config_dir_env};

    let _env = ProcessEnvLock::acquire().unwrap();
    let _config_dir = unset_config_dir_env();

    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["schema"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "jira schema failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let schema_output: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("jira schema must emit valid JSON");

    let meta_schema_str = include_str!("fixtures/clispec-v0.2.json");
    let meta_schema: serde_json::Value = serde_json::from_str(meta_schema_str)
        .expect("bundled clispec v0.2 schema must be valid JSON");

    let validator = jsonschema::validator_for(&meta_schema)
        .expect("clispec v0.2 JSON Schema must be compilable");

    let errors: Vec<String> = validator
        .iter_errors(&schema_output)
        .map(|e| format!("{e}"))
        .collect();

    assert!(
        errors.is_empty(),
        "jira schema output failed clispec v0.2 validation:\n{}",
        errors.join("\n")
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
    let last_line = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .expect("stderr must not be empty");

    let envelope: serde_json::Value = serde_json::from_str(last_line).unwrap_or_else(|_| {
        panic!("last line of stderr must be a JSON error envelope; got: {last_line:?}")
    });

    assert!(
        envelope["error"]["kind"].as_str().is_some(),
        "error envelope must contain a 'kind' field; got: {envelope}"
    );
}
