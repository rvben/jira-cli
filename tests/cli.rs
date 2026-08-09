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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    let path = write_config(
        dir.path(),
        "[default]\nhost = \"first.atlassian.net\"\ntoken = \"tok1\"\n\n\
         [profiles.work]\nhost = \"work.atlassian.net\"\ntoken = \"tok2\"\n",
    )
    .unwrap();
    let _config_dir = set_config_dir_env(dir.path());

    let output = Command::cargo_bin("jira")
        .unwrap()
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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    write_config(
        dir.path(),
        "[default]\nhost = \"first.atlassian.net\"\ntoken = \"tok1\"\n\n\
         [profiles.work]\nhost = \"work.atlassian.net\"\ntoken = \"tok2\"\n",
    )
    .unwrap();
    let _config_dir = set_config_dir_env(dir.path());

    let output = Command::cargo_bin("jira")
        .unwrap()
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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    write_config(dir.path(), config_fixture()).unwrap();
    let _config_dir = set_config_dir_env(dir.path());
    let _host = EnvVarGuard::unset("JIRA_HOST");
    let _email = EnvVarGuard::unset("JIRA_EMAIL");
    let _token = EnvVarGuard::unset("JIRA_TOKEN");
    let _profile = EnvVarGuard::unset("JIRA_PROFILE");

    let output = Command::cargo_bin("jira")
        .unwrap()
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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    // `config_fixture` defines [default] only, so there are no named profiles.
    write_config(dir.path(), config_fixture()).unwrap();
    let _config_dir = set_config_dir_env(dir.path());
    let _profile = EnvVarGuard::unset("JIRA_PROFILE");

    let output = Command::cargo_bin("jira")
        .unwrap()
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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    let _config_dir = set_config_dir_env(dir.path());
    let _host = EnvVarGuard::unset("JIRA_HOST");
    let _email = EnvVarGuard::unset("JIRA_EMAIL");
    let _token = EnvVarGuard::unset("JIRA_TOKEN");
    let _profile = EnvVarGuard::unset("JIRA_PROFILE");

    let output = Command::cargo_bin("jira")
        .unwrap()
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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    let _config_dir = set_config_dir_env(dir.path());
    let _host = EnvVarGuard::unset("JIRA_HOST");
    let _email = EnvVarGuard::unset("JIRA_EMAIL");
    let _token = EnvVarGuard::unset("JIRA_TOKEN");
    let _profile = EnvVarGuard::unset("JIRA_PROFILE");

    let output = Command::cargo_bin("jira")
        .unwrap()
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
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    write_config(dir.path(), config_fixture()).unwrap();
    let _config_dir = set_config_dir_env(dir.path());
    let _host = EnvVarGuard::unset("JIRA_HOST");
    let _email = EnvVarGuard::unset("JIRA_EMAIL");
    let _token = EnvVarGuard::unset("JIRA_TOKEN");
    let _profile = EnvVarGuard::unset("JIRA_PROFILE");

    // stdin is a pipe under `output()`, so the confirmation cannot be prompted
    // for and the command must refuse instead of proceeding.
    let output = Command::cargo_bin("jira")
        .unwrap()
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
