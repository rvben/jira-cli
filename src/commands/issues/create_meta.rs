use crate::api::{ApiError, JiraClient};
use crate::output::OutputConfig;
use serde_json::Value;

pub async fn create_meta(
    client: &JiraClient,
    out: &OutputConfig,
    project: &str,
    issue_type: Option<&str>,
) -> Result<(), ApiError> {
    let metadata = client.issue_create_metadata(project, issue_type).await?;
    if out.json {
        out.print_result(&metadata, "");
        return Ok(());
    }
    if issue_type.is_none() {
        for item in metadata["issueTypes"].as_array().expect("issue type array") {
            println!(
                "{:<12} {}{}",
                item["id"].as_str().unwrap_or(""),
                item["name"].as_str().unwrap_or(""),
                if item["subtask"] == true {
                    " (subtask)"
                } else {
                    ""
                }
            );
        }
        out.print_message(&format!(
            "Inspect a create screen with: jira issues create-meta -p {project} -t <TYPE>"
        ));
        return Ok(());
    }
    println!("{project}: {}", option_text(&metadata["issueType"]));
    println!(
        "Epic support: {}{}",
        metadata["epic"]["status"].as_str().unwrap_or("unavailable"),
        metadata["epic"]["field"]
            .as_str()
            .map(|id| format!(" ({id})"))
            .unwrap_or_default()
    );
    if let Some(fields) = metadata["fields"].as_object() {
        for (id, field) in fields {
            let requirement = match field["required"].as_bool() {
                Some(true) if field["hasDefaultValue"] == true => "required, default provided",
                Some(true) => "required",
                Some(false) => "optional",
                None => "requirement unknown",
            };
            println!(
                "\n{} [{id}]: {requirement}",
                field["name"].as_str().unwrap_or(id)
            );
            if !field["defaultValue"].is_null() {
                println!("  Default: {}", option_text(&field["defaultValue"]));
            }
            if let Some(options) = field["allowedValues"].as_array() {
                let labels = options
                    .iter()
                    .map(option_text)
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "  Allowed: {}",
                    if labels.is_empty() { "(none)" } else { &labels }
                );
            }
        }
    } else {
        println!("Field metadata unavailable.");
    }
    for warning in metadata["warnings"].as_array().expect("warning array") {
        out.print_message(warning.as_str().unwrap_or(""));
    }
    Ok(())
}

fn option_text(value: &Value) -> String {
    if let Some(name) = value["name"].as_str().or_else(|| value["value"].as_str()) {
        match value["id"].as_str() {
            Some(id) => format!("{name} ({id})"),
            None => name.into(),
        }
    } else if let Some(value) = value.as_str() {
        value.into()
    } else {
        value.to_string()
    }
}
