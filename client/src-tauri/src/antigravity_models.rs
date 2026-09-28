use crate::model::QuotaWindow;
use crate::protocol::ir::ReasoningEffort;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub(crate) const ANTIGRAVITY_MODEL_ROUTES_KEY: &str = "antigravity_model_routes";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AntigravityModelRoute {
    pub(crate) canonical_model: String,
    pub(crate) upstream_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) available: Option<bool>,
    #[serde(default)]
    pub(crate) recommended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedAntigravityModel {
    pub(crate) canonical_model: String,
    pub(crate) upstream_model: String,
    pub(crate) route_encodes_reasoning: bool,
    pub(crate) preserve_explicit_disable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReasoningRoutePreference {
    Disabled,
    Automatic,
    Low,
    Medium,
    High,
}

impl ReasoningRoutePreference {
    fn parse(value: &str) -> Option<Self> {
        if matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "auto" | "automatic" | "adaptive" | "tiered"
        ) {
            return Some(Self::Automatic);
        }
        if matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "disabled" | "off"
        ) {
            return Some(Self::Disabled);
        }
        let effort = ReasoningEffort::parse(value)?;
        if effort == ReasoningEffort::None {
            return Some(Self::Disabled);
        }
        match effort.route_tier() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Automatic => "auto",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    const fn from_budget(budget: i64) -> Self {
        if budget < 0 {
            Self::Automatic
        } else if budget == 0 {
            Self::Disabled
        } else if budget <= 2_000 {
            Self::Low
        } else if budget <= 7_000 {
            Self::Medium
        } else {
            Self::High
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedAntigravityRequest {
    pub(crate) canonical_model: String,
    pub(crate) upstream_model: String,
    pub(crate) native_body: String,
}

pub(crate) fn model_routes_from_catalog(value: &serde_json::Value) -> Vec<AntigravityModelRoute> {
    let Some(models) = value.get("models").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let mut entries = models
        .iter()
        .enumerate()
        .filter_map(|(index, (upstream_model, metadata))| {
            let upstream_model = upstream_model.trim();
            if upstream_model.is_empty()
                || metadata
                    .get("isInternal")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            {
                return None;
            }
            let upstream_model = callable_upstream_model_id(upstream_model);
            let (canonical_model, reasoning_effort) = canonical_model_and_effort(&upstream_model);
            (!canonical_model.is_empty()).then(|| {
                (
                    metadata
                        .get("recommended")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    index,
                    AntigravityModelRoute {
                        canonical_model,
                        upstream_model,
                        reasoning_effort,
                        available: route_availability(metadata),
                        recommended: metadata
                            .get("recommended")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|(recommended, index, _)| (!*recommended, *index));

    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter_map(|(_, _, route)| {
            seen.insert(route.upstream_model.to_ascii_lowercase())
                .then_some(route)
        })
        .collect()
}

pub(crate) fn public_models_from_catalog(value: &serde_json::Value) -> Vec<String> {
    public_models_from_routes(&model_routes_from_catalog(value))
}

pub(crate) fn public_models_from_routes(routes: &[AntigravityModelRoute]) -> Vec<String> {
    let mut seen = HashSet::new();
    routes
        .iter()
        .filter_map(|route| {
            let model = route.canonical_model.trim();
            (!model.is_empty() && seen.insert(model.to_ascii_lowercase()))
                .then(|| model.to_string())
        })
        .collect()
}

pub(crate) fn model_routes_from_credential(
    credential: &serde_json::Value,
) -> Vec<AntigravityModelRoute> {
    credential
        .get(ANTIGRAVITY_MODEL_ROUTES_KEY)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

pub(crate) fn model_routes_json(
    routes: &[AntigravityModelRoute],
) -> anyhow::Result<serde_json::Value> {
    serde_json::to_value(routes).map_err(Into::into)
}

pub(crate) fn canonical_model_id(model: &str) -> String {
    canonical_model_and_effort(model).0
}

pub(crate) fn resolve_model_for_request(
    credential: &serde_json::Value,
    requested_model: &str,
    request_bodies: &[&str],
) -> ResolvedAntigravityModel {
    let routes = model_routes_from_credential(credential);
    let (requested_base, suffix_effort) = split_model_suffix(requested_model);
    if let Some(route) = routes.iter().find(|route| {
        route.upstream_model.eq_ignore_ascii_case(requested_base)
            && !route.canonical_model.eq_ignore_ascii_case(requested_base)
    }) {
        // An explicitly requested private route is authoritative. This keeps
        // backward compatibility for callers that deliberately select a
        // concrete `-high`/`-low` route instead of the public canonical model.
        return resolved_from_route(route, false);
    }

    let (canonical_model, inferred_effort) = canonical_model_and_effort(requested_base);
    let desired_reasoning = suffix_effort
        .as_deref()
        .and_then(ReasoningRoutePreference::parse)
        .or_else(|| {
            inferred_effort
                .as_deref()
                .and_then(ReasoningRoutePreference::parse)
        })
        .or_else(|| {
            request_bodies
                .iter()
                .find_map(|body| reasoning_preference_from_body(body))
        });
    let preserve_explicit_disable = desired_reasoning == Some(ReasoningRoutePreference::Disabled);
    let candidates = routes
        .iter()
        .filter(|route| route.canonical_model.eq_ignore_ascii_case(&canonical_model))
        .collect::<Vec<_>>();
    let available_candidates = candidates
        .iter()
        .copied()
        .filter(|route| route.available != Some(false))
        .collect::<Vec<_>>();
    let candidates = if available_candidates.is_empty() {
        candidates
    } else {
        available_candidates
    };

    if let Some(desired_reasoning) = desired_reasoning {
        if let Some(route) = candidates.iter().copied().find(|route| {
            route
                .reasoning_effort
                .as_deref()
                .and_then(ReasoningRoutePreference::parse)
                == Some(desired_reasoning)
        }) {
            return resolved_from_route(route, preserve_explicit_disable);
        }
        if desired_reasoning == ReasoningRoutePreference::Disabled {
            if let Some(route) = candidates.iter().copied().find(|route| {
                route.reasoning_effort.is_none()
                    && !route_encodes_reasoning(&route.canonical_model, &route.upstream_model)
            }) {
                return resolved_from_route(route, true);
            }
        }
    }
    if let Some(route) = candidates.iter().copied().find(|route| route.recommended) {
        return resolved_from_route(route, preserve_explicit_disable);
    }
    if let Some(route) = candidates
        .iter()
        .copied()
        .find(|route| route.upstream_model.eq_ignore_ascii_case(&canonical_model))
    {
        return resolved_from_route(route, preserve_explicit_disable);
    }
    for effort in ["auto", "high", "medium", "low"] {
        if let Some(route) = candidates.iter().copied().find(|route| {
            route
                .reasoning_effort
                .as_deref()
                .is_some_and(|candidate| candidate == effort)
        }) {
            return resolved_from_route(route, preserve_explicit_disable);
        }
    }
    if let Some(route) = candidates.first() {
        return resolved_from_route(route, preserve_explicit_disable);
    }

    let upstream_model = if requested_base.eq_ignore_ascii_case(&canonical_model) {
        fallback_upstream_model(&canonical_model, desired_reasoning)
    } else {
        callable_upstream_model_id(requested_base)
    };
    ResolvedAntigravityModel {
        route_encodes_reasoning: route_encodes_reasoning(&canonical_model, &upstream_model),
        preserve_explicit_disable,
        canonical_model,
        upstream_model,
    }
}

pub(crate) fn prepare_subscription_request(
    credential: &serde_json::Value,
    requested_model: &str,
    original_body: &str,
    native_body: &str,
) -> anyhow::Result<PreparedAntigravityRequest> {
    let resolved =
        resolve_model_for_request(credential, requested_model, &[original_body, native_body]);
    let mut native_body = remove_route_encoded_reasoning(native_body, &resolved)?;
    if antigravity_uses_claude_bridge(&resolved) {
        native_body = normalize_antigravity_claude_request(&native_body)?;
    }
    Ok(PreparedAntigravityRequest {
        canonical_model: resolved.canonical_model,
        upstream_model: resolved.upstream_model,
        native_body,
    })
}

fn antigravity_uses_claude_bridge(resolved: &ResolvedAntigravityModel) -> bool {
    resolved
        .canonical_model
        .to_ascii_lowercase()
        .contains("claude")
        || resolved
            .upstream_model
            .to_ascii_lowercase()
            .contains("claude")
}

fn normalize_antigravity_claude_request(body: &str) -> anyhow::Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    let history_changed = remove_empty_antigravity_claude_text_parts(&mut value);
    let tools = value
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut);

    let mut has_function_declarations = false;
    for declarations in tools.into_iter().flatten().filter_map(|tool| {
        tool.get_mut("functionDeclarations")
            .and_then(serde_json::Value::as_array_mut)
    }) {
        for declaration in declarations {
            let Some(declaration) = declaration.as_object_mut() else {
                continue;
            };
            has_function_declarations = true;
            let parameters = declaration.remove("parameters");
            let json_schema = declaration.remove("parametersJsonSchema");
            let schema = parameters.or(json_schema);
            declaration.insert(
                "parameters".to_string(),
                normalize_antigravity_claude_tool_schema(schema),
            );
        }
    }

    if !history_changed && !has_function_declarations {
        return Ok(body.to_string());
    }
    if has_function_declarations {
        ensure_antigravity_claude_validated_mode(&mut value);
    }
    serde_json::to_string(&value).map_err(Into::into)
}

fn remove_empty_antigravity_claude_text_parts(value: &mut serde_json::Value) -> bool {
    let Some(contents) = value
        .get_mut("contents")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    let mut changed = false;
    contents.retain_mut(|content| {
        let Some(parts) = content
            .get_mut("parts")
            .and_then(serde_json::Value::as_array_mut)
        else {
            return true;
        };
        let before = parts.len();
        // The Antigravity -> Claude bridge can drop an empty scalar and produce
        // a text block without its required text field. Only discard empty plain
        // text: signed parts, thoughts, tool results and future metadata must survive.
        parts.retain(|part| {
            !part.as_object().is_some_and(|part| {
                part.len() == 1 && part.get("text").and_then(serde_json::Value::as_str) == Some("")
            })
        });
        if parts.len() == before {
            return true;
        }
        changed = true;
        !parts.is_empty()
    });
    changed
}

fn normalize_antigravity_claude_tool_schema(
    schema: Option<serde_json::Value>,
) -> serde_json::Value {
    let schema = match schema {
        Some(serde_json::Value::Object(schema)) => serde_json::Value::Object(schema),
        _ => serde_json::json!({}),
    };
    let mut schema = lower_antigravity_claude_schema(schema)
        .as_object()
        .cloned()
        .unwrap_or_default();
    schema.insert("type".to_string(), serde_json::json!("object"));
    ensure_antigravity_claude_object_schema(&mut schema);
    serde_json::Value::Object(schema)
}

const ANTIGRAVITY_CLAUDE_SCHEMA_CONSTRAINTS: [&str; 10] = [
    "minLength",
    "maxLength",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "pattern",
    "minItems",
    "maxItems",
    "format",
    "default",
    "examples",
];

const ANTIGRAVITY_CLAUDE_UNSUPPORTED_SCHEMA_KEYS: [&str; 20] = [
    "minLength",
    "maxLength",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "pattern",
    "minItems",
    "maxItems",
    "format",
    "default",
    "examples",
    "$schema",
    "$defs",
    "definitions",
    "const",
    "$ref",
    "additionalProperties",
    "propertyNames",
    "title",
    "$id",
    "$comment",
];

fn lower_antigravity_claude_schema(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(lower_antigravity_claude_schema)
                .collect(),
        ),
        serde_json::Value::Object(mut schema) => {
            if let Some(reference) = schema
                .get("$ref")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
            {
                let name = reference.rsplit('/').next().unwrap_or(reference.as_str());
                let mut replacement = serde_json::Map::new();
                replacement.insert("type".to_string(), serde_json::json!("object"));
                if let Some(description) = schema
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                {
                    replacement.insert("description".to_string(), serde_json::json!(description));
                }
                append_antigravity_schema_text_hint(&mut replacement, &format!("See: {name}"));
                ensure_antigravity_claude_object_schema(&mut replacement);
                return serde_json::Value::Object(replacement);
            }

            if !schema.contains_key("enum") {
                if let Some(constant) = schema.remove("const") {
                    schema.insert("enum".to_string(), serde_json::json!([constant]));
                }
            }

            let keys = schema.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                if matches!(
                    key.as_str(),
                    "enum" | "const" | "default" | "example" | "examples"
                ) {
                    continue;
                }
                if key == "properties" {
                    if let Some(properties) = schema
                        .get_mut("properties")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        for property_schema in properties.values_mut() {
                            let current = std::mem::take(property_schema);
                            *property_schema = lower_antigravity_claude_schema(current);
                        }
                    }
                    continue;
                }
                if let Some(current) = schema.remove(&key) {
                    schema.insert(key, lower_antigravity_claude_schema(current));
                }
            }

            merge_antigravity_claude_all_of(&mut schema);
            flatten_antigravity_claude_union(&mut schema, "anyOf");
            flatten_antigravity_claude_union(&mut schema, "oneOf");
            flatten_antigravity_claude_type_array(&mut schema);

            if let Some(values) = schema
                .get("enum")
                .and_then(serde_json::Value::as_array)
                .filter(|values| (2..=10).contains(&values.len()))
            {
                let values = values
                    .iter()
                    .map(render_antigravity_schema_value)
                    .collect::<Vec<_>>()
                    .join(", ");
                append_antigravity_schema_text_hint(&mut schema, &format!("Allowed: {values}"));
            }
            if schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false)) {
                append_antigravity_schema_text_hint(&mut schema, "No extra properties allowed");
            }
            for keyword in ANTIGRAVITY_CLAUDE_SCHEMA_CONSTRAINTS {
                if let Some(value) = schema
                    .get(keyword)
                    .filter(|value| !value.is_object() && !value.is_array() && !value.is_null())
                {
                    let value = value.clone();
                    append_antigravity_schema_hint(&mut schema, keyword, &value);
                }
            }
            for keyword in ANTIGRAVITY_CLAUDE_UNSUPPORTED_SCHEMA_KEYS {
                schema.remove(keyword);
            }

            clean_antigravity_claude_required(&mut schema);
            ensure_antigravity_claude_object_schema(&mut schema);
            serde_json::Value::Object(schema)
        }
        other => other,
    }
}

fn merge_antigravity_claude_all_of(schema: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(serde_json::Value::Array(items)) = schema.remove("allOf") else {
        return;
    };
    let mut merged_properties = serde_json::Map::new();
    let mut merged_required = Vec::new();
    let mut merged_other = serde_json::Map::new();
    for item in items {
        let serde_json::Value::Object(mut item) = item else {
            continue;
        };
        if let Some(serde_json::Value::Object(properties)) = item.remove("properties") {
            merged_properties.extend(properties);
        }
        if let Some(serde_json::Value::Array(required)) = item.remove("required") {
            for name in required {
                if !merged_required.contains(&name) {
                    merged_required.push(name);
                }
            }
        }
        for (key, value) in item {
            merged_other.entry(key).or_insert(value);
        }
    }

    if !merged_properties.is_empty() {
        let properties = schema
            .entry("properties")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(properties) = properties.as_object_mut() {
            properties.extend(merged_properties);
            if properties.len() > 1 {
                properties.remove("_placeholder");
            }
        }
    }
    if !merged_required.is_empty() {
        let mut required = schema
            .remove("required")
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
        for name in merged_required {
            if !required.contains(&name) {
                required.push(name);
            }
        }
        schema.insert("required".to_string(), serde_json::Value::Array(required));
    }
    for (key, value) in merged_other {
        schema.entry(key).or_insert(value);
    }
}

fn flatten_antigravity_claude_union(
    schema: &mut serde_json::Map<String, serde_json::Value>,
    keyword: &str,
) {
    let Some(serde_json::Value::Array(options)) = schema.remove(keyword) else {
        return;
    };
    if options.is_empty() {
        return;
    }

    if let Some(values) = merge_antigravity_claude_union_enum(&options) {
        schema.insert("type".to_string(), serde_json::json!("string"));
        schema.insert("enum".to_string(), serde_json::Value::Array(values));
        return;
    }

    let type_names = options
        .iter()
        .filter_map(antigravity_claude_schema_type_name)
        .collect::<Vec<_>>();
    let selected = options
        .into_iter()
        .max_by_key(antigravity_claude_schema_score)
        .unwrap_or_else(|| serde_json::json!({"type":"string"}));
    let serde_json::Value::Object(mut selected) = selected else {
        schema.insert("type".to_string(), serde_json::json!("string"));
        return;
    };
    if let Some(parent_description) = schema
        .get("description")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
    {
        match selected
            .get("description")
            .and_then(serde_json::Value::as_str)
        {
            Some(child_description) if child_description != parent_description => {
                selected.insert(
                    "description".to_string(),
                    serde_json::json!(format!("{parent_description} ({child_description})")),
                );
            }
            None => {
                selected.insert(
                    "description".to_string(),
                    serde_json::json!(parent_description),
                );
            }
            _ => {}
        }
    }
    schema.extend(selected);

    let mut unique_types = Vec::new();
    for name in type_names {
        if !unique_types.contains(&name) {
            unique_types.push(name);
        }
    }
    if unique_types.len() > 1 {
        append_antigravity_schema_text_hint(
            schema,
            &format!("Accepts: {}", unique_types.join(" | ")),
        );
    }
}

fn merge_antigravity_claude_union_enum(
    options: &[serde_json::Value],
) -> Option<Vec<serde_json::Value>> {
    let mut output = Vec::new();
    for option in options {
        let values = option
            .as_object()?
            .get("enum")
            .and_then(serde_json::Value::as_array)
            .filter(|values| !values.is_empty())?;
        for value in values {
            if !output.contains(value) {
                output.push(value.clone());
            }
        }
    }
    (!output.is_empty()).then_some(output)
}

fn antigravity_claude_schema_score(value: &serde_json::Value) -> u8 {
    let Some(schema) = value.as_object() else {
        return 0;
    };
    if schema.get("type").and_then(serde_json::Value::as_str) == Some("object")
        || schema.contains_key("properties")
    {
        3
    } else if schema.get("type").and_then(serde_json::Value::as_str) == Some("array")
        || schema.contains_key("items")
    {
        2
    } else if schema
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind != "null")
    {
        1
    } else {
        0
    }
}

