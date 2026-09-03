use crate::api::{ApiError, JiraClient};
use crate::config::Config;
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

    let me = match client.get_myself().await {
        Ok(me) => me,
        Err(error) => {
            let error = contextualize_auth_error(error, config);
            render_failure(out, &configuration, safety, "authentication", &error);
            return Err(error);
        }
    };

    let projects = match client.list_projects().await {
        Ok(projects) => projects,
        Err(error) => {
            render_project_failure(out, &configuration, &me.display_name, safety, &error);
            return Err(error);
        }
    };

    let project_detail = match projects.len() {
        0 => "accessible; no projects visible".to_string(),
        1 => "1 project accessible".to_string(),
        count => format!("{count} projects accessible"),
    };
    let checks = serde_json::json!([
        {"name": "configuration", "ok": true, "detail": configuration},
        {"name": "authentication", "ok": true, "detail": me.display_name},
        {"name": "projects", "ok": true, "detail": project_detail},
        {"name": "write_safety", "ok": true, "detail": safety}
    ]);

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

fn render_failure(
    out: &OutputConfig,
    configuration: &str,
    safety: &str,
    failed_check: &str,
    error: &ApiError,
) {
    let checks = serde_json::json!([
        {"name": "configuration", "ok": true, "detail": configuration},
        {"name": failed_check, "ok": false, "detail": error.to_string()},
        {"name": "projects", "ok": false, "detail": "not run"},
        {"name": "write_safety", "ok": true, "detail": safety}
    ]);
    render_failed_checks(out, checks);
}

fn render_project_failure(
    out: &OutputConfig,
    configuration: &str,
    user: &str,
    safety: &str,
    error: &ApiError,
) {
    let checks = serde_json::json!([
        {"name": "configuration", "ok": true, "detail": configuration},
        {"name": "authentication", "ok": true, "detail": user},
        {"name": "projects", "ok": false, "detail": error.to_string()},
        {"name": "write_safety", "ok": true, "detail": safety}
    ]);
    render_failed_checks(out, checks);
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
