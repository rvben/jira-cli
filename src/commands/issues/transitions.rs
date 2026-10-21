use crate::api::{ApiError, Transition};
use owo_colors::OwoColorize;
use serde_json::{Value, json};

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

/// A transition as `list-transitions` reports it: the destination status and
/// the screen fields, required first. `fields` is null when Jira did not
/// report the screen and empty when the transition has none.
pub(super) fn describe(transition: &Transition) -> Value {
    let fields = transition.fields.as_ref().map(|fields| {
        let mut fields = fields
            .iter()
            .map(|(id, field)| {
                json!({
                    "id": id,
                    "name": field["name"].as_str().unwrap_or(id),
                    "required": field["required"] == true,
                    "hasDefaultValue": field["hasDefaultValue"] == true,
                    "type": field["schema"]["type"].as_str(),
                    "items": field["schema"]["items"].as_str(),
                    "allowedValues": field["allowedValues"].as_array().map(|options| {
                        options.iter().map(option).collect::<Vec<_>>()
                    }),
                })
            })
            .collect::<Vec<_>>();
        fields.sort_by_key(|f| f["required"] != true);
        fields
    });
    json!({"id": transition.id, "name": transition.name, "to": transition.to, "fields": fields})
}

/// An allowed value reduced to what `--resolution` and `--field` accept.
fn option(value: &Value) -> Value {
    json!({
        "id": value["id"].as_str(),
        "name": value["name"].as_str().or_else(|| value["value"].as_str()),
    })
}

/// The text form of [`describe`]: one line per transition, then one per
/// screen field, so the flags a transition needs are visible up front.
pub(super) fn print(described: &[Value], bold: bool) {
    let header = format!("{:<6} {:<24} {}", "ID", "Name", "To");
    if bold {
        println!("{}", header.bold());
    } else {
        println!("{header}");
    }
    for t in described {
        println!(
            "{:<6} {:<24} {}",
            t["id"].as_str().unwrap_or(""),
            t["name"].as_str().unwrap_or(""),
            t["to"]["name"].as_str().unwrap_or("?")
        );
        for field in t["fields"].as_array().into_iter().flatten() {
            let requirement = match (field["required"] == true, field["hasDefaultValue"] == true) {
                (true, false) => "required",
                (true, true) => "required, default provided",
                _ => "optional",
            };
            let mut line = format!(
                "       {} [{}]: {requirement}",
                field["name"].as_str().unwrap_or(""),
                field["id"].as_str().unwrap_or("")
            );
            if let Some(options) = field["allowedValues"].as_array() {
                let names = options
                    .iter()
                    .filter_map(|o| o["name"].as_str())
                    .collect::<Vec<_>>();
                line.push_str(" · ");
                line.push_str(&shorten(&names));
            }
            println!("{line}");
        }
    }
}

/// Join option names, keeping long lists to a line.
fn shorten(names: &[&str]) -> String {
    const SHOWN: usize = 8;
    match names.len() {
        0 => "(no values)".into(),
        n if n <= SHOWN => names.join(", "),
        n => format!("{}, … {} more", names[..SHOWN].join(", "), n - SHOWN),
    }
}
