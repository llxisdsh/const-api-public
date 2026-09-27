use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::{OnceLock, RwLock},
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ToolModelInfo {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) family: String,
    pub(crate) reasoning: bool,
    pub(crate) reasoning_efforts: Vec<String>,
    pub(crate) tool_call: bool,
    pub(crate) attachment: bool,
    pub(crate) context_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) supports_1m: bool,
    pub(crate) input_modalities: Vec<String>,
    pub(crate) output_modalities: Vec<String>,
    pub(crate) display_priority: i32,
    // Shared once per fetched list, including when a tool generates Claude
    // aliases after discovery. Never serialized into third-party tool schemas.
    pub(crate) presentation: Option<std::sync::Arc<crate::model_discovery::ModelPresentation>>,
}

pub(crate) fn tool_model_display_order(
    left: &ToolModelInfo,
    right: &ToolModelInfo,
) -> std::cmp::Ordering {
    right
        .display_priority
        .cmp(&left.display_priority)
        .then_with(|| left.id.cmp(&right.id))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ModelCatalogVersionInfo {
    pub(crate) release_id: String,
    pub(crate) model_version_id: String,
    pub(crate) compatibility_version_id: String,
    pub(crate) sequence: Option<i64>,
    pub(crate) source: String,
    pub(crate) delivery_source: String,
    pub(crate) delivery_sources: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolModelMetadataDocument {
    schema_version: u32,
    #[serde(default)]
    source_revision: String,
    #[serde(default)]
    pub(crate) compatibility_version_id: String,
    #[serde(default)]
    pub(crate) compatibility_groups: Vec<EmbeddedToolCompatibilityGroup>,
    #[serde(default)]
    pub(crate) catalog_models: Vec<EmbeddedToolCatalogModel>,
    #[serde(default)]
    pub(crate) models: HashMap<String, ToolModelCatalogEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmbeddedToolCompatibilityGroup {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) aliases: Vec<String>,
    #[serde(default)]
    pub(crate) models: Vec<String>,
    #[serde(default)]
    pub(crate) match_models: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmbeddedToolCatalogModel {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) display_name: String,
    #[serde(default)]
    pub(crate) vendor: String,
    #[serde(default)]
    pub(crate) family: String,
    #[serde(default)]
    pub(crate) context_tokens: u64,
    #[serde(default)]
    pub(crate) output_tokens: u64,
    #[serde(default)]
    pub(crate) input_modalities: Vec<String>,
    #[serde(default)]
    pub(crate) output_modalities: Vec<String>,
    #[serde(default)]
    pub(crate) reasoning: bool,
    #[serde(default)]
    pub(crate) tool_call: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolModelCatalogEntry {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    family: String,
    #[serde(default)]
    context_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    reasoning_efforts: Vec<String>,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    source_key: String,
}

pub(crate) fn tool_models_from_response(value: &Value) -> Vec<ToolModelInfo> {
    let presentation =
        crate::model_discovery::ModelPresentation::from_payload(value).map(std::sync::Arc::new);
    with_document(|document| {
        let mut seen = HashSet::new();
        let mut models = value
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let id = entry.get("id").and_then(Value::as_str)?.trim();
                let normalized = crate::config::public_model_name(id);
                if normalized.is_empty() || !seen.insert(normalized.clone()) {
                    return None;
                }
                let mut model = tool_model_from_entry(
                    id,
                    entry,
                    document.models.get(&normalized).or_else(|| {
                        document
                            .models
                            .get(&normalize_model_id(crate::config::short_model_name(id)))
                    }),
                );
                model.id = normalized.clone();
                model.display_name = normalized;
                if let Some(policy) = presentation.as_ref() {
                    let (priority, hidden) = policy.rank(&model.id);
                    if hidden {
                        return None;
                    }
                    model.display_priority = priority;
                    model.presentation = Some(policy.clone());
                }
                Some(model)
            })
            .collect::<Vec<_>>();
        models.sort_by(tool_model_display_order);
        models
    })
}

pub(crate) fn active_tool_catalog_models() -> Vec<EmbeddedToolCatalogModel> {
    with_document(|document| {
        document
            .catalog_models
            .iter()
            .cloned()
            .map(|mut model| {
                if let Some(metadata) = document.models.get(&normalize_model_id(&model.id)) {
                    if model.display_name.trim().is_empty() {
                        model.display_name = metadata.display_name.clone();
                    }
                    model.family = metadata.family.clone();
                    model.context_tokens = metadata.context_tokens;
                    model.output_tokens = metadata.output_tokens;
                    model.input_modalities = metadata.input_modalities.clone();
                    model.output_modalities = metadata.output_modalities.clone();
                    model.reasoning = metadata.reasoning;
                    model.tool_call = metadata.tool_call;
                }
                model
            })
            .collect()
    })
}

pub(crate) fn active_tool_models() -> Vec<ToolModelInfo> {
    let model_ids = active_tool_catalog_models()
        .into_iter()
        .map(|model| model.id)
        .collect::<Vec<_>>();
    tool_models_from_ids(&model_ids)
}

pub(crate) fn active_tool_compatibility_groups() -> Vec<EmbeddedToolCompatibilityGroup> {
    with_document(|document| document.compatibility_groups.clone())
}

pub(crate) fn active_tool_compatibility_version_id() -> String {
    with_document(|document| document.compatibility_version_id.trim().to_string())
}

pub(crate) fn active_tool_compatibility_release_id() -> String {
    let version = active_tool_compatibility_version_id();
    assert!(
        !version.is_empty(),
        "active compatibility version must not be empty"
    );
    format!("client-{version}")
}

pub(crate) fn tool_models_from_ids(model_ids: &[String]) -> Vec<ToolModelInfo> {
    let value = serde_json::json!({
        "data": model_ids.iter().map(|id| serde_json::json!({"id": id})).collect::<Vec<_>>()
    });
    tool_models_from_response(&value)
}

pub(crate) fn catalog_token_limits(id: &str) -> (Option<u64>, Option<u64>) {
    with_document(|document| {
        document
            .models
            .get(&crate::config::public_model_name(id))
            .map(|model| {
                (
                    positive(model.context_tokens),
                    positive(model.output_tokens),
                )
            })
            .unwrap_or_default()
    })
}

// Null is an explicit unknown after intersecting route capacities. Do not turn
// it back into a large static default; a missing field is the legacy fallback.
pub(crate) fn metadata_token_limit(value: Option<&Value>, fallback: Option<u64>) -> Option<u64> {
    match value {
        Some(Value::Null) => None,
        Some(value) => value
            .as_u64()
            .or_else(|| value.as_str()?.parse().ok())
            .filter(|value| *value > 0)
            .or(fallback),
        None => fallback,
    }
}

pub(crate) fn channel_token_limits(
    channel: &crate::model::ChannelConfig,
    id: &str,
) -> (Option<u64>, Option<u64>) {
    let mut context = None;
    let mut output = None;
    for profile in &channel.capability_profiles {
        if profile.catalog_metadata
            && profile.model_pattern.eq_ignore_ascii_case(id)
            && !matches!(profile.release_status.as_str(), "suspended" | "prepared")
            && profile.verification_state != "rejected"
        {
            context = crate::model_catalog::conservative_limit(context, profile.context_tokens);
            output = crate::model_catalog::conservative_limit(output, profile.output_tokens);
        }
    }
    let fallback = catalog_token_limits(id);
    (context.or(fallback.0), output.or(fallback.1))
}

pub(crate) fn route_token_limits(
    config: &crate::model::ClientConfig,
    id: &str,
    models: &[ToolModelInfo],
) -> (Option<u64>, Option<u64>, bool) {
    let id = crate::config::public_model_name(id);
    let mut candidates = vec![id.clone()];
    if config.allow_model_equivalence {
        candidates.extend(crate::model_compatibility::model_compatibility_candidates(
            config, &id,
        ));
    }
    // Hidden models remain callable. If a hidden compatibility candidate's
    // observed capacity is absent from the tool list, do not enlarge the route
    // window based only on the remaining (possibly larger) visible models.
    if let Some(policy) = models
        .iter()
        .find_map(|model| model.presentation.as_deref())
    {
        if candidates.iter().any(|candidate| {
            policy.rank(&crate::config::public_model_name(candidate)).1
                && !models
                    .iter()
                    .any(|model| crate::config::model_name_matches(&model.id, candidate))
        }) {
            return (None, None, false);
        }
    }
    let mut matching = models.iter().filter(|model| {
        candidates
            .iter()
            .any(|candidate| crate::config::model_name_matches(&model.id, candidate))
    });
    let Some(first) = matching.next() else {
        return (None, None, false);
    };
    let mut limits = (first.context_tokens, first.output_tokens, first.supports_1m);
    for model in matching {
        limits.0 = limits.0.zip(model.context_tokens).map(|(a, b)| a.min(b));
        limits.1 = limits.1.zip(model.output_tokens).map(|(a, b)| a.min(b));
        limits.2 &= model.supports_1m;
    }
    limits
}

fn tool_model_from_entry(
    id: &str,
    entry: &Value,
    catalog_entry: Option<&ToolModelCatalogEntry>,
) -> ToolModelInfo {
    let metadata = entry.get("const_api").unwrap_or(&Value::Null);
    let catalog_has_entry = catalog_entry.is_some();
    let catalog_entry = catalog_entry.cloned().unwrap_or_default();
    let mut reasoning = effective_capability(metadata, id, "reasoning", catalog_entry.reasoning);
    let mut reasoning_efforts = metadata
        .get("reasoning_efforts")
        .and_then(Value::as_array)
        .map(|values| {
            normalized_reasoning_efforts(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            )
        })
        .unwrap_or_else(|| normalized_reasoning_efforts(catalog_entry.reasoning_efforts));
    if capability_evidence_state(metadata, id, "reasoning") == Some(false) {
        reasoning = false;
    } else if !reasoning_efforts.is_empty() {
        reasoning = true;
    }
    if !reasoning {
        reasoning_efforts.clear();
    }
    // Keep the historical permissive default for models that are not yet in
    // the embedded metadata catalog. An explicit server capability still wins,
    // and known catalog entries can opt out by setting `tool_call: false`.
    let tool_call = effective_capability(
        metadata,
        id,
        "tool_calls",
        if catalog_has_entry {
            catalog_entry.tool_call
        } else {
            true
        },
    );
    let vision = effective_capability(
        metadata,
        id,
        "vision",
        catalog_entry
            .input_modalities
            .iter()
            .any(|value| value == "image"),
    );

    let mut input_modalities = normalized_modalities(catalog_entry.input_modalities);
    push_unique(&mut input_modalities, "text");
    if vision {
        push_unique(&mut input_modalities, "image");
    } else {
        input_modalities.retain(|value| value != "image");
    }
    let mut output_modalities = normalized_modalities(catalog_entry.output_modalities);
    push_unique(&mut output_modalities, "text");
    let attachment = input_modalities.iter().any(|value| value != "text");
    let context_tokens = metadata_token_limit(
        metadata.get("context_tokens"),
        positive(catalog_entry.context_tokens),
    );
    let output_tokens = metadata_token_limit(
        metadata.get("output_tokens"),
        positive(catalog_entry.output_tokens),
    );

    ToolModelInfo {
        id: id.to_string(),
        display_name: non_empty(&catalog_entry.display_name)
            .unwrap_or(id)
            .to_string(),
        family: catalog_entry.family,
        reasoning,
        reasoning_efforts,
        tool_call,
        attachment,
        context_tokens,
        output_tokens,
        supports_1m: context_tokens.is_some_and(|tokens| tokens >= 1_000_000)
            && metadata.get("supports_1m").and_then(Value::as_bool) != Some(false),
        input_modalities,
        output_modalities,
        display_priority: metadata
            .get("display_priority")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or_default(),
        presentation: None,
    }
}

fn effective_capability(metadata: &Value, model: &str, feature: &str, fallback: bool) -> bool {
    match capability_evidence_state(metadata, model, feature) {
        Some(value) => value,
        None => metadata.get(feature).and_then(Value::as_bool) == Some(true) || fallback,
    }
}

fn capability_evidence_state(metadata: &Value, model: &str, feature: &str) -> Option<bool> {
    let compact_state = metadata
        .get("capability_states")
        .and_then(|states| states.get(feature))
        .and_then(Value::as_bool);
    if compact_state == Some(true) {
        return Some(true);
    }
    let mut unsupported = compact_state == Some(false);
    for key in ["effective_capabilities", "native_capabilities"] {
        for evidence in metadata
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if evidence.get("feature").and_then(Value::as_str) != Some(feature) {
                continue;
            }
            let pattern = evidence
                .get("model_pattern")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !model_pattern_matches(pattern, model) {
                continue;
            }
            match evidence
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "supported" | "verified" | "declared" => return Some(true),
                "unsupported" | "rejected" => unsupported = true,
                _ => {}
            }
        }
    }
    unsupported.then_some(false)
}

