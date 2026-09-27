use crate::model::{
    ChannelConfig, ChannelExecutorLocator, ChannelProtocolBinding, ChannelSurfaceBinding,
};
#[cfg(test)]
use crate::model::{ProtocolVerification, SurfaceVerification};
use crate::protocol::kind::ProtocolKind;
use crate::surface::ApiSurface;
use anyhow::{Context, Result, anyhow};
use reqwest::Url;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChannelSurfaceTarget {
    pub(crate) surface: ApiSurface,
    pub(crate) base_url: String,
    pub(crate) endpoint_profile: String,
    pub(crate) auth_scheme: String,
    pub(crate) protocol: ProtocolKind,
}

pub(crate) fn parse_http_surface_base(base_url: &str) -> Result<Url> {
    let base_url = base_url.trim();
    let scheme_end = base_url
        .find("://")
        .ok_or_else(|| anyhow!("configured surface base must use HTTP or HTTPS"))?;
    let scheme = &base_url[..scheme_end];
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(anyhow!("configured surface base must use HTTP or HTTPS"));
    }
    let remainder = &base_url[scheme_end + 3..];
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty()
        || authority.starts_with('/')
        || authority.starts_with('\\')
        || base_url.contains('\\')
    {
        return Err(anyhow!("configured surface base must contain a host"));
    }
    if authority.contains('@') {
        return Err(anyhow!(
            "configured surface base must not contain user info"
        ));
    }

    let parsed = Url::parse(base_url).context("invalid configured surface base URL")?;
    if parsed.host_str().is_none() {
        return Err(anyhow!("configured surface base must contain a host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(anyhow!(
            "configured surface base must not contain user info"
        ));
    }
    if parsed.query().is_some() {
        return Err(anyhow!("configured surface base must not contain a query"));
    }
    if parsed.fragment().is_some() {
        return Err(anyhow!(
            "configured surface base must not contain a fragment"
        ));
    }
    Ok(parsed)
}

pub(crate) fn validate_channel_v2(channel: &ChannelConfig) -> Result<()> {
    crate::coding_plan::validate_key(channel)?;
    let driver = crate::source_driver::source_driver(channel.source_driver())?;
    if channel.v2.executor != driver.executor {
        return Err(anyhow!(
            "channel executor locator does not match source_driver execution kind"
        ));
    }
    if channel.v2.discovery.strategy.trim().is_empty() {
        return Err(anyhow!("channel discovery strategy is required"));
    }
    if channel.surface_bindings.is_empty() {
        return Err(anyhow!("channel must declare at least one surface binding"));
    }

    let mut surfaces = HashSet::new();
    for binding in &channel.surface_bindings {
        if !surfaces.insert(binding.surface) {
            return Err(anyhow!(
                "channel has duplicate surface {}",
                binding.surface.as_str()
            ));
        }
        let base = match driver.execution_kind() {
            crate::source_driver::ExecutionKind::HttpSurface => {
                Some(parse_http_surface_base(&binding.base_url).map_err(|error| {
                    anyhow!(
                        "{} surface has an invalid base URL: {error}",
                        binding.surface.as_str()
                    )
                })?)
            }
            _ => {
                if !matches!(
                    channel.v2.executor,
                    ChannelExecutorLocator::RetainedSubscription { .. }
                ) || !binding.base_url.trim().is_empty()
                {
                    return Err(anyhow!(
                        "{} subscription surface must use its retained executor locator without a base URL",
                        binding.surface.as_str()
                    ));
                }
                None
            }
        };
        if binding.endpoint_profile.trim().is_empty() || binding.auth_scheme.trim().is_empty() {
            return Err(anyhow!(
                "{} surface has an incomplete endpoint or authentication policy",
                binding.surface.as_str()
            ));
        }
        if binding.protocols.is_empty() {
            return Err(anyhow!(
                "{} surface must declare at least one protocol",
                binding.surface.as_str()
            ));
        }
        let preferred_count = binding
            .protocols
            .iter()
            .filter(|protocol| protocol.preferred)
            .count();
        if preferred_count != 1 {
            return Err(anyhow!(
                "{} surface must declare exactly one preferred protocol",
                binding.surface.as_str()
            ));
        }
        let mut protocols = HashSet::new();
        for protocol in &binding.protocols {
            let parsed = ProtocolKind::parse(&protocol.protocol).with_context(|| {
                format!(
                    "{} surface has invalid protocol {}",
                    binding.surface.as_str(),
                    protocol.protocol
                )
            })?;
            if surface_for_protocol(parsed) != binding.surface {
                return Err(anyhow!(
                    "protocol {} does not belong to {} surface",
                    parsed,
                    binding.surface.as_str()
                ));
            }
            if !protocols.insert(parsed) {
                return Err(anyhow!(
                    "{} surface has duplicate protocol {}",
                    binding.surface.as_str(),
                    parsed
                ));
            }
        }

        if !binding.operation_overrides.is_empty() && !driver.advanced {
            return Err(anyhow!(
                "operation endpoint overrides require an advanced source driver"
            ));
        }
        let mut overrides = HashSet::new();
        for endpoint in &binding.operation_overrides {
            if endpoint.operation.trim().is_empty()
                || endpoint.method.trim().is_empty()
                || endpoint.url.trim().is_empty()
            {
                return Err(anyhow!("operation endpoint override is incomplete"));
            }
            if !overrides.insert((
                endpoint.operation.trim().to_ascii_lowercase(),
                endpoint.method.trim().to_ascii_uppercase(),
            )) {
                return Err(anyhow!("duplicate operation endpoint override"));
            }
            let base = base.as_ref().ok_or_else(|| {
                anyhow!("operation endpoint overrides require an HTTP surface executor")
            })?;
            let override_url = parse_http_operation_override(&endpoint.url)
                .context("operation endpoint override has an invalid URL")?;
            if override_url.scheme() != base.scheme()
                || override_url.host_str() != base.host_str()
                || override_url.port_or_known_default() != base.port_or_known_default()
            {
                return Err(anyhow!(
                    "operation endpoint override cannot replace the configured authority"
                ));
            }
        }
    }

    if !channel.surface_bindings.iter().any(|binding| {
        binding.surface == channel.v2.default_target.surface
            && binding.protocols.iter().any(|protocol| {
                ProtocolKind::parse(&protocol.protocol).ok()
                    == Some(channel.v2.default_target.protocol)
            })
    }) {
        return Err(anyhow!(
            "channel default_target does not reference a declared surface protocol binding"
        ));
    }
    Ok(())
}

