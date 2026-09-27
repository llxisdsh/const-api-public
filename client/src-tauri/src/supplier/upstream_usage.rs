const SUPPLIER_UPSTREAM_USAGE_FIELD: &str = "upstream_usage";
const MAX_UPSTREAM_USAGE_LINE_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct SupplierUsageMeter {
    meter_kind: String,
    unit: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    qualifier: String,
    quantity: u64,
    #[serde(default = "one")]
    quantity_scale: u64,
    included_in_total: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AnthropicUsageTotals {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    thinking_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    cache_write_5m_tokens: Option<u64>,
    cache_write_1h_tokens: Option<u64>,
    search_queries: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AnthropicUsageState {
    top_level: AnthropicUsageTotals,
    compaction: AnthropicUsageTotals,
    iterations: AnthropicUsageTotals,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct SupplierUpstreamUsage {
    source_protocol: String,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    cache_write_5m_tokens: Option<u64>,
    cache_write_1h_tokens: Option<u64>,
    input_tokens_include_cache_read: bool,
    input_tokens_include_cache_write: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    usage_meters: Vec<SupplierUsageMeter>,
    // Anthropic SSE reports compaction iterations before later cumulative top-level
    // usage deltas. Keep both components privately so a later message_delta cannot
    // overwrite already observed, separately billed compaction work.
    #[serde(skip)]
    anthropic_usage: AnthropicUsageState,
    #[serde(skip)]
    terminal_error: Option<serde_json::Value>,
}

impl SupplierUpstreamUsage {
    fn from_json(protocol: &str, value: &serde_json::Value) -> Self {
        let mut usage = Self {
            source_protocol: protocol.trim().to_string(),
            input_tokens_include_cache_read: protocol != "anthropic_messages",
            input_tokens_include_cache_write: protocol != "anthropic_messages",
            ..Default::default()
        };
        usage.absorb_json(value);
        usage
    }

    fn absorb_json(&mut self, value: &serde_json::Value) {
        if self.terminal_error.is_none() && crate::upstream_failure::error_object(value).is_some()
            && serde_json::to_vec(value).is_ok_and(|raw| raw.len() <= crate::upstream_failure::MAX_OBSERVATION_BYTES) {
            self.terminal_error = Some(value.clone());
        }
        let protocol = self.source_protocol.clone();
        let usage_value = match protocol.as_str() {
            "openai_chat" => value
                .get("usage")
                .or_else(|| value.pointer("/response/usage"))
                .unwrap_or(value),
            "openai_responses" => value
                .pointer("/response/usage")
                .or_else(|| value.get("usage"))
                .unwrap_or(value),
            "anthropic_messages" => value
                .pointer("/message/usage")
                .or_else(|| value.get("usage"))
                .unwrap_or(value),
            "gemini_native" => value
                .pointer("/response/usageMetadata")
                .or_else(|| value.get("usageMetadata"))
                .or_else(|| value.pointer("/interaction/usage"))
                .or_else(|| value.get("usage"))
                .unwrap_or(value),
            _ => value,
        };
        match protocol.as_str() {
            "openai_chat" => {
                replace(&mut self.input_tokens, usage_value.get("prompt_tokens"));
                replace(
                    &mut self.output_tokens,
                    usage_value.get("completion_tokens"),
                );
                replace(
                    &mut self.cache_read_tokens,
                    usage_value.pointer("/prompt_tokens_details/cached_tokens"),
                );
                replace(
                    &mut self.cache_write_tokens,
                    usage_value.pointer("/prompt_tokens_details/cache_write_tokens"),
                );
            }
            "openai_responses" => {
                replace(
                    &mut self.input_tokens,
                    usage_value
                        .get("input_tokens")
                        .or_else(|| usage_value.get("prompt_tokens")),
                );
                replace(
                    &mut self.output_tokens,
                    usage_value
                        .get("output_tokens")
                        .or_else(|| usage_value.get("completion_tokens")),
                );
                replace(
                    &mut self.cache_read_tokens,
                    usage_value
                        .pointer("/input_tokens_details/cached_tokens")
                        .or_else(|| usage_value.pointer("/input_token_details/cached_tokens"))
                        .or_else(|| usage_value.pointer("/prompt_tokens_details/cached_tokens")),
                );
                replace(
                    &mut self.cache_write_tokens,
                    usage_value
                        .pointer("/input_tokens_details/cache_write_tokens")
                        .or_else(|| usage_value.pointer("/input_token_details/cache_write_tokens"))
                        .or_else(|| {
                            usage_value.pointer("/prompt_tokens_details/cache_write_tokens")
                        }),
                );
            }
            "anthropic_messages" => {
                self.anthropic_usage.top_level.absorb(usage_value);
                if let Some((iterations, compaction)) = anthropic_iteration_usage(usage_value) {
                    // `iterations` is a request-level snapshot. Replace instead of adding so
                    // cumulative SSE events never double count the same sampling iteration.
                    self.anthropic_usage.iterations = iterations;
                    // A later message_delta may omit compaction entries (or the entire array).
                    // Retain the most recent explicit compaction snapshot in that case.
                    if let Some(compaction) = compaction {
                        self.anthropic_usage.compaction = compaction;
                    }
                }
                let top_level = self.anthropic_usage.top_level;
                let compaction = self.anthropic_usage.compaction;
                let iterations = self.anthropic_usage.iterations;
                self.input_tokens = optional_max([
                    optional_saturating_sum([
                        top_level.input_tokens,
                        compaction.input_tokens,
                    ]),
                    iterations.input_tokens,
                ]);
                self.output_tokens = optional_max([
                    optional_saturating_sum([
                        top_level.output_tokens,
                        compaction.output_tokens,
                    ]),
                    iterations.output_tokens,
                ]);
                let top_level_thinking = if compaction.output_tokens.unwrap_or(0) > 0 {
                    top_level
                        .thinking_tokens
                        .zip(compaction.thinking_tokens)
                        .map(|(main, compact)| main.saturating_add(compact))
                } else {
                    top_level.thinking_tokens
                };
                // A breakdown must describe the same output snapshot. Older
                // iteration totals cannot fill in a missing final breakdown.
                let thinking = optional_max([
                    top_level_thinking.filter(|_| {
                        optional_saturating_sum([top_level.output_tokens, compaction.output_tokens])
                            == self.output_tokens
                    }),
                    iterations
                        .thinking_tokens
                        .filter(|_| iterations.output_tokens == self.output_tokens),
                ])
                .filter(|quantity| self.output_tokens.is_some_and(|output| *quantity <= output));
                if let Some(quantity) = thinking {
                    self.replace_meter("reasoning_tokens", "per_1m_tokens", quantity, 1, true);
                } else {
                    self.usage_meters
                        .retain(|meter| meter.meter_kind != "reasoning_tokens");
                }
                self.cache_read_tokens = optional_max([
                    optional_saturating_sum([
                        top_level.cache_read_tokens,
                        compaction.cache_read_tokens,
                    ]),
                    iterations.cache_read_tokens,
                ]);
                self.cache_write_tokens = optional_max([
                    optional_saturating_sum([
                        top_level.cache_write_tokens,
                        compaction.cache_write_tokens,
                    ]),
                    iterations.cache_write_tokens,
                ]);
                self.cache_write_5m_tokens = optional_max([
                    optional_saturating_sum([
                        top_level.cache_write_5m_tokens,
                        compaction.cache_write_5m_tokens,
                    ]),
                    iterations.cache_write_5m_tokens,
                ]);
                self.cache_write_1h_tokens = optional_max([
                    optional_saturating_sum([
                        top_level.cache_write_1h_tokens,
                        compaction.cache_write_1h_tokens,
                    ]),
                    iterations.cache_write_1h_tokens,
                ]);
                if let Some(quantity) = optional_max([
                    optional_saturating_sum([
                        top_level.search_queries,
                        compaction.search_queries,
                    ]),
                    iterations.search_queries,
                ]) {
                    self.replace_meter("search_query", "per_1k_queries", quantity, 1, false);
                }
            }
            "gemini_native" => {
                replace(
                    &mut self.input_tokens,
                    usage_value
                        .get("promptTokenCount")
                        .or_else(|| usage_value.get("total_input_tokens")),
                );
                replace(
                    &mut self.output_tokens,
                    usage_value
                        .get("candidatesTokenCount")
                        .or_else(|| usage_value.get("responseTokenCount"))
                        .or_else(|| usage_value.get("total_output_tokens")),
                );
                replace(
                    &mut self.cache_read_tokens,
                    usage_value
                        .get("cachedContentTokenCount")
                        .or_else(|| usage_value.get("total_cached_tokens")),
                );
            }
            _ => {}
        }
        self.absorb_usage_meters(&protocol, usage_value);
        if matches!(protocol.as_str(), "openai_chat" | "openai_responses") {
            self.output_tokens = crate::protocol::ir::output_tokens_with_reasoning(
                self.input_tokens,
                self.output_tokens,
                usage_value
                    .get("total_tokens")
                    .and_then(serde_json::Value::as_u64),
                first_u64_pointer(
                    usage_value,
                    &[
                        "/completion_tokens_details/reasoning_tokens",
                        "/output_tokens_details/reasoning_tokens",
                        "/output_token_details/reasoning_tokens",
                    ],
                ),
            );
        }
    }

    fn absorb_usage_meters(&mut self, protocol: &str, usage: &serde_json::Value) {
        match protocol {
            "openai_chat" | "openai_responses" => {
                for (meter_kind, included_in_total, pointers) in [
                    (
                        "audio_input_tokens",
                        true,
                        &[
                            "/prompt_tokens_details/audio_tokens",
                            "/input_tokens_details/audio_tokens",
                            "/input_token_details/audio_tokens",
                        ][..],
                    ),
                    (
                        "image_input_tokens",
                        true,
                        &[
                            "/prompt_tokens_details/image_tokens",
                            "/input_tokens_details/image_tokens",
                            "/input_token_details/image_tokens",
                        ][..],
                    ),
                    (
                        "audio_output_tokens",
                        true,
                        &[
                            "/completion_tokens_details/audio_tokens",
                            "/output_tokens_details/audio_tokens",
                            "/output_token_details/audio_tokens",
                        ][..],
                    ),
                    (
                        "image_output_tokens",
                        true,
                        &[
                            "/completion_tokens_details/image_tokens",
                            "/output_tokens_details/image_tokens",
                            "/output_token_details/image_tokens",
                        ][..],
                    ),
                    (
                        "reasoning_tokens",
                        true,
                        &[
                            "/completion_tokens_details/reasoning_tokens",
                            "/output_tokens_details/reasoning_tokens",
                            "/output_token_details/reasoning_tokens",
                        ][..],
                    ),
                    (
                        "cached_audio_input_tokens",
                        true,
                        &[
                            "/prompt_tokens_details/cached_tokens_details/audio_tokens",
                            "/input_tokens_details/cached_tokens_details/audio_tokens",
                            "/input_token_details/cached_tokens_details/audio_tokens",
                        ][..],
                    ),
                    (
                        "cached_image_input_tokens",
                        true,
                        &[
                            "/prompt_tokens_details/cached_tokens_details/image_tokens",
                            "/input_tokens_details/cached_tokens_details/image_tokens",
                            "/input_token_details/cached_tokens_details/image_tokens",
                        ][..],
                    ),
                ] {
                    if let Some(quantity) = first_u64_pointer(usage, pointers) {
                        self.replace_meter(
                            meter_kind,
                            "per_1m_tokens",
                            quantity,
                            1,
                            included_in_total,
                        );
                    }
                }
            }
            "gemini_native" => {
                for (details, output) in [
                    (usage.get("promptTokensDetails"), false),
                    (
                        usage
                            .get("candidatesTokensDetails")
                            .or_else(|| usage.get("responseTokensDetails")),
                        true,
                    ),
                ] {
                    for (modality, stem) in [
                        ("IMAGE", "image"),
                        ("AUDIO", "audio"),
                        ("VIDEO", "video"),
                        ("DOCUMENT", "document"),
                    ] {
                        if let Some(quantity) = gemini_modality_tokens(details, modality) {
                            self.replace_meter(
                                &format!(
                                    "{stem}_{}_tokens",
                                    if output { "output" } else { "input" }
                                ),
                                "per_1m_tokens",
                                quantity,
                                1,
                                true,
                            );
                        }
                    }
                }
                for (modality, stem) in [
                    ("IMAGE", "image"),
                    ("AUDIO", "audio"),
                    ("VIDEO", "video"),
                    ("DOCUMENT", "document"),
                ] {
                    if let Some(quantity) =
                        gemini_modality_tokens(usage.get("cacheTokensDetails"), modality)
                    {
                        self.replace_meter(
                            &format!("cached_{stem}_input_tokens"),
                            "per_1m_tokens",
                            quantity,
                            1,
                            true,
                        );
                    }
                    if let Some(quantity) =
                        gemini_modality_tokens(usage.get("toolUsePromptTokensDetails"), modality)
                    {
                        self.replace_meter(
                            &format!("tool_{stem}_input_tokens"),
                            "per_1m_tokens",
                            quantity,
                            1,
                            true,
                        );
                    }
                }
                if let Some(quantity) = usage
                    .get("toolUsePromptTokenCount")
                    .or_else(|| usage.get("total_tool_use_tokens"))
                    .and_then(serde_json::Value::as_u64)
                {
                    self.replace_meter("tool_input_tokens", "per_1m_tokens", quantity, 1, false);
                }
                if let Some(quantity) = usage
                    .get("thoughtsTokenCount")
                    .or_else(|| usage.get("total_thought_tokens"))
                    .and_then(serde_json::Value::as_u64)
                {
                    self.replace_meter("reasoning_tokens", "per_1m_tokens", quantity, 1, false);
                }
                for (details_key, prefix, included_in_total) in [
                    ("input_tokens_by_modality", "", true),
                    ("output_tokens_by_modality", "", true),
                    ("cached_tokens_by_modality", "cached_", true),
                    ("tool_use_tokens_by_modality", "tool_", true),
                ] {
                    let output = details_key.starts_with("output_");
                    for (modality, stem) in [
                        ("image", "image"),
                        ("audio", "audio"),
                        ("video", "video"),
                        ("document", "document"),
                    ] {
                        let Some(quantity) =
                            interaction_modality_tokens(usage.get(details_key), modality)
                        else {
                            continue;
                        };
                        let meter_kind = match (prefix, output) {
                            ("cached_", _) => format!("cached_{stem}_input_tokens"),
                            ("tool_", _) => format!("tool_{stem}_input_tokens"),
                            (_, true) => format!("{stem}_output_tokens"),
                            _ => format!("{stem}_input_tokens"),
                        };
                        self.replace_meter(
                            &meter_kind,
                            "per_1m_tokens",
                            quantity,
                            1,
                            included_in_total,
                        );
                    }
                }
                if let Some(items) = usage
                    .get("grounding_tool_count")
                    .and_then(serde_json::Value::as_array)
                {
                    for item in items {
                        let Some(qualifier) = item
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                        else {
                            continue;
                        };
                        let Some(quantity) = item.get("count").and_then(serde_json::Value::as_u64)
                        else {
                            continue;
                        };
                        self.replace_qualified_meter(
                            "search_query",
                            "per_1k_queries",
                            qualifier,
                            quantity,
                            1,
                            false,
                        );
                    }
                }
            }
            _ => {}
        }
        self.absorb_common_usage_meters(usage);
    }

    fn absorb_common_usage_meters(&mut self, usage: &serde_json::Value) {
        if usage.get("type").and_then(serde_json::Value::as_str) == Some("duration") {
            if let Some(seconds) = usage.get("seconds").and_then(serde_json::Value::as_f64) {
                if seconds.is_finite() && seconds > 0.0 && seconds < u64::MAX as f64 / 1000.0 {
                    self.replace_meter("audio_input_second", "per_second", (seconds * 1000.0).ceil() as u64, 1000, false);
                }
            }
        }
        for (meter_kind, pointer) in [
            ("image_input_unit", "/input_images"),
            ("image_output_unit", "/output_images"),
            ("page", "/pages"),
        ] {
            if let Some(quantity) = usage.pointer(pointer).and_then(serde_json::Value::as_u64) {
                self.replace_meter(
                    meter_kind,
                    match meter_kind {
                        "page" => "per_page",
                        _ => "per_image",
                    },
                    quantity,
                    1,
                    false,
                );
            }
        }
        for (meter_kind, pointer) in [
            ("audio_input_second", "/input_audio_seconds"),
            ("audio_output_second", "/output_audio_seconds"),
            ("video_input_second", "/input_video_seconds"),
            ("video_output_second", "/output_video_seconds"),
        ] {
            if let Some(seconds) = usage.pointer(pointer).and_then(serde_json::Value::as_f64) {
                if seconds.is_finite() && seconds > 0.0 {
                    self.replace_meter(
                        meter_kind,
                        "per_second",
                        (seconds * 1000.0).ceil() as u64,
                        1000,
                        false,
                    );
                }
            }
        }
    }

    fn replace_meter(
        &mut self,
        meter_kind: &str,
        unit: &str,
        quantity: u64,
        quantity_scale: u64,
        included_in_total: bool,
    ) {
        self.replace_qualified_meter(
            meter_kind,
            unit,
            "",
            quantity,
            quantity_scale,
            included_in_total,
        );
    }

    fn replace_qualified_meter(
        &mut self,
        meter_kind: &str,
        unit: &str,
        qualifier: &str,
        quantity: u64,
        quantity_scale: u64,
        included_in_total: bool,
    ) {
        if quantity == 0 && meter_kind != "reasoning_tokens" {
            return;
        }
        let meter = SupplierUsageMeter {
            meter_kind: meter_kind.to_string(),
            unit: unit.to_string(),
            qualifier: qualifier.trim().to_ascii_lowercase(),
            quantity,
            quantity_scale,
            included_in_total,
        };
        if let Some(existing) = self.usage_meters.iter_mut().find(|existing| {
            existing.meter_kind == meter.meter_kind
                && existing.unit == meter.unit
                && existing.qualifier == meter.qualifier
        }) {
            *existing = meter;
        } else {
            self.usage_meters.push(meter);
        }
    }

    fn observed(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.cache_write_5m_tokens.is_some()
            || self.cache_write_1h_tokens.is_some()
            || !self.usage_meters.is_empty()
    }

    pub(crate) fn lan_share_token_counts(&self) -> UsageTokenCounts {
        let cache_write_tokens = self.cache_write_tokens.or_else(|| {
            match (self.cache_write_5m_tokens, self.cache_write_1h_tokens) {
                (Some(five_minutes), Some(one_hour)) => {
                    Some(five_minutes.saturating_add(one_hour))
                }
                (Some(value), None) | (None, Some(value)) => Some(value),
                (None, None) => None,
            }
        });
        let input_tokens = optional_saturating_sum([
            self.input_tokens,
            (!self.input_tokens_include_cache_read)
                .then_some(self.cache_read_tokens)
                .flatten(),
            (!self.input_tokens_include_cache_write)
                .then_some(cache_write_tokens)
                .flatten(),
        ]);
        let extra_output_tokens = self
            .usage_meters
            .iter()
            .filter(|meter| meter.meter_kind == "reasoning_tokens" && !meter.included_in_total)
            .map(|meter| meter.quantity.div_ceil(meter.quantity_scale.max(1)))
            .reduce(u64::saturating_add);
        UsageTokenCounts {
            input_tokens,
            output_tokens: optional_saturating_sum([
                self.output_tokens,
                extra_output_tokens,
            ]),
        }
    }
}

impl AnthropicUsageTotals {
    fn absorb(&mut self, usage: &serde_json::Value) {
        if let Some(output) = usage
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            if Some(output) != self.output_tokens {
                self.thinking_tokens = None;
            }
        }
        replace(&mut self.input_tokens, usage.get("input_tokens"));
        replace(&mut self.output_tokens, usage.get("output_tokens"));
        replace(
            &mut self.thinking_tokens,
            usage.pointer("/output_tokens_details/thinking_tokens"),
        );
        replace(
            &mut self.cache_read_tokens,
            usage.get("cache_read_input_tokens"),
        );
        replace(
            &mut self.cache_write_tokens,
            usage.get("cache_creation_input_tokens"),
        );
        replace(
            &mut self.cache_write_5m_tokens,
            usage.pointer("/cache_creation/ephemeral_5m_input_tokens"),
        );
        replace(
            &mut self.cache_write_1h_tokens,
            usage.pointer("/cache_creation/ephemeral_1h_input_tokens"),
        );
        replace(
            &mut self.search_queries,
            usage.pointer("/server_tool_use/web_search_requests"),
        );
    }

    fn add_iteration(&mut self, iteration: &serde_json::Value) {
        add_optional_u64(&mut self.input_tokens, iteration.get("input_tokens"));
        add_optional_u64(&mut self.output_tokens, iteration.get("output_tokens"));
        add_optional_u64(
            &mut self.cache_read_tokens,
            iteration.get("cache_read_input_tokens"),
        );
        add_optional_u64(
            &mut self.cache_write_tokens,
            iteration.get("cache_creation_input_tokens"),
        );
        add_optional_u64(
            &mut self.cache_write_5m_tokens,
            iteration.pointer("/cache_creation/ephemeral_5m_input_tokens"),
        );
        add_optional_u64(
            &mut self.cache_write_1h_tokens,
            iteration.pointer("/cache_creation/ephemeral_1h_input_tokens"),
        );
        add_optional_u64(
            &mut self.search_queries,
            iteration.pointer("/server_tool_use/web_search_requests"),
        );
    }
}

fn anthropic_iteration_usage(
    usage: &serde_json::Value,
) -> Option<(AnthropicUsageTotals, Option<AnthropicUsageTotals>)> {
    let iterations = usage.get("iterations")?.as_array()?;
    if iterations.is_empty() {
        return None;
    }
    let mut all = AnthropicUsageTotals::default();
    let mut compaction = AnthropicUsageTotals::default();
    let mut observed_compaction = false;
    for iteration in iterations {
        all.add_iteration(iteration);
        if iteration.get("type").and_then(serde_json::Value::as_str) == Some("compaction") {
            observed_compaction = true;
            compaction.add_iteration(iteration);
        }
    }
    all.thinking_tokens = sum_anthropic_thinking_tokens(iterations.iter());
    compaction.thinking_tokens =
        sum_anthropic_thinking_tokens(iterations.iter().filter(|iteration| {
            iteration.get("type").and_then(serde_json::Value::as_str) == Some("compaction")
        }));
    Some((all, observed_compaction.then_some(compaction)))
}

fn sum_anthropic_thinking_tokens<'a>(
    mut iterations: impl Iterator<Item = &'a serde_json::Value>,
) -> Option<u64> {
    iterations.try_fold(0_u64, |sum, iteration| {
        sum.checked_add(
            iteration
                .pointer("/output_tokens_details/thinking_tokens")?
                .as_u64()?,
        )
    })
}

fn add_optional_u64(target: &mut Option<u64>, value: Option<&serde_json::Value>) {
    let Some(value) = value.and_then(serde_json::Value::as_u64) else {
        return;
    };
    *target = Some(target.unwrap_or_default().saturating_add(value));
}

fn optional_saturating_sum<const N: usize>(values: [Option<u64>; N]) -> Option<u64> {
    let mut observed = false;
    let mut total = 0_u64;
    for value in values.into_iter().flatten() {
        observed = true;
        total = total.saturating_add(value);
    }
    observed.then_some(total)
}

fn optional_max<const N: usize>(values: [Option<u64>; N]) -> Option<u64> {
    values.into_iter().flatten().max()
}

fn first_u64_pointer(value: &serde_json::Value, pointers: &[&str]) -> Option<u64> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_u64))
}

