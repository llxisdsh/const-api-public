//! The native Trae form's editable schema. Capability descriptors used by its
//! built-in model menus are a different, server-owned contract.
use super::*;

pub(super) type Fields = BTreeMap<String, Value>;

const HYPER_FIELDS: [&str; 6] = [
    "prompt_max_tokens",
    "max_tokens",
    "thinking_enable",
    "temperature",
    "top_p",
    "top_k",
];

#[derive(Clone, Default, Deserialize, Serialize)]
pub(super) struct AdvancedState {
    pub(super) applied: Fields,
    // The vendor can force vision off by model name (observed for GLM-5.1/5.2)
    // even after acknowledging the native form. Remember the effective value,
    // report the limitation, and don't repeat the same rejected capability write.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) image_input_limited: bool,
    // A protocol migration creates a new vendor ID. Values copied from the
    // user's old entry must not become managed defaults just because we wrote
    // them while migrating it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) user_overrides: Fields,
    // Write-ahead expectations, not proof of a successful write. No credentials.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) pending: Fields,
}

fn verification_error(model: &str, field: &str) -> anyhow::Error {
    // Only model identity and our editable field name, never remote payloads or keys.
    anyhow!(
        json!({
            "code": "tool_config_trae_advanced_verification_failed",
            "params": {"model": model, "field": field}
        })
        .to_string()
    )
}

