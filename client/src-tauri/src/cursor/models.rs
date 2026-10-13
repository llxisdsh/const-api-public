//! Cursor's catalog expands reasoning settings into model IDs. Public identity
//! and pricing use the base model; only this adapter selects the original ID.
use crate::{
    model::{ChannelCapabilityProfile, ChannelConfig},
    protocol::ir::{ReasoningConfig, ReasoningEffort, ReasoningMode},
    source_driver::SourceDriverId,
};
use anyhow::{Result, ensure};
use std::collections::HashSet;

struct Variant {
    public: String,
    effort: Option<ReasoningEffort>,
    thinking: bool,
    fast: bool,
}

fn variant(model: &str) -> Variant {
    let normalized = model.trim().to_ascii_lowercase();
    let known = [
        "claude-",
        "gpt-",
        "gemini-",
        "grok-",
        "cursor-grok-",
        "glm-",
        "kimi-",
        "muse-",
        "composer-",
    ]
    .iter()
    .any(|prefix| normalized.starts_with(prefix));
    let mut base = normalized.as_str();
    let fast = known && base.ends_with("-fast");
    if fast {
        base = base.strip_suffix("-fast").unwrap();
    }
    let mut thinking = false;
    if base.starts_with("claude-")
        && let Some(value) = base.strip_suffix("-thinking")
    {
        base = value;
        thinking = true;
    }
    let mut effort = None;
    if known {
        for suffix in [
            "extra-high",
            "xhigh",
            "minimal",
            "medium",
            "ultra",
            "high",
            "low",
            "none",
            "max",
        ] {
            if let Some(value) = base.strip_suffix(&format!("-{suffix}")) {
                base = value;
                effort = ReasoningEffort::parse(suffix);
                break;
            }
        }
    }
    if base.starts_with("claude-")
        && let Some(value) = base.strip_suffix("-thinking")
    {
        base = value;
        thinking = true;
    }
    let mut public = base
        .strip_prefix("cursor-")
        .filter(|_| base.starts_with("cursor-grok-"))
        .unwrap_or(base)
        .to_string();
    // Cursor's older Claude IDs put the numeric version before the family.
    if let Some((version, family)) = base.strip_prefix("claude-").and_then(|s| s.split_once('-'))
        && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        && version.chars().any(|c| c.is_ascii_digit())
        && matches!(family, "sonnet" | "opus" | "haiku" | "fable")
    {
        public = format!("claude-{family}-{}", version.replace('.', "-"));
    }
    public = crate::tool_model_metadata::model_identity(&public);
    Variant {
        public,
        effort,
        thinking,
        fast,
    }
}

pub(crate) fn public_id(model: &str) -> String {
    let v = variant(model);
    if v.fast {
        format!("{}-fast", v.public)
    } else {
        v.public
    }
}

pub(super) fn encoded_effort(model: &str) -> Option<ReasoningEffort> {
    variant(model).effort
}

fn standard_models<'a>(ids: impl Iterator<Item = &'a str>) -> HashSet<String> {
    ids.map(variant)
        .filter(|v| !v.fast)
        .map(|v| v.public)
        .collect()
}

impl Variant {
    fn public_in(&self, standard: &HashSet<String>) -> String {
        if self.fast && !standard.contains(&self.public) {
            // A Fast-only product is not evidence of a selectable speed mode.
            format!("{}-fast", self.public)
        } else {
            self.public.clone()
        }
    }
}

pub(crate) fn public_id_in_catalog(model: &str, ids: &[String]) -> String {
    variant(model).public_in(&standard_models(ids.iter().map(String::as_str)))
}

pub(super) fn fast_variant(model: &str, ids: &[String]) -> bool {
    let v = variant(model);
    v.fast
        && ids.iter().any(|id| {
            let other = variant(id);
            !other.fast && other.public == v.public
        })
}

pub(crate) fn same_family(left: &str, right: &str) -> bool {
    variant(left).public == variant(right).public
}