fn gemini_modality_tokens(
    details: Option<&serde_json::Value>,
    expected_modality: &str,
) -> Option<u64> {
    details?
        .as_array()?
        .iter()
        .filter(|item| {
            item.get("modality")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|modality| modality.eq_ignore_ascii_case(expected_modality))
        })
        .filter_map(|item| {
            item.get("tokenCount")
                .or_else(|| item.get("token_count"))
                .and_then(serde_json::Value::as_u64)
        })
        .reduce(u64::saturating_add)
}

fn interaction_modality_tokens(
    details: Option<&serde_json::Value>,
    expected_modality: &str,
) -> Option<u64> {
    details?
        .as_array()?
        .iter()
        .filter(|item| {
            item.get("modality")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|modality| modality.eq_ignore_ascii_case(expected_modality))
        })
        .filter_map(|item| item.get("tokens").and_then(serde_json::Value::as_u64))
        .reduce(u64::saturating_add)
}

const fn one() -> u64 {
    1
}

fn replace(target: &mut Option<u64>, value: Option<&serde_json::Value>) {
    if let Some(value) = value.and_then(serde_json::Value::as_u64) {
        *target = Some(value);
    }
}

fn upstream_usage_from_body(protocol: &str, body: &[u8]) -> Option<SupplierUpstreamUsage> {
    let value = serde_json::from_slice(body).ok()?;
    let usage = SupplierUpstreamUsage::from_json(protocol, &value);
    usage.observed().then_some(usage)
}