fn antigravity_claude_schema_type_name(value: &serde_json::Value) -> Option<String> {
    let schema = value.as_object()?;
    if schema.contains_key("properties") {
        return Some("object".to_string());
    }
    if schema.contains_key("items") {
        return Some("array".to_string());
    }
    schema
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn flatten_antigravity_claude_type_array(schema: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(types) = schema
        .get("type")
        .and_then(serde_json::Value::as_array)
        .cloned()
    else {
        return;
    };
    let has_null = types.iter().any(|value| value.as_str() == Some("null"));
    let non_null = types
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter(|kind| *kind != "null" && !kind.is_empty())
        .collect::<Vec<_>>();
    schema.insert(
        "type".to_string(),
        serde_json::json!(non_null.first().copied().unwrap_or("string")),
    );
    if non_null.len() > 1 {
        append_antigravity_schema_text_hint(schema, &format!("Accepts: {}", non_null.join(" | ")));
    }
    if has_null {
        append_antigravity_schema_text_hint(schema, "nullable");
    }
}

fn clean_antigravity_claude_required(schema: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        schema.remove("required");
        return;
    };
    let nullable = properties
        .iter()
        .filter_map(|(name, property)| {
            property
                .get("description")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|description| description.contains("nullable"))
                .then_some(name.clone())
        })
        .collect::<HashSet<_>>();
    let property_names = properties.keys().cloned().collect::<HashSet<_>>();
    let Some(required) = schema
        .remove("required")
        .and_then(|value| value.as_array().cloned())
    else {
        return;
    };
    let mut seen = HashSet::new();
    let required = required
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .filter(|name| {
            property_names.contains(name) && !nullable.contains(name) && seen.insert(name.clone())
        })
        .map(serde_json::Value::String)
        .collect::<Vec<_>>();
    if !required.is_empty() {
        schema.insert("required".to_string(), serde_json::Value::Array(required));
    }
}

