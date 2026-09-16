use super::{ApiError, Fields, OptionMatch, Value, match_option, named_id, option_names};

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
            Some(value) => match value {
                Value::Null => true,
                Value::String(s) => s.trim().is_empty(),
                Value::Array(items) => items.is_empty(),
                Value::Object(items) => {
                    items.is_empty() || (value["type"] == "doc" && empty_adf(value))
                }
                _ => false,
            },
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
