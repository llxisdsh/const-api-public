use super::*;
use futures_util::StreamExt;

#[derive(Debug, Clone, Copy)]
pub(super) struct SurfaceOperationFamilyProbe {
    pub(super) capability: &'static str,
    pub(super) path: &'static str,
}

const OPENAI_OPERATION_FAMILY_PROBES: &[SurfaceOperationFamilyProbe] = &[
    SurfaceOperationFamilyProbe {
        capability: "openai.chat_completions",
        path: "/v1/chat/completions?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "openai.files_uploads",
        path: "/v1/files?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "openai.batches",
        path: "/v1/batches?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "openai.videos",
        path: "/v1/videos?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "openai.vector_stores",
        path: "/v1/vector_stores?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "openai.containers",
        path: "/v1/containers?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "openai.skills",
        path: "/v1/skills?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "openai.assistants_threads",
        path: "/v1/assistants?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "openai.fine_tuning_evals_chatkit",
        path: "/v1/fine_tuning/jobs?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "openai.fine_tuning_evals_chatkit",
        path: "/v1/evals?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "openai.fine_tuning_evals_chatkit",
        path: "/v1/chatkit/threads?limit=1",
    },
];

const ANTHROPIC_OPERATION_FAMILY_PROBES: &[SurfaceOperationFamilyProbe] = &[
    SurfaceOperationFamilyProbe {
        capability: "anthropic.files",
        path: "/v1/files?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.message_batches",
        path: "/v1/messages/batches?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/skills?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/agents?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/sessions?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/environments?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/deployments?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/deployment_runs?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/vaults?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/memory_stores?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/dreams?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/tunnels?limit=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "anthropic.skills_agents",
        path: "/v1/user_profiles?limit=1",
    },
];

const GEMINI_OPERATION_FAMILY_PROBES: &[SurfaceOperationFamilyProbe] = &[
    SurfaceOperationFamilyProbe {
        capability: "gemini.batches",
        path: "/v1beta/batches?pageSize=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "gemini.file_search",
        path: "/v1beta/fileSearchStores?pageSize=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "gemini.generated_media",
        path: "/v1beta/generatedFiles?pageSize=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "gemini.generated_media",
        path: "/v1beta/environments?pageSize=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "gemini.tuning_corpora_permissions",
        path: "/v1beta/tunedModels?pageSize=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "gemini.tuning_corpora_permissions",
        path: "/v1beta/corpora?pageSize=1",
    },
    SurfaceOperationFamilyProbe {
        capability: "gemini.operations",
        path: "/v1beta/operations?pageSize=1",
    },
];

pub(super) fn operation_family_probes(
    surface: crate::surface::ApiSurface,
) -> &'static [SurfaceOperationFamilyProbe] {
    match surface {
        crate::surface::ApiSurface::OpenAi => OPENAI_OPERATION_FAMILY_PROBES,
        crate::surface::ApiSurface::Anthropic => ANTHROPIC_OPERATION_FAMILY_PROBES,
        crate::surface::ApiSurface::Gemini => GEMINI_OPERATION_FAMILY_PROBES,
    }
}

pub(super) fn source_has_authoritative_surface_contract(
    source: crate::source_driver::SourceDriverId,
    surface: crate::surface::ApiSurface,
) -> bool {
    use crate::source_driver::SourceDriverId;
    matches!(
        (source, surface),
        (
            SourceDriverId::OpenAiApi,
            crate::surface::ApiSurface::OpenAi
        ) | (
            SourceDriverId::AnthropicApi,
            crate::surface::ApiSurface::Anthropic
        ) | (
            SourceDriverId::GeminiApi,
            crate::surface::ApiSurface::Gemini
        )
    )
}

pub(super) fn should_probe_custom_surface_families(
    source: crate::source_driver::SourceDriverId,
) -> bool {
    use crate::source_driver::SourceDriverId;
    matches!(
        source,
        SourceDriverId::CustomEndpoint | SourceDriverId::Omniroute | SourceDriverId::Openrouter
    )
}