fn ensure_antigravity_claude_object_schema(
    schema: &mut serde_json::Map<String, serde_json::Value>,
) {
    if schema.get("type").and_then(serde_json::Value::as_str) != Some("object") {
        return;
    }
    let has_properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|properties| !properties.is_empty());
    if has_properties {
        return;
    }
    schema.insert(
        "properties".to_string(),
        serde_json::json!({
            "_placeholder": {
                "type": "boolean",
                "description": "Placeholder. Always pass true."
            }
        }),
    );
    let mut required = schema
        .remove("required")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let placeholder = serde_json::json!("_placeholder");
    if !required.contains(&placeholder) {
        required.push(placeholder);
    }
    schema.insert("required".to_string(), serde_json::Value::Array(required));
}

fn append_antigravity_schema_hint(
    schema: &mut serde_json::Map<String, serde_json::Value>,
    keyword: &str,
    value: &serde_json::Value,
) {
    let hint = format!("{keyword}: {}", render_antigravity_schema_value(value));
    append_antigravity_schema_text_hint(schema, &hint);
}

fn render_antigravity_schema_value(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn append_antigravity_schema_text_hint(
    schema: &mut serde_json::Map<String, serde_json::Value>,
    hint: &str,
) {
    let description = schema
        .get("description")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(|description| format!("{description} ({hint})"))
        .unwrap_or_else(|| hint.to_string());
    schema.insert("description".to_string(), serde_json::json!(description));
}

fn ensure_antigravity_claude_validated_mode(body: &mut serde_json::Value) {
    let Some(body) = body.as_object_mut() else {
        return;
    };
    let tool_config = body
        .entry("toolConfig")
        .or_insert_with(|| serde_json::json!({}));
    if !tool_config.is_object() {
        return;
    }
    let Some(tool_config) = tool_config.as_object_mut() else {
        return;
    };
    let function_config = tool_config
        .entry("functionCallingConfig")
        .or_insert_with(|| serde_json::json!({}));
    let Some(function_config) = function_config.as_object_mut() else {
        return;
    };
    let mode = function_config
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .map(str::to_ascii_uppercase);
    if mode.as_deref().is_none_or(|mode| mode == "AUTO") {
        function_config.insert("mode".to_string(), serde_json::json!("VALIDATED"));
    }
}

pub(crate) fn remove_route_encoded_reasoning(
    body: &str,
    resolved: &ResolvedAntigravityModel,
) -> anyhow::Result<String> {
    if !resolved.route_encodes_reasoning || resolved.preserve_explicit_disable {
        return Ok(body.to_string());
    }
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    remove_nested_object_field(&mut value, "generationConfig", "thinkingConfig");
    remove_nested_object_field(&mut value, "generation_config", "thinking_config");
    if let Some(object) = value.as_object_mut() {
        object.remove("reasoning");
        object.remove("reasoning_effort");
        object.remove("thinking");
        object.remove("thinking_level");
    }
    serde_json::to_string(&value).map_err(Into::into)
}

pub(crate) fn canonicalize_quota_windows(windows: Vec<QuotaWindow>) -> Vec<QuotaWindow> {
    let mut collapsed = Vec::<QuotaWindow>::new();
    let mut indices = HashMap::<(String, String), usize>::new();
    for mut window in windows {
        let Some(raw_model) = window
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            collapsed.push(window);
            continue;
        };
        let canonical_model = canonical_model_id(raw_model);
        if canonical_model.is_empty() {
            continue;
        }
        window.model = Some(canonical_model.clone());
        window.window = match window
            .token_type
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            Some(token_type) => format!("{token_type}:{canonical_model}"),
            None => canonical_model.clone(),
        };
        let key = (
            canonical_model.to_ascii_lowercase(),
            window
                .token_type
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase(),
        );
        if let Some(index) = indices.get(&key).copied() {
            if window.remaining_ratio > collapsed[index].remaining_ratio {
                collapsed[index] = window;
            } else {
                collapsed[index].checked_at_unix =
                    collapsed[index].checked_at_unix.max(window.checked_at_unix);
                collapsed[index].reset_at_unix =
                    collapsed[index].reset_at_unix.max(window.reset_at_unix);
            }
        } else {
            indices.insert(key, collapsed.len());
            collapsed.push(window);
        }
    }
    collapsed
}