fn upstream_usage_from_response(protocol: &str, body: &[u8]) -> Option<SupplierUpstreamUsage> {
    upstream_usage_from_body(protocol, body).or_else(|| {
        let mut accumulator = SupplierUpstreamUsageAccumulator::new(protocol);
        accumulator.push(body);
        accumulator.finish()
    })
}

pub(crate) fn attach_upstream_usage(
    payload: &mut serde_json::Map<String, serde_json::Value>,
    usage: Option<&SupplierUpstreamUsage>,
) {
    let Some(usage) = usage.filter(|usage| usage.observed()) else {
        return;
    };
    if let Ok(value) = serde_json::to_value(usage) {
        payload.insert(SUPPLIER_UPSTREAM_USAGE_FIELD.to_string(), value);
    }
}

#[derive(Clone)]
pub(crate) struct SupplierUpstreamUsageAccumulator {
    usage: SupplierUpstreamUsage,
    line: Vec<u8>,
    multiline_data: Vec<u8>,
}

impl SupplierUpstreamUsageAccumulator {
    pub(crate) fn new(protocol: &str) -> Self {
        Self {
            usage: SupplierUpstreamUsage {
                source_protocol: protocol.trim().to_string(),
                input_tokens_include_cache_read: protocol != "anthropic_messages",
                input_tokens_include_cache_write: protocol != "anthropic_messages",
                ..Default::default()
            },
            line: Vec::new(),
            multiline_data: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.line.extend_from_slice(chunk);
        let mut consumed = 0;
        while let Some(relative) = self.line[consumed..].iter().position(|byte| *byte == b'\n') {
            let end = consumed + relative;
            absorb_upstream_usage_event_line(&mut self.usage, &mut self.multiline_data, &self.line[consumed..end]);
            consumed = end + 1;
        }
        if consumed > 0 {
            self.line.drain(..consumed);
        }
        if self.line.len() + self.multiline_data.len() > MAX_UPSTREAM_USAGE_LINE_BYTES {
            self.line.clear();
            self.multiline_data.clear();
        }
    }

    pub(crate) fn finish(self) -> Option<SupplierUpstreamUsage> {
        self.finish_observed(None).0
    }

    pub(crate) fn finish_observed(mut self, model: Option<&str>) -> (Option<SupplierUpstreamUsage>, Option<crate::upstream_failure::Failure>) {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            absorb_upstream_usage_event_line(&mut self.usage, &mut self.multiline_data, &line);
        }
        absorb_upstream_usage_event_line(&mut self.usage, &mut self.multiline_data, b"");
        let failure = self.usage.terminal_error.as_ref().and_then(|value| crate::upstream_failure::observe_value(200, value, model));
        (self.usage.observed().then_some(self.usage), failure)
    }
}