/// Validate user-supplied/persisted configuration, not the internal single-protocol
/// projections used by capability checks. The manifest owns a preset's connection.
pub(crate) fn validate_channel_source_policy(channel: &ChannelConfig) -> Result<()> {
    let driver = crate::source_driver::source_driver(channel.source_driver())?;
    if !driver.fixed_connection {
        return Ok(());
    }
    let matches = channel.v2.default_target == driver.default_target
        && channel.v2.discovery == driver.discovery
        && channel.surface_bindings.len() == driver.surfaces.len()
        && channel.surface_bindings.iter().all(|binding| {
            let Some(preset) = driver
                .surfaces
                .iter()
                .find(|item| item.surface == binding.surface)
            else {
                return false;
            };
            let base_matches = parse_http_surface_base(&binding.base_url)
                .ok()
                .zip(
                    preset
                        .base_url
                        .as_deref()
                        .and_then(|base| parse_http_surface_base(base).ok()),
                )
                .is_some_and(|(actual, expected)| {
                    actual.as_str().trim_end_matches('/') == expected.as_str().trim_end_matches('/')
                });
            base_matches
                && binding.endpoint_profile == preset.endpoint_profile
                && binding.auth_scheme == preset.auth_scheme
                && binding.operation_overrides.is_empty()
                && binding.protocols.len() == preset.protocols.len()
                && binding.protocols.iter().all(|protocol| {
                    preset.protocols.contains(&protocol.protocol)
                        && protocol.preferred == (protocol.protocol == preset.preferred_protocol)
                })
        });
    anyhow::ensure!(
        matches,
        "{} uses fixed official endpoints and protocols. Use Custom API Endpoint for other connections.",
        driver.title
    );
    Ok(())
}

/// Repairs representational drift from older channel editors without inventing
/// upstream capabilities. URL and endpoint validation remains strict; only
/// duplicate bindings, protocol preference flags, and a dangling default target
/// are normalized here.
pub(crate) fn repair_channel_surface_contract(channel: &mut ChannelConfig) {
    let declared_default = channel.v2.default_target.clone();
    for binding in &mut channel.surface_bindings {
        let mut protocols: Vec<ChannelProtocolBinding> = Vec::new();
        for mut protocol in std::mem::take(&mut binding.protocols) {
            let Ok(parsed) = ProtocolKind::parse(&protocol.protocol) else {
                protocols.push(protocol);
                continue;
            };
            if surface_for_protocol(parsed) != binding.surface {
                protocols.push(protocol);
                continue;
            }
            protocol.protocol = parsed.as_str().to_string();
            if let Some(existing) = protocols
                .iter_mut()
                .find(|candidate| candidate.protocol == protocol.protocol)
            {
                existing.preferred |= protocol.preferred;
            } else {
                protocols.push(protocol);
            }
        }
        binding.protocols = protocols;
        let preferred_count = binding
            .protocols
            .iter()
            .filter(|protocol| protocol.preferred)
            .count();
        if preferred_count == 1 || binding.protocols.is_empty() {
            continue;
        }
        let preferred_protocol = if binding.surface == declared_default.surface
            && binding.protocols.iter().any(|protocol| {
                ProtocolKind::parse(&protocol.protocol).ok() == Some(declared_default.protocol)
            }) {
            declared_default.protocol.as_str().to_string()
        } else {
            binding
                .protocols
                .iter()
                .find(|protocol| protocol.preferred)
                .unwrap_or(&binding.protocols[0])
                .protocol
                .clone()
        };
        for protocol in &mut binding.protocols {
            protocol.preferred = protocol.protocol == preferred_protocol;
        }
    }

    let default_is_declared = channel.surface_bindings.iter().any(|binding| {
        binding.surface == declared_default.surface
            && binding.protocols.iter().any(|protocol| {
                ProtocolKind::parse(&protocol.protocol).ok() == Some(declared_default.protocol)
            })
    });
    if !default_is_declared {
        if let Some(target) = channel.surface_bindings.iter().find_map(|binding| {
            binding
                .protocols
                .iter()
                .find(|protocol| protocol.preferred)
                .or_else(|| binding.protocols.first())
                .and_then(|protocol| ProtocolKind::parse(&protocol.protocol).ok())
                .map(|protocol| crate::model::ChannelTarget {
                    surface: binding.surface,
                    protocol,
                })
        }) {
            channel.v2.default_target = target;
        }
    }
    // Keep the in-memory V4 compatibility projection aligned with the repaired
    // V5 contract. Older runtime helpers still read these fields, although only
    // `surfaces` and `default_target` are persisted by V5.
    channel.api_format = channel.v2.default_target.protocol.as_str().to_string();
    channel.upstream_base_url = channel
        .surface_bindings
        .iter()
        .find(|binding| binding.surface == channel.v2.default_target.surface)
        .map(|binding| binding.base_url.clone())
        .unwrap_or_default();
    channel.supported_protocols = protocols_from_surface_bindings(channel);
}