pub(crate) fn canonicalize_native_response_body(
    body: &str,
    canonical_model: &str,
) -> anyhow::Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    canonicalize_native_response_value(&mut value, canonical_model);
    serde_json::to_string(&value).map_err(Into::into)
}

pub(crate) fn canonicalize_native_response_value(
    value: &mut serde_json::Value,
    canonical_model: &str,
) {
    let canonical_model = canonical_model.trim();
    if canonical_model.is_empty() {
        return;
    }
    let Some(object) = value.as_object_mut() else {
        return;
    };
    for field in ["model", "modelVersion", "model_version"] {
        let Some(reported_model) = object.get(field).and_then(serde_json::Value::as_str) else {
            continue;
        };
        if canonical_model_id(reported_model).eq_ignore_ascii_case(canonical_model) {
            object.insert(
                field.to_string(),
                serde_json::Value::String(canonical_model.to_string()),
            );
        }
    }
}

fn resolved_from_route(
    route: &AntigravityModelRoute,
    preserve_explicit_disable: bool,
) -> ResolvedAntigravityModel {
    ResolvedAntigravityModel {
        route_encodes_reasoning: route_encodes_reasoning(
            &route.canonical_model,
            &route.upstream_model,
        ),
        preserve_explicit_disable,
        canonical_model: route.canonical_model.clone(),
        upstream_model: route.upstream_model.clone(),
    }
}

fn route_availability(metadata: &serde_json::Value) -> Option<bool> {
    metadata
        .get("quotaInfo")
        .and_then(|quota| quota.get("remainingFraction"))
        .and_then(numeric_f64)
        .map(|remaining| remaining > 0.0)
        .or_else(|| {
            metadata
                .get("available")
                .and_then(serde_json::Value::as_bool)
        })
}

pub(crate) fn callable_upstream_model_id(model: &str) -> String {
    match model.trim().to_ascii_lowercase().as_str() {
        // Antigravity may still advertise retired aliases; resolve them to
        // the callable upstream route IDs before dispatch.
        "gemini-3.1-pro" | "gemini-3-pro-preview" | "gemini-3.1-pro-high" | "gemini-3-pro-high" => {
            "gemini-pro-agent".to_string()
        }
        "gemini-claude-sonnet-4-5" | "gemini-claude-sonnet-4-5-thinking" => {
            "claude-sonnet-4-6".to_string()
        }
        "gemini-claude-opus-4-5-thinking" => "claude-opus-4-6-thinking".to_string(),
        _ => model.trim().to_string(),
    }
}

