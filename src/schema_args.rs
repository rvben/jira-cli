//! Reflect the parser's accepted inputs instead of guessing from flag names.
use clap::{Arg, ArgAction, Command};
use serde_json::{Map, Value, json};
use std::any::TypeId;

// Clap exposes conflicts but not requirements. Both the derive declaration and
// schema use this one relationship, with a CLI test exercising enforcement.
pub const CREATE_BOARD_REQUIREMENT: (&str, &str) = ("board", "sprint");

pub fn name(arg: &Arg) -> String {
    arg.get_long()
        .map(|v| format!("--{v}"))
        .unwrap_or_else(|| arg.get_id().to_string())
}

pub fn arg_type(arg: &Arg) -> &'static str {
    let id = arg.get_value_parser().type_id();
    let scalar = if matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse) {
        "boolean"
    } else if id == TypeId::of::<usize>()
        || id == TypeId::of::<u64>()
        || matches!(arg.get_action(), ArgAction::Count)
    {
        "integer"
    } else if id == TypeId::of::<std::path::PathBuf>() {
        "path"
    } else if id == TypeId::of::<String>()
        || id == TypeId::of::<clap_complete::Shell>()
        || id == TypeId::of::<(String, Value)>()
    {
        "string"
    } else {
        panic!(
            "Unclassified parser type for {}: {id:?}; extend schema type inference",
            arg.get_id()
        )
    };
    if matches!(arg.get_action(), ArgAction::Append) {
        match scalar {
            "integer" => "integer[]",
            "path" => "path[]",
            _ => "string[]",
        }
    } else {
        scalar
    }
}

pub fn enrich(command: &Command, path: &str, arg: &Arg, result: &mut Map<String, Value>) {
    let values: Vec<_> = arg
        .get_possible_values()
        .iter()
        .filter(|v| !v.is_hide_set())
        .map(|v| json!(v.get_name()))
        .collect();
    if !values.is_empty() {
        result.insert("enum".into(), json!(values));
    }
    let mut conflicts: Vec<_> = command
        .get_arguments()
        .filter(|other| other.get_id() != arg.get_id())
        .filter(|other| {
            command
                .get_arg_conflicts_with(arg)
                .iter()
                .any(|a| a.get_id() == other.get_id())
                || command
                    .get_arg_conflicts_with(other)
                    .iter()
                    .any(|a| a.get_id() == arg.get_id())
        })
        .map(name)
        .collect();
    conflicts.sort();
    conflicts.dedup();
    if !conflicts.is_empty() {
        result.insert("conflicts_with".into(), json!(conflicts));
    }
    if path == "issues create" && arg.get_id() == CREATE_BOARD_REQUIREMENT.0 {
        result.insert(
            "requires".into(),
            json!([format!("--{}", CREATE_BOARD_REQUIREMENT.1)]),
        );
    }
    if let Some(delimiter) = arg.get_value_delimiter() {
        result.insert("value_delimiter".into(), json!(delimiter.to_string()));
    }
    if matches!(arg.get_action(), ArgAction::Append) {
        result.insert("repeatable".into(), json!(true));
    }
    if !arg.get_default_values().is_empty() {
        let ty = arg_type(arg);
        let defaults: Vec<Value> = arg
            .get_default_values()
            .iter()
            .map(|v| {
                let v = v.to_string_lossy();
                match ty {
                    "boolean" => json!(v.parse::<bool>().expect("boolean default")),
                    "integer" | "integer[]" => json!(v.parse::<u64>().expect("integer default")),
                    _ => json!(v),
                }
            })
            .collect();
        result.insert(
            "default".into(),
            if matches!(arg.get_action(), ArgAction::Append) {
                json!(defaults)
            } else {
                defaults[0].clone()
            },
        );
    }
}

/// Turn the compact field contract into a complete JSON Schema without a second
/// handwritten list of output keys. Opaque objects deliberately allow dynamic keys.
pub fn output_schema(fields: &Value) -> Value {
    let fields = fields.as_array().expect("output fields");
    let properties: Map<String, Value> = fields
        .iter()
        .map(|field| {
            (
                field["name"].as_str().expect("field name").to_owned(),
                field_schema(field),
            )
        })
        .collect();
    let required: Vec<_> = fields
        .iter()
        .filter(|field| field["optional"] != true)
        .map(|field| field["name"].clone())
        .collect();
    json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false})
}

fn field_schema(field: &Value) -> Value {
    let kind = field["type"].as_str().expect("field type");
    let mut schema = match kind {
        "object" => field
            .get("fields")
            .map(output_schema)
            .unwrap_or_else(|| json!({"type":"object"})),
        "array" => json!({"type":"array","items":field_schema(&field["items"])}),
        "object[]" => {
            json!({"type":"array","items":field.get("fields").map(output_schema).unwrap_or_else(|| json!({"type":"object"}))})
        }
        "string[]" => json!({"type":"array","items":{"type":"string"}}),
        _ => json!({"type":kind}),
    };
    if field["nullable"] == true {
        schema["type"] = json!([schema["type"], "null"]);
    }
    schema
}