pub(crate) fn normalize_channel_surface_bindings(
    channel: &ChannelConfig,
) -> Vec<ChannelSurfaceBinding> {
    if validate_channel_v2(channel).is_err() {
        return Vec::new();
    }

    let mut seen_surfaces = HashSet::new();
    let mut bindings = Vec::new();
    let http_surface = crate::source_driver::source_driver(channel.source_driver())
        .map(|driver| driver.execution_kind() == crate::source_driver::ExecutionKind::HttpSurface)
        .unwrap_or(true);
    for binding in &channel.surface_bindings {
        let base_url = binding.base_url.trim().trim_end_matches('/').to_string();
        if (http_surface && base_url.is_empty())
            || !seen_surfaces.insert(binding.surface)
            || binding.endpoint_profile.trim().is_empty()
            || binding.auth_scheme.trim().is_empty()
        {
            return Vec::new();
        }
        let Some(protocols) =
            normalize_runtime_protocol_bindings(&binding.protocols, binding.surface)
        else {
            return Vec::new();
        };
        if binding.operation_overrides.iter().any(|endpoint| {
            endpoint.operation.trim().is_empty()
                || endpoint.method.trim().is_empty()
                || !valid_saved_endpoint(&endpoint.url)
        }) {
            return Vec::new();
        }
        let mut normalized = binding.clone();
        normalized.base_url = base_url;
        normalized.endpoint_profile = binding.endpoint_profile.trim().to_string();
        normalized.auth_scheme = binding.auth_scheme.trim().to_string();
        normalized.protocols = protocols;
        bindings.push(normalized);
    }
    bindings
}

pub(crate) fn channel_surface_target(
    channel: &ChannelConfig,
    inbound_surface: ApiSurface,
    inbound_protocol: Option<ProtocolKind>,
    opaque: bool,
) -> Option<ChannelSurfaceTarget> {
    let bindings = normalize_channel_surface_bindings(channel);

    if opaque {
        return bindings
            .iter()
            .find(|binding| {
                binding.surface == inbound_surface && surface_binding_is_verified(binding)
            })
            .and_then(preferred_target_for_binding);
    }

    if let Some(protocol) = inbound_protocol {
        if let Some(target) = bindings
            .iter()
            .filter(|binding| binding.surface == inbound_surface)
            .find_map(|binding| target_for_protocol(binding, protocol))
        {
            return Some(target);
        }
    }

    // Resource/media operations have no generation protocol, but still belong
    // to a concrete surface. Do not mistake that for an unmatched protocol.
    if inbound_protocol.is_none() {
        if let Some(target) = bindings
            .iter()
            .find(|binding| binding.surface == inbound_surface)
            .and_then(preferred_target_for_binding)
        {
            return Some(target);
        }
    }

    // Once no exact protocol binding exists, the explicit channel default is
    // authoritative. The per-surface preference remains a compatibility
    // fallback for malformed or rejected defaults, not a hidden override of
    // the user's selected conversion target.
    default_target_for_channel(channel, &bindings).or_else(|| {
        bindings
            .iter()
            .find(|binding| binding.surface == inbound_surface)
            .and_then(preferred_target_for_binding)
    })
}

pub(crate) fn channel_protocol_target(
    channel: &ChannelConfig,
    protocol: ProtocolKind,
) -> Option<ChannelSurfaceTarget> {
    normalize_channel_surface_bindings(channel)
        .iter()
        .find_map(|binding| target_for_protocol(binding, protocol))
}

fn default_target_for_channel(
    channel: &ChannelConfig,
    bindings: &[ChannelSurfaceBinding],
) -> Option<ChannelSurfaceTarget> {
    bindings
        .iter()
        .find(|binding| binding.surface == channel.v2.default_target.surface)
        .and_then(|binding| target_for_protocol(binding, channel.v2.default_target.protocol))
}

pub(crate) fn preferred_channel_surface_target(
    channel: &ChannelConfig,
) -> Option<ChannelSurfaceTarget> {
    let bindings = normalize_channel_surface_bindings(channel);
    default_target_for_channel(channel, &bindings)
        .or_else(|| {
            bindings.iter().find_map(|binding| {
                binding
                    .protocols
                    .iter()
                    .find(|protocol| protocol.preferred)
                    .and_then(|protocol| target_for_protocol_name(binding, &protocol.protocol))
            })
        })
        .or_else(|| bindings.iter().find_map(preferred_target_for_binding))
}

pub(crate) fn protocols_from_surface_bindings(channel: &ChannelConfig) -> Vec<String> {
    let mut seen = HashSet::new();
    normalize_channel_surface_bindings(channel)
        .into_iter()
        .flat_map(|binding| binding.protocols)
        .filter_map(|protocol| {
            if !protocol_binding_is_usable(&protocol) {
                return None;
            }
            let parsed = ProtocolKind::parse(&protocol.protocol).ok()?;
            let value = parsed.as_str().to_string();
            seen.insert(value.clone()).then_some(value)
        })
        .collect()
}

fn normalize_runtime_protocol_bindings(
    bindings: &[ChannelProtocolBinding],
    surface: ApiSurface,
) -> Option<Vec<ChannelProtocolBinding>> {
    if bindings.is_empty() || bindings.iter().filter(|binding| binding.preferred).count() != 1 {
        return None;
    }
    let mut seen = HashSet::new();
    let normalized = bindings
        .iter()
        .map(|binding| {
            let protocol = ProtocolKind::parse(&binding.protocol).ok()?;
            if surface_for_protocol(protocol) != surface || !seen.insert(protocol) {
                return None;
            }
            let mut normalized = binding.clone();
            normalized.protocol = protocol.as_str().to_string();
            Some(normalized)
        })
        .collect::<Option<Vec<_>>>()?;
    (normalized.len() == bindings.len()).then_some(normalized)
}

fn surface_for_protocol(protocol: ProtocolKind) -> ApiSurface {
    match protocol {
        ProtocolKind::OpenAiResponses | ProtocolKind::OpenAiChat => ApiSurface::OpenAi,
        ProtocolKind::AnthropicMessages => ApiSurface::Anthropic,
        ProtocolKind::GeminiNative => ApiSurface::Gemini,
    }
}