fn canonical_model_and_effort(model: &str) -> (String, Option<String>) {
    let (model, suffix_effort) = split_model_suffix(model);
    let normalized = model.trim().to_ascii_lowercase();
    let mapped = match normalized.as_str() {
        "gemini-3.6-flash-high" => ("gemini-3.6-flash", Some("high")),
        "gemini-3.6-flash-medium" => ("gemini-3.6-flash", Some("medium")),
        "gemini-3.6-flash-low" => ("gemini-3.6-flash", Some("low")),
        "gemini-3.6-flash-tiered" => ("gemini-3.6-flash", Some("auto")),
        "gemini-3-flash-agent" => ("gemini-3.5-flash", Some("high")),
        "gemini-3.5-flash-low" => ("gemini-3.5-flash", Some("medium")),
        "gemini-3.5-flash-extra-low" => ("gemini-3.5-flash", Some("low")),
        "gemini-3-flash" => ("gemini-3-flash-preview", None),
        "gemini-pro-agent" | "gemini-3.1-pro-high" | "gemini-3-pro-high" => {
            ("gemini-3.1-pro-preview", Some("high"))
        }
        "gemini-3.1-pro-low" | "gemini-3-pro-low" => ("gemini-3.1-pro-preview", Some("low")),
        "gemini-3.1-pro" | "gemini-3-pro-preview" => ("gemini-3.1-pro-preview", None),
        "gemini-2.5-flash-thinking" => ("gemini-2.5-flash", Some("high")),
        "claude-opus-4-6-thinking" | "gemini-claude-opus-4-5-thinking" => {
            ("claude-opus-4-6", Some("high"))
        }
        "claude-sonnet-4-6-thinking"
        | "gemini-claude-sonnet-4-5"
        | "gemini-claude-sonnet-4-5-thinking" => ("claude-sonnet-4-6", Some("high")),
        "gpt-oss-120b-medium" => ("gpt-oss-120b", Some("medium")),
        _ => {
            if let Some(base) = normalized
                .strip_prefix("claude-")
                .and_then(|value| value.strip_suffix("-thinking"))
            {
                return (
                    format!("claude-{base}"),
                    suffix_effort.or_else(|| Some("high".to_string())),
                );
            }
            for (suffix, effort) in [
                ("-extra-low", "low"),
                ("-medium", "medium"),
                ("-high", "high"),
                ("-low", "low"),
                ("-tiered", "auto"),
            ] {
                if let Some(base) = normalized.strip_suffix(suffix) {
                    if base.starts_with("gemini-") || base.starts_with("gpt-oss-") {
                        return (
                            base.to_string(),
                            suffix_effort.or_else(|| Some(effort.to_string())),
                        );
                    }
                }
            }
            return (model.trim().to_string(), suffix_effort);
        }
    };
    (
        mapped.0.to_string(),
        suffix_effort.or_else(|| mapped.1.map(str::to_string)),
    )
}

fn split_model_suffix(model: &str) -> (&str, Option<String>) {
    let model = model.trim();
    let Some(open) = model.rfind('(') else {
        return (model, None);
    };
    if !model.ends_with(')') || open == 0 {
        return (model, None);
    }
    let effort = normalize_effort(&model[open + 1..model.len() - 1]);
    match effort {
        Some(effort) => (model[..open].trim(), Some(effort)),
        None => (model, None),
    }
}

fn reasoning_preference_from_body(body: &str) -> Option<ReasoningRoutePreference> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;

    let explicitly_disabled = value
        .pointer("/reasoning/enabled")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        || value
            .pointer("/thinking/type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case("disabled"));
    if explicitly_disabled {
        return Some(ReasoningRoutePreference::Disabled);
    }

    let budgets = [
        "/reasoning/max_tokens",
        "/reasoning/budget_tokens",
        "/thinking/budget_tokens",
        "/generationConfig/thinkingConfig/thinkingBudget",
        "/generation_config/thinking_config/thinking_budget",
    ]
    .into_iter()
    .filter_map(|pointer| value.pointer(pointer).and_then(numeric_i64))
    .collect::<Vec<_>>();
    if budgets.contains(&0) {
        return Some(ReasoningRoutePreference::Disabled);
    }

    for pointer in [
        "/reasoning/effort",
        "/reasoning_effort",
        "/thinking/level",
        "/thinking_level",
        "/output_config/effort",
        "/generationConfig/thinkingConfig/thinkingLevel",
        "/generation_config/thinking_config/thinking_level",
    ] {
        if let Some(preference) = value
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .and_then(ReasoningRoutePreference::parse)
        {
            return Some(preference);
        }
    }
    if let Some(budget) = budgets.first().copied() {
        return Some(ReasoningRoutePreference::from_budget(budget));
    }

    let automatic_mode = value
        .pointer("/reasoning/enabled")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        || value
            .pointer("/thinking/type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "adaptive" | "automatic" | "enabled"
                )
            })
        || value.pointer("/generationConfig/thinkingConfig").is_some()
        || value
            .pointer("/generation_config/thinking_config")
            .is_some()
        || (value.get("input").is_some() && value.get("reasoning").is_some());
    if automatic_mode {
        return Some(ReasoningRoutePreference::Automatic);
    }
    None
}

fn normalize_effort(effort: &str) -> Option<String> {
    ReasoningRoutePreference::parse(effort).map(|preference| preference.as_str().to_string())
}

fn numeric_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn numeric_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn fallback_upstream_model(
    canonical_model: &str,
    preference: Option<ReasoningRoutePreference>,
) -> String {
    if preference == Some(ReasoningRoutePreference::Disabled) {
        // Without catalog evidence, do not invent a fixed thinking route for
        // an explicit disable request. Keeping the canonical model plus the
        // native `thinkingBudget: 0` is safer than silently turning it into low.
        return canonical_model.to_string();
    }
    match canonical_model.to_ascii_lowercase().as_str() {
        "gemini-3.6-flash" => format!(
            "gemini-3.6-flash-{}",
            match preference {
                Some(ReasoningRoutePreference::Automatic) => "tiered",
                Some(ReasoningRoutePreference::Low) => "low",
                Some(ReasoningRoutePreference::Medium) => "medium",
                Some(ReasoningRoutePreference::High) => "high",
                Some(ReasoningRoutePreference::Disabled) => {
                    return canonical_model.to_string();
                }
                None => "high",
            }
        ),
        "gemini-3.5-flash" => match preference {
            Some(ReasoningRoutePreference::Low) => "gemini-3.5-flash-extra-low".to_string(),
            Some(ReasoningRoutePreference::Medium) => "gemini-3.5-flash-low".to_string(),
            _ => "gemini-3-flash-agent".to_string(),
        },
        "gemini-3.1-pro-preview" => match preference {
            Some(ReasoningRoutePreference::Low) => "gemini-3.1-pro-low".to_string(),
            _ => "gemini-pro-agent".to_string(),
        },
        "gemini-3-flash-preview" => "gemini-3-flash".to_string(),
        "gpt-oss-120b" => "gpt-oss-120b-medium".to_string(),
        "claude-opus-4-6" => "claude-opus-4-6-thinking".to_string(),
        _ => canonical_model.to_string(),
    }
}

fn route_encodes_reasoning(canonical_model: &str, upstream_model: &str) -> bool {
    if canonical_model.eq_ignore_ascii_case(upstream_model) {
        return false;
    }
    matches!(
        upstream_model.to_ascii_lowercase().as_str(),
        "gemini-3.6-flash-high"
            | "gemini-3.6-flash-medium"
            | "gemini-3.6-flash-low"
            | "gemini-3.6-flash-tiered"
            | "gemini-3-flash-agent"
            | "gemini-3.5-flash-low"
            | "gemini-3.5-flash-extra-low"
            | "gpt-oss-120b-medium"
    )
}