impl AdvancedState {
    pub(super) fn confirm(&mut self, model: &str, remote: &Fields) -> Result<()> {
        let mut failure = None;
        for (key, expected) in std::mem::take(&mut self.pending) {
            if remote.get(&key) == Some(&expected) {
                self.applied.insert(key, expected);
            } else if key == "multimodal"
                && expected == true
                && remote.get(&key) == Some(&json!(false))
            {
                // A one-way, visible capability reduction is usable for text.
                // Never accept enabled vision when we explicitly requested off,
                // nor normalize credentials, protocol, context or other fields.
                self.image_input_limited = true;
                self.applied.insert(key, json!(false));
            } else {
                failure.get_or_insert_with(|| verification_error(model, &key));
                self.pending.insert(key, expected);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub(super) fn recover(&mut self, model: &str, remote: &Fields) -> Result<()> {
        // An explicit retry can recognize either a completed write or the old
        // value. A third value for an established baseline must be preserved.
        // Newly created fields have no old baseline: keep their expectation so
        // the explicit retry can finish initialization on the same owned ID.
        for (key, expected) in &self.pending {
            if remote.get(key) != Some(expected)
                && self.applied.contains_key(key)
                && remote.get(key) != self.applied.get(key)
            {
                return Err(verification_error(model, key));
            }
        }
        for (key, expected) in std::mem::take(&mut self.pending) {
            if remote.get(&key) == Some(&expected) {
                self.applied.insert(key, expected);
            } else if !self.applied.contains_key(&key) {
                self.pending.insert(key, expected);
            }
        }
        Ok(())
    }
}

pub(super) fn desired_fields(body: &Value) -> Fields {
    let detail = &body["config_detail"];
    let mut result = Fields::from([
        ("multimodal".into(), detail["multimodal"].clone()),
        (
            "thinking_enable".into(),
            detail["model_hyper_params"]["thinking_enable"].clone(),
        ),
    ]);
    for key in ["prompt_max_tokens", "max_tokens"] {
        if let Some(value) = detail["model_hyper_params"].get(key) {
            result.insert(key.into(), value.clone());
        }
    }
    result
}

pub(super) fn remote_fields(config: &Value, model: &Value) -> Fields {
    let custom = &model["custom_config"];
    let mut result = Fields::from([
        (
            "custom_model_type".into(),
            // get_detail_param stores the series on config_info_list entries,
            // alongside display_config, not inside model_detail_list/custom_config.
            config
                .get("custom_model_type")
                .filter(|value| value.as_str().is_some_and(|value| !value.is_empty()))
                .cloned()
                .unwrap_or_else(|| json!("other")),
        ),
        (
            "thinking_enable".into(),
            custom
                .get("thinking_enable")
                .filter(|value| !value.is_null())
                .cloned()
                .unwrap_or_else(|| json!(0)),
        ),
    ]);
    // A missing/unknown field is not a confirmed capability reduction.
    if let Some(value) = config["display_config"]
        .get("multimodal")
        .and_then(Value::as_bool)
    {
        result.insert("multimodal".into(), json!(value));
    }
    for key in ["prompt_max_tokens", "max_tokens", "max_turn"] {
        if let Some(value) = model.get(key).filter(|value| !value.is_null()) {
            result.insert(key.into(), value.clone());
        }
    }
    for key in ["temperature", "top_p", "top_k"] {
        if let Some(value) = custom.get(key).filter(|value| !value.is_null()) {
            result.insert(key.into(), value.clone());
        }
    }
    result
}

pub(super) fn plan(
    previous: Option<&AdvancedState>,
    current: &Fields,
    desired: &Fields,
) -> AdvancedState {
    let applied = previous
        .map(|state| state.applied.clone())
        .unwrap_or_else(|| {
            // Old CONST versions always wrote follow-default and omitted the model
            // series. Other old values have no baseline and must not be guessed.
            Fields::from([
                ("thinking_enable".into(), json!(0)),
                ("custom_model_type".into(), json!("other")),
            ])
        });
    let mut next = AdvancedState {
        applied,
        image_input_limited: previous.is_some_and(|state| state.image_input_limited)
            && desired.get("multimodal") == Some(&json!(true))
            && current.get("multimodal") == Some(&json!(false)),
        user_overrides: previous
            .map(|state| state.user_overrides.clone())
            .unwrap_or_default(),
        pending: Fields::new(),
    };
    for (key, target) in desired {
        if key == "multimodal" && next.image_input_limited {
            continue;
        } else if current.get(key) == Some(target) {
            next.applied.insert(key.clone(), target.clone());
            next.user_overrides.remove(key);
        } else if next
            .user_overrides
            .get(key)
            .is_some_and(|value| current.get(key) == Some(value))
        {
            continue;
        } else if next
            .applied
            .get(key)
            .is_some_and(|last| current.get(key) == Some(last))
            || previous.is_some_and(|state| {
                !state.applied.contains_key(key) && state.pending.contains_key(key)
            })
        {
            next.pending.insert(key.clone(), target.clone());
        }
        // A changed value belongs to the user. Retain the old baseline rather
        // than relabeling the user's choice as a value written by CONST.
    }
    next
}

pub(super) fn write_update(body: &mut Value, current: &Fields, changes: &Fields) {
    let mut merged = current.clone();
    merged.extend(changes.clone());
    if let Some(series) = merged
        .get("custom_model_type")
        .filter(|value| value.as_str() != Some("other"))
    {
        body["custom_model_type"] = series.clone();
    }
    // Native form updates submit the whole advanced form. Preserve sampling,
    // tool turns, and every unchanged capability when applying a series preset.
    let mut detail = serde_json::Map::new();
    for key in ["multimodal", "max_turn"] {
        if let Some(value) = merged.get(key) {
            detail.insert(key.into(), value.clone());
        }
    }
    let hyper: serde_json::Map<String, Value> = HYPER_FIELDS
        .iter()
        .filter_map(|key| merged.get(*key).map(|value| ((*key).into(), value.clone())))
        .collect();
    detail.insert("model_hyper_params".into(), Value::Object(hyper));
    body["config_detail"] = Value::Object(detail);
}

pub(super) fn series_hint(model: &ToolModelInfo) -> Option<&'static str> {
    // This selects a vendor preset, never a protocol or an invented capability.
    // Version boundaries avoid treating GPT-50 / Gemini-30 as GPT-5 / Gemini-3.
    for name in [
        model.id.rsplit('/').next().unwrap_or(&model.id),
        &model.family,
    ] {
        let name = name.to_ascii_lowercase();
        for (prefix, series) in [
            ("gpt-6", "gpt6"),
            ("gpt-5", "gpt5"),
            ("gemini-3", "gemini3"),
            ("deepseek-v4", "deepseek-v4"),
        ] {
            if name
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(['-', '.']))
            {
                return Some(series);
            }
        }
    }
    None
}

pub(super) fn parse_series(response: &Value) -> Result<BTreeSet<String>> {
    let invalid = || {
        anyhow!(crate::native_i18n::text(
            "Trae 返回了无法识别的模型系列配置，未写入模型，请更新 Trae 后重试。",
            "Trae returned an unsupported model-series configuration. No models were written; update Trae and retry.",
        ))
    };
    response
        .get("custom_model_type_list")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?
        .iter()
        .map(|entry| {
            entry
                .get("type_name")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(invalid)
        })
        .collect()
}