pub(crate) fn model_pattern_matches(pattern: &str, model: &str) -> bool {
    let pattern = normalize_model_id(pattern);
    let model = normalize_model_id(model);
    if pattern.is_empty() {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == model;
    }
    let starts_anchored = !pattern.starts_with('*');
    let ends_anchored = !pattern.ends_with('*');
    let parts = pattern
        .split('*')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut cursor = 0;
    for (index, part) in parts.iter().enumerate() {
        let Some(found) = model[cursor..].find(part) else {
            return false;
        };
        if index == 0 && starts_anchored && found != 0 {
            return false;
        }
        cursor += found + part.len();
    }
    !ends_anchored || parts.last().is_none_or(|part| model.ends_with(part))
}

fn embedded_document() -> &'static ToolModelMetadataDocument {
    static DOCUMENT: OnceLock<ToolModelMetadataDocument> = OnceLock::new();
    DOCUMENT.get_or_init(|| {
        let document = serde_json::from_str::<ToolModelMetadataDocument>(include_str!(
            "../resources/tool-model-metadata.json"
        ))
        .expect("embedded tool model metadata must be valid");
        validate_tool_model_metadata_document(&document, None)
            .expect("embedded tool model metadata must be internally consistent");
        document
    })
}

struct ActiveToolModelMetadata {
    document: Option<ToolModelMetadataDocument>,
    version: ModelCatalogVersionInfo,
}