fn absorb_upstream_usage_event_line(usage: &mut SupplierUpstreamUsage, pending: &mut Vec<u8>, line: &[u8]) {
    let line = trim_ascii(line);
    if line.len() > MAX_UPSTREAM_USAGE_LINE_BYTES { pending.clear(); return; }
    if line.is_empty() {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(pending) { usage.absorb_json(&value); }
        pending.clear();
    } else if let Some(data) = line.strip_prefix(b"data:") {
        let data = trim_ascii(data);
        if pending.is_empty() {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) {
                usage.absorb_json(&value);
                return;
            }
        }
        if pending.len() + data.len() < MAX_UPSTREAM_USAGE_LINE_BYTES {
            pending.extend_from_slice(data);
            pending.push(b'\n');
        } else { pending.clear(); }
    } else {
        absorb_upstream_usage_line(usage, line);
    }
}

fn absorb_upstream_usage_line(usage: &mut SupplierUpstreamUsage, line: &[u8]) {
    let mut line = trim_ascii(line);
    if let Some(value) = line.strip_prefix(b"data:") {
        line = trim_ascii(value);
    }
    if line.is_empty() || line.len() > MAX_UPSTREAM_USAGE_LINE_BYTES || line[0] != b'{' {
        return;
    }
    if let Ok(value) = serde_json::from_slice(line) {
        usage.absorb_json(&value);
    }
}

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

#[cfg(test)]
mod upstream_usage_tests {
    use super::*;

    #[test]
    fn multiline_terminal_error_preserves_usage_and_reports_semantic_failure() {
        let raw = b"data: {\"usage\":{\"prompt_tokens\":17,\"completion_tokens\":3}}\r\n\r\ndata: {\"error\":\r\ndata: {\"code\":\"rate_limit_exceeded\",\"message\":\"slow down\"}}\r\n\r\n";
        let mut accumulator = SupplierUpstreamUsageAccumulator::new("openai_chat");
        for chunk in raw.chunks(3) { accumulator.push(chunk); }
        let (usage, failure) = accumulator.finish_observed(Some("model-a"));
        let usage = usage.unwrap();
        assert_eq!(usage.input_tokens, Some(17)); assert_eq!(usage.output_tokens, Some(3));
        assert_eq!(failure.unwrap().cause, "rate_limited");
    }

