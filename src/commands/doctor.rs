use crate::api::{ApiError, JiraClient};
use crate::config::{Config, DeploymentCheck};
use crate::output::OutputConfig;

/// Verify the complete read-only path from resolved configuration to Jira.
///
/// Checks are intentionally sequential: a failed authentication request makes
/// a project-access request both noisy and uninformative. Successful output is
/// stable JSON for automation and a compact checklist for a person at a TTY.
pub async fn run(
    client: &JiraClient,
    config: &Config,
    out: &OutputConfig,
    offline: bool,
) -> Result<(), ApiError> {
    let safety = if config.read_only {
        "read-only mode enabled"
    } else {
        "write operations enabled"
    };
    let configuration = format!("{} · REST API v{}", config.host, config.api_version);

    if offline {
        let checks = serde_json::json!([
            {"name": "configuration", "ok": true, "detail": configuration},
            {"name": "deployment", "ok": true, "detail": "network check skipped"},
            {"name": "authentication", "ok": true, "detail": format!("credential available from {}; network not checked", config.credential_store)},
            {"name": "projects", "ok": true, "detail": "network check skipped"},
            {"name": "write_safety", "ok": true, "detail": safety}
        ]);
        if out.json {
            out.print_data(
                &serde_json::to_string_pretty(
                    &serde_json::json!({"ok": true, "offline": true, "checks": checks}),
                )
                .expect("failed to serialize doctor result"),
            );
        } else {
            println!("Jira connection (offline)\n");
            for check in checks.as_array().expect("checks are an array") {
                println!(
                    "  ✓ {:<16} {}",
                    check["name"].as_str().unwrap_or("check"),
                    check["detail"].as_str().unwrap_or_default()
                );
            }
        }
        return Ok(());
    }

    let mut checks = Checks::new(safety);
    checks.pass("configuration", configuration);

    match crate::config::check_deployment(config).await {
        DeploymentCheck::Matches(runs) => checks.pass("deployment", runs),
        DeploymentCheck::Unknown(reason) => checks.pass(
            "deployment",
            format!("not identified ({reason}); checks continue"),
        ),
        DeploymentCheck::Mismatch(problem) => {
            let error = ApiError::InvalidInput(problem);
            checks.fail("deployment", &error, &["authentication", "projects"]);
            render_failed_checks(out, checks.finish());
            return Err(error);
        }
    }

    let me = match client.get_myself().await {
        Ok(me) => me,
        Err(error) => {
            let error = contextualize_auth_error(error, config);
            checks.fail("authentication", &error, &["projects"]);
            render_failed_checks(out, checks.finish());
            return Err(error);
        }
    };
    checks.pass("authentication", me.display_name.clone());

    let projects = match client.list_projects().await {
        Ok(projects) => projects,
        Err(error) => {
            checks.fail("projects", &error, &[]);
            render_failed_checks(out, checks.finish());
            return Err(error);
        }
    };
    checks.pass("projects", project_access_detail(projects.len()));
    let checks = checks.finish();

    if out.json {
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "instance": client.browse_base_url(),
                "user": {
                    "accountId": me.account_id,
                    "displayName": me.display_name,
                    "email": me.email_address,
                },
                "projectCount": projects.len(),
                "checks": checks,
            }))
            .expect("failed to serialize doctor result"),
        );
    } else {
        println!("Jira connection\n");
        for check in checks.as_array().expect("checks are an array") {
            println!(
                "  ✓ {:<16} {}",
                check["name"].as_str().unwrap_or("check"),
                check["detail"].as_str().unwrap_or_default()
            );
        }
        println!("\nReady.");
    }

    Ok(())
}

fn contextualize_auth_error(error: ApiError, config: &Config) -> ApiError {
    match error {
        ApiError::NotFound(_) => ApiError::NotFound(format!(
            "Jira REST API at {} was not found. Confirm the site is active and REST API v{} matches this deployment",
            config.host, config.api_version
        )),
        other => other,
    }
}

/// Describe how many projects the account can see, as `doctor` and `init` report it.
pub(crate) fn project_access_detail(count: usize) -> String {
    match count {
        0 => "accessible; no projects visible".to_string(),
        1 => "1 project accessible".to_string(),
        count => format!("{count} projects accessible"),
    }
}

/// Doctor's checklist in the order the checks run. A failed check marks the
/// checks it prevented as not run, so the list always has the same entries.
struct Checks {
    entries: Vec<serde_json::Value>,
    safety: &'static str,
}

impl Checks {
    fn new(safety: &'static str) -> Self {
        Self {
            entries: Vec::new(),
            safety,
        }
    }

    fn pass(&mut self, name: &str, detail: String) {
        self.entries
            .push(serde_json::json!({"name": name, "ok": true, "detail": detail}));
    }

    fn fail(&mut self, name: &str, error: &ApiError, not_run: &[&str]) {
        self.entries
            .push(serde_json::json!({"name": name, "ok": false, "detail": error.to_string()}));
        for skipped in not_run {
            self.entries
                .push(serde_json::json!({"name": skipped, "ok": false, "detail": "not run"}));
        }
    }

    fn finish(mut self) -> serde_json::Value {
        self.pass("write_safety", self.safety.to_owned());
        serde_json::Value::Array(self.entries)
    }
}

fn render_failed_checks(out: &OutputConfig, checks: serde_json::Value) {
    if out.json {
        out.print_data(
            &serde_json::to_string_pretty(&serde_json::json!({
                "ok": false,
                "checks": checks,
            }))
            .expect("failed to serialize doctor result"),
        );
    } else {
        println!("Jira connection\n");
        for check in checks.as_array().expect("checks are an array") {
            let marker = if check["ok"].as_bool().unwrap_or(false) {
                "✓"
            } else {
                "✗"
            };
            println!(
                "  {marker} {:<16} {}",
                check["name"].as_str().unwrap_or("check"),
                check["detail"].as_str().unwrap_or_default()
            );
        }
    }
}
