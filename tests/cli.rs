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

// ── Attachment commands: CLI surface ─────────────────────────────────────────

/// Same as `run_jira_against`, but with `JIRA_READ_ONLY` set, to exercise the
/// read-only write guard against a real subprocess.
fn run_jira_against_read_only(server: &MockServer, args: &[&str]) -> std::process::Output {
    let _env = ProcessEnvLock::acquire().unwrap();
    let dir = TempDir::new().unwrap();
    let _config_dir = set_config_dir_env(dir.path());
    let host = server.uri();
    let _host = EnvVarGuard::set("JIRA_HOST", &host);
    let _email = EnvVarGuard::set("JIRA_EMAIL", "test@example.com");
    let _token = EnvVarGuard::set("JIRA_TOKEN", "test-token");
    let _profile = EnvVarGuard::unset("JIRA_PROFILE");
    let _read_only = EnvVarGuard::set("JIRA_READ_ONLY", "1");
    Command::cargo_bin("jira")
        .unwrap()
        .args(args)
        .env("NO_COLOR", "1")
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
    let _env = ProcessEnvLock::acquire().unwrap();
    let _config_dir = jira_cli::test_support::unset_config_dir_env();
    let output = Command::cargo_bin("jira")
        .unwrap()
        .args(["schema"])
        .output()
        .unwrap();
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
fn assert_json_keys_match_schema(command: &str, actual: &serde_json::Value, extra: &[&str]) {
    let schema = schema_command(command);
    let declared: std::collections::BTreeSet<&str> = schema["output_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    let mut emitted: std::collections::BTreeSet<&str> = actual
        .as_object()
        .unwrap_or_else(|| panic!("{command} output must be a JSON object; got: {actual}"))
        .keys()
        .map(String::as_str)
        .collect();
    emitted.extend(extra.iter().copied());
    assert_eq!(
        emitted, declared,
        "{command} JSON keys must match the fields `jira schema` declares"
    );
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
async fn attachment_write_commands_are_blocked_in_read_only_mode() {
    let server = MockServer::start().await;

    for args in [
        &["issues", "attach", "PROJ-1", "--file", "irrelevant.bin"][..],
        &["issues", "delete-attachment", "10001"][..],
    ] {
        let output = run_jira_against_read_only(&server, args);
        assert_eq!(
            output.status.code(),
            Some(exit_codes::INPUT_ERROR),
            "args: {args:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("read-only"),
            "args: {args:?}, stderr: {stderr}"
        );
    }
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
    assert_eq!(attachment["author"], "-");

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