fn target_for_protocol(
    binding: &ChannelSurfaceBinding,
    protocol: ProtocolKind,
) -> Option<ChannelSurfaceTarget> {
    binding.protocols.iter().find(|candidate| {
        protocol_binding_is_usable(candidate)
            && ProtocolKind::parse(&candidate.protocol).ok() == Some(protocol)
    })?;
    Some(ChannelSurfaceTarget {
        surface: binding.surface,
        base_url: binding.base_url.clone(),
        endpoint_profile: binding.endpoint_profile.clone(),
        auth_scheme: binding.auth_scheme.clone(),
        protocol,
    })
}

fn target_for_protocol_name(
    binding: &ChannelSurfaceBinding,
    protocol: &str,
) -> Option<ChannelSurfaceTarget> {
    target_for_protocol(binding, ProtocolKind::parse(protocol).ok()?)
}

fn preferred_target_for_binding(binding: &ChannelSurfaceBinding) -> Option<ChannelSurfaceTarget> {
    let protocol = binding
        .protocols
        .iter()
        .find(|protocol| protocol.preferred && protocol_binding_is_usable(protocol))
        .or_else(|| {
            binding
                .protocols
                .iter()
                .find(|protocol| protocol_binding_is_usable(protocol))
        })?;
    target_for_protocol_name(binding, &protocol.protocol)
}

fn protocol_binding_is_usable(binding: &ChannelProtocolBinding) -> bool {
    !matches!(
        binding
            .verification
            .state
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "rejected" | "failed" | "unsupported"
    )
}

fn surface_binding_is_verified(binding: &ChannelSurfaceBinding) -> bool {
    binding
        .verification
        .state
        .trim()
        .eq_ignore_ascii_case("verified")
}

fn valid_saved_endpoint(url: &str) -> bool {
    let url = url.trim();
    url.starts_with("https://") || url.starts_with("http://")
}