pub(super) async fn surface_operation_family_evidence(
    client: &Client,
    context: &SurfaceProbeContext,
) -> Vec<ChannelDetectionCheck> {
    let protocol = context
        .binding
        .protocols
        .first()
        .map(|binding| binding.protocol.clone())
        .unwrap_or_default();
    if source_has_authoritative_surface_contract(context.source_driver, context.binding.surface) {
        return crate::surface::official_model_api_family_ids(context.binding.surface)
            .into_iter()
            .map(|capability| ChannelDetectionCheck {
                name: "surface_operation_family".to_string(),
                status: "driver_contract".to_string(),
                checked_at_unix: now_unix(),
                protocol: protocol.clone(),
                capability,
                message: "official API driver contract".to_string(),
            })
            .collect();
    }
    if context.execution_kind != crate::source_driver::ExecutionKind::HttpSurface
        || !should_probe_custom_surface_families(context.source_driver)
    {
        return Vec::new();
    }
    let Ok(channel) = probe_channel_from_context(context) else {
        return Vec::new();
    };
    let Some(target) = crate::channel_surface::preferred_channel_surface_target(&channel) else {
        return Vec::new();
    };
    let observations = futures_util::stream::iter(
        operation_family_probes(context.binding.surface)
            .iter()
            .copied(),
    )
    .map(|probe| {
        let client = client.clone();
        let channel = channel.clone();
        let target = target.clone();
        async move {
            (
                probe.capability,
                probe_surface_operation_family(&client, &channel, &target, probe.path).await,
            )
        }
    })
    .buffered(4)
    .collect::<Vec<_>>()
    .await;
    let mut grouped = std::collections::BTreeMap::<&str, Vec<(&str, String)>>::new();
    for (capability, observation) in observations {
        grouped.entry(capability).or_default().push(observation);
    }
    grouped
        .into_iter()
        .map(|(capability, observations)| {
            let count = |state: &str| {
                observations
                    .iter()
                    .filter(|(observed, _)| *observed == state)
                    .count()
            };
            let verified = count("verified");
            let unsupported = count("unsupported");
            let credential_limited = count("credential_limited");
            let status = if verified == observations.len() {
                "verified"
            } else if unsupported == observations.len() {
                "unsupported"
            } else if credential_limited == observations.len() {
                "credential_limited"
            } else {
                "unknown"
            };
            ChannelDetectionCheck {
                name: "surface_operation_family".to_string(),
                status: status.to_string(),
                checked_at_unix: now_unix(),
                protocol: protocol.clone(),
                capability: capability.to_string(),
                message: format!(
                    "{} read-only probes: verified={verified}, unsupported={unsupported}, credential_limited={credential_limited}, unknown={}",
                    observations.len(),
                    observations.len().saturating_sub(verified + unsupported + credential_limited),
                ),
            }
        })
        .collect()
}

async fn probe_surface_operation_family(
    client: &Client,
    channel: &ChannelConfig,
    target: &crate::channel_surface::ChannelSurfaceTarget,
    path: &str,
) -> (&'static str, String) {
    let url = if target.surface == crate::surface::ApiSurface::Gemini {
        match join_gemini_native_url(&target.base_url, path) {
            Ok(url) => url,
            Err(error) => return ("unknown", format!("probe URL unavailable: {error}")),
        }
    } else {
        join_upstream_url(&target.base_url, path)
    };
    let mut headers = HeaderMap::new();
    crate::channel_user_agent::apply_to_headers(channel, &mut headers);
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    if target.surface == crate::surface::ApiSurface::Anthropic {
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    }
    if let Err(error) =
        crate::channel_executor::apply_upstream_auth(&mut headers, channel, target, true)
    {
        return (
            "unknown",
            format!("probe authentication unavailable: {error}"),
        );
    }
    let response = match client.get(url).headers(headers).send_adaptive().await {
        Ok(response) => response,
        Err(error) => return ("unknown", format!("probe transport unavailable: {error}")),
    };
    let code = response.status().as_u16();
    let state = classify_surface_operation_probe_status(code);
    (
        state,
        format!("read-only family probe returned HTTP {code}"),
    )
}

pub(super) fn classify_surface_operation_probe_status(code: u16) -> &'static str {
    match code {
        200..=299 => "verified",
        404 | 501 => "unsupported",
        401 | 403 => "credential_limited",
        _ => "unknown",
    }
}