fn packaged_model_catalog_version() -> ModelCatalogVersionInfo {
    ModelCatalogVersionInfo {
        release_id: env!("CONST_API_PACKAGED_CATALOG_RELEASE_ID").to_string(),
        model_version_id: env!("CONST_API_PACKAGED_MODEL_VERSION_ID").to_string(),
        compatibility_version_id: env!("CONST_API_PACKAGED_COMPATIBILITY_VERSION_ID").to_string(),
        sequence: Some(
            env!("CONST_API_PACKAGED_CATALOG_SEQUENCE")
                .parse::<i64>()
                .expect("build script must provide a positive packaged catalog sequence"),
        ),
        source: "packaged".to_string(),
        delivery_source: "embedded".to_string(),
        delivery_sources: Vec::new(),
    }
}

fn active_metadata() -> &'static RwLock<ActiveToolModelMetadata> {
    static ACTIVE: OnceLock<RwLock<ActiveToolModelMetadata>> = OnceLock::new();
    ACTIVE.get_or_init(|| {
        let version = packaged_model_catalog_version();
        assert_eq!(
            version.compatibility_version_id,
            embedded_document().compatibility_version_id.trim(),
            "packaged catalog and tool metadata compatibility versions must match"
        );
        RwLock::new(ActiveToolModelMetadata {
            document: None,
            version,
        })
    })
}

