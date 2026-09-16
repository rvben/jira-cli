use crate::api::{ApiError, Transition};
use serde_json::json;

/// IDs take precedence over action names, which take precedence over destination
/// statuses. Never choose a workflow action merely because it appeared first.
pub(super) fn resolve<'a>(
    key: &str,
    input: &str,
    transitions: &'a [Transition],
) -> Result<&'a Transition, ApiError> {
    let input = input.trim();
    let lower = input.to_lowercase();
    let mut ambiguous = false;
    if !input.is_empty() {
        for tier in 0..3 {
            let matches: Vec<_> = transitions
                .iter()
                .filter(|t| match tier {
                    0 => t.id == input,
                    1 => t.name.to_lowercase() == lower,
                    _ => {
                        t.to.as_ref()
                            .is_some_and(|s| s.name.to_lowercase() == lower)
                    }
                })
                .collect();
            if let [matched] = matches.as_slice() {
                return Ok(matched);
            }
            if matches.len() > 1 {
                ambiguous = true;
                break;
            }
        }
    }
    let choices: Vec<_> = transitions
        .iter()
        .map(|t| {
            json!({
                "id": t.id, "name": t.name, "status": t.to.as_ref().map(|s| &s.name)
            })
        })
        .collect();
    let available = if transitions.is_empty() {
        "(none available)".into()
    } else {
        transitions
            .iter()
            .map(|t| {
                format!(
                    "{} ({}, status: {})",
                    t.name,
                    t.id,
                    t.to.as_ref().map_or("unknown", |s| s.name.as_str())
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let source = if ambiguous {
        ApiError::InvalidInput(format!(
            "Transition '{input}' is ambiguous for {key}. Available: {available}. Use a transition ID."
        ))
    } else {
        ApiError::NotFound(format!(
            "Transition '{input}' not found for {key}. Available: {available}. Use a transition ID, action name, or destination status."
        ))
    };
    Err(ApiError::WithDetails {
        source: Box::new(source),
        details: json!({"issue": key, "input": input, "candidates": choices,
            "hint": format!("jira issues list-transitions {key}")}),
    })
}