fn remove_nested_object_field(value: &mut serde_json::Value, parent: &str, field: &str) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let remove_parent = object
        .get_mut(parent)
        .and_then(serde_json::Value::as_object_mut)
        .map(|parent_object| {
            parent_object.remove(field);
            parent_object.is_empty()
        })
        .unwrap_or(false);
    if remove_parent {
        object.remove(parent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential_with_routes(routes: Vec<AntigravityModelRoute>) -> serde_json::Value {
        serde_json::json!({
            ANTIGRAVITY_MODEL_ROUTES_KEY: routes
        })
    }

    #[test]
    fn discovery_collapses_private_routes_to_canonical_models() {
        let catalog = serde_json::json!({
            "models": {
                "gemini-3.6-flash-medium": {"recommended": false},
                "gemini-3.6-flash-high": {"recommended": true},
                "gemini-3.6-flash-low": {"recommended": false},
                "gemini-3-flash": {"recommended": false},
                "gemini-3-flash-agent": {"recommended": false},
                "gemini-3.1-pro-high": {"recommended": false},
                "gemini-3.5-flash-low": {"recommended": false},
                "gemini-pro-agent": {"recommended": false},
                "claude-opus-4-6-thinking": {"recommended": false},
                "gpt-oss-120b-medium": {"recommended": false},
                "tab-model": {"recommended": true, "isInternal": true}
            }
        });

        assert_eq!(
            public_models_from_catalog(&catalog),
            vec![
                "gemini-3.6-flash",
                "claude-opus-4-6",
                "gemini-3-flash-preview",
                "gemini-3.5-flash",
                "gemini-3.1-pro-preview",
                "gpt-oss-120b",
            ]
        );
        let routes = model_routes_from_catalog(&catalog);
        assert_eq!(routes[0].upstream_model, "gemini-3.6-flash-high");
        assert!(routes[0].recommended);
        assert_eq!(
            routes
                .iter()
                .filter(|route| route.upstream_model == "gemini-pro-agent")
                .count(),
            1
        );
    }

    #[test]
    fn request_resolution_uses_discovered_route_and_reasoning_effort() {
        let routes = vec![
            AntigravityModelRoute {
                canonical_model: "gemini-3.6-flash".to_string(),
                upstream_model: "gemini-3.6-flash-high".to_string(),
                reasoning_effort: Some("high".to_string()),
                available: None,
                recommended: true,
            },
            AntigravityModelRoute {
                canonical_model: "gemini-3.6-flash".to_string(),
                upstream_model: "gemini-3.6-flash-medium".to_string(),
                reasoning_effort: Some("medium".to_string()),
                available: None,
                recommended: false,
            },
            AntigravityModelRoute {
                canonical_model: "gemini-3.6-flash".to_string(),
                upstream_model: "gemini-3.6-flash-low".to_string(),
                reasoning_effort: Some("low".to_string()),
                available: None,
                recommended: false,
            },
        ];
        let credential = credential_with_routes(routes);
        let resolved = resolve_model_for_request(
            &credential,
            "gemini-3.6-flash",
            &[r#"{"reasoning":{"effort":"medium"}}"#],
        );
        assert_eq!(resolved.canonical_model, "gemini-3.6-flash");
        assert_eq!(resolved.upstream_model, "gemini-3.6-flash-medium");
        assert!(resolved.route_encodes_reasoning);

        let default = resolve_model_for_request(&credential, "gemini-3.6-flash", &["{}"]);
        assert_eq!(default.upstream_model, "gemini-3.6-flash-high");
    }

    #[test]
    fn automatic_modes_choose_the_tiered_route_across_all_ingress_protocols() {
        let credential = credential_with_routes(vec![
            AntigravityModelRoute {
                canonical_model: "gemini-3.6-flash".to_string(),
                upstream_model: "gemini-3.6-flash-high".to_string(),
                reasoning_effort: Some("high".to_string()),
                available: None,
                recommended: true,
            },
            AntigravityModelRoute {
                canonical_model: "gemini-3.6-flash".to_string(),
                upstream_model: "gemini-3.6-flash-tiered".to_string(),
                reasoning_effort: Some("auto".to_string()),
                available: None,
                recommended: false,
            },
        ]);

        for body in [
            r#"{"messages":[],"reasoning":{"enabled":true}}"#,
            r#"{"input":[],"reasoning":{}}"#,
            r#"{"messages":[],"thinking":{"type":"adaptive"}}"#,
            r#"{"contents":[],"generationConfig":{"thinkingConfig":{}}}"#,
            r#"{"contents":[],"generationConfig":{"thinkingConfig":{"thinkingBudget":-1}}}"#,
        ] {
            let resolved = resolve_model_for_request(&credential, "gemini-3.6-flash", &[body]);
            assert_eq!(resolved.upstream_model, "gemini-3.6-flash-tiered");
            assert!(!resolved.preserve_explicit_disable);
        }

        let adaptive_with_depth = resolve_model_for_request(
            &credential,
            "gemini-3.6-flash",
            &[
                r#"{"messages":[],"thinking":{"type":"adaptive"},"output_config":{"effort":"high"}}"#,
            ],
        );
        assert_eq!(adaptive_with_depth.upstream_model, "gemini-3.6-flash-high");
    }

    #[test]
    fn explicit_disable_prefers_a_plain_route_and_preserves_budget_zero() {
        let credential = credential_with_routes(vec![
            AntigravityModelRoute {
                canonical_model: "gemini-3.6-flash".to_string(),
                upstream_model: "gemini-3.6-flash-high".to_string(),
                reasoning_effort: Some("high".to_string()),
                available: None,
                recommended: true,
            },
            AntigravityModelRoute {
                canonical_model: "gemini-3.6-flash".to_string(),
                upstream_model: "gemini-3.6-flash".to_string(),
                reasoning_effort: None,
                available: None,
                recommended: false,
            },
        ]);
        let prepared = prepare_subscription_request(
            &credential,
            "gemini-3.6-flash",
            r#"{"reasoning_effort":"none"}"#,
            r#"{"contents":[],"generationConfig":{"thinkingConfig":{"thinkingBudget":0}}}"#,
        )
        .expect("disabled request");

        assert_eq!(prepared.upstream_model, "gemini-3.6-flash");
        let body: serde_json::Value =
            serde_json::from_str(&prepared.native_body).expect("native JSON");
        assert_eq!(
            body.pointer("/generationConfig/thinkingConfig/thinkingBudget"),
            Some(&serde_json::json!(0))
        );
    }

    #[test]
    fn explicit_disable_is_not_stripped_when_only_fixed_routes_exist() {
        let credential = credential_with_routes(vec![AntigravityModelRoute {
            canonical_model: "gemini-3.6-flash".to_string(),
            upstream_model: "gemini-3.6-flash-high".to_string(),
            reasoning_effort: Some("high".to_string()),
            available: None,
            recommended: true,
        }]);
        let prepared = prepare_subscription_request(
            &credential,
            "gemini-3.6-flash",
            r#"{"reasoning":{"effort":"none"}}"#,
            r#"{"contents":[],"generationConfig":{"thinkingConfig":{"thinkingBudget":0}}}"#,
        )
        .expect("disabled request");

        assert_eq!(prepared.upstream_model, "gemini-3.6-flash-high");
        let body: serde_json::Value =
            serde_json::from_str(&prepared.native_body).expect("native JSON");
        assert_eq!(
            body.pointer("/generationConfig/thinkingConfig/thinkingBudget"),
            Some(&serde_json::json!(0))
        );
    }

    #[test]
    fn route_encoded_reasoning_is_removed_from_native_payload() {
        let resolved = ResolvedAntigravityModel {
            canonical_model: "gemini-3.6-flash".to_string(),
            upstream_model: "gemini-3.6-flash-medium".to_string(),
            route_encodes_reasoning: true,
            preserve_explicit_disable: false,
        };
        let body = remove_route_encoded_reasoning(
            r#"{"contents":[],"generationConfig":{"temperature":0.2,"thinkingConfig":{"thinkingLevel":"MEDIUM"}}}"#,
            &resolved,
        )
        .expect("sanitized body");
        let body: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(body["generationConfig"]["temperature"], 0.2);
        assert!(body.pointer("/generationConfig/thinkingConfig").is_none());
    }

    #[test]
    fn pro_alias_uses_the_callable_route_without_dropping_supported_thinking() {
        let catalog = serde_json::json!({
            "models": {
                "gemini-3.1-pro-high": {"recommended": true}
            }
        });
        let credential = credential_with_routes(model_routes_from_catalog(&catalog));
        let prepared = prepare_subscription_request(
            &credential,
            "gemini-3.1-pro-preview",
            r#"{"reasoning_effort":"high"}"#,
            r#"{"contents":[],"generationConfig":{"thinkingConfig":{"thinkingLevel":"HIGH"}}}"#,
        )
        .expect("prepared request");

        assert_eq!(prepared.canonical_model, "gemini-3.1-pro-preview");
        assert_eq!(prepared.upstream_model, "gemini-pro-agent");
        let body: serde_json::Value = serde_json::from_str(&prepared.native_body).expect("json");
        assert_eq!(
            body.pointer("/generationConfig/thinkingConfig/thinkingLevel")
                .and_then(serde_json::Value::as_str),
            Some("HIGH")
        );
    }

    #[test]
    fn claude_history_drops_only_empty_unsigned_text_parts() {
        let retained_parts = serde_json::json!([
            {"text": " "},
            {"text": "", "thoughtSignature": "text-signature"},
            {"text": "", "thought": true, "thoughtSignature": "thinking-signature"},
            {"text": "", "futureMetadata": true},
            {"inlineData": {"mimeType": "image/png", "data": "aW1hZ2U="}}
        ]);
        let mut model_parts = retained_parts.as_array().unwrap().clone();
        model_parts.insert(0, serde_json::json!({"text": ""}));
        let native = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "start"}]},
                {"role": "model", "parts": [{"text": ""}]},
                {"role": "model", "parts": [
                    {"text": ""},
                    {"functionCall": {"id": "call-1", "name": "read_file", "args": {}},
                     "thoughtSignature": "tool-signature"}
                ]},
                {"role": "user", "parts": [
                    {"functionResponse": {"id": "call-1", "name": "read_file", "response": {"result": ""}}}
                ]},
                {"role": "model", "parts": model_parts},
                {"role": "user", "parts": [{"text": "continue"}]}
            ]
        });
        // No tools declaration: history cleanup must not depend on tool normalization.
        let prepared = prepare_subscription_request(
            &serde_json::json!({}),
            "claude-opus-4-6",
            "{}",
            &native.to_string(),
        )
        .unwrap();
        let actual: serde_json::Value = serde_json::from_str(&prepared.native_body).unwrap();
        let mut expected = native;
        expected["contents"].as_array_mut().unwrap().remove(1);
        expected["contents"][1]["parts"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        expected["contents"][3]["parts"] = retained_parts;
        assert_eq!(actual, expected);
    }

    #[test]
    fn history_cleanup_leaves_gemini_and_unchanged_claude_bytes_alone() {
        for (model, native) in [
            (
                "gemini-2.5-pro",
                r#"{ "contents": [{"role":"model","parts":[{"text":""}]}] }"#,
            ),
            (
                "claude-opus-4-6",
                r#"{ "contents": [{"role":"user","parts":[{"text":"hello"}]}] }"#,
            ),
        ] {
            let prepared =
                prepare_subscription_request(&serde_json::json!({}), model, "{}", native).unwrap();
            assert_eq!(prepared.native_body, native, "{model}");
        }
    }

    #[test]
    fn claude_routes_use_antigravity_tool_schema_dialect_without_changing_gemini() {
        let native_body = serde_json::json!({
            "contents": [],
            "tools": [{
                "functionDeclarations": [
                    {
                        "name": "read_file",
                        "parametersJsonSchema": {
                            "$schema": "https://json-schema.org/draft/2020-12/schema",
                            "properties": {
                                "path": {"type": "string"},
                                "offset": {
                                    "type": "integer",
                                    "exclusiveMinimum": 0
                                },
                                "$schema": {
                                    "type": "string"
                                }
                            },
                            "required": ["path", "offset"]
                        }
                    },
                    {
                        "name": "noop",
                        "parametersJsonSchema": {}
                    }
                ]
            }],
            "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}}
        })
        .to_string();

        let claude = prepare_subscription_request(
            &serde_json::json!({}),
            "claude-opus-4-6",
            "{}",
            &native_body,
        )
        .expect("Claude request");
        let claude: serde_json::Value =
            serde_json::from_str(&claude.native_body).expect("Claude JSON");
        let first = &claude["tools"][0]["functionDeclarations"][0];
        assert!(first.get("parametersJsonSchema").is_none());
        assert_eq!(first["parameters"]["type"], "object");
        assert_eq!(first["parameters"]["properties"]["path"]["type"], "string");
        assert!(first["parameters"].get("$schema").is_none());
        assert!(
            first["parameters"]["properties"]["offset"]
                .get("exclusiveMinimum")
                .is_none()
        );
        assert_eq!(
            first["parameters"]["properties"]["offset"]["description"],
            "exclusiveMinimum: 0"
        );
        assert_eq!(
            first["parameters"]["properties"]["$schema"]["type"],
            "string"
        );
        assert_eq!(
            claude["tools"][0]["functionDeclarations"][1]["parameters"]["required"][0],
            "_placeholder"
        );
        assert_eq!(
            claude["toolConfig"]["functionCallingConfig"]["mode"],
            "VALIDATED"
        );

        let gemini = prepare_subscription_request(
            &serde_json::json!({}),
            "gemini-3.6-flash",
            "{}",
            &native_body,
        )
        .expect("Gemini request");
        let gemini: serde_json::Value =
            serde_json::from_str(&gemini.native_body).expect("Gemini JSON");
        assert!(
            gemini["tools"][0]["functionDeclarations"][0]
                .get("parameters")
                .is_none()
        );
        assert!(
            gemini["tools"][0]["functionDeclarations"][0]
                .get("parametersJsonSchema")
                .is_some()
        );
        assert_eq!(
            gemini["toolConfig"]["functionCallingConfig"]["mode"],
            "AUTO"
        );
    }

    #[test]
    fn claude_tool_normalization_preserves_explicit_tool_choice() {
        let native_body = serde_json::json!({
            "contents": [],
            "tools": [{
                "functionDeclarations": [{
                    "name": "noop",
                    "parametersJsonSchema": {"type": "object", "properties": {}}
                }]
            }],
            "toolConfig": {"functionCallingConfig": {"mode": "NONE"}}
        })
        .to_string();
        let prepared = prepare_subscription_request(
            &serde_json::json!({}),
            "claude-sonnet-4-6",
            "{}",
            &native_body,
        )
        .expect("Claude request");
        let body: serde_json::Value = serde_json::from_str(&prepared.native_body).expect("JSON");
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "NONE");
    }

    #[test]
    fn claude_tool_schema_lowers_the_antigravity_validated_dialect() {
        let schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$defs": {
                "Config": {
                    "type": "object",
                    "properties": {"value": {"type": "string"}}
                }
            },
            "type": "object",
            "properties": {
                "mode": {
                    "anyOf": [
                        {"const": "fast"},
                        {"const": "safe"}
                    ],
                    "default": "fast"
                },
                "limit": {
                    "type": "integer",
                    "exclusiveMinimum": 0,
                    "maximum": 10
                },
                "path": {
                    "type": ["string", "null"],
                    "minLength": 1
                },
                "options": {
                    "allOf": [
                        {
                            "type": "object",
                            "properties": {"enabled": {"type": "boolean"}},
                            "required": ["enabled"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "label": {"type": "string", "pattern": "^[a-z]+$"}
                            }
                        }
                    ]
                },
                "reference": {"$ref": "#/$defs/Config"},
                "$schema": {"type": "string"}
            },
            "required": ["mode", "limit", "path", "options", "reference", "$schema"],
            "additionalProperties": false
        });

        let lowered = normalize_antigravity_claude_tool_schema(Some(schema));
        assert_eq!(lowered["type"], "object");
        assert!(lowered.get("$schema").is_none());
        assert!(lowered.get("$defs").is_none());
        assert!(lowered.get("additionalProperties").is_none());
        assert!(
            lowered["description"]
                .as_str()
                .is_some_and(|description| description.contains("No extra properties allowed"))
        );

        let mode = &lowered["properties"]["mode"];
        assert_eq!(mode["type"], "string");
        assert_eq!(mode["enum"], serde_json::json!(["fast", "safe"]));
        assert!(mode.get("anyOf").is_none());
        assert!(mode.get("default").is_none());
        assert!(
            mode["description"]
                .as_str()
                .is_some_and(|description| description.contains("default: fast"))
        );

        let limit = &lowered["properties"]["limit"];
        assert_eq!(limit["maximum"], 10);
        assert!(limit.get("exclusiveMinimum").is_none());
        assert!(
            limit["description"]
                .as_str()
                .is_some_and(|description| description.contains("exclusiveMinimum: 0"))
        );

        let path = &lowered["properties"]["path"];
        assert_eq!(path["type"], "string");
        assert!(path.get("minLength").is_none());
        assert!(
            path["description"]
                .as_str()
                .is_some_and(|description| description.contains("nullable"))
        );
        assert!(
            !lowered["required"]
                .as_array()
                .expect("required")
                .contains(&serde_json::json!("path"))
        );

        let options = &lowered["properties"]["options"];
        assert!(options.get("allOf").is_none());
        assert_eq!(options["properties"]["enabled"]["type"], "boolean");
        assert_eq!(options["properties"]["label"]["type"], "string");
        assert!(options["properties"]["label"].get("pattern").is_none());
        assert_eq!(options["required"], serde_json::json!(["enabled"]));

        let reference = &lowered["properties"]["reference"];
        assert_eq!(reference["type"], "object");
        assert!(reference.get("$ref").is_none());
        assert!(
            reference["description"]
                .as_str()
                .is_some_and(|description| description.contains("See: Config"))
        );
        assert_eq!(reference["required"], serde_json::json!(["_placeholder"]));
        assert_eq!(lowered["properties"]["$schema"]["type"], "string");
    }

    #[test]
    fn quota_variants_collapse_to_best_canonical_route() {
        let windows = vec![
            QuotaWindow {
                source: "antigravity_fetch_available_models".to_string(),
                window: "gemini-3.6-flash-high".to_string(),
                remaining_ratio: 0.0,
                used_percent: 100.0,
                model: Some("gemini-3.6-flash-high".to_string()),
                ..Default::default()
            },
            QuotaWindow {
                source: "antigravity_fetch_available_models".to_string(),
                window: "gemini-3.6-flash-medium".to_string(),
                remaining_ratio: 0.8,
                used_percent: 20.0,
                model: Some("gemini-3.6-flash-medium".to_string()),
                ..Default::default()
            },
        ];
        let collapsed = canonicalize_quota_windows(windows);
        assert_eq!(collapsed.len(), 1);
        assert_eq!(collapsed[0].model.as_deref(), Some("gemini-3.6-flash"));
        assert_eq!(collapsed[0].remaining_ratio, 0.8);
    }

    #[test]
    fn explicit_private_route_remains_callable_for_backward_compatibility() {
        let credential = credential_with_routes(vec![AntigravityModelRoute {
            canonical_model: "gemini-3.6-flash".to_string(),
            upstream_model: "gemini-3.6-flash-low".to_string(),
            reasoning_effort: Some("low".to_string()),
            available: None,
            recommended: false,
        }]);

        let resolved = resolve_model_for_request(&credential, "gemini-3.6-flash-low", &["{}"]);
        assert_eq!(resolved.canonical_model, "gemini-3.6-flash");
        assert_eq!(resolved.upstream_model, "gemini-3.6-flash-low");
        assert!(resolved.route_encodes_reasoning);
    }

    #[test]
    fn unknown_catalog_route_is_preserved_in_both_directions() {
        let catalog = serde_json::json!({
            "models": {
                "future-provider-route": {"recommended": true}
            }
        });
        let routes = model_routes_from_catalog(&catalog);
        assert_eq!(
            public_models_from_routes(&routes),
            vec!["future-provider-route"]
        );

        let credential = credential_with_routes(routes);
        let resolved = resolve_model_for_request(&credential, "future-provider-route", &["{}"]);
        assert_eq!(resolved.canonical_model, "future-provider-route");
        assert_eq!(resolved.upstream_model, "future-provider-route");
        assert!(!resolved.route_encodes_reasoning);
    }

    #[test]
    fn unknown_tiered_route_survives_before_the_first_catalog_refresh() {
        let resolved =
            resolve_model_for_request(&serde_json::json!({}), "gemini-4-code-high", &["{}"]);
        assert_eq!(resolved.canonical_model, "gemini-4-code");
        assert_eq!(resolved.upstream_model, "gemini-4-code-high");
        assert!(!resolved.route_encodes_reasoning);
    }

    #[test]
    fn default_resolution_skips_an_exhausted_recommended_route() {
        let credential = credential_with_routes(vec![
            AntigravityModelRoute {
                canonical_model: "gemini-3.6-flash".to_string(),
                upstream_model: "gemini-3.6-flash-high".to_string(),
                reasoning_effort: Some("high".to_string()),
                available: Some(false),
                recommended: true,
            },
            AntigravityModelRoute {
                canonical_model: "gemini-3.6-flash".to_string(),
                upstream_model: "gemini-3.6-flash-medium".to_string(),
                reasoning_effort: Some("medium".to_string()),
                available: Some(true),
                recommended: false,
            },
        ]);

        let resolved = resolve_model_for_request(&credential, "gemini-3.6-flash", &["{}"]);
        assert_eq!(resolved.upstream_model, "gemini-3.6-flash-medium");
    }

    #[test]
    fn native_response_hides_the_private_route_name() {
        let body = canonicalize_native_response_body(
            r#"{"modelVersion":"gemini-3.6-flash-medium","candidates":[]}"#,
            "gemini-3.6-flash",
        )
        .expect("canonical response");
        let body: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(body["modelVersion"], "gemini-3.6-flash");
    }
}
