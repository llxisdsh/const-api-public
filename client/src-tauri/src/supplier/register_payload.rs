pub(crate) fn supplier_register_message(config: &SupplierAgentConfig) -> SupplierMessage {
    let payload = serde_json::json!({
        "client_id": config.client_id,
        "client_version": env!("CARGO_PKG_VERSION"),
        "client_revision": option_env!("GITHUB_SHA").unwrap_or("unknown"),
        "client_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "session_id": config.session_id,
        "partial_update": true,
        "channels": config
            .suppliers
            .iter()
            .filter(|supplier| {
                supplier.source_driver != crate::source_driver::SourceDriverId::LanShare
            })
            .map(|supplier| supplier_register_payload(config, supplier))
            .collect::<Vec<_>>(),
        "transport_features": [
            SUPPLIER_TRANSPORT_FRAME_COMPRESSION_FEATURE,
            SUPPLIER_MESSAGE_CHUNKS_FEATURE,
            SUPPLIER_TRANSPORT_STREAM_RESUME_FEATURE,
            SUPPLIER_TRANSPORT_STREAM_PROGRESS_FEATURE
        ]
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    SupplierMessage {
        id: "register".to_string(),
        kind: "register".to_string(),
        payload,
    }
}

pub(crate) fn supplier_register_payload(
    agent: &SupplierAgentConfig,
    config: &SupplierConfig,
) -> serde_json::Map<String, serde_json::Value> {
    let execution_kind = crate::channel_executor::execution_kind_for_source(config.source_driver)
        .unwrap_or(crate::source_driver::ExecutionKind::HttpSurface);
    let is_subscription = execution_kind != crate::source_driver::ExecutionKind::HttpSurface
        || crate::coding_plan::has_quota(config.source_driver);
    let quota = if is_subscription {
        subscription_quota_snapshot(config)
    } else {
        SubscriptionQuotaSnapshot {
            status: "not_applicable".to_string(),
            remaining_ratio: 0.0,
            daily_used: 0,
            daily_limit: 0,
            checked_at_unix: now_unix(),
            quota_source: "unknown".to_string(),
            quota_window: String::new(),
            quota_used_percent: 0.0,
            quota_reset_at_unix: 0,
            quota_windows: Vec::new(),
        }
    };
    let safety = if is_subscription {
        subscription_safety_state_for_channel(config).ok()
    } else {
        None
    };
    let quota_reserve = quota_reserve_decision(
        &quota.quota_windows,
        config.quota_reserve_percent(),
        now_unix(),
    );
    let safety_state = safety
        .as_ref()
        .map(|state| state.state.clone())
        .unwrap_or_else(|| "not_applicable".to_string());
    let derived_health_status = match safety_state.as_str() {
        "risk_blocked" | "auth_error" | "disabled" => "blocked",
        "cooldown" => "degraded",
        _ => "available",
    };
    let health_status = config
        .registration_health_status
        .as_deref()
        .unwrap_or(derived_health_status);
    let derived_quota_status = if safety_state == "quota_exhausted" {
        "exhausted".to_string()
    } else {
        quota.status.clone()
    };
    let quota_status = config
        .registration_quota_status
        .clone()
        .unwrap_or(derived_quota_status);
    let channel_id = if config.channel_id.trim().is_empty() {
        config.node_id.trim().to_string()
    } else {
        config.channel_id.trim().to_string()
    };
    let unit_id = supplier_unit_id(&agent.client_id, &channel_id);
    let provider_kind = supplier_provider_kind(config);
    let endpoint_kind = supplier_endpoint_kind(config);
    let cache_domain_id = crate::channel_identity::channel_cache_domain_id(
        &crate::channel_from_supplier(channel_id.clone(), config),
        &agent.endpoint_key(),
    )
    .unwrap_or_default();
    let cache_domain_confidence = if cache_domain_id.is_empty() {
        "unknown"
    } else {
        "exact"
    };
    let surface_bindings = supplier_surface_bindings(config, &channel_id);
    let native_protocols = supplier_native_protocols(config);
    let supported_operations = supplier_supported_operations(config);
    let native_capabilities = supplier_native_capability_evidence(config, &native_protocols);
    let effective_capabilities =
        supplier_effective_capability_evidence(&native_capabilities, &native_protocols);
    let conversion_reachability = supplier_conversion_reachability(&native_protocols);
    let mut models = config.models.clone();
    if config.source_driver == crate::source_driver::SourceDriverId::OpenAiSubscription
        && config.registration_health_status.as_deref() != Some("blocked")
        && !models.iter().any(|model| model == "gpt-live-1-codex") {
        // Voice entitlement is separate from the Responses model listing. This
        // declares an executable model, not a successful/paid entitlement probe.
        models.push("gpt-live-1-codex".to_string());
    }
    models = availability::effective_models(&channel_from_supplier(channel_id.clone(), config), models);
    let mut payload = serde_json::json!({
             "channel_id": channel_id,
             "supplier_unit_id": unit_id,
             "source_driver": config.source_driver,
             "executor": config.executor,
             "name": config.name,
            "public_model": config.public_model,
            "upstream_model": config.upstream_model,
            "models": models,
            "provider_kind": provider_kind,
            "endpoint_kind": endpoint_kind,
            "cache_domain_id": cache_domain_id,
            "cache_domain_confidence": cache_domain_confidence,
            "surface_bindings": surface_bindings,
            "default_target_protocol": config.default_target.protocol.as_str(),
            "duplex_session_kinds": ["openai_responses", "native_realtime", "native_realtime_call"],
            "native_protocols": native_protocols,
            "native_capabilities": native_capabilities,
            "conversion_reachability": conversion_reachability,
            "supported_protocols": native_protocols,
            "protocol_capabilities": supplier_protocol_capabilities(config),
            "price_ratio": config.price_ratio,
            "health_status": health_status,
            "quota_status": quota_status,
            "remaining_ratio": quota.remaining_ratio,
            "daily_used": quota.daily_used,
            "daily_limit": quota.daily_limit,
            "quota_checked_at_unix": quota.checked_at_unix,
            "quota_source": quota.quota_source,
            "quota_window": quota.quota_window,
            "quota_used_percent": quota.quota_used_percent,
            "quota_reset_at_unix": quota.quota_reset_at_unix,
            "quota_windows": quota.quota_windows,
            "safety_state": safety_state,
            "last_error_kind": safety.as_ref().map(|state| state.last_error_kind.clone()).unwrap_or_default(),
            "last_status": safety.as_ref().map(|state| state.last_status).unwrap_or_default(),
            "cooldown_until_unix": safety.as_ref().map(|state| state.cooldown_until_unix).unwrap_or_default(),
            "success_ewma": safety.as_ref().map(|state| state.success_ewma).unwrap_or(1.0),
            "latency_ewma_ms": safety.as_ref().map(|state| state.latency_ewma_ms).unwrap_or_default(),
            "current_concurrency": safety.as_ref().map(|state| state.current_concurrency).unwrap_or_default(),
            "max_concurrency": safety.as_ref().map(|state| state.max_concurrency).unwrap_or(config.max_concurrency()),
            "last_probe_ms": 0
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
    payload.insert(
        "quota_reserve_percent".to_string(),
        serde_json::json!(config.quota_reserve_percent()),
    );
    payload.insert(
        "quota_reserve_blocked".to_string(),
        serde_json::json!(quota_reserve
            .as_ref()
            .is_some_and(|decision| decision.blocked)),
    );
    payload.insert(
        "quota_reserve_window".to_string(),
        serde_json::json!(quota_reserve
            .as_ref()
            .map(|decision| decision.window.clone())
            .unwrap_or_default()),
    );
    payload.insert(
        "quota_reserve_remaining_percent".to_string(),
        serde_json::json!(quota_reserve
            .as_ref()
            .map(|decision| decision.remaining_percent)
            .unwrap_or_default()),
    );
    payload.insert(
        "quota_reserve_reset_at_unix".to_string(),
        serde_json::json!(quota_reserve
            .as_ref()
            .map(|decision| decision.reset_at_unix)
            .unwrap_or_default()),
    );
    payload.insert(
        "effective_capabilities".to_string(),
        serde_json::json!(effective_capabilities),
    );
    payload.insert(
        "supported_operations".to_string(),
        serde_json::json!(supported_operations),
    );
    payload.insert(
        "transport_features".to_string(),
        serde_json::json!([
            SUPPLIER_TRANSPORT_FRAME_COMPRESSION_FEATURE,
            SUPPLIER_TRANSPORT_STREAM_RESUME_FEATURE
        ]),
    );
    if config.registration_removed {
        payload.insert("removed".to_string(), serde_json::Value::Bool(true));
    }
    payload
}

fn supplier_supported_operations(config: &SupplierConfig) -> Vec<String> {
    let execution_kind = crate::channel_executor::execution_kind_for_source(config.source_driver)
        .unwrap_or(crate::source_driver::ExecutionKind::HttpSurface);
    let bound_surfaces = if execution_kind == crate::source_driver::ExecutionKind::HttpSurface {
        let channel_id = if config.channel_id.trim().is_empty() {
            config.node_id.trim()
        } else {
            config.channel_id.trim()
        };
        let channel = crate::channel_from_supplier(channel_id.to_string(), config);
        crate::channel_surface::normalize_channel_surface_bindings(&channel)
            .into_iter()
            .map(|binding| binding.surface)
            .collect::<std::collections::HashSet<_>>()
    } else {
        std::collections::HashSet::new()
    };
    let mut operations = crate::surface::registered_supplier_http_operations()
        .into_iter()
        .filter(|(surface, operation, protocol)| match execution_kind {
            crate::source_driver::ExecutionKind::HttpSurface => bound_surfaces.contains(surface),
            _ => {
                crate::source_driver::admit_subscription_operation(
                    config.source_driver,
                    *surface,
                    *operation,
                    *protocol,
                    crate::source_driver::SubscriptionResponseMode::Buffered,
                )
                .is_ok()
                    || crate::source_driver::admit_subscription_operation(
                        config.source_driver,
                        *surface,
                        *operation,
                        *protocol,
                        crate::source_driver::SubscriptionResponseMode::Sse,
                    )
                    .is_ok()
            }
        })
        .map(|(surface, operation, _)| format!("{}.{}", surface.as_str(), operation.as_str()))
        .collect::<Vec<_>>();
    let responses_websocket = match config.source_driver {
        crate::source_driver::SourceDriverId::OpenAiSubscription => true,
        crate::source_driver::SourceDriverId::OpenAiApi => {
            bound_surfaces.contains(&crate::surface::ApiSurface::OpenAi)
                && crate::detection::responses_features::operation_support(&config.detection_checks, "openai.responses_websocket") != Some(false)
        }
        _ => bound_surfaces.contains(&crate::surface::ApiSurface::OpenAi)
            && crate::detection::responses_features::operation_support(&config.detection_checks, "openai.responses_websocket") == Some(true),
    };
    if responses_websocket {
        operations.push("openai.responses_websocket".to_string());
    }
    if config.source_driver == crate::source_driver::SourceDriverId::OpenAiSubscription {
        operations.push("openai.realtime_live_connect".to_string());
        operations.push("openai.realtime_live_call_create".to_string());
        operations.push("openai.realtime_calls_create".to_string());
    } else if execution_kind == crate::source_driver::ExecutionKind::HttpSurface
        && bound_surfaces.contains(&crate::surface::ApiSurface::OpenAi) {
        operations.push("openai.realtime_websocket".to_string());
        operations.push("openai.realtime_calls_create".to_string());
        operations.push("openai.realtime_live_call_create".to_string());
    }
    if execution_kind == crate::source_driver::ExecutionKind::HttpSurface
        && bound_surfaces.contains(&crate::surface::ApiSurface::Gemini) {
        operations.push("gemini.live_websocket".to_string());
    }
    // Every HTTP Surface model channel can answer cross-protocol token-count
    // preflights with the reviewed local estimator. This explicit declaration
    // is also the rolling-upgrade gate: old clients are never sent an operation
    // they may accidentally turn into a paid generation request.
    let model_channel = execution_kind == crate::source_driver::ExecutionKind::HttpSurface
        && !bound_surfaces.is_empty();
    if model_channel {
        operations.extend([
            "anthropic.count_tokens".to_string(),
            "gemini.count_tokens".to_string(),
            "openai.responses_input_tokens".to_string(),
        ]);
    }
    operations.retain(|operation| operation != "openai.responses_compact"
        || crate::detection::responses_features::operation_support(&config.detection_checks, operation) != Some(false));
    if crate::coding_gateway::is_gateway(config.source_driver) {
        // A coding gateway is not the provider's files/realtime/media API.
        operations.retain(|operation| matches!(operation.as_str(),
            "openai.chat_completions" | "openai.responses" | "anthropic.messages"
            | "gemini.generate_content" | "gemini.stream_generate_content"
            | "anthropic.count_tokens" | "gemini.count_tokens" | "openai.responses_input_tokens")
            || crate::detection::responses_features::operation_support(&config.detection_checks, operation) == Some(true));
    }
    operations.sort_unstable();
    operations.dedup();
    operations
}

fn supplier_surface_bindings(config: &SupplierConfig, channel_id: &str) -> Vec<serde_json::Value> {
    let mut channel = crate::channel_from_supplier(channel_id.to_string(), config);
    channel.surface_bindings = config.surface_bindings.clone();
    crate::channel_surface::normalize_channel_surface_bindings(&channel)
        .into_iter()
        .map(|binding| {
            serde_json::json!({
                "surface": binding.surface,
                "endpoint_profile": binding.endpoint_profile,
                "verification": binding.verification,
                "protocols": binding.protocols,
            })
        })
        .collect()
}

fn supplier_native_protocols(config: &SupplierConfig) -> Vec<String> {
    let channel_id = if config.channel_id.trim().is_empty() {
        config.node_id.trim()
    } else {
        config.channel_id.trim()
    };
    let channel = crate::channel_from_supplier(channel_id.to_string(), config);
    crate::channel_surface::protocols_from_surface_bindings(&channel)
}

fn supplier_native_capability_evidence(
    config: &SupplierConfig,
    native_protocols: &[String],
) -> Vec<SupplierCapabilityEvidence> {
    let mut evidence = Vec::new();
    for protocol in native_protocols {
        let mut profiles = config
            .capability_profiles
            .iter()
            .cloned()
            .filter(|profile| profile.protocol.trim() == protocol)
            .collect::<Vec<_>>();
        if profiles.is_empty() {
            profiles.push(ChannelCapabilityProfile {
                protocol: protocol.clone(),
                capability_layer: "model_wire".to_string(),
                non_stream_json: true,
                stream_sse: true,
                verification_state: "inferred".to_string(),
                verified_at_unix: now_unix(),
                ..Default::default()
            });
        }
        for profile in profiles {
            if profile.catalog_metadata && !profile.model_pattern.is_empty()
                && !profile.model_pattern.contains(['*', '?'])
                && !matches!(profile.release_status.as_str(), "suspended" | "prepared")
                && profile.verification_state != "rejected" {
                for (feature, token_limit) in [("context_tokens", profile.context_tokens), ("output_tokens", profile.output_tokens)] {
                    if let Some(tokens) = token_limit.filter(|tokens| *tokens > 0 && *tokens <= i64::MAX as u64) {
                        evidence.push(SupplierCapabilityEvidence {
                            feature: feature.into(), token_limit: Some(tokens), state: "supported".into(),
                            source: "provider_metadata".into(), capability_layer: "model_wire".into(),
                            model_pattern: Some(profile.model_pattern.clone()), protocol: protocol.clone(),
                            observed_at_unix: profile.verified_at_unix, target_protocol: None,
                            conversion_level: None, release_status: Some("supported".into()),
                        });
                    }
                }
            }
            let capability_layer =
                crate::model::normalized_capability_layer(&profile.capability_layer).to_string();
            let routable_layer =
                crate::model::capability_layer_is_routable(&capability_layer);
            let model_pattern = if profile.model_pattern.trim().is_empty() {
                None
            } else {
                Some(profile.model_pattern.trim().to_string())
            };
            let source = supplier_capability_evidence_source(config, &profile);
            let observed_at_unix = profile.verified_at_unix.max(0);
            let release_status = (!profile.release_status.trim().is_empty())
                .then(|| profile.release_status.trim().to_ascii_lowercase());
            let release_status_value = release_status.as_deref().unwrap_or("supported");
            for (feature, supported, unsupported) in [
                ("non_stream_json", profile.non_stream_json, profile.catalog_protocol_supported == Some(false)),
                ("stream", profile.stream_sse, profile.stream_sse_unsupported || profile.catalog_protocol_supported == Some(false)),
                ("tool_calls", profile.tool_calls, false),
                ("tool_choice", profile.tool_choice, false),
                ("parallel_tool_calls", profile.parallel_tool_calls, false),
                ("json_schema", profile.json_schema, false),
                ("reasoning", profile.reasoning, false),
                ("thinking", profile.thinking, false),
                ("vision", profile.vision, false),
                (
                    "image_input",
                    profile.vision || profile.image_input,
                    profile.input_modalities_authoritative
                        && !(profile.vision || profile.image_input),
                ),
                (
                    "image_output",
                    profile.image_output,
                    profile.output_modalities_authoritative && !profile.image_output,
                ),
                (
                    "audio_input",
                    profile.audio_input,
                    profile.input_modalities_authoritative && !profile.audio_input,
                ),
                (
                    "audio_output",
                    profile.audio_output,
                    profile.output_modalities_authoritative && !profile.audio_output,
                ),
                (
                    "video_input",
                    profile.video_input,
                    profile.input_modalities_authoritative && !profile.video_input,
                ),
                (
                    "video_output",
                    profile.video_output,
                    profile.output_modalities_authoritative && !profile.video_output,
                ),
                (
                    "file_input",
                    profile.file_input,
                    profile.input_modalities_authoritative && !profile.file_input,
                ),
                (
                    "file_output",
                    profile.file_output,
                    profile.output_modalities_authoritative && !profile.file_output,
                ),
                ("cache_control", profile.cache_control, false),
                ("custom_tool", profile.custom_tool, false),
            ] {
                if (model_pattern.is_some() || !routable_layer) && !supported && !unsupported {
                    continue;
                }
                evidence.push(SupplierCapabilityEvidence {
                    feature: feature.to_string(),
                    token_limit: None,
                    state: if release_status_value == "prepared" {
                        "unknown"
                    } else if release_status_value == "suspended" {
                        "unsupported"
                    } else if supported {
                        "supported"
                    } else if unsupported {
                        "unsupported"
                    } else {
                        "unknown"
                    }
                    .to_string(),
                    source: source.clone(),
                    capability_layer: capability_layer.clone(),
                    model_pattern: model_pattern.clone(),
                    protocol: protocol.clone(),
                    observed_at_unix,
                    target_protocol: None,
                    conversion_level: None,
                    release_status: release_status.clone(),
                });
            }
            let inferred_hosted_tools =
                if routable_layer {
                    supplier_native_hosted_tools_for_protocol(
                        config,
                        protocol,
                        &typed_subscription_provider(config),
                    )
                } else {
                    Vec::new()
                };
            for hosted_tool in
                merged_hosted_tools(profile.hosted_tools, inferred_hosted_tools)
            {
                evidence.push(SupplierCapabilityEvidence {
                    feature: format!("hosted_tool:{hosted_tool}"),
                    token_limit: None,
                    state: match release_status_value {
                        "prepared" => "unknown",
                        "suspended" => "unsupported",
                        _ => "supported",
                    }
                    .to_string(),
                    source: source.clone(),
                    capability_layer: capability_layer.clone(),
                    model_pattern: model_pattern.clone(),
                    protocol: protocol.clone(),
                    observed_at_unix,
                    target_protocol: None,
                    conversion_level: None,
                    release_status: release_status.clone(),
                });
            }
        }
    }
    let compaction_models =
        supplier_server_side_compaction_model_patterns(config, native_protocols);
    let compaction_observed_at = now_unix();
    for model_pattern in compaction_models {
        evidence.push(SupplierCapabilityEvidence {
            feature: "server_side_compaction".to_string(),
            token_limit: None,
            state: "supported".to_string(),
            source: "driver_contract".to_string(),
            capability_layer: "model_wire".to_string(),
            model_pattern: Some(model_pattern),
            protocol: "anthropic_messages".to_string(),
            observed_at_unix: compaction_observed_at,
            target_protocol: None,
            conversion_level: None,
            release_status: Some("experimental".to_string()),
        });
    }
    evidence.extend(supplier_surface_operation_evidence(
        config,
        native_protocols,
    ));
    crate::coding_gateway::scope_native_evidence(config.source_driver, &config.models, &config.capability_profiles, &mut evidence);
    evidence
}

fn supplier_surface_operation_evidence(
    config: &SupplierConfig,
    native_protocols: &[String],
) -> Vec<SupplierCapabilityEvidence> {
    config
        .detection_checks
        .iter()
        .filter(|check| check.name == "surface_operation_family")
        .filter(|check| {
            check.protocol.trim().is_empty()
                || native_protocols
                    .iter()
                    .any(|protocol| protocol == check.protocol.trim())
        })
        .filter_map(|check| {
            let capability = check.capability.trim();
            if capability.is_empty() {
                return None;
            }
            let (state, source) = match check.status.trim().to_ascii_lowercase().as_str() {
                "driver_contract" => ("supported", "driver_contract"),
                "verified" => ("supported", "live_probe"),
                "unsupported" => ("unsupported", "live_probe"),
                "credential_limited" => ("unknown", "credential_scope"),
                _ => ("unknown", "inferred"),
            };
            let protocol = check
                .protocol
                .trim()
                .is_empty()
                .then(|| native_protocols.first().cloned())
                .flatten()
                .unwrap_or_else(|| check.protocol.trim().to_string());
            (!protocol.is_empty()).then(|| SupplierCapabilityEvidence {
                token_limit: None,
                feature: format!("surface_family:{capability}"),
                state: state.to_string(),
                source: source.to_string(),
                capability_layer: "driver".to_string(),
                model_pattern: None,
                protocol,
                observed_at_unix: check.checked_at_unix.max(0),
                target_protocol: None,
                conversion_level: None,
                release_status: Some("supported".to_string()),
            })
        })
        .collect()
}

fn supplier_capability_evidence_source(
    config: &SupplierConfig,
    profile: &ChannelCapabilityProfile,
) -> String {
    match profile.verification_state.trim() {
        "driver_contract" => "driver_contract",
        "verified" => "live_probe",
        "declared" if source_driver_owns_provider_metadata(config.source_driver) => {
            "provider_metadata"
        }
        "declared" => "user_declaration",
        _ => "inferred",
    }
    .to_string()
}

fn supplier_conversion_reachability(
    native_protocols: &[String],
) -> Vec<SupplierConversionReachability> {
    const INBOUND_PROTOCOLS: [&str; 4] = [
        "openai_responses",
        "openai_chat",
        "anthropic_messages",
        "gemini_native",
    ];
    let mut reachability = Vec::new();
    for source in INBOUND_PROTOCOLS {
        for target in native_protocols {
            if crate::protocol::kind::ProtocolKind::parse(target).is_err() {
                continue;
            }
            reachability.push(SupplierConversionReachability {
                source_protocol: source.to_string(),
                target_protocol: target.clone(),
                state: if source == target {
                    "native"
                } else {
                    "convertible"
                }
                .to_string(),
            });
        }
    }
    reachability
}

fn supplier_effective_capability_evidence(
    native: &[SupplierCapabilityEvidence],
    native_protocols: &[String],
) -> Vec<SupplierCapabilityEvidence> {
    use crate::protocol::kind::ProtocolKind;

    const INBOUND_PROTOCOLS: [&str; 4] = [
        "openai_responses",
        "openai_chat",
        "anthropic_messages",
        "gemini_native",
    ];
    let native_protocols = native_protocols
        .iter()
        .filter_map(|protocol| ProtocolKind::parse(protocol).ok())
        .collect::<std::collections::HashSet<_>>();
    let mut effective = Vec::new();
    for item in native {
        if !crate::model::capability_layer_is_routable(&item.capability_layer) {
            continue;
        }
        if item.state == "unknown" {
            continue;
        }
        if matches!(
            item.release_status.as_deref(),
            Some("prepared") | Some("suspended")
        ) {
            continue;
        }
        let Ok(target) = ProtocolKind::parse(&item.protocol) else {
            continue;
        };
        if !native_protocols.contains(&target) {
            continue;
        }
        for source_name in INBOUND_PROTOCOLS {
            let Ok(source) = ProtocolKind::parse(source_name) else {
                continue;
            };
            if source != target
                && !effective_feature_is_representable_from(source, &item.feature)
            {
                continue;
            }
            let mut projected = item.clone();
            projected.protocol = source_name.to_string();
            projected.target_protocol = Some(item.protocol.clone());
            projected.conversion_level = Some(
                if source == target {
                    "native"
                } else {
                    "lossless"
                }
                .to_string(),
            );
            effective.push(projected);
        }
    }
    effective
}

fn effective_feature_is_representable_from(
    protocol: crate::protocol::kind::ProtocolKind,
    feature: &str,
) -> bool {
    use crate::protocol::capability::{protocol_wire_supports, Feature};

    let feature = match feature {
        "non_stream_json" => return true,
        "stream" => Feature::Stream,
        "tool_calls" => Feature::Tools,
        "tool_choice" => Feature::ToolChoiceNamed,
        "parallel_tool_calls" => Feature::ParallelTools,
        "json_schema" => Feature::JsonSchema,
        "reasoning" | "thinking" => Feature::Reasoning,
        "vision" | "image_input" => Feature::ImageInput,
        "audio_input" => Feature::AudioInput,
        "audio_output" => Feature::AudioOutput,
        "video_input" => Feature::VideoInput,
        "file_input" => Feature::FileInput,
        "cache_control" => Feature::CacheControl,
        "custom_tool" => Feature::CustomTools,
        feature if feature.starts_with("hosted_tool:") => match &feature["hosted_tool:".len()..] {
            "web_search" => Feature::HostedWebSearch,
            "file_search" => Feature::HostedFileSearch,
            "code_interpreter" | "code_execution" => Feature::HostedCodeExecution,
            "computer_use" => Feature::HostedComputerUse,
            "mcp" => Feature::HostedMcp,
            "url_context" => Feature::HostedUrlContext,
            _ => Feature::HostedProviderTool,
        },
        // Output-only media and resource surfaces do not yet have a protocol-neutral request
        // mapping. Keep them native until an operation adapter proves equivalence.
        _ => return false,
    };
    protocol_wire_supports(protocol, feature)
}

fn supplier_server_side_compaction_model_patterns(
    config: &SupplierConfig,
    native_protocols: &[String],
) -> Vec<String> {
    if config.source_driver != crate::source_driver::SourceDriverId::ClaudeSubscription
        || !native_protocols
            .iter()
            .any(|protocol| protocol.trim() == "anthropic_messages")
    {
        return Vec::new();
    }

    // Older configs can carry a stale discovered model list while the public or
    // default upstream model remains routable. Consider all three sources so an
    // existing alias is not accidentally excluded from the model-scoped contract.
    let candidates = config
        .models
        .iter()
        .map(String::as_str)
        .chain([config.public_model.as_str(), config.upstream_model.as_str()]);
    let mut seen = std::collections::HashSet::new();
    candidates
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .filter(|model| {
            let upstream = crate::supplier::supplier_upstream_model_for_request(
                config,
                Some(model),
            );
            crate::protocol::anthropic_dialect::anthropic_model_supports_server_side_compaction(
                &upstream,
            )
        })
        .filter(|model| seen.insert(model.to_ascii_lowercase()))
        .map(str::to_string)
        .collect()
}

fn supplier_provider_kind(config: &SupplierConfig) -> String {
    match config.source_driver {
        crate::source_driver::SourceDriverId::OpenAiApi
        | crate::source_driver::SourceDriverId::OpenAiSubscription => "openai",
        crate::source_driver::SourceDriverId::AnthropicApi
        | crate::source_driver::SourceDriverId::ClaudeSubscription => "anthropic",
        crate::source_driver::SourceDriverId::GeminiApi
        | crate::source_driver::SourceDriverId::GeminiSubscription => "google",
        crate::source_driver::SourceDriverId::XaiApi
        | crate::source_driver::SourceDriverId::GrokSubscription => "xai",
        crate::source_driver::SourceDriverId::MistralApi => "mistral",
        crate::source_driver::SourceDriverId::DeepseekApi => "deepseek",
        crate::source_driver::SourceDriverId::DashscopeApi => "dashscope",
        crate::source_driver::SourceDriverId::MoonshotApi => "moonshot",
        crate::source_driver::SourceDriverId::ZhipuApi => "zhipu",
        crate::source_driver::SourceDriverId::MinimaxApi => "minimax",
        crate::source_driver::SourceDriverId::StepfunApi => "stepfun",
        crate::source_driver::SourceDriverId::AzureOpenAi => "azure_openai",
        crate::source_driver::SourceDriverId::BedrockMantle => "aws_bedrock",
        crate::source_driver::SourceDriverId::GroqApi => "groq",
        crate::source_driver::SourceDriverId::TogetherApi => "together",
        crate::source_driver::SourceDriverId::FireworksApi => "fireworks",
        crate::source_driver::SourceDriverId::PerplexityApi => "perplexity",
        crate::source_driver::SourceDriverId::HuggingfaceApi => "huggingface",
        crate::source_driver::SourceDriverId::NvidiaApi => "nvidia",
        crate::source_driver::SourceDriverId::SiliconflowApi => "siliconflow",
        crate::source_driver::SourceDriverId::VolcengineArkApi => "volcengine_ark",
        crate::source_driver::SourceDriverId::BaiduQianfanApi => "baidu_qianfan",
        crate::source_driver::SourceDriverId::TencentHunyuanApi => "tencent_hunyuan",
        crate::source_driver::SourceDriverId::Openrouter => "openrouter",
        crate::source_driver::SourceDriverId::OpencodeGo
        | crate::source_driver::SourceDriverId::OpencodeZen => "opencode",
        crate::source_driver::SourceDriverId::KiloGateway => "kilo",
        crate::source_driver::SourceDriverId::ClineApi => "cline",
        crate::source_driver::SourceDriverId::CommandCode => "command_code",
        crate::source_driver::SourceDriverId::KimiCode => "moonshot",
        crate::source_driver::SourceDriverId::GlmCodingPlan => "zhipu",
        crate::source_driver::SourceDriverId::MinimaxTokenPlan => "minimax",
        crate::source_driver::SourceDriverId::OllamaCloud => "ollama",
        crate::source_driver::SourceDriverId::Ollama
        | crate::source_driver::SourceDriverId::LmStudio
        | crate::source_driver::SourceDriverId::Vllm => "local",
        crate::source_driver::SourceDriverId::LanShare
        | crate::source_driver::SourceDriverId::CustomEndpoint
        | crate::source_driver::SourceDriverId::Omniroute => "custom",
    }
    .to_string()
}

fn supplier_endpoint_kind(config: &SupplierConfig) -> String {
    match config.source_driver {
        crate::source_driver::SourceDriverId::OpenAiSubscription
        | crate::source_driver::SourceDriverId::ClaudeSubscription
        | crate::source_driver::SourceDriverId::GeminiSubscription
        | crate::source_driver::SourceDriverId::GrokSubscription => "subscription",
        crate::source_driver::SourceDriverId::Ollama
        | crate::source_driver::SourceDriverId::LmStudio
        | crate::source_driver::SourceDriverId::Vllm => "local",
        crate::source_driver::SourceDriverId::LanShare
        | crate::source_driver::SourceDriverId::CustomEndpoint
        | crate::source_driver::SourceDriverId::Openrouter
        | crate::source_driver::SourceDriverId::OpencodeGo
        | crate::source_driver::SourceDriverId::OpencodeZen
        | crate::source_driver::SourceDriverId::KiloGateway
        | crate::source_driver::SourceDriverId::ClineApi
        | crate::source_driver::SourceDriverId::CommandCode
        | crate::source_driver::SourceDriverId::KimiCode
        | crate::source_driver::SourceDriverId::GlmCodingPlan
        | crate::source_driver::SourceDriverId::MinimaxTokenPlan
        | crate::source_driver::SourceDriverId::OllamaCloud
        | crate::source_driver::SourceDriverId::Omniroute => "compatible_api",
        crate::source_driver::SourceDriverId::AzureOpenAi
        | crate::source_driver::SourceDriverId::BedrockMantle
        | crate::source_driver::SourceDriverId::GroqApi
        | crate::source_driver::SourceDriverId::TogetherApi
        | crate::source_driver::SourceDriverId::FireworksApi
        | crate::source_driver::SourceDriverId::PerplexityApi
        | crate::source_driver::SourceDriverId::HuggingfaceApi
        | crate::source_driver::SourceDriverId::NvidiaApi
        | crate::source_driver::SourceDriverId::SiliconflowApi
        | crate::source_driver::SourceDriverId::VolcengineArkApi
        | crate::source_driver::SourceDriverId::BaiduQianfanApi
        | crate::source_driver::SourceDriverId::TencentHunyuanApi => "cloud_api",
        crate::source_driver::SourceDriverId::OpenAiApi
        | crate::source_driver::SourceDriverId::AnthropicApi
        | crate::source_driver::SourceDriverId::GeminiApi
        | crate::source_driver::SourceDriverId::XaiApi
        | crate::source_driver::SourceDriverId::MistralApi
        | crate::source_driver::SourceDriverId::DeepseekApi
        | crate::source_driver::SourceDriverId::DashscopeApi
        | crate::source_driver::SourceDriverId::MoonshotApi
        | crate::source_driver::SourceDriverId::ZhipuApi
        | crate::source_driver::SourceDriverId::MinimaxApi
        | crate::source_driver::SourceDriverId::StepfunApi => "official_api",
    }
    .to_string()
}

fn supplier_protocol_capabilities(config: &SupplierConfig) -> Vec<serde_json::Value> {
    if !config.capability_profiles.is_empty() {
        return supplier_protocol_capabilities_from_profiles(config);
    }
    let protocols = supplier_native_protocols(config);
    let subscription_provider = typed_subscription_provider(config);
    protocols
        .into_iter()
        .map(|protocol| {
            let mut capability = serde_json::json!({
                "protocol": protocol,
                "non_stream_json": true
            });
            let hosted_tools = supplier_native_hosted_tools_for_protocol(
                config,
                &protocol,
                &subscription_provider,
            );
            if !hosted_tools.is_empty() {
                capability["hosted_tools"] = serde_json::json!(hosted_tools);
            }
            if supplier_native_custom_tool_for_protocol(config, &protocol, &subscription_provider) {
                capability["custom_tool"] = serde_json::json!(true);
            }
            capability
        })
        .collect()
}

fn supplier_protocol_capabilities_from_profiles(config: &SupplierConfig) -> Vec<serde_json::Value> {
    let subscription_provider = typed_subscription_provider(config);
    let native_protocols = supplier_native_protocols(config);
    config
        .capability_profiles
        .iter()
        .filter(|profile| {
            (profile.verification_state == "verified" || profile.verification_state == "declared")
                && native_protocols.contains(&profile.protocol)
                && profile.model_pattern.trim().is_empty()
                && crate::model::capability_layer_is_routable(&profile.capability_layer)
                && !matches!(
                    profile.release_status.trim().to_ascii_lowercase().as_str(),
                    "prepared" | "suspended"
                )
        })
        .map(|profile| {
            let mut capability = serde_json::json!({
                "protocol": profile.protocol,
                "non_stream_json": profile.non_stream_json,
                "stream_sse": profile.stream_sse
            });
            if profile.tool_calls {
                capability["tool_calls"] = serde_json::json!(true);
            }
            if profile.tool_choice {
                capability["tool_choice"] = serde_json::json!(true);
            }
            if profile.parallel_tool_calls {
                capability["parallel_tool_calls"] = serde_json::json!(true);
            }
            if profile.json_schema {
                capability["json_schema"] = serde_json::json!(true);
            }
            if profile.reasoning {
                capability["reasoning"] = serde_json::json!(true);
            }
            if profile.thinking {
                capability["thinking"] = serde_json::json!(true);
            }
            if profile.vision {
                capability["vision"] = serde_json::json!(true);
            }
            if profile.vision || profile.image_input {
                capability["image_input"] = serde_json::json!(true);
            }
            for (field, supported) in [
                ("image_output", profile.image_output),
                ("audio_input", profile.audio_input),
                ("audio_output", profile.audio_output),
                ("video_input", profile.video_input),
                ("video_output", profile.video_output),
                ("file_input", profile.file_input),
                ("file_output", profile.file_output),
            ] {
                if supported {
                    capability[field] = serde_json::json!(true);
                }
            }
            if profile.cache_control {
                capability["cache_control"] = serde_json::json!(true);
            }
            let hosted_tools = merged_hosted_tools(
                profile.hosted_tools.clone(),
                supplier_native_hosted_tools_for_protocol(
                    config,
                    &profile.protocol,
                    &subscription_provider,
                ),
            );
            if !hosted_tools.is_empty() {
                capability["hosted_tools"] = serde_json::json!(hosted_tools);
            }
            if profile.custom_tool
                || supplier_native_custom_tool_for_protocol(
                    config,
                    &profile.protocol,
                    &subscription_provider,
                )
            {
                capability["custom_tool"] = serde_json::json!(true);
            }
            capability
        })
        .collect()
}

fn merged_hosted_tools(first: Vec<String>, second: Vec<String>) -> Vec<String> {
    let mut merged = Vec::new();
    for tool in first.into_iter().chain(second) {
        let tool = tool.trim();
        if tool.is_empty() || merged.iter().any(|existing| existing == tool) {
            continue;
        }
        merged.push(tool.to_string());
    }
    merged
}

fn supplier_native_hosted_tools_for_protocol(
    config: &SupplierConfig,
    protocol: &str,
    subscription_provider: &str,
) -> Vec<String> {
    if protocol == "openai_responses" && matches!(subscription_provider, "openai" | "codex") {
        return [
            "code_interpreter",
            "file_search",
            "image_generation",
            "mcp",
            "web_search",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
    }
    if !subscription_provider.is_empty() {
        return Vec::new();
    }
    match protocol {
        "openai_responses" if is_native_openai_channel(config) => [
            "code_interpreter",
            "file_search",
            "image_generation",
            "mcp",
            "web_search",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        "gemini_native" if is_native_gemini_channel(config) => {
            ["code_execution", "file_search", "url_context", "web_search"]
                .into_iter()
                .map(str::to_string)
                .collect()
        }
        _ => Vec::new(),
    }
}

fn supplier_native_custom_tool_for_protocol(
    config: &SupplierConfig,
    protocol: &str,
    subscription_provider: &str,
) -> bool {
    if protocol != "openai_responses" {
        return false;
    }
    if !subscription_provider.is_empty() {
        return matches!(subscription_provider, "openai" | "codex");
    }
    subscription_provider.is_empty() && is_native_openai_channel(config)
}

fn is_native_openai_channel(config: &SupplierConfig) -> bool {
    config.source_driver == crate::source_driver::SourceDriverId::OpenAiApi
}

fn is_native_gemini_channel(config: &SupplierConfig) -> bool {
    config.source_driver == crate::source_driver::SourceDriverId::GeminiApi
}

fn typed_subscription_provider(config: &SupplierConfig) -> String {
    crate::channel_executor::execution_kind_for_source(config.source_driver)
        .ok()
        .and_then(crate::channel_executor::subscription_provider_for_execution_kind)
        .unwrap_or_default()
        .to_string()
}

fn source_driver_owns_provider_metadata(
    source_driver: crate::source_driver::SourceDriverId,
) -> bool {
    matches!(
        source_driver,
        crate::source_driver::SourceDriverId::OpenAiApi
            | crate::source_driver::SourceDriverId::AnthropicApi
            | crate::source_driver::SourceDriverId::GeminiApi
            | crate::source_driver::SourceDriverId::XaiApi
            | crate::source_driver::SourceDriverId::MistralApi
            | crate::source_driver::SourceDriverId::DeepseekApi
            | crate::source_driver::SourceDriverId::DashscopeApi
            | crate::source_driver::SourceDriverId::MoonshotApi
            | crate::source_driver::SourceDriverId::ZhipuApi
            | crate::source_driver::SourceDriverId::MinimaxApi
            | crate::source_driver::SourceDriverId::StepfunApi
            | crate::source_driver::SourceDriverId::AzureOpenAi
            | crate::source_driver::SourceDriverId::BedrockMantle
            | crate::source_driver::SourceDriverId::GroqApi
            | crate::source_driver::SourceDriverId::TogetherApi
            | crate::source_driver::SourceDriverId::FireworksApi
            | crate::source_driver::SourceDriverId::PerplexityApi
            | crate::source_driver::SourceDriverId::HuggingfaceApi
            | crate::source_driver::SourceDriverId::NvidiaApi
            | crate::source_driver::SourceDriverId::SiliconflowApi
            | crate::source_driver::SourceDriverId::VolcengineArkApi
            | crate::source_driver::SourceDriverId::BaiduQianfanApi
            | crate::source_driver::SourceDriverId::TencentHunyuanApi
            | crate::source_driver::SourceDriverId::OpencodeGo
            | crate::source_driver::SourceDriverId::OpencodeZen
            | crate::source_driver::SourceDriverId::KiloGateway
            | crate::source_driver::SourceDriverId::ClineApi
            | crate::source_driver::SourceDriverId::OpenAiSubscription
            | crate::source_driver::SourceDriverId::ClaudeSubscription
            | crate::source_driver::SourceDriverId::GeminiSubscription
            | crate::source_driver::SourceDriverId::GrokSubscription
    )
}

#[cfg(test)]
mod surface_operation_evidence_tests {
    use super::*;

    #[test]
    fn coding_gateways_register_model_scoped_dialects_and_conversion_evidence() {
        use crate::source_driver::SourceDriverId;
        for driver in [SourceDriverId::OpencodeGo, SourceDriverId::OpencodeZen] {
            let mut channel = crate::coding_gateway::tests::channel(driver, "https://opencode.ai/zen/v1");
            channel.models = vec!["gpt-5.6-luna".into(), "qwen3.8-max".into(), "kimi-k3".into()];
            if driver == SourceDriverId::OpencodeZen {
                channel.models.push("gemini-3.8-flash".into());
            }
            let config = crate::supplier_from_channel(&channel);
            let operations = supplier_supported_operations(&config);
            for operation in ["gemini.generate_content", "gemini.stream_generate_content"] {
                assert_eq!(operations.iter().any(|item| item == operation), driver == SourceDriverId::OpencodeZen);
            }
            assert!(!operations.iter().any(|item| item == "gemini.live_websocket" || item == "gemini.files_create"));
            let protocols = supplier_native_protocols(&config);
            let evidence = supplier_native_capability_evidence(&config, &protocols);
            let effective = supplier_effective_capability_evidence(&evidence, &protocols);
            for model in &channel.models {
                let native = crate::coding_gateway::model_protocol(driver, model).unwrap();
                for protocol in &protocols {
                    let mode = evidence.iter().find(|item| {
                        item.model_pattern.as_ref() == Some(model)
                            && item.protocol == *protocol && item.feature == "non_stream_json"
                    }).unwrap();
                    assert_eq!(mode.state, if protocol == native.as_str() { "supported" } else { "unsupported" });
                }
                assert!(effective.iter().any(|item| {
                    item.model_pattern.as_ref() == Some(model) && item.protocol == "openai_responses"
                        && item.target_protocol.as_deref() == Some(native.as_str())
                        && item.feature == "non_stream_json" && item.state == "supported"
                }));
            }
            assert!(!evidence.iter().any(|item| item.model_pattern.is_none()));
        }
    }

    #[test]
    fn numeric_catalog_limits_are_exact_scoped_and_survive_registration_json() {
        let mut channel = crate::default_config().channels.remove(0);
        channel.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_responses".into(), model_pattern:"Vendor/model".into(),
            catalog_metadata:true, context_tokens:Some(1000000), output_tokens:Some(64000),
            verification_state:"declared".into(), verified_at_unix:42, ..Default::default()
        }];
        let evidence = supplier_native_capability_evidence(&crate::supplier_from_channel(&channel), &["openai_responses".into()]);
        let limit = evidence.iter().find(|e| e.feature == "context_tokens").unwrap();
        let json = serde_json::to_value(limit).unwrap();
        assert_eq!(json["token_limit"], 1000000);
        assert_eq!(json["model_pattern"], "Vendor/model");
        assert_eq!(json["source"], "provider_metadata");
        channel.capability_profiles[0].release_status = "suspended".into();
        assert!(!supplier_native_capability_evidence(&crate::supplier_from_channel(&channel), &["openai_responses".into()]).iter().any(|e| e.token_limit.is_some()));
    }

    #[test]
    fn responses_extension_probes_limit_operations_and_scope_custom_tools() {
        let mut channel = crate::channel_from_supplier("router".into(), &crate::default_supplier_config());
        channel.set_source_driver(crate::source_driver::SourceDriverId::Openrouter);
        channel.surface_bindings = crate::source_driver_surface_bindings(channel.source_driver(), "https://openrouter.ai/api/v1");
        channel.models = vec!["gpt-5.6-luna".into(), "glm-5.3".into()];
        channel.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_responses".into(), model_pattern:"gpt-5.6-luna".into(), custom_tool:true,
            verification_state:"verified".into(), ..Default::default()
        }];
        channel.detection_checks = vec![
            ChannelDetectionCheck { name:"responses_operation".into(), capability:"openai.responses_compact".into(), status:"unsupported".into(), ..Default::default() },
            ChannelDetectionCheck { name:"responses_operation".into(), capability:"openai.responses_websocket".into(), status:"verified".into(), ..Default::default() },
        ];
        let config = crate::supplier_from_channel(&channel);
        let operations = supplier_supported_operations(&config);
        assert!(!operations.contains(&"openai.responses_compact".into()));
        assert!(operations.contains(&"openai.responses_input_tokens".into()));
        assert!(operations.contains(&"openai.responses_websocket".into()));
        let evidence = supplier_native_capability_evidence(&config, &["openai_responses".into()]);
        let custom: Vec<_> = evidence.iter().filter(|e| e.feature == "custom_tool" && e.state == "supported").collect();
        assert_eq!(custom.len(), 1);
        assert_eq!(custom[0].model_pattern.as_deref(), Some("gpt-5.6-luna"));
    }

    #[test]
    fn surface_family_checks_are_advisory_capability_evidence() {
        let mut config = crate::default_supplier_config();
        config.detection_checks = vec![
            ChannelDetectionCheck {
                name: "surface_operation_family".to_string(),
                status: "verified".to_string(),
                checked_at_unix: 42,
                protocol: "openai_responses".to_string(),
                capability: "openai.files_uploads".to_string(),
                message: "HTTP 200".to_string(),
            },
            ChannelDetectionCheck {
                name: "surface_operation_family".to_string(),
                status: "unsupported".to_string(),
                checked_at_unix: 43,
                protocol: "openai_responses".to_string(),
                capability: "openai.vector_stores".to_string(),
                message: "HTTP 404".to_string(),
            },
        ];
        let evidence = supplier_surface_operation_evidence(
            &config,
            &["openai_responses".to_string()],
        );
        assert_eq!(evidence.len(), 2);
        assert!(evidence.iter().any(|item| {
            item.feature == "surface_family:openai.files_uploads"
                && item.state == "supported"
                && item.source == "live_probe"
        }));
        assert!(evidence.iter().any(|item| {
            item.feature == "surface_family:openai.vector_stores"
                && item.state == "unsupported"
        }));
    }
}
