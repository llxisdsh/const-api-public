use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Map, Value};

const MAX_SCHEMA_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchemaError {
    code: &'static str,
    path: String,
    message: String,
}

impl SchemaError {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }

    pub(crate) const fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for SchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.code, self.path, self.message
        )
    }
}

impl std::error::Error for SchemaError {}

pub(crate) fn canonicalize_schema(schema: &Value) -> Result<Value, SchemaError> {
    if schema.is_boolean() {
        return Ok(schema.clone());
    }
    if !schema.is_object() {
        return Err(SchemaError::new(
            "schema_not_object",
            "$",
            "schema root must be an object",
        ));
    }
    canonicalize_value(schema, "$", 0)
}

pub(crate) fn canonicalize_tool_schema(schema: &Value) -> Result<Value, SchemaError> {
    let canonical = canonicalize_schema(schema)?;
    let object_root = match canonical.get("type") {
        None => true,
        Some(Value::String(kind)) => kind == "object",
        Some(Value::Array(kinds)) => {
            kinds.len() == 1 && kinds.first().and_then(Value::as_str) == Some("object")
        }
        _ => false,
    };
    if !object_root {
        return Err(SchemaError::new(
            "tool_schema_not_object",
            "$.type",
            "tool input schema must have an object root",
        ));
    }
    Ok(canonical)
}

fn canonicalize_value(value: &Value, path: &str, depth: usize) -> Result<Value, SchemaError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(SchemaError::new(
            "schema_too_deep",
            path,
            "schema nesting exceeds the conversion limit",
        ));
    }
    match value {
        Value::Object(object) => canonicalize_object(object, path, depth),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| canonicalize_value(item, &format!("{path}[{index}]"), depth + 1))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        _ => Ok(value.clone()),
    }
}

fn canonicalize_object(
    object: &Map<String, Value>,
    path: &str,
    depth: usize,
) -> Result<Value, SchemaError> {
    let mut keys = object.keys().collect::<Vec<_>>();
    keys.sort_unstable();
    let mut output = Map::new();
    for key in keys {
        let value = &object[key];
        if is_empty_metadata(key, value) {
            continue;
        }
        let child_path = format!("{path}.{}", escape_path_key(key));
        let canonical = if key == "required" {
            canonicalize_required(value, &child_path)?
        } else {
            canonicalize_value(value, &child_path, depth + 1)?
        };
        output.insert(key.clone(), canonical);
    }
    validate_properties_and_required(&output, path)?;
    Ok(Value::Object(output))
}

fn canonicalize_required(value: &Value, path: &str) -> Result<Value, SchemaError> {
    let items = value.as_array().ok_or_else(|| {
        SchemaError::new(
            "required_not_array",
            path,
            "required must be an array of property names",
        )
    })?;
    let mut names = BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        let name = item.as_str().ok_or_else(|| {
            SchemaError::new(
                "required_name_not_string",
                format!("{path}[{index}]"),
                "required entries must be strings",
            )
        })?;
        names.insert(name.to_string());
    }
    Ok(Value::Array(names.into_iter().map(Value::String).collect()))
}

fn validate_properties_and_required(
    object: &Map<String, Value>,
    path: &str,
) -> Result<(), SchemaError> {
    let properties = match object.get("properties") {
        Some(Value::Object(properties)) => Some(properties),
        Some(_) => {
            return Err(SchemaError::new(
                "properties_not_object",
                format!("{path}.properties"),
                "properties must be an object",
            ));
        }
        None => None,
    };
    if let Some(required) = object.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !properties.is_some_and(|properties| properties.contains_key(name)) {
                return Err(SchemaError::new(
                    "required_property_missing",
                    format!("{path}.required"),
                    format!("required property {name:?} is not declared in properties"),
                ));
            }
        }
    }
    Ok(())
}

fn is_empty_metadata(key: &str, value: &Value) -> bool {
    matches!(key, "title" | "description" | "$comment")
        && value.as_str().is_some_and(|text| text.trim().is_empty())
}

fn escape_path_key(key: &str) -> String {
    key.replace('~', "~0").replace('.', "~1")
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::{Map, Value, json};

    use super::*;

    #[test]
    fn schema_canonicalization_is_stable_and_preserves_constraints() {
        let mut properties = Map::new();
        properties.insert("zeta".to_string(), json!({"type":"integer","minimum":2}));
        properties.insert(
            "alpha".to_string(),
            json!({"type":"string","minLength":3,"description":""}),
        );
        let schema = Value::Object(Map::from_iter([
            ("required".to_string(), json!(["zeta", "alpha", "alpha"])),
            ("properties".to_string(), Value::Object(properties)),
            ("type".to_string(), json!("object")),
            ("title".to_string(), json!("")),
            ("additionalProperties".to_string(), json!(false)),
        ]));
        let canonical = canonicalize_schema(&schema).unwrap();
        assert_eq!(canonicalize_schema(&canonical).unwrap(), canonical);
        assert_eq!(canonical["required"], json!(["alpha", "zeta"]));
        assert_eq!(canonical["properties"]["zeta"]["minimum"], 2);
        assert_eq!(canonical["properties"]["alpha"]["minLength"], 3);
        assert_eq!(canonical["additionalProperties"], false);
        assert!(canonical.get("title").is_none());
        assert!(
            canonical["properties"]["alpha"]
                .get("description")
                .is_none()
        );
        assert_eq!(canonicalize_schema(&json!(true)).unwrap(), json!(true));

        let serialized = serde_json::to_string(&canonical).unwrap();
        assert!(
            serialized.find("additionalProperties").unwrap()
                < serialized.find("properties").unwrap()
        );
        assert_eq!(canonicalize_tool_schema(&json!({})).unwrap(), json!({}));
    }

    #[test]
    fn schema_validation_rejects_inconsistent_required_and_non_object_tool_roots() {
        let missing = canonicalize_schema(&json!({
            "type":"object",
            "properties":{"known":{"type":"string"}},
            "required":["missing"]
        }))
        .unwrap_err();
        assert_eq!(missing.code(), "required_property_missing");

        let non_object = canonicalize_tool_schema(&json!({"type":"string"})).unwrap_err();
        assert_eq!(non_object.code(), "tool_schema_not_object");
        assert_eq!(
            canonicalize_schema(&json!([])).unwrap_err().code(),
            "schema_not_object"
        );
    }

    proptest! {
        #[test]
        fn canonicalization_is_idempotent_for_generated_object_schemas(
            names in prop::collection::vec("[a-z]{1,8}", 0..12),
            minima in prop::collection::vec(0i64..1000, 0..12),
        ) {
            let mut properties = Map::new();
            for (index, name) in names.iter().enumerate() {
                properties.insert(name.clone(), json!({
                    "type":"integer",
                    "minimum": minima.get(index).copied().unwrap_or_default(),
                    "description":""
                }));
            }
            let schema = json!({"type":"object","properties":properties});
            let once = canonicalize_schema(&schema).unwrap();
            let twice = canonicalize_schema(&once).unwrap();
            prop_assert_eq!(once, twice);
        }
    }
}