fn parse_http_operation_override(value: &str) -> Result<Url> {
    let value = value.trim();
    if value.contains('\\') {
        return Err(anyhow!(
            "operation endpoint override must not contain backslashes"
        ));
    }
    let parsed = Url::parse(value).context("invalid operation endpoint override URL")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(anyhow!(
            "operation endpoint override must use HTTP or HTTPS"
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(anyhow!(
            "operation endpoint override must not contain user info"
        ));
    }
    if parsed.fragment().is_some() {
        return Err(anyhow!(
            "operation endpoint override must not contain a fragment"
        ));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChannelCapabilityProfile;
    use crate::{channel_from_supplier, default_supplier_config};

    #[test]
    fn fixed_gateway_configuration_is_checked_at_ipc_and_persistence_boundaries() {
        use crate::source_driver::SourceDriverId;
        for id in [
            SourceDriverId::OpencodeGo,
            SourceDriverId::OpencodeZen,
            SourceDriverId::KiloGateway,
            SourceDriverId::ClineApi,
            SourceDriverId::CommandCode,
            SourceDriverId::KimiCode,
            SourceDriverId::GlmCodingPlan,
            SourceDriverId::MinimaxTokenPlan,
            SourceDriverId::OllamaCloud,
        ] {
            let driver = crate::source_driver::source_driver(id).unwrap();
            let mut channel = crate::coding_gateway::tests::channel(
                id,
                driver.surfaces[0].base_url.as_deref().unwrap(),
            );
            channel.surface_bindings = crate::source_driver_surface_bindings(id, "");
            channel.v2.credential_ref = "sk-cp-replacement-key".into();
            channel.v2.max_concurrency = 7;
            channel.price_ratio = 0.5;
            channel.surface_bindings[0].base_url.push('/');
            channel.surface_bindings[0].verification.state = "verified".into();
            let wire = serde_json::to_value(&channel).unwrap();
            let reloaded: ChannelConfig = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(reloaded.kind, "openai_compatible");
            assert_eq!(reloaded.v2.credential_ref, "sk-cp-replacement-key");
            assert_eq!(reloaded.v2.max_concurrency, 7);
            validate_channel_source_policy(&reloaded).unwrap();

            for mutate in [
                (|v: &mut serde_json::Value| {
                    v["surfaces"][0]["base_url"] = "https://proxy.example.test/v1".into();
                }) as fn(&mut serde_json::Value),
                |v| {
                    v["surfaces"][0]["base_url"] = format!(
                        "{}other-plan",
                        v["surfaces"][0]["base_url"].as_str().unwrap()
                    )
                    .into();
                },
                |v| {
                    v["surfaces"][0]["auth_scheme"] = "x_api_key".into();
                },
                |v| {
                    v["surfaces"][0]["endpoint_profile"] = "custom".into();
                },
                |v| {
                    v["discovery"]["strategy"] = "custom".into();
                },
                |v| {
                    v["default_target"]["protocol"] = "openai_responses".into();
                    v["surfaces"][0]["protocols"] =
                        serde_json::json!([{"protocol":"openai_responses","preferred":true}]);
                },
            ] {
                let mut altered = wire.clone();
                mutate(&mut altered);
                let error = serde_json::from_value::<ChannelConfig>(altered.clone()).unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("fixed official endpoints and protocols"),
                    "{id:?}: {error}"
                );

                // An in-memory config write must reject the same changes even
                // when it did not pass through the IPC deserializer.
                let raw: crate::model::ChannelConfigV2 = serde_json::from_value(altered).unwrap();
                let mut config = crate::default_config();
                config.channels = vec![raw.into()];
                let dir = tempfile::tempdir().unwrap();
                assert!(
                    crate::config::write_config_to_path(&dir.path().join("config.json"), &config)
                        .is_err()
                );
                assert!(!dir.path().join("config.json").exists());
            }
        }
    }

    #[test]
    fn fixed_gateway_policy_does_not_block_internal_protocol_probes_or_custom_channels() {
        use crate::source_driver::SourceDriverId;
        let channel = crate::coding_gateway::tests::channel(
            SourceDriverId::OpencodeGo,
            "https://opencode.ai/zen/go/v1",
        );
        let contexts = crate::detection::surface_probe_contexts(
            channel,
            crate::detection::SurfaceProbeMode::Creation,
        )
        .unwrap();
        assert_eq!(contexts.len(), 3);
        let custom = driver_channel("custom_endpoint", "openai_chat", "http://localhost:1234/v1");
        validate_channel_source_policy(&custom).unwrap();
        serde_json::from_value::<ChannelConfig>(serde_json::to_value(&custom).unwrap()).unwrap();
    }

    #[test]
    fn fixed_gateway_legacy_zen_upgrades_only_the_official_preset() {
        use crate::source_driver::SourceDriverId;
        let mut channel = crate::coding_gateway::tests::channel(
            SourceDriverId::OpencodeZen,
            "https://opencode.ai/zen/v1",
        );
        channel
            .surface_bindings
            .retain(|binding| binding.surface != ApiSurface::Gemini);
        channel.models = vec!["gemini-3.8-flash".into()];
        channel.v2.max_concurrency = 7;
        channel.surface_bindings[0].verification.state = "verified".into();
        let old = serde_json::to_value(&channel).unwrap();
        let upgraded: ChannelConfig = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(upgraded.models, channel.models);
        assert_eq!(upgraded.v2.credential_ref, channel.v2.credential_ref);
        assert_eq!(upgraded.v2.max_concurrency, 7);
        assert_eq!(
            &upgraded.surface_bindings[..2],
            channel.surface_bindings.as_slice()
        );
        let gemini = &upgraded.surface_bindings[2];
        assert_eq!(gemini.surface, ApiSurface::Gemini);
        assert_eq!(gemini.auth_scheme, "x_goog_api_key");
        assert_ne!(gemini.verification.state, "verified");
        assert!(
            upgraded
                .supported_protocols
                .iter()
                .any(|protocol| protocol == "gemini_native")
        );
        let contexts = crate::detection::surface_probe_contexts(
            upgraded,
            crate::detection::SurfaceProbeMode::Creation,
        )
        .unwrap();
        assert_eq!(contexts.len(), 4);

        // The in-memory save boundary performs the same upgrade as deserialization.
        let mut config = crate::default_config();
        config.channels = vec![channel];
        let dir = tempfile::tempdir().unwrap();
        let saved =
            crate::config::write_config_to_path(&dir.path().join("client.json"), &config).unwrap();
        assert_eq!(saved.channels[0].surface_bindings.len(), 3);
        for change in ["base_url", "auth_scheme", "endpoint_profile"] {
            let mut altered = old.clone();
            altered["surfaces"][0][change] = if change == "base_url" {
                "https://proxy.example.test/v1".into()
            } else {
                "custom".into()
            };
            assert!(serde_json::from_value::<ChannelConfig>(altered).is_err());
        }
    }

    fn driver_channel(kind: &str, api_format: &str, base_url: &str) -> ChannelConfig {
        let mut channel =
            channel_from_supplier(format!("channel-{kind}"), &default_supplier_config());
        let source_driver = match kind {
            "openai" => crate::source_driver::SourceDriverId::OpenAiApi,
            "anthropic" => crate::source_driver::SourceDriverId::AnthropicApi,
            "gemini" => crate::source_driver::SourceDriverId::GeminiApi,
            "azure_openai" => crate::source_driver::SourceDriverId::AzureOpenAi,
            "aws_bedrock" => crate::source_driver::SourceDriverId::BedrockMantle,
            _ => crate::source_driver::SourceDriverId::CustomEndpoint,
        };
        channel.set_source_driver(source_driver);
        channel.kind = kind.to_string();
        channel.api_format = api_format.to_string();
        channel.upstream_base_url = base_url.to_string();
        channel.supported_protocols.clear();
        channel.surface_bindings = crate::source_driver_surface_bindings(source_driver, base_url);
        channel
    }

    #[test]
    fn runtime_normalization_does_not_reconstruct_v5_bindings_from_legacy_fields() {
        let mut channel = driver_channel("openai", "openai_responses", "https://api.openai.com/v1");
        channel.surface_bindings.clear();

        assert!(normalize_channel_surface_bindings(&channel).is_empty());
    }

    #[test]
    fn runtime_normalization_does_not_repair_invalid_v5_bindings() {
        let mut channel = driver_channel("openai", "openai_responses", "https://api.openai.com/v1");
        channel.surface_bindings[0].protocols[0].preferred = false;
        channel.surface_bindings[0].verification = SurfaceVerification::default();
        channel.surface_bindings[0].protocols[0].verification = ProtocolVerification::default();

        assert!(normalize_channel_surface_bindings(&channel).is_empty());
    }

    #[test]
    fn runtime_normalization_does_not_repair_invalid_v5_default_target() {
        let mut channel = driver_channel("openai", "openai_responses", "https://api.openai.com/v1");
        channel.v2.default_target = crate::model::ChannelTarget {
            surface: crate::surface::ApiSurface::Anthropic,
            protocol: crate::protocol::kind::ProtocolKind::AnthropicMessages,
        };

        assert!(normalize_channel_surface_bindings(&channel).is_empty());
    }

    #[test]
    fn load_boundary_repair_deduplicates_protocols_and_rehomes_default_target() {
        let mut channel = driver_channel("openai", "openai_responses", "https://api.openai.com/v1");
        let duplicate = channel.surface_bindings[0].protocols[0].clone();
        channel.surface_bindings[0].protocols.push(duplicate);
        for protocol in &mut channel.surface_bindings[0].protocols {
            protocol.preferred = true;
        }
        channel.v2.default_target = crate::model::ChannelTarget {
            surface: ApiSurface::Anthropic,
            protocol: ProtocolKind::AnthropicMessages,
        };

        repair_channel_surface_contract(&mut channel);

        assert_eq!(channel.surface_bindings.len(), 1);
        assert_eq!(channel.surface_bindings[0].protocols.len(), 2);
        assert_eq!(
            channel.surface_bindings[0]
                .protocols
                .iter()
                .filter(|protocol| protocol.preferred)
                .count(),
            1
        );
        assert_eq!(channel.v2.default_target.surface, ApiSurface::OpenAi);
        assert_eq!(
            channel.api_format,
            channel.v2.default_target.protocol.as_str()
        );
        assert_eq!(channel.upstream_base_url, "https://api.openai.com/v1");
        assert!(
            channel.surface_bindings[0]
                .protocols
                .iter()
                .any(|protocol| {
                    ProtocolKind::parse(&protocol.protocol).ok()
                        == Some(channel.v2.default_target.protocol)
                })
        );
        validate_channel_v2(&channel).expect("repaired channel contract");
    }

    #[test]
    fn openai_driver_creates_one_surface_with_two_protocols() {
        let mut channel =
            channel_from_supplier("channel-openai".to_string(), &default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::OpenAiApi);
        channel.kind = "openai".to_string();
        channel.api_format = "openai_responses".to_string();
        channel.upstream_base_url = "https://api.openai.com/v1".to_string();
        channel.supported_protocols.clear();
        channel.surface_bindings = crate::source_driver_surface_bindings(
            crate::source_driver::SourceDriverId::OpenAiApi,
            &channel.upstream_base_url,
        );

        let bindings = normalize_channel_surface_bindings(&channel);
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].surface, ApiSurface::OpenAi);
        assert_eq!(bindings[0].protocols.len(), 2);
        assert!(
            bindings[0]
                .protocols
                .iter()
                .any(|protocol| protocol.protocol == "openai_responses" && protocol.preferred)
        );
    }

    #[test]
    fn bedrock_driver_creates_openai_and_anthropic_surfaces() {
        let channel = driver_channel(
            "aws_bedrock",
            "openai_responses",
            "https://bedrock.example/v1",
        );

        let bindings = normalize_channel_surface_bindings(&channel);
        assert_eq!(bindings.len(), 2);
        assert!(
            bindings
                .iter()
                .any(|binding| binding.surface == ApiSurface::OpenAi)
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.surface == ApiSurface::Anthropic)
        );
    }

    #[test]
    fn official_driver_contracts_create_only_their_declared_surfaces() {
        let cases = [
            (
                "anthropic",
                "anthropic_messages",
                "https://api.anthropic.com",
                vec![(ApiSurface::Anthropic, vec!["anthropic_messages"])],
            ),
            (
                "gemini",
                "gemini_native",
                "https://generativelanguage.googleapis.com",
                vec![
                    (ApiSurface::Gemini, vec!["gemini_native"]),
                    (ApiSurface::OpenAi, vec!["openai_chat"]),
                ],
            ),
            (
                "azure_openai",
                "openai_responses",
                "https://example.openai.azure.com/openai/v1",
                vec![(ApiSurface::OpenAi, vec!["openai_responses", "openai_chat"])],
            ),
        ];
        for (kind, api_format, base_url, expected) in cases {
            let channel = driver_channel(kind, api_format, base_url);
            let bindings = normalize_channel_surface_bindings(&channel);
            assert_eq!(bindings.len(), expected.len(), "kind={kind}");
            for (surface, protocols) in expected {
                let binding = bindings
                    .iter()
                    .find(|binding| binding.surface == surface)
                    .unwrap_or_else(|| panic!("kind={kind} missing surface {surface:?}"));
                assert_eq!(
                    binding
                        .protocols
                        .iter()
                        .map(|protocol| protocol.protocol.as_str())
                        .collect::<Vec<_>>(),
                    protocols,
                    "kind={kind} surface={surface:?}"
                );
            }
        }
    }

    #[test]
    fn custom_endpoint_keeps_only_explicit_protocols() {
        let mut channel = driver_channel(
            "custom_endpoint",
            "openai_chat",
            "https://gateway.example/v1",
        );
        channel.supported_protocols =
            vec!["openai_chat".to_string(), "anthropic_messages".to_string()];
        channel.surface_bindings = vec![
            ChannelSurfaceBinding {
                surface: ApiSurface::OpenAi,
                base_url: "https://gateway.example/v1".to_string(),
                endpoint_profile: "openai_compatible".to_string(),
                auth_scheme: "bearer".to_string(),
                protocols: vec![ChannelProtocolBinding {
                    protocol: "openai_chat".to_string(),
                    preferred: true,
                    verification: ProtocolVerification::default(),
                }],
                operation_overrides: Vec::new(),
                verification: SurfaceVerification::default(),
            },
            ChannelSurfaceBinding {
                surface: ApiSurface::Anthropic,
                base_url: "https://gateway.example/v1".to_string(),
                endpoint_profile: "anthropic_compatible".to_string(),
                auth_scheme: "x_api_key".to_string(),
                protocols: vec![ChannelProtocolBinding {
                    protocol: "anthropic_messages".to_string(),
                    preferred: true,
                    verification: ProtocolVerification::default(),
                }],
                operation_overrides: Vec::new(),
                verification: SurfaceVerification::default(),
            },
        ];
        let bindings = normalize_channel_surface_bindings(&channel);
        assert_eq!(bindings.len(), 2);
        assert!(
            bindings
                .iter()
                .any(|binding| binding.surface == ApiSurface::OpenAi)
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.surface == ApiSurface::Anthropic)
        );
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.surface == ApiSurface::Gemini)
        );
    }

    #[test]
    fn subscription_drivers_publish_only_their_known_native_contract() {
        let cases = [
            (
                crate::source_driver::SourceDriverId::OpenAiSubscription,
                "openai",
                ApiSurface::OpenAi,
                "openai_responses",
            ),
            (
                crate::source_driver::SourceDriverId::ClaudeSubscription,
                "claude",
                ApiSurface::Anthropic,
                "anthropic_messages",
            ),
            (
                crate::source_driver::SourceDriverId::GeminiSubscription,
                "gemini",
                ApiSurface::Gemini,
                "gemini_native",
            ),
            (
                crate::source_driver::SourceDriverId::GrokSubscription,
                "grok",
                ApiSurface::OpenAi,
                "openai_responses",
            ),
        ];
        for (source_driver, platform, surface, protocol) in cases {
            let mut channel =
                channel_from_supplier(format!("channel-{platform}"), &default_supplier_config());
            channel.set_source_driver(source_driver);
            channel.subscription.platform = platform.to_string();
            channel.surface_bindings = crate::source_driver_surface_bindings(source_driver, "");
            validate_channel_v2(&channel).expect("typed subscription executor contract");
            let bindings = normalize_channel_surface_bindings(&channel);
            assert_eq!(bindings.len(), 1, "platform={platform}");
            assert_eq!(bindings[0].surface, surface, "platform={platform}");
            assert!(bindings[0].base_url.is_empty(), "platform={platform}");
            assert_eq!(bindings[0].protocols.len(), 1, "platform={platform}");
            assert_eq!(bindings[0].protocols[0].protocol, protocol);
        }
    }

    #[test]
    fn opaque_request_never_crosses_surface() {
        let mut channel =
            channel_from_supplier("channel-anthropic".to_string(), &default_supplier_config());
        channel.kind = "anthropic".to_string();
        channel.api_format = "anthropic_messages".to_string();
        channel.upstream_base_url = "https://api.anthropic.com".to_string();
        channel.supported_protocols.clear();

        assert!(channel_surface_target(&channel, ApiSurface::OpenAi, None, true).is_none());
    }

    #[test]
    fn existing_v5_binding_preference_is_not_rewritten_from_legacy_projection() {
        let mut channel =
            channel_from_supplier("v5-preference".to_string(), &default_supplier_config());
        channel.surface_bindings = vec![ChannelSurfaceBinding {
            surface: ApiSurface::OpenAi,
            base_url: "https://api.example.test/v1".to_string(),
            endpoint_profile: "openai".to_string(),
            auth_scheme: "bearer".to_string(),
            protocols: vec![
                ChannelProtocolBinding {
                    protocol: "openai_chat".to_string(),
                    preferred: true,
                    verification: ProtocolVerification::default(),
                },
                ChannelProtocolBinding {
                    protocol: "openai_responses".to_string(),
                    preferred: false,
                    verification: ProtocolVerification::default(),
                },
            ],
            operation_overrides: Vec::new(),
            verification: SurfaceVerification::default(),
        }];
        channel.v2.default_target = crate::model::ChannelTarget {
            surface: ApiSurface::OpenAi,
            protocol: ProtocolKind::OpenAiResponses,
        };
        // A compatibility projection must not edit persisted V5 binding truth.
        channel.api_format = "openai_responses".to_string();

        let bindings = normalize_channel_surface_bindings(&channel);

        assert_eq!(bindings.len(), 1);
        assert_eq!(
            bindings[0]
                .protocols
                .iter()
                .filter(|protocol| protocol.preferred)
                .map(|protocol| protocol.protocol.as_str())
                .collect::<Vec<_>>(),
            vec!["openai_chat"]
        );
    }

    #[test]
    fn known_routes_use_exact_protocol_then_channel_default_target() {
        let mut channel =
            channel_from_supplier("surface-fallback".to_string(), &default_supplier_config());
        channel.surface_bindings = vec![
            ChannelSurfaceBinding {
                surface: ApiSurface::OpenAi,
                base_url: "https://openai.example.test/v1".to_string(),
                endpoint_profile: "openai".to_string(),
                auth_scheme: "bearer".to_string(),
                protocols: vec![ChannelProtocolBinding {
                    protocol: "openai_responses".to_string(),
                    preferred: true,
                    verification: ProtocolVerification::default(),
                }],
                operation_overrides: Vec::new(),
                verification: SurfaceVerification::default(),
            },
            ChannelSurfaceBinding {
                surface: ApiSurface::Anthropic,
                base_url: "https://anthropic.example.test".to_string(),
                endpoint_profile: "anthropic".to_string(),
                auth_scheme: "x_api_key".to_string(),
                protocols: vec![ChannelProtocolBinding {
                    protocol: "anthropic_messages".to_string(),
                    preferred: true,
                    verification: ProtocolVerification::default(),
                }],
                operation_overrides: Vec::new(),
                verification: SurfaceVerification::default(),
            },
        ];
        channel.v2.default_target = crate::model::ChannelTarget {
            surface: ApiSurface::Anthropic,
            protocol: ProtocolKind::AnthropicMessages,
        };

        let exact = channel_surface_target(
            &channel,
            ApiSurface::OpenAi,
            Some(ProtocolKind::OpenAiResponses),
            false,
        )
        .expect("exact OpenAI protocol");
        assert_eq!(exact.surface, ApiSurface::OpenAi);
        assert_eq!(exact.protocol, ProtocolKind::OpenAiResponses);

        let resource = channel_surface_target(&channel, ApiSurface::OpenAi, None, false)
            .expect("OpenAI resource/media surface");
        assert_eq!(resource.surface, ApiSurface::OpenAi);
        assert_eq!(resource.base_url, "https://openai.example.test/v1");

        let unmatched = channel_surface_target(
            &channel,
            ApiSurface::OpenAi,
            Some(ProtocolKind::OpenAiChat),
            false,
        )
        .expect("channel default fallback");
        assert_eq!(unmatched.surface, ApiSurface::Anthropic);
        assert_eq!(unmatched.protocol, ProtocolKind::AnthropicMessages);

        let channel_default = channel_surface_target(
            &channel,
            ApiSurface::Gemini,
            Some(ProtocolKind::GeminiNative),
            false,
        )
        .expect("channel default fallback");
        assert_eq!(channel_default.surface, ApiSurface::Anthropic);
        assert_eq!(channel_default.protocol, ProtocolKind::AnthropicMessages);

        let forced_openai = channel_protocol_target(&channel, ProtocolKind::OpenAiResponses)
            .expect("explicit cache-bound target protocol");
        assert_eq!(forced_openai.surface, ApiSurface::OpenAi);
        assert_eq!(forced_openai.protocol, ProtocolKind::OpenAiResponses);
    }

    #[test]
    fn verified_capability_evidence_cannot_revive_rejected_native_binding() {
        let mut channel =
            channel_from_supplier("channel-openai".to_string(), &default_supplier_config());
        channel.kind = "openai".to_string();
        channel.surface_bindings = vec![ChannelSurfaceBinding {
            surface: ApiSurface::OpenAi,
            base_url: "https://api.openai.com/v1".to_string(),
            endpoint_profile: "openai".to_string(),
            auth_scheme: "bearer".to_string(),
            protocols: vec![ChannelProtocolBinding {
                protocol: "openai_chat".to_string(),
                preferred: true,
                verification: ProtocolVerification {
                    state: "rejected".to_string(),
                    ..Default::default()
                },
            }],
            operation_overrides: Vec::new(),
            verification: SurfaceVerification {
                state: "rejected".to_string(),
                ..Default::default()
            },
        }];
        channel.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_chat".to_string(),
            verification_state: "verified".to_string(),
            verified_at_unix: 42,
            ..Default::default()
        }];

        let bindings = normalize_channel_surface_bindings(&channel);
        assert_eq!(bindings[0].verification.state, "rejected");
        assert_eq!(bindings[0].protocols[0].verification.state, "rejected");
        assert!(protocols_from_surface_bindings(&channel).is_empty());
        assert!(
            channel_surface_target(
                &channel,
                ApiSurface::OpenAi,
                Some(ProtocolKind::OpenAiChat),
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn rejected_capability_evidence_cannot_disable_verified_native_binding() {
        let mut channel =
            channel_from_supplier("channel-openai".to_string(), &default_supplier_config());
        channel.kind = "openai".to_string();
        channel.surface_bindings = vec![ChannelSurfaceBinding {
            surface: ApiSurface::OpenAi,
            base_url: "https://api.openai.com/v1".to_string(),
            endpoint_profile: "openai".to_string(),
            auth_scheme: "bearer".to_string(),
            protocols: vec![ChannelProtocolBinding {
                protocol: "openai_chat".to_string(),
                preferred: true,
                verification: ProtocolVerification {
                    state: "verified".to_string(),
                    checked_at_unix: 7,
                    summary: "native probe".to_string(),
                },
            }],
            operation_overrides: Vec::new(),
            verification: SurfaceVerification {
                state: "verified".to_string(),
                checked_at_unix: 7,
                summary: "native probe".to_string(),
            },
        }];
        channel.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_chat".to_string(),
            verification_state: "rejected".to_string(),
            verified_at_unix: 42,
            ..Default::default()
        }];

        let bindings = normalize_channel_surface_bindings(&channel);
        assert_eq!(bindings[0].verification.state, "verified");
        assert_eq!(bindings[0].protocols[0].verification.state, "verified");
        assert_eq!(
            protocols_from_surface_bindings(&channel),
            vec!["openai_chat".to_string()]
        );
        assert!(
            channel_surface_target(
                &channel,
                ApiSurface::OpenAi,
                Some(ProtocolKind::OpenAiChat),
                false,
            )
            .is_some()
        );
    }

    #[test]
    fn rejected_protocol_stays_visible_but_is_not_executable() {
        let mut channel =
            channel_from_supplier("channel-openai".to_string(), &default_supplier_config());
        channel.kind = "openai".to_string();
        channel.surface_bindings = vec![ChannelSurfaceBinding {
            surface: ApiSurface::OpenAi,
            base_url: "https://api.openai.com/v1".to_string(),
            endpoint_profile: "openai".to_string(),
            auth_scheme: "bearer".to_string(),
            protocols: vec![ChannelProtocolBinding {
                protocol: "openai_chat".to_string(),
                preferred: true,
                verification: ProtocolVerification {
                    state: "rejected".to_string(),
                    ..Default::default()
                },
            }],
            operation_overrides: Vec::new(),
            verification: SurfaceVerification::default(),
        }];

        assert_eq!(normalize_channel_surface_bindings(&channel).len(), 1);
        assert!(protocols_from_surface_bindings(&channel).is_empty());
        assert!(
            channel_surface_target(
                &channel,
                ApiSurface::OpenAi,
                Some(ProtocolKind::OpenAiChat),
                false
            )
            .is_none()
        );
    }

    #[test]
    fn advanced_operation_override_accepts_a_fixed_raw_query_on_the_bound_authority() {
        let mut channel =
            channel_from_supplier("fixed-query".to_string(), &default_supplier_config());
        channel.surface_bindings[0].operation_overrides = vec![crate::OperationEndpointOverride {
            operation: "chat_completions".to_string(),
            method: "POST".to_string(),
            url: "http://127.0.0.1:8317/fixed?alt=sse&escape=%2f&empty=&bare".to_string(),
        }];

        validate_channel_v2(&channel).expect("fixed query remains on the configured authority");
    }

    #[test]
    fn operation_override_rejects_userinfo_fragment_and_authority_replacement() {
        for unsafe_url in [
            "http://user@127.0.0.1:8317/fixed?ok=1",
            "http://127.0.0.1:8317/fixed?ok=1#fragment",
            "http://127.0.0.1:8318/fixed?ok=1",
        ] {
            let mut channel =
                channel_from_supplier("unsafe-override".to_string(), &default_supplier_config());
            channel.surface_bindings[0].operation_overrides =
                vec![crate::OperationEndpointOverride {
                    operation: "chat_completions".to_string(),
                    method: "POST".to_string(),
                    url: unsafe_url.to_string(),
                }];

            assert!(
                validate_channel_v2(&channel).is_err(),
                "unsafe override was accepted: {unsafe_url}"
            );
        }
    }
}