fn with_document<T>(read: impl FnOnce(&ToolModelMetadataDocument) -> T) -> T {
    let guard = active_metadata()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.document.as_ref() {
        Some(document) => read(document),
        None => read(embedded_document()),
    }
}

pub(crate) fn active_model_catalog_version() -> ModelCatalogVersionInfo {
    active_metadata()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .version
        .clone()
}

#[tauri::command]
pub(crate) fn get_model_catalog_version_info() -> ModelCatalogVersionInfo {
    active_model_catalog_version()
}

pub(crate) fn parse_tool_model_metadata(
    raw: &[u8],
    expected_compatibility_version: Option<&str>,
) -> Result<ToolModelMetadataDocument> {
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    let document = ToolModelMetadataDocument::deserialize(&mut deserializer)
        .context("decode tool model metadata")?;
    deserializer
        .end()
        .context("tool model metadata contains trailing JSON")?;
    validate_tool_model_metadata_document(&document, expected_compatibility_version)?;
    Ok(document)
}

pub(crate) fn install_tool_model_metadata(
    raw: &[u8],
    expected_compatibility_version: &str,
    version: ModelCatalogVersionInfo,
) -> Result<()> {
    let document = parse_tool_model_metadata(raw, Some(expected_compatibility_version))?;
    install_tool_model_metadata_document(document, version)
}

pub(crate) fn install_tool_model_metadata_document(
    document: ToolModelMetadataDocument,
    version: ModelCatalogVersionInfo,
) -> Result<()> {
    validate_model_catalog_version(&version, &document.compatibility_version_id)?;
    let mut guard = active_metadata()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.document = Some(document);
    guard.version = version;
    Ok(())
}

pub(crate) fn restore_embedded_tool_model_metadata() {
    let mut guard = active_metadata()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.document = None;
    guard.version = packaged_model_catalog_version();
}

fn validate_model_catalog_version(
    version: &ModelCatalogVersionInfo,
    document_compatibility_version: &str,
) -> Result<()> {
    for (label, value) in [
        ("release", version.release_id.as_str()),
        ("model", version.model_version_id.as_str()),
        ("compatibility", version.compatibility_version_id.as_str()),
        ("source", version.source.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(anyhow!("active model catalog {label} version is required"));
        }
    }
    if version.sequence.is_some_and(|sequence| sequence <= 0) {
        return Err(anyhow!(
            "active model catalog sequence must be positive when present"
        ));
    }
    if version.compatibility_version_id.trim() != document_compatibility_version.trim() {
        return Err(anyhow!(
            "active model catalog compatibility version does not match tool metadata"
        ));
    }
    Ok(())
}

