use super::{
    ApiError, Fields, OptionMatch, Value, match_option, named_id, option_names, unresolved_option,
};
use crate::api::types::text_to_adf;
use serde_json::json;

/// Resolve names and IDs using only options allowed in the current context.
/// Raw --field values remain an escape hatch, including custom JSON shapes.
pub(super) fn normalize_named_arrays(
    fields: &mut Value,
    meta: Option<&Fields>,
    custom: &[(String, Value)],
) -> Result<(), ApiError> {
    let Some(meta) = meta else {
        return Ok(());
    };
    for (field, flag) in [
        ("components", "--components"),
        ("fixVersions", "--fix-versions"),
    ] {
        if custom.iter().any(|(key, _)| key == field) {
            continue;
        }
        let Some(values) = fields.get(field).and_then(Value::as_array) else {
            continue;
        };
        let definition = meta.get(field).ok_or_else(|| ApiError::InvalidInput(format!(
            "{field} is not available on this issue's create/edit screen; omit {flag} or ask an administrator to enable it"
        )))?;
        let Some(options) = definition["allowedValues"].as_array() else {
            continue;
        };
        let normalized = values
            .iter()
            .map(|value| {
                let input = value["name"]
                    .as_str()
                    .or_else(|| value["id"].as_str())
                    .ok_or_else(|| {
                        ApiError::InvalidInput(format!("{flag} requires a name or ID"))
                    })?;
                let selected = &options[match_option(field, input, options, OptionMatch::Prefix)?];
                Ok(named_id(
                    selected["id"].as_str().unwrap_or(""),
                    selected["name"].as_str().unwrap_or(input),
                ))
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        fields[field] = Value::Array(normalized);
    }
    Ok(())
}

/// Check raw `--field` values against the field's schema and convert the
/// plain forms people type into the shape Jira expects: numbers for number
/// fields, text for text fields, and option, version, component or priority
/// names into references. Cloud takes rich-text fields as ADF. JSON objects
/// and nulls pass through as the escape hatch, and fields without metadata are
/// sent as given.
pub(super) fn normalize_field_values(
    fields: &mut Value,
    meta: Option<&Fields>,
    custom: &[(String, Value)],
    api_version: u8,
) -> Result<(), ApiError> {
    let Some(meta) = meta else {
        return Ok(());
    };
    for (id, _) in custom {
        let Some(definition) = meta.get(id) else {
            continue;
        };
        let value = fields[id].take();
        fields[id] = typed_value(id, definition, value, api_version)?;
    }
    Ok(())
}

fn typed_value(
    id: &str,
    definition: &Value,
    value: Value,
    api_version: u8,
) -> Result<Value, ApiError> {
    if value.is_null() || value.is_object() {
        return Ok(value);
    }
    let schema = &definition["schema"];
    let name = definition["name"].as_str().unwrap_or(id);
    let wrong = |expected: &str, value: &Value| {
        ApiError::InvalidInput(format!("{name} ({id}) expects {expected}; got {value}"))
    };
    match (schema["type"].as_str(), schema["items"].as_str()) {
        (Some("number"), _) => match &value {
            Value::Number(_) => Ok(value),
            Value::String(s) => s
                .trim()
                .parse::<i64>()
                .map(Value::from)
                .ok()
                .or_else(|| {
                    s.trim()
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map(Value::Number)
                })
                .ok_or_else(|| wrong("a number", &value)),
            _ => Err(wrong("a number", &value)),
        },
        (Some("string"), _) => match &value {
            Value::String(text) if api_version >= 3 && is_rich_text(schema) => {
                Ok(text_to_adf(text))
            }
            Value::String(_) => Ok(value),
            Value::Number(_) | Value::Bool(_) => Ok(Value::String(value.to_string())),
            _ => Err(wrong("text", &value)),
        },
        (Some("date"), _) => match value.as_str() {
            Some(s) if is_date(s) => Ok(value),
            _ => Err(wrong("a date such as 2026-01-31", &value)),
        },
        (Some(kind @ ("option" | "priority" | "version" | "component" | "resolution")), _) => {
            match scalar(&value) {
                Some(input) => reference(name, kind, definition, &input),
                None => Err(wrong("one value", &value)),
            }
        }
        (Some("array"), Some(kind @ ("option" | "version" | "component" | "string"))) => {
            let items = match value {
                Value::Array(items) => items,
                single => vec![single],
            };
            items
                .into_iter()
                .map(|item| match (kind, scalar(&item)) {
                    (_, None) if item.is_object() => Ok(item),
                    ("string", Some(text)) => Ok(Value::String(text)),
                    (_, Some(input)) => reference(name, kind, definition, &input),
                    _ => Err(wrong("a list of values", &item)),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        _ => Ok(value),
    }
}

fn is_rich_text(schema: &Value) -> bool {
    matches!(
        schema["system"].as_str(),
        Some("description" | "environment")
    ) || schema["custom"]
        .as_str()
        .is_some_and(|custom| custom.ends_with(":textarea"))
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// Select options are named by `value`; versions, components and priorities
/// by `name`. Without allowed values the input is sent as a reference by name.
fn reference(name: &str, kind: &str, definition: &Value, input: &str) -> Result<Value, ApiError> {
    let label = if kind == "option" { "value" } else { "name" };
    let Some(options) = definition["allowedValues"].as_array() else {
        return Ok(if kind == "option" {
            json!({"value": input.trim()})
        } else {
            unresolved_option(input)
        });
    };
    let named = options
        .iter()
        .map(|option| json!({"id": option["id"], "name": option[label]}))
        .collect::<Vec<_>>();
    let style = match kind {
        "priority" => OptionMatch::Priority,
        "option" => OptionMatch::NameOrId,
        _ => OptionMatch::Prefix,
    };
    let selected = &named[match_option(name, input, &named, style)?];
    Ok(match selected["id"].as_str() {
        Some(id) => json!({"id": id}),
        None => json!({label: selected["name"]}),
    })
}

/// Whether a field holds one user or a list of users, from its schema, or by
/// name for the system user fields when metadata is unavailable.
pub(super) fn user_field(id: &str, definition: Option<&Value>) -> Option<UserCardinality> {
    match definition {
        Some(definition) => {
            let schema = &definition["schema"];
            match (schema["type"].as_str(), schema["items"].as_str()) {
                (Some("user"), _) => Some(UserCardinality::Single),
                (Some("array"), Some("user")) => Some(UserCardinality::Many),
                _ => None,
            }
        }
        None => matches!(id, "assignee" | "reporter").then_some(UserCardinality::Single),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum UserCardinality {
    Single,
    Many,
}

/// On create, absent required fields need a server default. On update, absent
/// fields stay untouched; only explicitly clearing a required value is invalid.
pub(super) fn validate_required(
    fields: &Value,
    meta: Option<&Fields>,
    creating: bool,
) -> Result<(), ApiError> {
    let Some(meta) = meta else {
        return Ok(());
    };
    let mut missing = Vec::new();
    for (id, definition) in meta {
        if definition["required"] != true {
            continue;
        }
        let invalid = match fields.get(id) {
            Some(value) => is_empty_value(value),
            None => {
                creating
                    && definition["hasDefaultValue"] != true
                    && definition["defaultValue"].is_null()
            }
        };
        if invalid {
            let name = definition["name"].as_str().unwrap_or(id);
            let mut detail = format!("{name:?} ({id})");
            if let Some(options) = definition["allowedValues"].as_array() {
                detail.push_str(&format!("; valid: {}", option_names(options)));
            }
            if user_field(id, Some(definition)).is_some() {
                detail.push_str(&format!("; e.g. --field {id}=me"));
            }
            missing.push(detail);
        }
    }
    if !missing.is_empty() {
        return Err(ApiError::InvalidInput(format!(
            "Required fields missing or empty: {}. Supply the corresponding flag or --field <ID>=<VALUE>",
            missing.join("; ")
        )));
    }
    Ok(())
}

/// Whether a field value counts as unset: null, blank text, an empty list or
/// object, or a rich-text document without content.
pub(super) fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(items) => items.is_empty() || (value["type"] == "doc" && empty_adf(value)),
        _ => false,
    }
}

// Plain-text flags become ADF on Cloud. An empty paragraph is still empty for
// required-field validation; media, mentions and unknown rich nodes are values.
fn empty_adf(node: &Value) -> bool {
    match node["type"].as_str() {
        Some("text") => node["text"].as_str().is_none_or(|s| s.trim().is_empty()),
        Some("hardBreak") => true,
        Some(
            "doc" | "paragraph" | "heading" | "blockquote" | "bulletList" | "orderedList"
            | "listItem" | "codeBlock",
        ) => node["content"]
            .as_array()
            .is_some_and(|items| items.iter().all(empty_adf)),
        _ => false,
    }
}