pub(crate) fn normalize_projection(
    models: &mut Vec<String>,
    profiles: &mut [ChannelCapabilityProfile],
) {
    for profile in profiles.iter_mut() {
        if profile.catalog_metadata && profile.catalog_source_model.is_none() {
            profile.catalog_source_model = Some(profile.model_pattern.clone());
        }
    }
    let standard = standard_models(
        models.iter().map(String::as_str).chain(
            profiles
                .iter()
                .filter(|p| p.catalog_metadata)
                .filter_map(|p| p.catalog_source_model.as_deref()),
        ),
    );
    *models = crate::config::dedupe_models(
        models
            .iter()
            .map(|id| variant(id).public_in(&standard))
            .collect(),
    );
    crate::config::sort_models_by_short_name(models);
    for profile in profiles {
        profile.model_pattern = variant(&profile.model_pattern).public_in(&standard);
    }
}

pub(crate) fn normalize_channel(channel: &mut ChannelConfig) {
    if channel.source_driver() != SourceDriverId::CursorSubscription {
        return;
    }
    // Old saved channels may predate source-ID evidence. Preserve their actual
    // advertised IDs before projecting, using the existing catalog field.
    let sources: std::collections::HashSet<_> = channel
        .capability_profiles
        .iter()
        .filter(|p| p.catalog_metadata)
        .map(|p| {
            p.catalog_source_model
                .as_ref()
                .unwrap_or(&p.model_pattern)
                .clone()
        })
        .collect();
    let covered: std::collections::HashSet<_> = sources.iter().map(|id| public_id(id)).collect();
    let unprojected = channel.models.iter().any(|id| *id != public_id(id));
    for id in &channel.models {
        // A partially populated old catalog must retain every original variant.
        // Once projected, do not mistake a public name for another wire route.
        if !sources.contains(id) && (unprojected || !covered.contains(&public_id(id))) {
            channel.capability_profiles.push(ChannelCapabilityProfile {
                protocol: channel.api_format.clone(),
                model_pattern: id.clone(),
                catalog_source_model: Some(id.clone()),
                catalog_metadata: true,
                capability_layer: "model_wire".into(),
                verification_state: "declared".into(),
                ..Default::default()
            });
        }
    }
    normalize_projection(&mut channel.models, &mut channel.capability_profiles);
    let ids = source_ids(channel);
    channel.public_model = public_id_in_catalog(&channel.public_model, &ids);
    channel.upstream_model = public_id_in_catalog(&channel.upstream_model, &ids);
}

pub(crate) fn source_ids(channel: &ChannelConfig) -> Vec<String> {
    let ids: Vec<_> = channel
        .capability_profiles
        .iter()
        .filter(|p| p.catalog_metadata)
        .filter_map(|p| p.catalog_source_model.clone())
        .collect();
    crate::config::dedupe_models(if ids.is_empty() {
        channel.models.clone()
    } else {
        ids
    })
}

fn depth(effort: ReasoningEffort) -> u8 {
    match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 1,
        ReasoningEffort::Low => 2,
        ReasoningEffort::Medium => 3,
        ReasoningEffort::High => 4,
        ReasoningEffort::XHigh => 5,
        ReasoningEffort::Max => 6,
        ReasoningEffort::Ultra => 7,
    }
}