fn validate_tool_model_metadata_document(
    document: &ToolModelMetadataDocument,
    expected_compatibility_version: Option<&str>,
) -> Result<()> {
    if document.schema_version != 1 {
        return Err(anyhow!(
            "unsupported tool model metadata schema version {}",
            document.schema_version
        ));
    }
    if document.source_revision.trim().is_empty() {
        return Err(anyhow!("tool model metadata source revision is required"));
    }
    let compatibility_version = document.compatibility_version_id.trim();
    if compatibility_version.is_empty() {
        return Err(anyhow!(
            "tool model metadata compatibility version is required"
        ));
    }
    if expected_compatibility_version
        .map(str::trim)
        .filter(|expected| !expected.is_empty())
        .is_some_and(|expected| expected != compatibility_version)
    {
        return Err(anyhow!(
            "tool model metadata compatibility version does not match release"
        ));
    }
    if document.compatibility_groups.is_empty() || document.catalog_models.is_empty() {
        return Err(anyhow!(
            "tool model metadata requires compatibility groups and catalog models"
        ));
    }
    let mut catalog_ids = HashSet::new();
    for model in &document.catalog_models {
        let id = normalize_model_id(&model.id);
        if id.is_empty() || !catalog_ids.insert(id) {
            return Err(anyhow!(
                "tool model metadata contains an empty or duplicate catalog model"
            ));
        }
    }
    let mut group_ids = HashSet::new();
    let mut compatibility_key_owners = HashMap::<String, String>::new();
    for group in &document.compatibility_groups {
        let id = normalize_model_id(&group.id);
        if id.is_empty() || !group_ids.insert(id) || group.models.is_empty() {
            return Err(anyhow!(
                "tool model metadata contains an empty, duplicate, or model-less group"
            ));
        }
        for model in &group.models {
            if !catalog_ids.contains(&normalize_model_id(model)) {
                return Err(anyhow!(
                    "tool compatibility group {} references unknown model {}",
                    group.id,
                    model
                ));
            }
            let key = normalize_model_id(model);
            if let Some(owner) = compatibility_key_owners.insert(key.clone(), group.id.clone()) {
                if owner != group.id {
                    return Err(anyhow!(
                        "tool compatibility model {model} belongs to both {owner} and {}",
                        group.id
                    ));
                }
            }
        }
    }
    for group in &document.compatibility_groups {
        let mut group_match_models = HashSet::new();
        for model in &group.match_models {
            let key = normalize_model_id(model);
            if key.is_empty() || !group_match_models.insert(key.clone()) {
                return Err(anyhow!(
                    "tool compatibility group {} contains an empty or duplicate match model",
                    group.id
                ));
            }
            if let Some(owner) = compatibility_key_owners.insert(key, group.id.clone()) {
                if owner != group.id {
                    return Err(anyhow!(
                        "tool compatibility match model {model} belongs to both {owner} and {}",
                        group.id
                    ));
                }
            }
        }
    }
    let mut capability_ids = HashSet::new();
    for (model, metadata) in &document.models {
        let normalized = normalize_model_id(model);
        if normalized.is_empty()
            || model != &normalized
            || !capability_ids.insert(normalized.clone())
            || !catalog_ids.contains(&normalized)
        {
            return Err(anyhow!(
                "tool capability metadata contains a non-canonical, duplicate, or unknown model {model}"
            ));
        }
        if !metadata.reasoning && !metadata.reasoning_efforts.is_empty() {
            return Err(anyhow!(
                "tool capability metadata for {model} declares reasoning efforts without reasoning support"
            ));
        }
        if normalized_reasoning_efforts(metadata.reasoning_efforts.clone())
            != metadata.reasoning_efforts
        {
            return Err(anyhow!(
                "tool capability metadata for {model} contains non-canonical reasoning efforts"
            ));
        }
    }
    Ok(())
}

fn normalized_modalities(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = value.trim().to_ascii_lowercase();
        if matches!(value.as_str(), "text" | "audio" | "image" | "video" | "pdf") {
            push_unique(&mut out, &value);
        }
    }
    out
}

fn normalized_reasoning_efforts(values: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let mut value = value.trim().to_string();
        let canonical = value.to_ascii_lowercase();
        value = match canonical.as_str() {
            "x-high" | "x_high" | "extra-high" | "extra_high" => "xhigh".to_string(),
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra" => canonical,
            // Unknown provider values have no documented canonical spelling.
            // Preserve them exactly so an open tool schema can forward them.
            _ => value,
        };
        if !value.is_empty() && !out.iter().any(|current| current == &value) {
            out.push(value);
        }
    }
    out.sort_by(|left, right| {
        reasoning_effort_rank(left)
            .cmp(&reasoning_effort_rank(right))
            .then_with(|| left.cmp(right))
    });
    out
}