    #[test]
    fn reasoning_usage_normalizes_exclusive_totals_without_double_counting() {
        for (protocol, raw, output) in [
            (
                "openai_chat",
                r#"{"prompt_tokens":32,"completion_tokens":9,"total_tokens":135,"completion_tokens_details":{"reasoning_tokens":94}}"#,
                103,
            ),
            (
                "openai_chat",
                r#"{"prompt_tokens":32,"completion_tokens":103,"total_tokens":135,"completion_tokens_details":{"reasoning_tokens":94}}"#,
                103,
            ),
            (
                "openai_responses",
                r#"{"input_tokens":32,"output_tokens":9,"total_tokens":135,"output_tokens_details":{"reasoning_tokens":94}}"#,
                103,
            ),
            (
                "openai_responses",
                r#"{"prompt_tokens":32,"completion_tokens":9,"total_tokens":135,"completion_tokens_details":{"reasoning_tokens":94}}"#,
                103,
            ),
            (
                "openai_chat",
                r#"{"prompt_tokens":32,"completion_tokens":100,"completion_tokens_details":{"reasoning_tokens":94}}"#,
                100,
            ),
            (
                "openai_chat",
                r#"{"prompt_tokens":32,"completion_tokens":100,"total_tokens":999,"completion_tokens_details":{"reasoning_tokens":94}}"#,
                100,
            ),
        ] {
            let body = format!(r#"{{"usage":{raw}}}"#);
            let usage = upstream_usage_from_body(protocol, body.as_bytes()).unwrap();
            assert_eq!(usage.output_tokens, Some(output), "{body}");
            assert_eq!(usage.lan_share_token_counts().output_tokens, Some(output));
            let stream = format!("data: {body}\n\ndata: {body}\n\n");
            for chunk_size in [1, 7, stream.len()] {
                let mut acc = SupplierUpstreamUsageAccumulator::new(protocol);
                for chunk in stream.as_bytes().chunks(chunk_size) {
                    acc.push(chunk);
                }
                assert_eq!(
                    acc.finish().unwrap(),
                    usage,
                    "{protocol}, chunk size {chunk_size}"
                );
            }
        }
    }

    #[test]
    fn anthropic_reasoning_uses_reported_details_and_preserves_unknown() {
        for thinking in [0, 94] {
            let body = format!(
                r#"{{"usage":{{"input_tokens":32,"output_tokens":103,"output_tokens_details":{{"thinking_tokens":{thinking}}}}}}}"#
            );
            let usage = upstream_usage_from_body("anthropic_messages", body.as_bytes()).unwrap();
            assert_eq!(usage.output_tokens, Some(103));
            let meter = usage
                .usage_meters
                .iter()
                .find(|m| m.meter_kind == "reasoning_tokens")
                .unwrap();
            assert_eq!(meter.quantity, thinking);
            assert!(meter.included_in_total);
        }
        let usage = upstream_usage_from_body("anthropic_messages", br#"{"usage":{"input_tokens":32,"output_tokens":103},"content":[{"type":"thinking","thinking":"A visible summary is not the billable reasoning."}]}"#).unwrap();
        assert!(usage
            .usage_meters
            .iter()
            .all(|m| m.meter_kind != "reasoning_tokens"));
    }

    #[test]
    fn anthropic_reasoning_compaction_and_final_deltas_are_not_counted_twice() {
        let start = br#"data: {"type":"message_start","message":{"usage":{"input_tokens":32,"output_tokens":20,"output_tokens_details":{"thinking_tokens":10},"iterations":[{"type":"compaction","input_tokens":1000,"output_tokens":100,"output_tokens_details":{"thinking_tokens":70}},{"type":"message","input_tokens":32,"output_tokens":20,"output_tokens_details":{"thinking_tokens":10}}]}}}

"#;
        let end = br#"data: {"type":"message_delta","usage":{"output_tokens":40,"output_tokens_details":{"thinking_tokens":30}}}

"#;
        let mut acc = SupplierUpstreamUsageAccumulator::new("anthropic_messages");
        acc.push(start);
        acc.push(end);
        acc.push(end);
        let usage = acc.finish().unwrap();
        assert_eq!(usage.output_tokens, Some(140));
        assert_eq!(usage.input_tokens, Some(1032));
        let meter = usage
            .usage_meters
            .iter()
            .find(|m| m.meter_kind == "reasoning_tokens")
            .unwrap();
        assert_eq!(meter.quantity, 100);
        assert!(meter.included_in_total);

        let mut acc = SupplierUpstreamUsageAccumulator::new("anthropic_messages");
        acc.push(start);
        acc.push(b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":40}}\n\n");
        let usage = acc.finish().unwrap();
        assert_eq!(usage.output_tokens, Some(140));
        assert!(
            usage
                .usage_meters
                .iter()
                .all(|meter| meter.meter_kind != "reasoning_tokens"),
            "a missing final breakdown must not reuse an earlier partial count"
        );
    }

    #[test]
    fn extracts_non_stream_usage_with_provider_inclusion_semantics() {
        let openai = upstream_usage_from_body(
            "openai_responses",
            br#"{"usage":{"input_tokens":30,"output_tokens":4,"input_tokens_details":{"cached_tokens":12,"cache_write_tokens":8}}}"#,
        )
        .unwrap();
        assert_eq!(openai.cache_read_tokens, Some(12));
        assert_eq!(openai.cache_write_tokens, Some(8));
        assert!(openai.input_tokens_include_cache_read);

        let anthropic = upstream_usage_from_body(
            "anthropic_messages",
            br#"{"usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":7,"cache_creation_input_tokens":5}}"#,
        )
        .unwrap();
        assert_eq!(anthropic.cache_read_tokens, Some(7));
        assert_eq!(anthropic.cache_write_tokens, Some(5));
        assert!(!anthropic.input_tokens_include_cache_read);
        assert!(!anthropic.input_tokens_include_cache_write);
    }

    #[test]
    fn granular_anthropic_cache_write_usage_is_observed_without_total() {
        let usage = upstream_usage_from_body(
            "anthropic_messages",
            br#"{"usage":{"cache_creation":{"ephemeral_5m_input_tokens":5,"ephemeral_1h_input_tokens":7}}}"#,
        )
        .unwrap();
        assert_eq!(usage.cache_write_5m_tokens, Some(5));
        assert_eq!(usage.cache_write_1h_tokens, Some(7));
    }

    #[test]
    fn anthropic_compaction_iterations_are_aggregated_for_marketplace_usage() {
        let usage = upstream_usage_from_body(
            "anthropic_messages",
            br#"{"usage":{"input_tokens":500,"output_tokens":20,"cache_read_input_tokens":4,"iterations":[{"type":"compaction","input_tokens":60000,"output_tokens":1000,"cache_read_input_tokens":100,"cache_creation_input_tokens":12,"cache_creation":{"ephemeral_5m_input_tokens":7,"ephemeral_1h_input_tokens":5}},{"type":"message","input_tokens":500,"output_tokens":20,"cache_read_input_tokens":4,"server_tool_use":{"web_search_requests":2}}]}}"#,
        )
        .unwrap();

        assert_eq!(usage.input_tokens, Some(60_500));
        assert_eq!(usage.output_tokens, Some(1_020));
        assert_eq!(usage.cache_read_tokens, Some(104));
        assert_eq!(usage.cache_write_tokens, Some(12));
        assert_eq!(usage.cache_write_5m_tokens, Some(7));
        assert_eq!(usage.cache_write_1h_tokens, Some(5));
        assert!(usage.usage_meters.iter().any(|meter| {
            meter.meter_kind == "search_query" && meter.quantity == 2 && !meter.included_in_total
        }));
    }

    #[test]
    fn anthropic_stream_terminal_usage_keeps_compaction_iterations() {
        let mut accumulator = SupplierUpstreamUsageAccumulator::new("anthropic_messages");
        accumulator.push(br#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":500,"output_tokens":1,"cache_read_input_tokens":4,"cache_creation_input_tokens":3,"cache_creation":{"ephemeral_5m_input_tokens":3,"ephemeral_1h_input_tokens":0},"iterations":[{"type":"compaction","input_tokens":60000,"output_tokens":1000,"cache_read_input_tokens":100,"cache_creation_input_tokens":12,"cache_creation":{"ephemeral_5m_input_tokens":7,"ephemeral_1h_input_tokens":5}},{"type":"message","input_tokens":500,"output_tokens":1,"cache_read_input_tokens":4,"cache_creation_input_tokens":3,"cache_creation":{"ephemeral_5m_input_tokens":3,"ephemeral_1h_input_tokens":0},"server_tool_use":{"web_search_requests":2}}]}}}

"#);
        accumulator.push(br#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20}}

"#);

        let usage = accumulator.finish().expect("stream usage");
        assert_eq!(usage.input_tokens, Some(60_500));
        assert_eq!(usage.output_tokens, Some(1_020));
        assert_eq!(usage.cache_read_tokens, Some(104));
        assert_eq!(usage.cache_write_tokens, Some(15));
        assert_eq!(usage.cache_write_5m_tokens, Some(10));
        assert_eq!(usage.cache_write_1h_tokens, Some(5));
        assert!(usage.usage_meters.iter().any(|meter| {
            meter.meter_kind == "search_query" && meter.quantity == 2 && !meter.included_in_total
        }));
    }

    #[test]
    fn preserves_anthropic_paid_web_search_across_protocol_conversion() {
        let usage = upstream_usage_from_body(
            "anthropic_messages",
            br#"{"usage":{"input_tokens":105,"output_tokens":20,"server_tool_use":{"web_search_requests":3}}}"#,
        )
        .unwrap();

        let meter = usage
            .usage_meters
            .iter()
            .find(|meter| meter.meter_kind == "search_query")
            .expect("search query meter");
        assert_eq!(meter.unit, "per_1k_queries");
        assert_eq!(meter.quantity, 3);
        assert!(!meter.included_in_total);
    }

    #[test]
    fn fragmented_stream_usage_keeps_terminal_cache_counters() {
        let mut accumulator = SupplierUpstreamUsageAccumulator::new("openai_chat");
        accumulator.push(b"data: {\"choices\":[]}\n\nda");
        accumulator.push(b"ta: {\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":3,\"prompt_tokens_details\":{\"cached_tokens\":8,\"cache_write_tokens\":4}}}\n\n");
        let usage = accumulator.finish().unwrap();
        assert_eq!(usage.input_tokens, Some(20));
        assert_eq!(usage.output_tokens, Some(3));
        assert_eq!(usage.cache_read_tokens, Some(8));
        assert_eq!(usage.cache_write_tokens, Some(4));
    }

    #[test]
    fn extracts_wrapped_gemini_usage_from_sse() {
        let usage = upstream_usage_from_response(
            "gemini_native",
            b"data: {\"response\":{\"usageMetadata\":{\"promptTokenCount\":30,\"candidatesTokenCount\":4,\"cachedContentTokenCount\":12}}}\n\n",
        )
        .unwrap();
        assert_eq!(usage.input_tokens, Some(30));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.cache_read_tokens, Some(12));
    }

    #[test]
    fn preserves_multimodal_usage_across_protocol_conversion() {
        let usage = upstream_usage_from_body(
            "gemini_native",
            br#"{"usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":20,"candidatesTokenCount":12,"toolUsePromptTokenCount":12,"promptTokensDetails":[{"modality":"TEXT","tokenCount":10},{"modality":"IMAGE","tokenCount":20},{"modality":"AUDIO","tokenCount":15},{"modality":"VIDEO","tokenCount":25},{"modality":"DOCUMENT","tokenCount":30}],"cacheTokensDetails":[{"modality":"TEXT","tokenCount":2},{"modality":"IMAGE","tokenCount":4},{"modality":"AUDIO","tokenCount":6},{"modality":"VIDEO","tokenCount":5},{"modality":"DOCUMENT","tokenCount":3}],"toolUsePromptTokensDetails":[{"modality":"TEXT","tokenCount":5},{"modality":"IMAGE","tokenCount":7}],"candidatesTokensDetails":[{"modality":"AUDIO","tokenCount":7},{"modality":"IMAGE","tokenCount":5}]}}"#,
        )
        .unwrap();

        let meters = usage
            .usage_meters
            .iter()
            .map(|meter| (meter.meter_kind.as_str(), meter.quantity))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(meters.get("image_input_tokens"), Some(&20));
        assert_eq!(meters.get("audio_input_tokens"), Some(&15));
        assert_eq!(meters.get("video_input_tokens"), Some(&25));
        assert_eq!(meters.get("document_input_tokens"), Some(&30));
        assert_eq!(meters.get("audio_output_tokens"), Some(&7));
        assert_eq!(meters.get("image_output_tokens"), Some(&5));
        assert_eq!(meters.get("cached_audio_input_tokens"), Some(&6));
        assert_eq!(meters.get("cached_image_input_tokens"), Some(&4));
        assert_eq!(meters.get("cached_video_input_tokens"), Some(&5));
        assert_eq!(meters.get("cached_document_input_tokens"), Some(&3));
        assert_eq!(meters.get("tool_input_tokens"), Some(&12));
        assert_eq!(meters.get("tool_image_input_tokens"), Some(&7));
    }

    #[test]
    fn preserves_gemini_live_audio_video_and_reasoning_usage() {
        let usage = upstream_usage_from_body(
            "gemini_native",
            br#"{"usageMetadata":{"promptTokenCount":90,"responseTokenCount":30,"cachedContentTokenCount":10,"thoughtsTokenCount":4,"promptTokensDetails":[{"modality":"AUDIO","tokenCount":50},{"modality":"VIDEO","tokenCount":20}],"cacheTokensDetails":[{"modality":"AUDIO","tokenCount":8},{"modality":"VIDEO","tokenCount":2}],"responseTokensDetails":[{"modality":"AUDIO","tokenCount":25}]}}"#,
        )
        .unwrap();

        assert_eq!(usage.input_tokens, Some(90));
        assert_eq!(usage.output_tokens, Some(30));
        assert_eq!(usage.cache_read_tokens, Some(10));
        for (kind, quantity, included) in [
            ("audio_input_tokens", 50, true),
            ("video_input_tokens", 20, true),
            ("cached_audio_input_tokens", 8, true),
            ("cached_video_input_tokens", 2, true),
            ("audio_output_tokens", 25, true),
            ("reasoning_tokens", 4, false),
        ] {
            let meter = usage
                .usage_meters
                .iter()
                .find(|meter| meter.meter_kind == kind)
                .unwrap_or_else(|| panic!("missing {kind}"));
            assert_eq!(meter.quantity, quantity);
            assert_eq!(meter.included_in_total, included);
        }
    }

    #[test]
    fn preserves_gemini_interactions_usage_and_grounding_across_conversion() {
        let usage = upstream_usage_from_body(
            "gemini_native",
            br#"{"interaction":{"usage":{"total_input_tokens":50,"total_output_tokens":11,"total_cached_tokens":12,"total_thought_tokens":7,"total_tool_use_tokens":7,"input_tokens_by_modality":[{"modality":"image","tokens":4},{"modality":"audio","tokens":6},{"modality":"video","tokens":5},{"modality":"document","tokens":5}],"cached_tokens_by_modality":[{"modality":"audio","tokens":2},{"modality":"document","tokens":2}],"tool_use_tokens_by_modality":[{"modality":"image","tokens":2},{"modality":"document","tokens":1}],"output_tokens_by_modality":[{"modality":"audio","tokens":2},{"modality":"image","tokens":2}],"grounding_tool_count":[{"type":"google_search","count":3},{"type":"google_maps","count":2}]}}}"#,
        )
        .unwrap();

        assert_eq!(usage.input_tokens, Some(50));
        assert_eq!(usage.output_tokens, Some(11));
        assert_eq!(usage.cache_read_tokens, Some(12));
        for (kind, qualifier, quantity) in [
            ("image_input_tokens", "", 4),
            ("audio_input_tokens", "", 6),
            ("video_input_tokens", "", 5),
            ("document_input_tokens", "", 5),
            ("audio_output_tokens", "", 2),
            ("image_output_tokens", "", 2),
            ("cached_audio_input_tokens", "", 2),
            ("cached_document_input_tokens", "", 2),
            ("tool_input_tokens", "", 7),
            ("tool_image_input_tokens", "", 2),
            ("tool_document_input_tokens", "", 1),
            ("reasoning_tokens", "", 7),
            ("search_query", "google_search", 3),
            ("search_query", "google_maps", 2),
        ] {
            let meter = usage
                .usage_meters
                .iter()
                .find(|meter| meter.meter_kind == kind && meter.qualifier == qualifier)
                .unwrap_or_else(|| panic!("missing {kind}:{qualifier}"));
            assert_eq!(meter.quantity, quantity);
        }
    }

    #[test]
    fn preserves_realtime_singular_details_and_cached_modalities() {
        let usage = upstream_usage_from_body(
            "openai_responses",
            br#"{"response":{"usage":{"input_tokens":80,"output_tokens":20,"input_token_details":{"cached_tokens":12,"audio_tokens":30,"image_tokens":10,"cached_tokens_details":{"audio_tokens":8,"image_tokens":4}},"output_token_details":{"audio_tokens":15}}}}"#,
        )
        .unwrap();

        assert_eq!(usage.cache_read_tokens, Some(12));
        let meters = usage
            .usage_meters
            .iter()
            .map(|meter| (meter.meter_kind.as_str(), meter.quantity))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(meters.get("audio_input_tokens"), Some(&30));
        assert_eq!(meters.get("image_input_tokens"), Some(&10));
        assert_eq!(meters.get("audio_output_tokens"), Some(&15));
        assert_eq!(meters.get("cached_audio_input_tokens"), Some(&8));
        assert_eq!(meters.get("cached_image_input_tokens"), Some(&4));
    }

    #[tokio::test]
    #[ignore = "defaults to a bounded live request budget; an explicit env override permits at most four"]
    async fn live_cache_usage_bounded_request_budget() -> Result<()> {
        let Some(provider) = std::env::var("CONST_API_LIVE_CACHE_PROVIDER")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| matches!(value.as_str(), "openai" | "antigravity" | "claude"))
        else {
            eprintln!("live cache test skipped: CONST_API_LIVE_CACHE_PROVIDER is not set");
            return Ok(());
        };
        let config_path = std::env::var_os("CONST_API_LIVE_CACHE_CONFIG_PATH")
            .map(std::path::PathBuf::from)
            .context("CONST_API_LIVE_CACHE_CONFIG_PATH is required")?;
        let config = crate::config::load_config_from_path(&config_path)?;
        let channel = config
            .channels
            .iter()
            .find(|channel| {
                channel.enabled
                    && matches!(
                        &channel.v2.executor,
                        ChannelExecutorLocator::RetainedSubscription {
                            provider: candidate
                        } if candidate.trim().eq_ignore_ascii_case(&provider)
                    )
            })
            .with_context(|| format!("no enabled retained {provider} channel"))?;
        let model = std::env::var("CONST_API_LIVE_CACHE_MODEL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                (!channel.upstream_model.trim().is_empty())
                    .then(|| channel.upstream_model.trim().to_string())
            })
            .or_else(|| channel.models.first().cloned())
            .context("live cache model is unavailable")?;
        if !channel.models.iter().any(|candidate| candidate == &model) {
            anyhow::bail!("model {model} is not declared by the selected {provider} channel");
        }
        let minimum_attempts = if provider == "antigravity" { 3 } else { 2 };
        let attempt_budget = match std::env::var("CONST_API_LIVE_CACHE_ATTEMPTS") {
            Ok(value) => {
                let parsed = value
                    .trim()
                    .parse::<usize>()
                    .with_context(|| {
                        format!(
                            "CONST_API_LIVE_CACHE_ATTEMPTS must be an integer from {minimum_attempts} to 4 for {provider}"
                        )
                    })?;
                if !(minimum_attempts..=4).contains(&parsed) {
                    anyhow::bail!(
                        "CONST_API_LIVE_CACHE_ATTEMPTS must be from {minimum_attempts} to 4 for {provider}"
                    );
                }
                parsed
            }
            // Gemini implicit caching can need one request to populate the cache and
            // another before the cached prefix becomes visible in usage metadata.
            // Three attempts avoids treating that normal warm-up as unsupported.
            Err(_) if provider == "antigravity" => 3,
            Err(_) => 2,
        };
        let openai_identity_profile = std::env::var("CONST_API_LIVE_CACHE_OPENAI_IDENTITY")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "current".to_string());
        if provider != "openai" && openai_identity_profile != "current" {
            anyhow::bail!(
                "CONST_API_LIVE_CACHE_OPENAI_IDENTITY only applies to the openai provider"
            );
        }
        if !matches!(
            openai_identity_profile.as_str(),
            "current" | "legacy-2026-03"
        ) {
            anyhow::bail!("CONST_API_LIVE_CACHE_OPENAI_IDENTITY must be current or legacy-2026-03");
        }
        let case_id = std::env::var("CONST_API_LIVE_CACHE_CASE_ID")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "baseline".to_string());
        if case_id.len() > 64
            || !case_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            anyhow::bail!(
                "CONST_API_LIVE_CACHE_CASE_ID must contain at most 64 ASCII letters, digits, '-' or '_'"
            );
        }

        let repetitions = match provider.as_str() {
            "openai" => 110,
            // Keep the shared prefix comfortably above Gemini 3.7 Flash's 4K
            // eligibility floor. The previous near-threshold sample could produce
            // no reusable cache segment even when implicit caching was working.
            "antigravity" => 400,
            _ => 240,
        };
        let stable_prefix = format!(
            "CONST API cache acceptance context {case_id}. Keep this exact prefix unchanged. \
             Routing identity, provider account, actual model, and target protocol must remain stable. "
        )
        .repeat(repetitions);
        let prompt = format!("{stable_prefix}\nReply with exactly: OK");
        let identity_digest = hex::encode(Sha256::digest(
            format!("const-api-live-cache:{case_id}").as_bytes(),
        ));
        let cache_session_id = format!(
            "{}-{}-{}-{}-{}",
            &identity_digest[0..8],
            &identity_digest[8..12],
            &identity_digest[12..16],
            &identity_digest[16..20],
            &identity_digest[20..32]
        );
        let cache_thread_id = format!(
            "{}-{}-{}-{}-{}",
            &identity_digest[32..40],
            &identity_digest[40..44],
            &identity_digest[44..48],
            &identity_digest[48..52],
            &identity_digest[52..64]
        );
        let installation_id = "019c050c-8cc0-7c31-a9c2-0c7d696f7d18";
        let openai_client_metadata = if openai_identity_profile == "legacy-2026-03" {
            serde_json::json!({
                "x-codex-installation-id": installation_id
            })
        } else {
            serde_json::json!({
                "session_id": cache_session_id,
                "thread_id": cache_thread_id,
                "x-codex-installation-id": installation_id,
                "x-codex-window-id": format!("{cache_thread_id}:0")
            })
        };
        let (path, bodies) = match provider.as_str() {
            "openai" => {
                let mut input = vec![serde_json::json!({
                    // Exercise implicit-message ID normalization on the real
                    // endpoint while retaining the same prefix in later turns.
                    "id": "item_cache_acceptance",
                    "role": "user",
                    "content": [{"type": "input_text", "text": prompt}]
                })];
                let mut bodies = Vec::with_capacity(attempt_budget);
                for attempt in 1..=attempt_budget {
                    if attempt > 1 {
                        input.push(serde_json::json!({
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "OK"}]
                        }));
                        input.push(serde_json::json!({
                            "role": "user",
                            "content": [{
                                "type": "input_text",
                                "text": format!("cache follow-up {attempt}; reply exactly OK")
                            }]
                        }));
                    }
                    bodies.push(serde_json::json!({
                        "model": model,
                        "input": input,
                        "max_output_tokens": 16,
                        "prompt_cache_key": cache_session_id,
                        "client_metadata": openai_client_metadata,
                        "store": false,
                        "stream": false
                    }).to_string());
                }
                ("/v1/responses", bodies)
            }
            "claude" => {
                let mut messages = vec![serde_json::json!({
                    "role": "user",
                    "content": [{"type": "text", "text": prompt}]
                })];
                let mut bodies = Vec::with_capacity(attempt_budget);
                for attempt in 1..=attempt_budget {
                    if attempt > 1 {
                        messages.push(serde_json::json!({"role": "assistant", "content": "OK"}));
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": format!("cache follow-up {attempt}; reply exactly OK")
                        }));
                    }
                    bodies.push(serde_json::json!({
                        "model": model,
                        "cache_control": {"type": "ephemeral"},
                        "messages": messages,
                        "metadata": {
                            "user_id": serde_json::json!({
                                "device_id": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                                "account_uuid": "",
                                "session_id": cache_session_id
                            }).to_string()
                        },
                        "max_tokens": 16,
                        "stream": false
                    }).to_string());
                }
                ("/v1/messages", bodies)
            }
            _ => {
                let mut messages = vec![serde_json::json!({"role": "user", "content": prompt})];
                let mut bodies = Vec::with_capacity(attempt_budget);
                for attempt in 1..=attempt_budget {
                    if attempt > 1 {
                        messages.push(serde_json::json!({"role": "assistant", "content": "OK"}));
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": format!("cache follow-up {attempt}; reply exactly OK")
                        }));
                    }
                    bodies.push(serde_json::json!({
                        "model": model,
                        "messages": messages,
                        "max_tokens": 16,
                        "stream": false
                    }).to_string());
                }
                ("/v1/chat/completions", bodies)
            }
        };
        let supplier = crate::config::supplier_from_channel(channel);
        let client = long_http_client().clone();
        let legacy_openai_auth =
            if provider == "openai" && openai_identity_profile == "legacy-2026-03" {
                let (token, credential, _) =
                    ensure_openai_subscription_access_token(&client, channel).await?;
                Some((token, credential))
            } else {
                None
            };
        let mut reports = Vec::with_capacity(attempt_budget);
        for attempt in 1..=attempt_budget {
            let body = &bodies[attempt - 1];
            let body_sha256 = hex::encode(Sha256::digest(body.as_bytes()));
            let started = std::time::Instant::now();
            if let Some((token, credential)) = legacy_openai_auth.as_ref() {
                let upstream_body = json_body_with_stream(body, true)?;
                let prepared = prepare_codex_subscription_endpoint_request(
                    &supplier,
                    &upstream_body,
                    false,
                    &model,
                    Some(&cache_session_id),
                    None,
                )?;
                let mut request = openai_subscription_response_request(
                    &client,
                    &codex_responses_url(),
                    token,
                    credential,
                    prepared.body,
                    None,
                )
                ?
                .build()?;
                for name in ["session-id", "thread-id", "x-client-request-id"] {
                    request.headers_mut().remove(name);
                }
                request.headers_mut().insert(
                    reqwest::header::HeaderName::from_static("session_id"),
                    reqwest::header::HeaderValue::from_str(&cache_session_id)?,
                );
                let response = client.execute(request).await?;
                let status = response.status().as_u16();
                let raw = response.bytes().await?;
                if !(200..300).contains(&status) {
                    anyhow::bail!(
                        "{provider} live cache attempt {attempt} failed: status={status}"
                    );
                }
                let usage = upstream_usage_from_response("openai_responses", &raw)
                    .context("legacy OpenAI response did not expose upstream usage")?;
                println!(
                    "provider={provider} model={model} identity={openai_identity_profile} case={case_id} attempt={attempt} body_sha256={body_sha256} input_tokens={:?} output_tokens={:?} cache_read_tokens={:?} cache_write_tokens={:?} elapsed_ms={} fault_codes=[]",
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_tokens,
                    usage.cache_write_tokens,
                    started.elapsed().as_millis(),
                );
                reports.push(usage);
                continue;
            }
            let payload =
                forward_subscription_supplier_request(&client, &supplier, path, body, false, None)
                    .await;
            let wire = crate::surface_wire::SurfaceEnvelope::from_payload(&payload)?;
            let status = wire.status.unwrap_or_default();
            if !(200..300).contains(&status) {
                let kind = payload
                    .get("error_kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown");
                let response_excerpt = wire
                    .body
                    .utf8()
                    .unwrap_or("<non-UTF-8 response>")
                    .chars()
                    .take(600)
                    .collect::<String>();
                anyhow::bail!(
                    "{provider} live cache attempt {attempt} failed: status={status} kind={kind} body={response_excerpt}"
                );
            }
            let usage: SupplierUpstreamUsage = serde_json::from_value(
                payload
                    .get(SUPPLIER_UPSTREAM_USAGE_FIELD)
                    .cloned()
                    .context("provider response did not expose upstream usage")?,
            )?;
            let fault_codes = payload
                .get(IMPROVEMENT_FAULTS_FIELD)
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|fault| fault.get("code").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>();
            println!(
                "provider={provider} model={model} identity={openai_identity_profile} case={case_id} attempt={attempt} body_sha256={body_sha256} input_tokens={:?} output_tokens={:?} cache_read_tokens={:?} cache_write_tokens={:?} elapsed_ms={} fault_codes={fault_codes:?}",
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_tokens,
                usage.cache_write_tokens,
                started.elapsed().as_millis(),
            );
            reports.push(usage);
        }
        if reports
            .iter()
            .skip(1)
            .all(|usage| usage.cache_read_tokens.unwrap_or_default() == 0)
        {
            anyhow::bail!(
                "{provider} attempts 2..={attempt_budget} observed no cache read in this run; implicit cache population is nondeterministic, so this does not prove that caching is unsupported"
            );
        }
        Ok(())
    }
}