pub(super) fn resolve<'a>(
    ids: &'a [String],
    requested: &str,
    reasoning: &ReasoningConfig,
    fast: bool,
) -> Result<&'a str> {
    let variants: Vec<_> = ids.iter().map(|id| (id, variant(id))).collect();
    let standard: HashSet<_> = variants
        .iter()
        .filter(|(_, v)| !v.fast)
        .map(|(_, v)| v.public.clone())
        .collect();
    let public = variant(requested).public_in(&standard);
    if public != requested.to_ascii_lowercase()
        && let Some(exact) = ids.iter().find(|id| id.eq_ignore_ascii_case(requested))
    {
        // Older clients can still deliberately request an exact private route.
        return Ok(exact);
    }
    let wants_thinking = reasoning.mode != ReasoningMode::Disabled;
    let selected = variants
        .iter()
        .filter_map(|(id, v)| {
            if v.public_in(&standard) != public {
                return None;
            }
            // Never upgrade ordinary requests to a paid speed variant. Fast
            // intent can fall back to the ordinary SKU when no Fast route exists.
            let fast_mode = v.fast && standard.contains(&v.public);
            if fast_mode && !fast {
                return None;
            }
            if reasoning.is_explicitly_disabled()
                && (v.thinking || v.effort.is_some_and(|e| e != ReasoningEffort::None))
            {
                return None;
            }
            let score = match reasoning.effort {
                Some(desired) => v
                    .effort
                    .map(|e| depth(e).abs_diff(depth(desired)))
                    .unwrap_or(1),
                None => match v.effort {
                    None => 0,
                    Some(ReasoningEffort::Medium) => 1,
                    Some(ReasoningEffort::Low) => 2,
                    Some(ReasoningEffort::High) => 3,
                    Some(ReasoningEffort::Minimal) => 4,
                    Some(ReasoningEffort::None) => 5,
                    Some(ReasoningEffort::XHigh) => 6,
                    Some(ReasoningEffort::Max) => 7,
                    Some(ReasoningEffort::Ultra) => 8,
                },
            };
            Some((
                (fast_mode != fast, score, v.thinking != wants_thinking, id),
                id.as_str(),
            ))
        })
        .min_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, id)| id);
    ensure!(
        selected.is_some(),
        "capability_mismatch: Cursor account catalog has no matching model/reasoning route for {requested}"
    );
    Ok(selected.unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<String> {
        serde_json::from_str(include_str!("model_catalog_fixture.json")).unwrap()
    }

    #[test]
    fn normalizes_catalog_names_without_collapsing_fast_or_unknown_products() {
        for (wire, public) in [
            ("claude-opus-5-5-xhigh", "claude-opus-5-5"),
            ("claude-4.6-sonnet-medium-thinking", "claude-sonnet-4-6"),
            ("claude-4.5-opus-high-thinking", "claude-opus-4-5"),
            ("gpt-5.5-extra-high-fast", "gpt-5.5-fast"),
            ("cursor-grok-4.5-medium", "grok-4.5"),
            ("muse-spark-1.3-minimal", "muse-spark-1.3"),
            ("gemini-3.8-flash-high", "gemini-3.8-flash"),
            ("gemini-3-flash", "gemini-3-flash-preview"),
            ("gemini-3.1-pro", "gemini-3.1-pro-preview"),
            ("composer-2.5-fast", "composer-2.5-fast"),
            ("new-vendor-high", "new-vendor-high"),
            ("gpt-5-mini", "gpt-5-mini"),
            ("kimi-k2.7-code", "kimi-k2.7-code"),
        ] {
            assert_eq!(public_id(wire), public);
        }
        for id in fixture() {
            assert_eq!(public_id(&public_id(&id)), public_id(&id), "{id}");
        }
    }

    #[test]
    fn catalog_aliases_preserve_cursor_wire_ids_without_cross_model_substitution() {
        let ids: Vec<String> = [
            "composer-2.5",
            "composer-2.5-fast",
            "gemini-3-flash",
            "gemini-3.1-pro",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let mut channel = crate::channel_from_supplier(
            "cursor-aliases".into(),
            &crate::default_supplier_config(),
        );
        channel.v2 = crate::channel_v2_contract_for_source(SourceDriverId::CursorSubscription);
        channel.models = ids.clone();
        normalize_channel(&mut channel);
        assert_eq!(source_ids(&channel), ids);
        assert_eq!(
            channel.models,
            [
                "composer-2.5",
                "gemini-3-flash-preview",
                "gemini-3.1-pro-preview"
            ]
        );
        let mut config = crate::default_config();
        config.allow_model_equivalence = false;
        for (public, wire) in [
            ("composer-2.5", "composer-2.5"),
            ("gemini-3-flash-preview", "gemini-3-flash"),
            ("gemini-3.1-pro-preview", "gemini-3.1-pro"),
        ] {
            assert!(
                crate::model_compatibility::model_compatibility_candidates(&config, public)
                    .is_empty()
            );
            assert_eq!(
                resolve(&ids, public, &ReasoningConfig::default(), false).unwrap(),
                wire
            );
            assert!(crate::proxy::channel_supports_model(&channel, public));
        }
        assert_eq!(
            resolve(&ids, "composer-2.5", &ReasoningConfig::default(), true).unwrap(),
            "composer-2.5-fast"
        );
        // Normalizing a stored legacy channel must never replace its wire IDs.
        normalize_channel(&mut channel);
        assert_eq!(source_ids(&channel), ids);
    }

    #[test]
    fn online_alias_refresh_reaches_saved_cursor_channels_without_restart() {
        // Metadata is process-global; isolate real activation from parallel tests.
        const CHILD_ENV: &str = "CONST_API_TEST_CURSOR_ALIAS_REFRESH";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "cursor::models::tests::online_alias_refresh_reaches_saved_cursor_channels_without_restart", "--nocapture"])
                .env(CHILD_ENV, "1")
                .output().unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let latest: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../resources/tool-model-metadata.json"))
                .unwrap();
        let mut old = latest.clone();
        let aliases = old["model_aliases"].as_object_mut().unwrap();
        aliases.remove("gemini-3-flash");
        aliases.remove("gemini-3.1-pro");
        let mut version = crate::tool_model_metadata::active_model_catalog_version();
        crate::tool_model_metadata::install_tool_model_metadata(
            &serde_json::to_vec(&old).unwrap(),
            &version.compatibility_version_id.clone(),
            version.clone(),
        )
        .unwrap();
        let mut channel = crate::channel_from_supplier(
            "cursor-saved-aliases".into(),
            &crate::default_supplier_config(),
        );
        channel.v2 = crate::channel_v2_contract_for_source(SourceDriverId::CursorSubscription);
        channel.models = vec!["gemini-3-flash".into(), "gemini-3.1-pro".into()];
        normalize_channel(&mut channel);
        assert!(!crate::proxy::channel_supports_model(
            &channel,
            "gemini-3-flash-preview"
        ));
        let before = channel.clone();
        version.release_id = "cursor-alias-online".into();
        version.sequence = Some(version.sequence.unwrap() + 1);
        version.source = "public".into();
        version.delivery_source = "network".into();
        crate::tool_model_metadata::install_tool_model_metadata(
            &serde_json::to_vec(&latest).unwrap(),
            &version.compatibility_version_id.clone(),
            version,
        )
        .unwrap();
        // No restart, re-detection or rewrite of the persisted channel.
        for (canonical, wire) in [
            ("gemini-3-flash-preview", "gemini-3-flash"),
            ("gemini-3.1-pro-preview", "gemini-3.1-pro"),
        ] {
            assert!(crate::proxy::channel_supports_model(&channel, canonical));
            assert_eq!(
                resolve(
                    &source_ids(&channel),
                    canonical,
                    &ReasoningConfig::default(),
                    false
                )
                .unwrap(),
                wire
            );
        }
        assert_eq!(channel.models, before.models);
        assert_eq!(channel.capability_profiles, before.capability_profiles);
    }

    #[test]
    fn resolves_every_public_model_only_to_an_advertised_variant() {
        let ids = fixture();
        assert_eq!(ids.len(), 256);
        let mut public = ids.clone();
        normalize_projection(&mut public, &mut []);
        assert_eq!(public.len(), 40);
        let default = ReasoningConfig::default();
        for id in public {
            assert_eq!(
                public_id_in_catalog(resolve(&ids, &id, &default, false).unwrap(), &ids),
                id
            );
        }
        assert_eq!(
            resolve(&ids, "claude-haiku-5-5", &default, false).unwrap(),
            "claude-haiku-5-5-medium"
        );
        assert_eq!(
            resolve(&ids, "gpt-5.2", &default, false).unwrap(),
            "gpt-5.2"
        );
        let high = ReasoningConfig {
            effort: Some(ReasoningEffort::XHigh),
            ..default.clone()
        };
        assert_eq!(
            resolve(&ids, "gpt-5.5", &high, false).unwrap(),
            "gpt-5.5-extra-high"
        );
        assert_eq!(
            resolve(&ids, "gpt-5.5-fast", &high, true).unwrap(),
            "gpt-5.5-extra-high-fast"
        );
        assert_eq!(
            resolve(&ids, "claude-haiku-5-5-low", &high, false).unwrap(),
            "claude-haiku-5-5-low"
        );
        let disabled = ReasoningConfig {
            effort: Some(ReasoningEffort::None),
            ..default
        };
        assert_eq!(
            resolve(&ids, "gpt-5.6-sol", &disabled, false).unwrap(),
            "gpt-5.6-sol-none"
        );
        assert!(resolve(&ids, "claude-haiku-5-5", &disabled, false).is_err());
        assert!(resolve(&ids, "invented-model", &high, false).is_err());
    }

    #[test]
    fn old_channel_roundtrip_keeps_all_wire_routes_and_other_drivers_unchanged() {
        let mut channel =
            crate::channel_from_supplier("cursor-names".into(), &crate::default_supplier_config());
        channel.v2 = crate::channel_v2_contract_for_source(SourceDriverId::CursorSubscription);
        channel.models = fixture();
        channel.capability_profiles = vec![ChannelCapabilityProfile {
            model_pattern: channel.models[0].clone(),
            catalog_source_model: Some(channel.models[0].clone()),
            catalog_metadata: true,
            ..Default::default()
        }];
        channel.public_model = "claude-haiku-5-5-low".into();
        channel.upstream_model = channel.public_model.clone();
        normalize_channel(&mut channel);
        assert_eq!(channel.public_model, "claude-haiku-5-5");
        assert_eq!(channel.models.len(), 40);
        let once = channel.clone();
        normalize_channel(&mut channel);
        assert_eq!(channel.capability_profiles, once.capability_profiles);
        let stored = serde_json::to_value(crate::model::ChannelConfigV2::from(channel)).unwrap();
        let restored: ChannelConfig =
            serde_json::from_value::<crate::model::ChannelConfigV2>(stored)
                .unwrap()
                .into();
        assert_eq!(source_ids(&restored).len(), 256);
        assert_eq!(restored.models, once.models);
        assert!(crate::proxy::channel_supports_model(
            &restored,
            "gpt-5.5-fast"
        ));
        let mut refreshed = restored.clone();
        refreshed.capability_profiles.clear();
        let detection = crate::model::ChannelDetectionResult {
            availability: None,
            models: restored.models.clone(),
            model_capability_evidence: restored.capability_profiles.clone(),
            surface_results: Vec::new(),
            detection_evidence: Vec::new(),
            quota_status: "unknown".into(),
            remaining_ratio: 0.0,
            quota_windows: Vec::new(),
            checks: Vec::new(),
            warnings: Vec::new(),
        };
        for _ in 0..2 {
            crate::supplier::merge_channel_detection_result(&mut refreshed, &detection);
            assert_eq!(source_ids(&refreshed).len(), 256);
            assert_eq!(
                refreshed.capability_profiles.len(),
                restored.capability_profiles.len()
            );
        }
        assert_eq!(
            resolve(
                &source_ids(&refreshed),
                "claude-haiku-5-5",
                &ReasoningConfig::default(),
                false
            )
            .unwrap(),
            "claude-haiku-5-5-medium"
        );
        let supplier = crate::supplier_from_channel_unfiltered(&restored);
        let registration = crate::supplier::supplier_register_payload(
            &crate::supplier::SupplierAgentConfig::single(supplier.clone()),
            &supplier,
        );
        assert_eq!(registration["models"], serde_json::json!(restored.models));
        let mut ordinary = once.clone();
        ordinary.v2 = crate::channel_v2_contract_for_source(SourceDriverId::OpenAiApi);
        ordinary.models = fixture();
        normalize_channel(&mut ordinary);
        assert_eq!(ordinary.models, fixture());
    }

    #[test]
    fn fast_mode_uses_original_ids_and_keeps_standalone_fast_products() {
        use crate::{
            cursor::{request, wire as w},
            protocol::kind::ProtocolKind,
        };
        let ids = fixture();
        for (protocol, base_body, field) in [
            (
                ProtocolKind::OpenAiChat,
                serde_json::json!({"messages":[{"role":"user","content":"hi"}]}),
                "service_tier",
            ),
            (
                ProtocolKind::OpenAiResponses,
                serde_json::json!({"input":"hi"}),
                "service_tier",
            ),
            (
                ProtocolKind::AnthropicMessages,
                serde_json::json!({"messages":[{"role":"user","content":"hi"}],"max_tokens":32}),
                "speed",
            ),
        ] {
            for (model, mode, expected_fast) in [
                ("gpt-5.5", None, false),
                ("gpt-5.5", Some("fast"), true),
                ("gpt-5.5-fast", None, true),
                ("claude-haiku-5-5", Some("fast"), false),
            ] {
                let mut body = base_body.clone();
                body["model"] = serde_json::json!(model);
                if let Some(mode) = mode {
                    body[field] = serde_json::json!(mode);
                }
                let prepared =
                    request::prepare_with_models(protocol, &body, model, "caller", &ids).unwrap();
                assert_eq!(prepared.fast, expected_fast, "{protocol:?} {model}");
                assert!(!prepared.model.ends_with("-fast"));
                let outer = w::fields(&prepared.run).unwrap();
                let run = w::fields(w::data(&outer, 1).unwrap()).unwrap();
                let wire_model = w::fields(w::data(&run, 9).unwrap()).unwrap();
                let wire_id = w::string(&wire_model, 1).unwrap();
                assert!(ids.contains(&wire_id.to_string()));
                assert_eq!(wire_id.ends_with("-fast"), expected_fast);
            }
        }
        for bad in [
            serde_json::json!("ultrafast"),
            serde_json::json!("flex"),
            serde_json::json!(12),
        ] {
            let body = serde_json::json!({"input":"hi","model":"gpt-5.5","service_tier":bad});
            assert!(
                request::prepare_with_models(
                    ProtocolKind::OpenAiResponses,
                    &body,
                    "gpt-5.5",
                    "caller",
                    &ids
                )
                .is_err()
            );
        }
        let standalone = vec!["grok-4.1-fast".to_string(), "gpt-5.5".to_string()];
        let mut public = standalone.clone();
        normalize_projection(&mut public, &mut []);
        assert!(public.contains(&standalone[0]));
        assert_eq!(
            resolve(
                &standalone,
                "grok-4.1-fast",
                &ReasoningConfig::default(),
                false
            )
            .unwrap(),
            "grok-4.1-fast"
        );
        assert!(!fast_variant("grok-4.1-fast", &standalone));
    }

    #[test]
    fn request_effort_selects_the_wire_route_without_leaking_it_to_the_response() {
        use crate::{
            cursor::{request, wire as w},
            protocol::kind::ProtocolKind,
        };
        let ids = fixture();
        for (protocol, body) in [
            (
                ProtocolKind::OpenAiChat,
                serde_json::json!({"messages":[{"role":"user","content":"hi"}],"reasoning_effort":"high"}),
            ),
            (
                ProtocolKind::OpenAiResponses,
                serde_json::json!({"input":"hi","reasoning":{"effort":"high"}}),
            ),
            (
                ProtocolKind::AnthropicMessages,
                serde_json::json!({"messages":[{"role":"user","content":"hi"}],"max_tokens":32,"thinking":{"type":"adaptive"},"output_config":{"effort":"high"}}),
            ),
            (
                ProtocolKind::GeminiNative,
                serde_json::json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"thinkingConfig":{"thinkingLevel":"HIGH"}}}),
            ),
        ] {
            let mut body = body;
            body["model"] = serde_json::json!("claude-haiku-5-5");
            let prepared =
                request::prepare_with_models(protocol, &body, "claude-haiku-5-5", "caller", &ids)
                    .unwrap();
            assert_eq!(prepared.model, "claude-haiku-5-5");
            let run = w::fields(w::data(&w::fields(&prepared.run).unwrap(), 1).unwrap()).unwrap();
            let model = w::fields(w::data(&run, 9).unwrap()).unwrap();
            let id = w::string(&model, 1).unwrap();
            assert!(
                id == "claude-haiku-5-5-high" || id == "claude-haiku-5-5-thinking-high",
                "{protocol:?}: {id}"
            );
            assert!(w::data(&model, 3).is_none());
        }
    }
}