fn reasoning_effort_rank(value: &str) -> u8 {
    match value {
        "none" => 0,
        "minimal" => 1,
        "low" => 2,
        "medium" => 3,
        "high" => 4,
        "xhigh" => 5,
        "max" => 6,
        "ultra" => 7,
        _ => 100,
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|current| current == value) {
        values.push(value.to_string());
    }
}

fn normalize_model_id(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn positive(value: u64) -> Option<u64> {
    (value > 0).then_some(value)
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_routes_intersect_context_limits_only_when_enabled() {
        let models = tool_models_from_response(&serde_json::json!({"data": [
            {"id": "claude-sonnet-4-6", "const_api": {"context_tokens": 1000000}},
            {"id": "claude-haiku-4-5", "const_api": {"context_tokens": 200000}}
        ]}));
        let mut cfg = crate::default_config();
        cfg.account_platform_id = "context-platform".into();
        cfg.account_user_id = "context-user".into();
        cfg.model_compatibility_profiles.insert(
            "context-platform::context-user".into(),
            crate::model::LocalModelCompatibilityProfile {
                platform_id: cfg.account_platform_id.clone(),
                user_id: cfg.account_user_id.clone(),
                model_groups: vec![crate::model::ModelCompatibilityGroupConfig {
                    id: "same-tier".into(),
                    models: models.iter().map(|model| model.id.clone()).collect(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        cfg.allow_model_equivalence = false;
        assert_eq!(
            route_token_limits(&cfg, "claude-sonnet-4-6", &models).0,
            Some(1000000)
        );
        cfg.allow_model_equivalence = true;
        assert_eq!(
            route_token_limits(&cfg, "claude-sonnet-4-6", &models).0,
            Some(200000)
        );
        assert!(!route_token_limits(&cfg, "claude-sonnet-4-6", &models).2);
        assert_eq!(
            crate::model_compatibility::model_compatibility_candidates(
                &cfg,
                "claude-sonnet-4-6[1m]"
            ),
            vec!["claude-haiku-4-5"],
        );
    }

    #[test]
    fn rejected_catalog_capacity_does_not_override_fallback() {
        let mut channel =
            crate::channel_from_supplier("capacity-test".into(), &crate::default_supplier_config());
        channel.capability_profiles = vec![crate::model::ChannelCapabilityProfile {
            model_pattern: "unknown-capacity-model".into(),
            catalog_metadata: true,
            context_tokens: Some(1000000),
            verification_state: "rejected".into(),
            ..Default::default()
        }];
        assert_eq!(
            channel_token_limits(&channel, "unknown-capacity-model").0,
            None
        );
        channel.capability_profiles[0].verification_state = "declared".into();
        assert_eq!(
            channel_token_limits(&channel, "unknown-capacity-model").0,
            Some(1000000)
        );
    }

    #[test]
    fn detected_limits_override_static_defaults_and_unknown_intersections_stay_unknown() {
        let models = tool_models_from_response(&serde_json::json!({"data":[
            {"id":"claude-sonnet-4-6","const_api":{"context_tokens":1000000,"output_tokens":128000}},
            {"id":"claude-opus-4-6","const_api":{"context_tokens":64000,"output_tokens":8000}},
            {"id":"gpt-5.6-terra","const_api":{"context_tokens":null,"supports_1m":false}}
        ]}));
        let sonnet = models.iter().find(|m| m.id == "claude-sonnet-4-6").unwrap();
        assert_eq!(
            (
                sonnet.context_tokens,
                sonnet.output_tokens,
                sonnet.supports_1m
            ),
            (Some(1000000), Some(128000), true)
        );
        let opus = models.iter().find(|m| m.id == "claude-opus-4-6").unwrap();
        assert_eq!(opus.context_tokens, Some(64000));
        assert!(!opus.supports_1m);
        assert_eq!(
            models
                .iter()
                .find(|m| m.id == "gpt-5.6-terra")
                .unwrap()
                .context_tokens,
            None
        );
        let mut cfg = crate::default_config();
        cfg.allow_model_equivalence = false;
        assert!(route_token_limits(&cfg, "claude-sonnet-4-6", &models).2);
        let mut duplicates = vec![sonnet.clone(), sonnet.clone()];
        duplicates[1].context_tokens = Some(200000);
        duplicates[1].supports_1m = false;
        assert_eq!(
            route_token_limits(&cfg, "claude-sonnet-4-6", &duplicates).0,
            Some(200000)
        );
        assert!(!route_token_limits(&cfg, "claude-sonnet-4-6", &duplicates).2);
        duplicates[1].context_tokens = None;
        assert_eq!(
            route_token_limits(&cfg, "claude-sonnet-4-6", &duplicates).0,
            None
        );
    }

    #[test]
    fn tool_models_merge_runtime_evidence_with_generated_limits() {
        let models = tool_models_from_response(&serde_json::json!({
            "data": [{
                "id": "gpt-5.6-terra",
                "const_api": {
                    "reasoning": true,
                    "tool_calls": true,
                    "vision": true
                }
            }]
        }));
        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert!(model.reasoning && model.tool_call && model.attachment);
        assert_eq!(model.context_tokens, Some(372_000));
        assert_eq!(model.output_tokens, Some(128_000));
        assert_eq!(
            model.reasoning_efforts,
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert!(model.input_modalities.iter().any(|value| value == "pdf"));
    }

    #[test]
    fn codex_client_overlay_exposes_auto_review_image_input() {
        let models = tool_models_from_response(&serde_json::json!({
            "data": [{"id": "codex-auto-review"}]
        }));
        assert_eq!(models[0].context_tokens, Some(272_000));
        assert!(models[0].reasoning);
        assert_eq!(
            models[0].reasoning_efforts,
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert!(models[0].tool_call);
        assert!(models[0].attachment);
        assert!(
            models[0]
                .input_modalities
                .iter()
                .any(|value| value == "image")
        );
    }

    #[test]
    fn explicit_unsupported_evidence_overrides_catalog_fallback() {
        let models = tool_models_from_response(&serde_json::json!({
            "data": [{
                "id": "gpt-5.6-terra",
                "const_api": {
                    "reasoning": true,
                    "effective_capabilities": [{
                        "feature": "reasoning",
                        "state": "unsupported",
                        "model_pattern": "gpt-5.6-*"
                    }]
                }
            }]
        }));
        assert!(!models[0].reasoning);
        assert!(models[0].reasoning_efforts.is_empty());
    }

    #[test]
    fn runtime_reasoning_efforts_are_normalized_and_override_catalog_levels() {
        let models = tool_models_from_response(&serde_json::json!({
            "data": [{
                "id": "gpt-5.6-terra",
                "const_api": {
                    "reasoning": true,
                    "reasoning_efforts": ["MAX", "low", "Future-Tier", "x_high", "low"]
                }
            }]
        }));

        assert!(models[0].reasoning);
        assert_eq!(
            models[0].reasoning_efforts,
            ["low", "xhigh", "max", "Future-Tier"]
        );
    }

    #[test]
    fn unknown_models_keep_the_existing_tool_default_without_inventing_other_capabilities() {
        let models = tool_models_from_response(&serde_json::json!({
            "data": [{"id": "future-model"}]
        }));
        assert_eq!(models[0].context_tokens, None);
        assert!(!models[0].reasoning);
        assert!(models[0].tool_call);
        assert!(!models[0].attachment);
        assert_eq!(models[0].input_modalities, vec!["text"]);
    }

    #[test]
    fn glm_vision_variant_is_distinct_from_text_only_glm_models() {
        let models = tool_models_from_ids(&[
            "glm-5".to_string(),
            "glm-5.1".to_string(),
            "glm-5v-turbo".to_string(),
        ]);

        assert!(!models[0].attachment);
        assert!(!models[1].attachment);
        assert!(models[2].attachment);
        assert!(models[2].tool_call);
        assert_eq!(models[2].context_tokens, Some(200_000));
    }

    #[test]
    fn embedded_compatibility_catalog_includes_active_models_without_tool_metadata() {
        let models = &embedded_document().catalog_models;
        assert!(models.iter().any(|model| model.id == "glm-5.2"));
        assert!(models.iter().any(|model| model.id == "hy3"));
    }

    #[test]
    fn active_catalog_joins_packaged_capabilities() {
        let model = active_tool_catalog_models()
            .into_iter()
            .find(|model| model.id == "gpt-5.5")
            .expect("gpt-5.5 catalog model");
        assert_eq!(model.family, "gpt-5");
        assert_eq!(model.context_tokens, 272_000);
        assert_eq!(model.output_tokens, 128_000);
        assert_eq!(model.input_modalities, ["image", "pdf", "text"]);
        assert!(model.reasoning);
        assert!(model.tool_call);
    }

    #[test]
    fn qwen_has_one_catalog_entry_and_legacy_wire_names_keep_packaged_limits() {
        let document = embedded_document();
        let short = document.models.get("qwen3.8-27b").unwrap();
        assert_eq!(short.context_tokens, 131_042);
        assert_eq!(short.output_tokens, 16_384);
        assert!(!document.models.contains_key("qwen/qwen3.8-27b"));
        assert!(
            document
                .catalog_models
                .iter()
                .any(|model| model.id == "qwen3.8-27b")
        );
        assert!(
            !document
                .catalog_models
                .iter()
                .any(|model| model.id == "qwen/qwen3.8-27b")
        );
        for id in ["qwen/qwen3.8-27b", "qwen3.8-27b"] {
            let models = tool_models_from_response(&serde_json::json!({"data": [{"id": id}]}));
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].id, "qwen3.8-27b"); // Tool IDs address CONST, not the upstream.
            assert_eq!(models[0].context_tokens, Some(short.context_tokens));
            assert_eq!(models[0].output_tokens, Some(short.output_tokens));
            assert_eq!(models[0].input_modalities, short.input_modalities);
        }
    }

    #[test]
    fn packaged_catalog_names_are_unique_lowercase_short_and_sorted() {
        let document = embedded_document();
        let mut previous = "";
        for model in &document.catalog_models {
            assert!(!model.id.contains('/'));
            assert_eq!(model.id, model.id.to_lowercase());
            assert!(
                model.id.as_str() > previous,
                "duplicate or unsorted model: {}",
                model.id
            );
            previous = &model.id;
        }
        for name in document.models.keys() {
            assert!(!name.contains('/'));
            assert_eq!(*name, name.to_lowercase());
        }
    }

    #[test]
    fn every_tool_snapshot_uses_short_sorted_ids_and_first_collision_metadata() {
        let payload = serde_json::json!({"data": [
            {"id": "Vendor/Zulu"},
            {"id": "First/MiniMax:free", "const_api": {"context_tokens": 1234}},
            {"id": "Second/minimax:free", "const_api": {"context_tokens": 999999}},
            {"id": "MINIMAX:FREE"},
            {"id": "models/path/Alpha[1M]"},
            {"id": "qwen/qwen3.8-27b"},
            {"id": ""}, {"id": "vendor/"}, null
        ]});
        let models = tool_models_from_response(&payload);
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "minimax:free", "qwen3.8-27b", "zulu"]
        );
        assert_eq!(models[1].context_tokens, Some(1234));
        assert!(models.iter().all(|model| model.display_name == model.id));
        assert_eq!(payload["data"][1]["id"], "First/MiniMax:free");
    }

    #[test]
    fn active_tool_models_provide_offline_config_fallback_metadata() {
        let models = active_tool_models();
        let model = models
            .iter()
            .find(|model| model.id == "gpt-5.6-sol")
            .expect("gpt-5.6-sol active tool model");
        assert_eq!(model.context_tokens, Some(372_000));
        assert_eq!(
            model.reasoning_efforts,
            ["low", "medium", "high", "xhigh", "max", "ultra"]
        );
    }

    #[test]
    fn embedded_compatibility_policy_has_generated_release_and_tiers() {
        let compatibility_version = embedded_document().compatibility_version_id.trim();
        assert!(compatibility_version.starts_with("compat-v"));
        assert_eq!(compatibility_version.matches('-').count(), 4);
        let groups = &embedded_document().compatibility_groups;
        assert_eq!(
            groups
                .iter()
                .map(|group| group.label.as_str())
                .collect::<Vec<_>>(),
            ["T0", "T1", "T2", "T3", "T4", "T5", "T6", "T7"]
        );
        let standard = groups
            .iter()
            .find(|group| group.id == "standard")
            .expect("generated T6 group");
        assert!(
            standard
                .models
                .iter()
                .any(|model| model == "gpt-5.3-codex-spark")
        );
        assert!(
            standard
                .match_models
                .iter()
                .any(|model| model == "claude-sonnet-4-5-20250929")
        );
        let reviewer = groups
            .iter()
            .find(|group| group.id == "expert")
            .expect("generated T3 group");
        assert!(reviewer.models.iter().any(|model| model == "gpt-5.6-luna"));
        assert!(
            reviewer
                .match_models
                .iter()
                .any(|model| model == "codex-auto-review")
        );
        assert!(groups.iter().all(|group| {
            !group
                .models
                .iter()
                .any(|model| model == "codex-auto-review")
        }));
    }

    #[test]
    fn packaged_model_catalog_identity_matches_embedded_tool_metadata() {
        let version = packaged_model_catalog_version();
        assert!(!version.release_id.is_empty());
        assert!(!version.model_version_id.is_empty());
        assert!(version.sequence.is_some_and(|sequence| sequence > 0));
        assert_eq!(
            version.compatibility_version_id,
            embedded_document().compatibility_version_id
        );
        validate_model_catalog_version(&version, &embedded_document().compatibility_version_id)
            .expect("packaged catalog identity");
    }

    #[test]
    fn omitted_catalog_capability_is_conservative() {
        let metadata = serde_json::from_value::<ToolModelCatalogEntry>(serde_json::json!({}))
            .expect("catalog metadata");
        assert!(!metadata.reasoning);
        assert!(!metadata.tool_call);
    }
}
