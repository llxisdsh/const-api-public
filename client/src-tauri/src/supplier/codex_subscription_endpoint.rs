#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubscriptionDialectAction {
    pub(crate) code: &'static str,
    pub(crate) path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubscriptionDialectAdjustment {
    pub(crate) body: String,
    pub(crate) actions: Vec<SubscriptionDialectAction>,
}

pub(crate) struct ReportedSubscriptionDialectAdjustment {
    pub(crate) body: String,
    pub(crate) faults: Vec<crate::supplier::ImprovementFault>,
}

/// Prepares the exact request body sent to the ChatGPT Codex subscription endpoint.
///
/// Generic OpenAI protocol conversion deliberately remains lossless. Every subscription-only
/// omission, normalization, and default is applied here after continuation recovery and cache
/// identity injection, immediately before the HTTP request is built. Keeping one production entry
/// point prevents a local-direct or supplier path from bypassing the endpoint contract.
pub(crate) fn prepare_codex_subscription_endpoint_request(
    config: &SupplierConfig,
    body: &str,
    compact: bool,
    model: &str,
    cache_identity: Option<&str>,
    request_headers: Option<&reqwest::header::HeaderMap>,
) -> Result<ReportedSubscriptionDialectAdjustment> {
    prepare_codex_subscription_endpoint_request_with_lite_override(
        config,
        body,
        compact,
        model,
        cache_identity,
        request_headers,
        None,
    )
}

fn prepare_codex_subscription_endpoint_request_with_lite_override(
    config: &SupplierConfig,
    body: &str,
    compact: bool,
    model: &str,
    cache_identity: Option<&str>,
    request_headers: Option<&reqwest::header::HeaderMap>,
    responses_lite_override: Option<bool>,
) -> Result<ReportedSubscriptionDialectAdjustment> {
    // Codex already emits the ChatGPT subscription dialect. Keep a genuine
    // first-party request byte-stable unless an old/resumed history contains
    // an invalid message ID or a reasoning item that cannot be replayed. Such a reasoning item has only
    // an rs_* server reference but no encrypted payload; the subscription
    // endpoint never persisted that reference and explicitly rejects it.
    if crate::codex_identity::codex_request_is_native(request_headers) {
        if let Some(adjustment) = repair_native_codex_history(body) {
            return report_codex_subscription_adjustment(adjustment, model);
        }
        return Ok(ReportedSubscriptionDialectAdjustment {
            body: body.to_string(),
            faults: Vec::new(),
        });
    }
    let (body, pre_continuation_action) = normalize_codex_string_input_before_continuation(body)?;
    let body = if compact {
        body
    } else {
        let body = prepare_codex_http_subscription_request(config, &body)?;
        openai_subscription_body_with_cache_identity(&body, cache_identity)?
    };
    let mut adjustment = adjust_codex_subscription_request_for_operation(&body, compact)?;
    if !compact {
        ensure_codex_subscription_session_metadata(&mut adjustment)?;
    }
    if !compact
        && responses_lite_override
            .or_else(|| crate::detection::openai_subscription_responses_lite_hint(config, model))
            == Some(true)
    {
        apply_codex_responses_lite_dialect(&mut adjustment)?;
    }
    if let Some(action) = pre_continuation_action {
        adjustment.actions.insert(0, action);
    }
    report_codex_subscription_adjustment(adjustment, model)
}

fn normalize_codex_string_input_before_continuation(
    body: &str,
) -> Result<(String, Option<SubscriptionDialectAction>)> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    let Some(object) = value.as_object_mut() else {
        return Ok((body.to_string(), None));
    };
    let Some(input) = object
        .get("input")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
    else {
        return Ok((body.to_string(), None));
    };
    object.insert(
        "input".to_string(),
        serde_json::json!([{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": input}]
        }]),
    );
    Ok((
        serde_json::to_string(&value)?,
        Some(SubscriptionDialectAction {
            code: "codex_string_input_normalized",
            path: "$.input".to_string(),
        }),
    ))
}

#[cfg(test)]
pub(crate) fn strip_codex_subscription_unsupported_params(body: &str) -> Result<String> {
    strip_codex_subscription_params_for_operation(body, false)
}

#[cfg(test)]
pub(crate) fn strip_codex_subscription_params_for_operation(
    body: &str,
    compact: bool,
) -> Result<String> {
    let adjustment = adjust_codex_subscription_request_for_operation(body, compact)?;
    for action in &adjustment.actions {
        log::debug!(
            "[const-api][protocol] subscription dialect adjustment endpoint=codex_responses code={} path={}",
            action.code, action.path
        );
    }
    Ok(adjustment.body)
}

#[cfg(test)]
pub(crate) fn adapt_codex_subscription_params_for_operation(
    body: &str,
    compact: bool,
    model: &str,
) -> Result<ReportedSubscriptionDialectAdjustment> {
    let adjustment = adjust_codex_subscription_request_for_operation(body, compact)?;
    report_codex_subscription_adjustment(adjustment, model)
}

fn report_codex_subscription_adjustment(
    adjustment: SubscriptionDialectAdjustment,
    model: &str,
) -> Result<ReportedSubscriptionDialectAdjustment> {
    let mut faults = Vec::new();
    // Count by action code only for diagnostics; retain all original actions/faults below.
    if log::log_enabled!(log::Level::Debug) {
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        for action in &adjustment.actions {
            *counts.entry(action.code).or_default() += 1;
        }
        log::debug!(
            "[const-api][subscription] endpoint adjustments endpoint=codex_responses model={} total={} codes={:?}",
            model, adjustment.actions.len(), counts
        );
    }
    for action in &adjustment.actions {
        log::trace!(
            "[const-api][subscription] endpoint adjustment endpoint=codex_responses code={} path={}",
            action.code, action.path
        );
        let Some(summary) = codex_subscription_loss_summary(action.code) else {
            continue;
        };
        faults.push(crate::supplier::ImprovementFault::endpoint_adjustment(
            action.code,
            "openai",
            "chatgpt_codex_subscription",
            "openai_responses",
            "openai_responses",
            model,
            &action.path,
            summary,
        ));
    }
    Ok(ReportedSubscriptionDialectAdjustment {
        body: adjustment.body,
        faults,
    })
}

fn codex_subscription_loss_summary(code: &str) -> Option<&'static str> {
    match code {
        "codex_token_limit_omitted" => Some(
            "ChatGPT Codex subscription transport does not accept this output-token limit; the field was omitted so the request could continue",
        ),
        "codex_sampling_hint_omitted" => Some(
            "ChatGPT Codex subscription transport does not accept this sampling control; the field was omitted so the request could continue",
        ),
        "codex_user_identity_omitted" => Some(
            "ChatGPT Codex subscription transport does not accept the public API user identity field; it was omitted so the request could continue",
        ),
        "codex_internal_extension_omitted" => Some(
            "ChatGPT Codex subscription transport does not accept this public API extension; it was omitted so the request could continue",
        ),
        "codex_prompt_cache_control_omitted" => Some(
            "ChatGPT Codex subscription transport does not accept these public API prompt-cache controls; they were omitted while prompt_cache_key was preserved for cache affinity",
        ),
        "codex_store_forced_false" => Some(
            "ChatGPT Codex subscription transport requires stateless execution; the requested store value was changed to false",
        ),
        "codex_reasoning_token_limit_omitted" => Some(
            "ChatGPT Codex subscription transport does not accept the requested reasoning token limit; it was omitted so the request could continue",
        ),
        "codex_parallel_tool_calls_disabled_for_lite" => Some(
            "This Codex model uses the Responses Lite dialect, which executes tool calls serially; parallel tool calls were disabled",
        ),
        "codex_unreplayable_reasoning_item_omitted" => Some(
            "A stateless Codex history item contained only an unpersisted reasoning reference and no encrypted reasoning payload; the unusable item was omitted so the request could continue",
        ),
        _ => None,
    }
}

fn repair_native_codex_history(body: &str) -> Option<SubscriptionDialectAdjustment> {
    // Healthy stateless Codex histories already carry one encrypted payload per
    // reasoning item. Count those fields without allocating a JSON tree and
    // preserve the native byte-stable hot path; build the DOM only when at least
    // one reasoning item may actually be unreplayable.
    let repair_reasoning = native_codex_stateless_reasoning_may_need_repair(body);
    let repair_message_ids = native_codex_message_ids_may_need_repair(body);
    if !repair_reasoning && !repair_message_ids {
        return None;
    }
    let mut value: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut actions = Vec::new();
    if repair_message_ids {
        normalize_codex_subscription_input_ids(&mut value, &mut actions, true);
    }
    if repair_reasoning && value.get("store").and_then(serde_json::Value::as_bool) == Some(false) {
        repair_codex_stateless_reasoning_items(&mut value, &mut actions)?;
    }
    if actions.is_empty() {
        return None;
    }
    Some(SubscriptionDialectAdjustment {
        body: serde_json::to_string(&value).ok()?,
        actions,
    })
}

fn repair_codex_stateless_reasoning_items(
    value: &mut serde_json::Value,
    actions: &mut Vec<SubscriptionDialectAction>,
) -> Option<()> {
    let input = value.get_mut("input")?.as_array_mut()?;
    let original = std::mem::take(input);
    let mut retained = Vec::with_capacity(original.len());
    let actions_before = actions.len();
    for (index, item) in original.into_iter().enumerate() {
        let unreplayable = item.get("type").and_then(serde_json::Value::as_str)
            == Some("reasoning")
            && item
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| !id.trim().is_empty())
            && !item
                .get("encrypted_content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|encrypted| !encrypted.is_empty());
        if unreplayable {
            actions.push(SubscriptionDialectAction {
                code: "codex_unreplayable_reasoning_item_omitted",
                path: format!("$.input[{index}]"),
            });
        } else {
            retained.push(item);
        }
    }
    *input = retained;
    if actions.len() == actions_before {
        return Some(());
    }
    let object = value.as_object_mut()?;
    let include = object
        .entry("include".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if let Some(include) = include.as_array_mut() {
        if !include
            .iter()
            .any(|entry| entry.as_str() == Some("reasoning.encrypted_content"))
        {
            include.push(serde_json::Value::String(
                "reasoning.encrypted_content".to_string(),
            ));
        }
    }
    Some(())
}

fn native_codex_message_ids_may_need_repair(body: &str) -> bool {
    // Inspect only item metadata, borrowing content instead of copying prompt/tool payloads.
    // Healthy native requests still return their original bytes, including unknown fields.
    #[derive(serde::Deserialize)]
    struct InputItem<'a> {
        id: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
        role: Option<String>,
        #[serde(borrow)]
        content: Option<&'a serde_json::value::RawValue>,
    }
    #[derive(serde::Deserialize)]
    struct Request<'a> {
        #[serde(borrow)]
        input: Vec<InputItem<'a>>,
    }
    let Ok(request) = serde_json::from_str::<Request<'_>>(body) else {
        return false;
    };
    request.input.iter().any(|item| {
        codex_input_is_complete_message(
            item.kind.as_deref(),
            item.role.as_deref(),
            item.content.is_some_and(|content| {
                content.get().starts_with('[') || content.get().starts_with('"')
            }),
        ) && item
            .id
            .as_ref()
            .is_some_and(|id| !id.is_empty() && (!id.starts_with("msg") || id.chars().count() > 64))
    })
}

fn native_codex_stateless_reasoning_may_need_repair(body: &str) -> bool {
    if !contains_json_literal_field_value(body, "store", "false") {
        return false;
    }
    let reasoning_items = count_json_string_field_value(body, "type", "reasoning");
    reasoning_items > 0
        && count_json_nonempty_string_field(body, "encrypted_content") < reasoning_items
}

fn next_json_field_value(body: &str, field: &str, mut offset: usize) -> Option<(usize, usize)> {
    let needle = format!("\"{field}\"");
    let bytes = body.as_bytes();
    while let Some(relative) = body[offset..].find(&needle) {
        let mut cursor = offset + relative + needle.len();
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b':') {
            offset = (cursor + 1).min(body.len());
            continue;
        }
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        return Some((cursor, (cursor + 1).min(body.len())));
    }
    None
}

fn count_json_string_field_value(body: &str, field: &str, expected: &str) -> usize {
    let expected = format!("\"{expected}\"");
    let mut count = 0;
    let mut offset = 0;
    while let Some((cursor, next)) = next_json_field_value(body, field, offset) {
        if body
            .get(cursor..)
            .is_some_and(|tail| tail.starts_with(&expected))
        {
            count += 1;
        }
        offset = next;
    }
    count
}

fn count_json_nonempty_string_field(body: &str, field: &str) -> usize {
    let bytes = body.as_bytes();
    let mut count = 0;
    let mut offset = 0;
    while let Some((cursor, next)) = next_json_field_value(body, field, offset) {
        if bytes.get(cursor) == Some(&b'\"') && bytes.get(cursor + 1) != Some(&b'\"') {
            count += 1;
        }
        offset = next;
    }
    count
}

fn contains_json_literal_field_value(body: &str, field: &str, expected: &str) -> bool {
    let mut offset = 0;
    while let Some((cursor, next)) = next_json_field_value(body, field, offset) {
        if body
            .get(cursor..)
            .is_some_and(|tail| tail.starts_with(expected))
        {
            return true;
        }
        offset = next;
    }
    false
}

#[cfg(test)]
pub(crate) fn adjust_codex_subscription_request(
    body: &str,
) -> Result<SubscriptionDialectAdjustment> {
    adjust_codex_subscription_request_for_operation(body, false)
}

fn adjust_codex_subscription_request_for_operation(
    body: &str,
    compact: bool,
) -> Result<SubscriptionDialectAdjustment> {
    let mut value: serde_json::Value = serde_json::from_str(body)?;
    let mut actions = Vec::new();
    if let Some(object) = value.as_object_mut() {
        // The public Responses schema and the ChatGPT Codex subscription endpoint are not the
        // same dialect. Generic adapters remain lossless; endpoint-only restrictions live only in
        // this final-send module.
        // Primary schema and transport baseline: the latest ref/codex request builder. Keep unknown
        // fields lossless; endpoint-specific removals come from authoritative clients or locally
        // captured upstream rejections.
        //
        // `previous_response_id` is intentionally left untouched in this generic dialect pass.
        // CLIProxyAPI's HTTP executor removes it, while its WebSocket v2 executor retains it. Our
        // subscription HTTP executor therefore expands the referenced response from its bounded
        // continuation cache before removing the field. Public Responses API channels never enter
        // that subscription-only repair path.
        // String input is normalized by the single production entry point before continuation
        // recovery. Keep this fallback for compact requests and direct unit coverage.
        normalize_codex_string_input(object, &mut actions);
        for field in ["max_tokens", "max_output_tokens", "max_completion_tokens"] {
            remove_codex_subscription_field(
                object,
                field,
                "codex_token_limit_omitted",
                &mut actions,
            );
        }
        for field in [
            "temperature",
            "top_p",
            "frequency_penalty",
            "presence_penalty",
        ] {
            remove_codex_subscription_field(
                object,
                field,
                "codex_sampling_hint_omitted",
                &mut actions,
            );
        }
        remove_codex_subscription_field(
            object,
            "user",
            "codex_user_identity_omitted",
            &mut actions,
        );
        // parallel_tool_calls is a valid Codex Responses control and may be retained across a
        // continuation turn even when the delta itself does not repeat the tools array.
        // Latest Codex also emits service_tier, stream_options, prompt_cache_key, text, and
        // client_metadata for both API-key and ChatGPT-backed providers; those fields remain
        // native and must not be stripped at this boundary.
        for field in ["prompt_cache_options", "prompt_cache_retention"] {
            remove_codex_subscription_field(
                object,
                field,
                "codex_prompt_cache_control_omitted",
                &mut actions,
            );
        }
        for field in [
            "metadata",
            "truncation",
            "context_management",
            "safety_identifier",
        ] {
            remove_codex_subscription_field(
                object,
                field,
                "codex_internal_extension_omitted",
                &mut actions,
            );
        }
        if !compact && object.get("store").and_then(serde_json::Value::as_bool) != Some(false) {
            let code = if object.contains_key("store") {
                "codex_store_forced_false"
            } else {
                "codex_store_defaulted_false"
            };
            object.insert("store".to_string(), serde_json::Value::Bool(false));
            actions.push(SubscriptionDialectAction {
                code,
                path: "$.store".to_string(),
            });
        }
        let remove_empty_reasoning = object
            .get_mut("reasoning")
            .and_then(serde_json::Value::as_object_mut)
            .is_some_and(|reasoning| {
                if reasoning.remove("max_tokens").is_some() {
                    actions.push(SubscriptionDialectAction {
                        code: "codex_reasoning_token_limit_omitted",
                        path: "$.reasoning.max_tokens".to_string(),
                    });
                }
                reasoning.is_empty()
            });
        if remove_empty_reasoning {
            object.remove("reasoning");
        }
        // store=false makes every HTTP turn self-contained. Ask Codex for the encrypted
        // continuation item even when the caller did not request visible reasoning, otherwise a
        // later tool-result turn can silently lose the state that the model must authenticate.
        if !compact {
            const ENCRYPTED_REASONING: &str = "reasoning.encrypted_content";
            match object.get_mut("include") {
                None => {
                    object.insert(
                        "include".to_string(),
                        serde_json::json!([ENCRYPTED_REASONING]),
                    );
                    actions.push(SubscriptionDialectAction {
                        code: "codex_reasoning_include_added",
                        path: "$.include".to_string(),
                    });
                }
                Some(serde_json::Value::Array(include))
                    if !include
                        .iter()
                        .any(|item| item.as_str() == Some(ENCRYPTED_REASONING)) =>
                {
                    include.push(serde_json::json!(ENCRYPTED_REASONING));
                    actions.push(SubscriptionDialectAction {
                        code: "codex_reasoning_include_added",
                        path: "$.include".to_string(),
                    });
                }
                _ => {}
            }
        }
    }
    strip_codex_subscription_prompt_cache_breakpoints(&mut value, &mut actions);
    strip_output_only_fields_from_responses_input(&mut value, &mut actions);
    shorten_codex_subscription_input_ids(&mut value, &mut actions);
    if normalize_codex_subscription_cache_prefix(&mut value) {
        actions.push(SubscriptionDialectAction {
            code: "codex_cache_prefix_normalized",
            path: "$.input|$.tools".to_string(),
        });
    }
    if normalize_codex_subscription_system_messages(&mut value) {
        actions.push(SubscriptionDialectAction {
            code: "codex_system_role_normalized",
            path: "$.input[*].role".to_string(),
        });
    }
    normalize_codex_subscription_audio(&mut value, "$", &mut actions)?;
    Ok(SubscriptionDialectAdjustment {
        body: serde_json::to_string(&value)?,
        actions,
    })
}

/// Gives a non-Codex caller the stable session identity that the official Codex
/// client sends alongside `prompt_cache_key`. The identifiers are derived from
/// the already opaque cache identity, so growing turns keep one upstream cache
/// domain without exposing a caller-supplied conversation key as a header.
fn ensure_codex_subscription_session_metadata(
    adjustment: &mut SubscriptionDialectAdjustment,
) -> Result<()> {
    let mut value: serde_json::Value = serde_json::from_str(&adjustment.body)?;
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };

    let metadata_seed = object
        .get("client_metadata")
        .and_then(serde_json::Value::as_object)
        .and_then(|metadata| {
            ["session_id", "thread_id"].into_iter().find_map(|key| {
                metadata
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| normalized_subscription_cache_identity(Some(value)))
            })
        })
        .or_else(|| {
            object
                .get("prompt_cache_key")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| normalized_subscription_cache_identity(Some(value)))
        })
        .map(str::to_string);
    let Some(seed) = metadata_seed else {
        return Ok(());
    };

    let metadata = match object.entry("client_metadata".to_string()) {
        serde_json::map::Entry::Vacant(entry) => {
            entry.insert(serde_json::Value::Object(serde_json::Map::new()))
        }
        serde_json::map::Entry::Occupied(entry) => entry.into_mut(),
    };
    let Some(metadata) = metadata.as_object_mut() else {
        return Ok(());
    };
    let mut changed = false;
    for (key, namespace) in [
        ("session_id", b"const-codex-session-v1\0".as_slice()),
        ("thread_id", b"const-codex-thread-v1\0".as_slice()),
    ] {
        let missing = metadata
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_none_or(|value| value.trim().is_empty());
        if missing {
            metadata.insert(
                key.to_string(),
                serde_json::Value::String(stable_codex_subscription_uuid(namespace, &seed)),
            );
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }

    adjustment.actions.push(SubscriptionDialectAction {
        code: "codex_session_identity_derived",
        path: "$.client_metadata".to_string(),
    });
    adjustment.body = serde_json::to_string(&value)?;
    Ok(())
}

fn stable_codex_subscription_uuid(namespace: &[u8], seed: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(namespace);
    digest.update(seed.as_bytes());
    let mut bytes: [u8; 32] = digest.finalize().into();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        hex::encode(&bytes[0..4]),
        hex::encode(&bytes[4..6]),
        hex::encode(&bytes[6..8]),
        hex::encode(&bytes[8..10]),
        hex::encode(&bytes[10..16])
    )
}

/// Adapts a public Responses-shaped request to the model-specific Responses Lite
/// dialect emitted by the official Codex client. This runs only at the final
/// ChatGPT subscription boundary; native Codex requests returned before this point
/// and generic same-protocol forwarding remains byte-stable.
fn apply_codex_responses_lite_dialect(
    adjustment: &mut SubscriptionDialectAdjustment,
) -> Result<()> {
    let mut value: serde_json::Value = serde_json::from_str(&adjustment.body)?;
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    let instructions = match object.remove("instructions") {
        Some(serde_json::Value::String(value)) => value,
        Some(serde_json::Value::Null) | None => String::new(),
        Some(value) => {
            object.insert("instructions".to_string(), value);
            return Ok(());
        }
    };
    let tools = match object.remove("tools") {
        Some(serde_json::Value::Array(tools)) => tools,
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(value) => {
            object.insert(
                "instructions".to_string(),
                serde_json::Value::String(instructions),
            );
            object.insert("tools".to_string(), value);
            return Ok(());
        }
    };
    // Codex 0.153 gives rebuilt Lite prefixes stable, thread-scoped content IDs.
    // Reuse our existing deterministic IDs: no randomness or new state is needed
    // on retries/continuations, and changing a prefix changes only that item's ID.
    let thread_id = object
        .get("client_metadata")
        .and_then(|metadata| metadata.get("thread_id"))
        .or_else(|| object.get("prompt_cache_key"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(input) = object
        .entry("input".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
    else {
        object.insert(
            "instructions".to_string(),
            serde_json::Value::String(instructions),
        );
        object.insert("tools".to_string(), serde_json::Value::Array(tools));
        return Ok(());
    };
    let lite_tools = codex_responses_lite_tools(tools);
    let tools_id = format!(
        "at_{}",
        stable_codex_subscription_uuid(
            b"const-codex-lite-tools-v1\0",
            &serde_json::to_string(&(&thread_id, &lite_tools))?,
        )
    );
    let instructions_id = format!(
        "msg_{}",
        stable_codex_subscription_uuid(
            b"const-codex-lite-instructions-v1\0",
            &serde_json::to_string(&(&thread_id, &instructions))?,
        )
    );

    // Continuation replay stores the exact request input. Remove only the prefix
    // generated by this adapter so a resumed turn receives one fresh prefix, not
    // an accumulating copy on every request.
    strip_generated_codex_responses_lite_prefix(input, &instructions);

    let mut prefix = vec![serde_json::json!({
        "id": tools_id,
        "type": "additional_tools",
        "role": "developer",
        "tools": lite_tools,
    })];
    if !instructions.is_empty() {
        prefix.push(serde_json::json!({
            "id": instructions_id,
            "type": "message",
            "role": "developer",
            "content": [{"type": "input_text", "text": instructions}],
            "internal_chat_message_metadata_passthrough": {
                "content_item_kinds": ["model.base_instructions"]
            }
        }));
    }
    input.splice(0..0, prefix);
    object.insert(
        "instructions".to_string(),
        serde_json::Value::String(String::new()),
    );
    if object
        .get("parallel_tool_calls")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        adjustment.actions.push(SubscriptionDialectAction {
            code: "codex_parallel_tool_calls_disabled_for_lite",
            path: "$.parallel_tool_calls".to_string(),
        });
    }
    object.insert(
        "parallel_tool_calls".to_string(),
        serde_json::Value::Bool(false),
    );
    match object.get_mut("reasoning") {
        Some(serde_json::Value::Object(reasoning)) => {
            reasoning.insert(
                "context".to_string(),
                serde_json::Value::String("all_turns".to_string()),
            );
        }
        Some(serde_json::Value::Null) | None => {
            object.insert(
                "reasoning".to_string(),
                serde_json::json!({"context": "all_turns"}),
            );
        }
        Some(_) => {}
    }
    adjustment.actions.push(SubscriptionDialectAction {
        code: "codex_responses_lite_applied",
        path: "$".to_string(),
    });
    adjustment.body = serde_json::to_string(&value)?;
    Ok(())
}

fn codex_responses_lite_tools(tools: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut output = Vec::new();
    let mut functions = Vec::new();
    let mut functions_description = String::new();
    let mut functions_index = None;
    for tool in tools {
        let kind = tool.get("type").and_then(serde_json::Value::as_str);
        let is_default_namespace = kind == Some("namespace")
            && tool.get("name").and_then(serde_json::Value::as_str) == Some("functions");
        if matches!(kind, Some("function" | "custom")) {
            functions_index.get_or_insert(output.len());
            functions.push(tool);
        } else if is_default_namespace {
            functions_index.get_or_insert(output.len());
            if let Some(description) = tool
                .get("description")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                functions_description = description.to_string();
            }
            functions.extend(
                tool.get("tools")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
        } else {
            output.push(tool);
        }
    }
    if let Some(index) = functions_index.filter(|_| !functions.is_empty()) {
        output.insert(
            index,
            serde_json::json!({
                "type": "namespace",
                "name": "functions",
                "description": functions_description,
                "tools": functions,
            }),
        );
    }
    output
}

fn strip_generated_codex_responses_lite_prefix(
    input: &mut Vec<serde_json::Value>,
    instructions: &str,
) {
    if input
        .first()
        .and_then(|item| item.get("type"))
        .and_then(serde_json::Value::as_str)
        == Some("additional_tools")
    {
        input.remove(0);
    }
    let generated_base_instructions = !instructions.is_empty()
        && input.first().is_some_and(|item| {
            item.get("role").and_then(serde_json::Value::as_str) == Some("developer")
                && item
                    .pointer("/content/0/text")
                    .and_then(serde_json::Value::as_str)
                    == Some(instructions)
        });
    if generated_base_instructions {
        input.remove(0);
    }
}

pub(crate) fn body_uses_codex_responses_lite(body: &serde_json::Value) -> bool {
    body.get("input")
        .and_then(serde_json::Value::as_array)
        .and_then(|input| input.first())
        .and_then(|item| item.get("type"))
        .and_then(serde_json::Value::as_str)
        == Some("additional_tools")
}

fn normalize_codex_string_input(
    object: &mut serde_json::Map<String, serde_json::Value>,
    actions: &mut Vec<SubscriptionDialectAction>,
) {
    let Some(input) = object
        .get("input")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    object.insert(
        "input".to_string(),
        serde_json::json!([{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": input}]
        }]),
    );
    actions.push(SubscriptionDialectAction {
        code: "codex_string_input_normalized",
        path: "$.input".to_string(),
    });
}

fn strip_codex_subscription_prompt_cache_breakpoints(
    value: &mut serde_json::Value,
    actions: &mut Vec<SubscriptionDialectAction>,
) {
    let Some(input) = value
        .get_mut("input")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for (item_index, item) in input.iter_mut().enumerate() {
        let Some(content) = item
            .get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for (block_index, block) in content.iter_mut().enumerate() {
            if block
                .as_object_mut()
                .is_some_and(|block| block.remove("prompt_cache_breakpoint").is_some())
            {
                actions.push(SubscriptionDialectAction {
                    code: "codex_prompt_cache_control_omitted",
                    path: format!(
                        "$.input[{item_index}].content[{block_index}].prompt_cache_breakpoint"
                    ),
                });
            }
        }
    }
}

const MAX_CODEX_AUDIO_INPUT_BYTES: usize = 50 * 1024 * 1024;

fn normalize_codex_subscription_audio(
    value: &mut serde_json::Value,
    path: &str,
    actions: &mut Vec<SubscriptionDialectAction>,
) -> Result<()> {
    match value {
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                normalize_codex_subscription_audio(item, &format!("{path}[{index}]"), actions)?;
            }
        }
        serde_json::Value::Object(object) => {
            if object.get("type").and_then(serde_json::Value::as_str) == Some("input_audio") {
                let original_audio_url = object
                    .get("audio_url")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let canonical = if let Some(audio_url) = original_audio_url.as_deref() {
                    canonical_codex_audio_data_url(audio_url, path)?
                } else {
                    let data = object
                        .get("data")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            anyhow!("Codex input_audio at {path} requires audio_url or data")
                        })?;
                    let format = object
                        .get("format")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("wav");
                    let mime = canonical_codex_audio_mime(format).ok_or_else(|| {
                        anyhow!(
                            "Codex input_audio at {path} uses unsupported format {format}; expected wav, mp3, m4a, webm, or ogg"
                        )
                    })?;
                    validate_codex_audio_base64(data, path)?;
                    format!("data:{mime};base64,{data}")
                };
                let changed = original_audio_url.as_deref() != Some(canonical.as_str())
                    || object.contains_key("data")
                    || object.contains_key("format");
                object.insert(
                    "audio_url".to_string(),
                    serde_json::Value::String(canonical),
                );
                object.remove("data");
                object.remove("format");
                if changed {
                    actions.push(SubscriptionDialectAction {
                        code: "codex_input_audio_normalized",
                        path: format!("{path}.audio_url"),
                    });
                }
                return Ok(());
            }
            let keys = object.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                if let Some(child) = object.get_mut(&key) {
                    normalize_codex_subscription_audio(child, &format!("{path}.{key}"), actions)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn canonical_codex_audio_data_url(audio_url: &str, path: &str) -> Result<String> {
    if audio_url
        .get(.."data:".len())
        .is_none_or(|prefix| !prefix.eq_ignore_ascii_case("data:"))
    {
        return Err(anyhow!(
            "Codex input_audio at {path} requires a base64 data URL"
        ));
    }
    let (metadata, payload) = audio_url
        .split_once(',')
        .ok_or_else(|| anyhow!("Codex input_audio at {path} has an invalid data URL"))?;
    let metadata = metadata
        .get("data:".len()..)
        .ok_or_else(|| anyhow!("Codex input_audio at {path} has an invalid data URL"))?;
    let mut parts = metadata.split(';');
    let mime = parts.next().unwrap_or_default();
    let canonical_mime = canonical_codex_audio_mime(mime).ok_or_else(|| {
        anyhow!(
            "Codex input_audio at {path} uses unsupported media type {mime}; expected wav, mp3, m4a, webm, or ogg"
        )
    })?;
    if !parts.any(|part| part.eq_ignore_ascii_case("base64")) {
        return Err(anyhow!(
            "Codex input_audio at {path} must be base64 encoded"
        ));
    }
    validate_codex_audio_base64(payload, path)?;
    Ok(format!("data:{canonical_mime};base64,{payload}"))
}

fn canonical_codex_audio_mime(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "wav" | "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => Some("audio/wav"),
        "mp3" | "mpeg" | "audio/mp3" | "audio/mpeg" => Some("audio/mpeg"),
        "m4a" | "mp4" | "audio/m4a" | "audio/x-m4a" | "audio/mp4" => Some("audio/mp4"),
        "webm" | "audio/webm" => Some("audio/webm"),
        "ogg" | "audio/ogg" => Some("audio/ogg"),
        _ => None,
    }
}

fn validate_codex_audio_base64(payload: &str, path: &str) -> Result<()> {
    let bytes = payload.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err(anyhow!(
            "Codex input_audio at {path} has invalid base64 padding"
        ));
    }
    let first_padding = bytes.iter().position(|byte| *byte == b'=');
    let payload_end = first_padding.unwrap_or(bytes.len());
    if bytes[..payload_end]
        .iter()
        .any(|byte| !matches!(*byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/'))
        || bytes[payload_end..].iter().any(|byte| *byte != b'=')
        || bytes.len().saturating_sub(payload_end) > 2
    {
        return Err(anyhow!(
            "Codex input_audio at {path} contains invalid base64 data"
        ));
    }
    let decoded_len = bytes.len() / 4 * 3 - bytes.len().saturating_sub(payload_end);
    if decoded_len > MAX_CODEX_AUDIO_INPUT_BYTES {
        return Err(anyhow!(
            "Codex input_audio at {path} exceeds the 50 MiB decoded limit"
        ));
    }
    Ok(())
}

fn shorten_codex_subscription_input_ids(
    value: &mut serde_json::Value,
    actions: &mut Vec<SubscriptionDialectAction>,
) {
    normalize_codex_subscription_input_ids(value, actions, false);
}

fn normalize_codex_subscription_input_ids(
    value: &mut serde_json::Value,
    actions: &mut Vec<SubscriptionDialectAction>,
    complete_messages_only: bool,
) {
    use sha2::{Digest, Sha256};

    // Mirrors Codex's message-ID shape and 64-character input-item limit. Stateful continuation
    // fields and call_id relationships stay untouched.
    const MAX_ID_CHARS: usize = 64;
    const PREFIX_CHARS: usize = 47;
    let Some(input) = value
        .get_mut("input")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    let mut used = input
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(serde_json::Value::as_str)?;
            Some(normalize_codex_subscription_input_id(item, id))
        })
        .filter(|id| id.chars().count() <= MAX_ID_CHARS)
        .collect::<std::collections::HashSet<_>>();

    for (item_index, item) in input.iter_mut().enumerate() {
        if complete_messages_only && !codex_input_value_is_complete_message(item) {
            continue;
        }
        let normalized_id = item
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(|id| normalize_codex_subscription_input_id(item, id));
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        let Some(original) = object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let mut normalized = normalized_id.unwrap_or_else(|| original.clone());
        if normalized != original {
            actions.push(SubscriptionDialectAction {
                code: "codex_message_id_normalized",
                path: format!("$.input[{item_index}].id"),
            });
        }
        if normalized.chars().count() > MAX_ID_CHARS {
            let prefix = normalized.chars().take(PREFIX_CHARS).collect::<String>();
            let mut collision = 0_u32;
            normalized = loop {
                let hash_input = if collision == 0 {
                    normalized.clone()
                } else {
                    format!("{normalized}:{collision}")
                };
                let digest = Sha256::digest(hash_input.as_bytes());
                let candidate = format!("{prefix}_{}", hex::encode(&digest[..8]));
                if used.insert(candidate.clone()) {
                    break candidate;
                }
                collision = collision.saturating_add(1);
            };
            actions.push(SubscriptionDialectAction {
                code: "codex_input_id_shortened",
                path: format!("$.input[{item_index}].id"),
            });
        } else {
            used.insert(normalized.clone());
        }
        if normalized != original {
            object.insert("id".to_string(), serde_json::Value::String(normalized));
        }
    }
}

fn normalize_codex_subscription_input_id(item: &serde_json::Value, id: &str) -> String {
    let is_message = item.get("type").and_then(serde_json::Value::as_str) == Some("message")
        || codex_input_value_is_complete_message(item);
    if !is_message || id.is_empty() || id.starts_with("msg") {
        id.to_string()
    } else {
        format!("msg_{id}")
    }
}

fn codex_input_value_is_complete_message(item: &serde_json::Value) -> bool {
    codex_input_is_complete_message(
        item.get("type").and_then(serde_json::Value::as_str),
        item.get("role").and_then(serde_json::Value::as_str),
        item.get("content")
            .is_some_and(|content| content.is_array() || content.is_string()),
    )
}

fn codex_input_is_complete_message(
    kind: Option<&str>,
    role: Option<&str>,
    has_content: bool,
) -> bool {
    matches!(kind, None | Some("message"))
        && matches!(role, Some("user" | "assistant" | "developer" | "system"))
        && has_content
}

fn remove_codex_subscription_field(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &'static str,
    code: &'static str,
    actions: &mut Vec<SubscriptionDialectAction>,
) {
    if object.remove(field).is_some() {
        actions.push(SubscriptionDialectAction {
            code,
            path: format!("$.{field}"),
        });
    }
}

fn strip_output_only_fields_from_responses_input(
    value: &mut serde_json::Value,
    actions: &mut Vec<SubscriptionDialectAction>,
) {
    let Some(input) = value
        .get_mut("input")
        .and_then(|input| input.as_array_mut())
    else {
        return;
    };
    for (item_index, item) in input.iter_mut().enumerate() {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        if object.get("type").and_then(serde_json::Value::as_str) == Some("reasoning") {
            // Under store=false, replaying an rs_* id can make the internal endpoint look up state
            // it never stored. Preserve encrypted_content, remove the unusable reference, and
            // provide the summary shape required by the endpoint.
            if object.remove("id").is_some() {
                actions.push(SubscriptionDialectAction {
                    code: "codex_reasoning_reference_omitted",
                    path: format!("$.input[{item_index}].id"),
                });
            }
            if object.get("summary").is_none_or(serde_json::Value::is_null) {
                object.insert("summary".to_string(), serde_json::json!([]));
                actions.push(SubscriptionDialectAction {
                    code: "codex_reasoning_summary_defaulted",
                    path: format!("$.input[{item_index}].summary"),
                });
            }
            // SDK response objects may be replayed as input by non-Codex clients. The ChatGPT
            // Codex endpoint rejects output-only reasoning status values and also rejects a
            // non-empty plaintext content array when the replayable encrypted payload is present.
            // Keep the native Codex fast path byte-stable; this normalization only runs for the
            // endpoint-adapted, non-native request path.
            if object.remove("status").is_some() {
                actions.push(SubscriptionDialectAction {
                    code: "codex_reasoning_status_omitted",
                    path: format!("$.input[{item_index}].status"),
                });
            }
            let has_replayable_encrypted_content = object
                .get("encrypted_content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|content| !content.trim().is_empty());
            let has_nonempty_plaintext_content = object
                .get("content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|content| !content.is_empty());
            if has_replayable_encrypted_content
                && has_nonempty_plaintext_content
                && object.remove("content").is_some()
            {
                actions.push(SubscriptionDialectAction {
                    code: "codex_redundant_reasoning_content_omitted",
                    path: format!("$.input[{item_index}].content"),
                });
            }
        }
        if object.get("id").is_some_and(serde_json::Value::is_null) {
            object.remove("id");
            actions.push(SubscriptionDialectAction {
                code: "codex_output_only_field_omitted",
                path: format!("$.input[{item_index}].id"),
            });
        }
        if object.get("status").is_some_and(serde_json::Value::is_null) {
            object.remove("status");
            actions.push(SubscriptionDialectAction {
                code: "codex_output_only_field_omitted",
                path: format!("$.input[{item_index}].status"),
            });
        }
        let Some(content) = object
            .get_mut("content")
            .and_then(|content| content.as_array_mut())
        else {
            continue;
        };
        for (block_index, block) in content.iter_mut().enumerate() {
            if block.get("type").and_then(serde_json::Value::as_str) == Some("input_text") {
                if let Some(block) = block.as_object_mut() {
                    if block.remove("annotations").is_some() {
                        actions.push(SubscriptionDialectAction {
                            code: "codex_output_only_field_omitted",
                            path: format!(
                                "$.input[{item_index}].content[{block_index}].annotations"
                            ),
                        });
                    }
                }
            }
        }
    }
}

/// Gives semantically equivalent public protocol conversions one stable
/// Responses prefix. Native Codex requests return before this endpoint-only
/// normalization, so their wire body remains caller-owned.
fn normalize_codex_subscription_cache_prefix(value: &mut serde_json::Value) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    if let Some(input) = object
        .get_mut("input")
        .and_then(serde_json::Value::as_array_mut)
    {
        for item in input {
            let Some(message) = item.as_object_mut() else {
                continue;
            };
            if !message.contains_key("type")
                && message
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
                && message.contains_key("content")
            {
                message.insert(
                    "type".to_string(),
                    serde_json::Value::String("message".to_string()),
                );
                changed = true;
            }
        }
    }
    if let Some(tools) = object
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut)
    {
        for tool in tools {
            changed |= remove_null_codex_tool_strict(tool);
        }
    }
    changed
}

fn remove_null_codex_tool_strict(tool: &mut serde_json::Value) -> bool {
    let Some(tool) = tool.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    if tool.get("strict").is_some_and(serde_json::Value::is_null) {
        tool.remove("strict");
        changed = true;
    }
    if let Some(nested) = tool
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut)
    {
        for child in nested {
            changed |= remove_null_codex_tool_strict(child);
        }
    }
    changed
}

fn normalize_codex_subscription_system_messages(value: &mut serde_json::Value) -> bool {
    let Some(input) = value
        .get_mut("input")
        .and_then(|input| input.as_array_mut())
    else {
        return false;
    };
    let mut modified = false;
    let mut system_texts = Vec::new();
    input.retain_mut(|item| {
        let Some(object) = item.as_object_mut() else {
            return true;
        };
        if object.get("role").and_then(|role| role.as_str()) != Some("system") {
            return true;
        }
        modified = true;
        let text = chat_message_content_to_text(object.get("content"));
        if !text.trim().is_empty() {
            system_texts.push(text);
        }
        false
    });
    if system_texts.is_empty() {
        return modified;
    }
    let extracted = system_texts.join("\n\n");
    let existing = value
        .get("instructions")
        .and_then(|instructions| instructions.as_str())
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty())
        .map(str::to_string);
    value["instructions"] = serde_json::Value::String(match existing {
        Some(existing) => format!("{extracted}\n\n{existing}"),
        None => extracted,
    });
    true
}

fn chat_message_content_to_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| item.get("content").and_then(serde_json::Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod native_codex_endpoint_tests {
    use super::*;

    #[test]
    fn native_codex_repairs_only_complete_message_ids_and_is_idempotent() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("originator", "codex_cli_rs".parse().unwrap());
        headers.insert("user-agent", "codex_cli_rs/0.153.4".parse().unwrap());
        let mut original = serde_json::json!({
            "model": "gpt-5.6-luna", "store": false, "stream": true,
            "prompt_cache_key": "preserve-cache-identity", "future_extension": {"keep": true},
            "input": [
                {"id":"item_8af9603617104500a810eedd", "role":"assistant", "content":[{"type":"output_text", "text":"hello"}]},
                {"type":"message", "id":"item_explicit", "role":"user", "content":"continue"},
                {"type":"message", "id":"msg_valid", "role":"user", "content":"keep"},
                {"type":"reasoning", "id":"rs_valid", "summary":[], "encrypted_content":"opaque"},
                {"type":"custom_tool_call", "id":"ctc_valid", "call_id":"call_keep", "name":"run", "input":"hello"},
                {"type":"custom_tool_call_output", "call_id":"call_keep", "output":"OK"},
                {"type":"item_reference", "id":"item_reference_only"},
                {"type":"future_item", "id":"item_future", "role":"user", "content":"keep"}
            ]
        });
        let prepare = |body: &str| {
            prepare_codex_subscription_endpoint_request(
                &crate::default_supplier_config(),
                body,
                false,
                "gpt-5.6-luna",
                Some("must-not-replace-cache-key"),
                Some(&headers),
            )
            .unwrap()
        };
        let fixed = prepare(&original.to_string());
        original["input"][0]["id"] = "msg_item_8af9603617104500a810eedd".into();
        original["input"][1]["id"] = "msg_item_explicit".into();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&fixed.body).unwrap(),
            original
        );
        assert_eq!(prepare(&fixed.body).body, fixed.body);
        assert!(fixed.faults.is_empty());
    }

    #[test]
    fn subscription_normalizes_implicit_message_ids_before_prefix_normalization() {
        let body = serde_json::json!({
            "input": [{"id":"item_old", "role":"user", "content":"hello"}],
            "store": false, "stream": true
        })
        .to_string();
        let fixed = adjust_codex_subscription_request(&body).unwrap();
        let value: serde_json::Value = serde_json::from_str(&fixed.body).unwrap();
        assert_eq!(value["input"][0]["id"], "msg_item_old");
        assert!(fixed
            .actions
            .iter()
            .any(|action| action.code == "codex_message_id_normalized"));
        assert_eq!(
            adjust_codex_subscription_request(&fixed.body).unwrap().body,
            fixed.body
        );
    }

    #[test]
    fn native_codex_message_id_repair_composes_with_reasoning_repair() {
        let body = serde_json::json!({
            "store":false, "input":[
                {"id":"item_old", "role":"user", "content":"hello"},
                {"type":"reasoning", "id":"rs_stale", "summary":[]},
                {"type":"reasoning", "id":"rs_ok", "summary":[], "encrypted_content":"opaque"}
            ]
        });
        let repaired = repair_native_codex_history(&body.to_string()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&repaired.body).unwrap();
        assert_eq!(value["input"].as_array().unwrap().len(), 2);
        assert_eq!(value["input"][0]["id"], "msg_item_old");
        assert_eq!(value["input"][1]["encrypted_content"], "opaque");
        assert!(repair_native_codex_history(&repaired.body).is_none());
    }

    #[test]
    fn native_codex_valid_implicit_messages_keep_original_bytes() {
        let body = r#"{ "store":false, "input":[
            {"id":"msg_valid", "role":"user", "content":"hello"},
            {"id":"item_reference_only", "type":"item_reference"},
            {"id":"item_future", "type":"future_item", "content":"keep", "role":"user"}
        ], "prompt_cache_key":"keep" }"#;
        assert!(repair_native_codex_history(body).is_none());
    }

    /// Explicit opt-in only: one model-list fetch and one short generation.
    /// Reads an existing credential without refreshing, saving, or logging it.
    #[tokio::test]
    #[ignore = "requires CONST_API_LIVE_OPENAI_CREDENTIAL; consumes one short subscription request"]
    async fn live_codex_subscription_catalog_and_responses_smoke() -> Result<()> {
        let path = std::env::var("CONST_API_LIVE_OPENAI_CREDENTIAL")?;
        let mut credential: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let token = credential["access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("credential has no access token"))?
            .to_string();
        if credential.get("account_id").is_none() {
            let encoded = token
                .split('.')
                .nth(1)
                .ok_or_else(|| anyhow!("expected JWT access token"))?;
            let claims: serde_json::Value =
                serde_json::from_slice(&crate::detection::decode_jwt_payload(encoded)?)?;
            if let Some(account_id) = claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .and_then(serde_json::Value::as_str)
            {
                credential["account_id"] = account_id.into();
            }
        }
        let client = Client::builder().timeout(Duration::from_secs(60)).build()?;
        let model = std::env::var("CONST_API_LIVE_OPENAI_MODEL")
            .unwrap_or_else(|_| "gpt-6-astra".to_string());
        let observations = crate::detection::fetch_codex_manifest_model_observations_with_resolver(
            &client,
            crate::codex_identity::codex_identity_resolver(),
            crate::CODEX_MODELS_URL,
            &token,
            credential["account_id"].as_str(),
        )
        .await?;
        let observed = observations
            .iter()
            .find(|entry| entry.id == model)
            .ok_or_else(|| anyhow!("requested model is absent from the live catalog"))?;
        let prepared = prepare_codex_subscription_endpoint_request_with_lite_override(
            &crate::default_supplier_config(),
            &serde_json::json!({
                "model": model, "instructions": "Reply with OK only.",
                "input": [{"role": "user", "content": "Reply OK."}],
                "reasoning": {"effort": "low"}, "tools": [], "store": false, "stream": true
            })
            .to_string(),
            false,
            &model,
            Some("codex-version-smoke"),
            None,
            observed.use_responses_lite,
        )?;
        let response = openai_subscription_response_request(
            &client,
            crate::CODEX_RESPONSES_URL,
            &token,
            &credential,
            prepared.body,
            None,
        )?
        .send()
        .await?;
        let status = response.status();
        anyhow::ensure!(status.is_success(), "subscription returned HTTP {status}");
        let body = response.text().await?;
        let completed = body
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
            .find(|event| event["type"] == "response.completed")
            .ok_or_else(|| anyhow!("subscription stream did not complete"))?;
        // Subscription streams need not repeat streamed items in completed.output.
        // Exercise the production accumulator instead of inspecting only that frame.
        let mut parser = crate::protocol::stream::StreamParser::new(
            crate::protocol::kind::ProtocolKind::OpenAiResponses,
            &model,
        );
        let mut events = parser.push(body.as_bytes())?;
        events.extend(parser.finish()?);
        let mut accumulator = crate::protocol::stream::CanonicalAccumulator::new();
        for event in &events {
            accumulator.push(event)?;
        }
        let accumulated = accumulator.finish()?;
        let output = accumulated
            .blocks
            .iter()
            .filter_map(|block| match block {
                crate::protocol::ir::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<String>();
        anyhow::ensure!(
            output.to_ascii_uppercase().contains("OK"),
            "unexpected smoke response"
        );
        println!(
            "version={} model={} lite={:?} output_tokens={}",
            crate::codex_identity::BUNDLED_CODEX_CLIENT_VERSION,
            model,
            observed.use_responses_lite,
            completed["response"]["usage"]["output_tokens"]
        );
        Ok(())
    }

    #[test]
    fn native_codex_body_skips_subscription_dialect_rebuild() {
        let body = r#"{
  "model": "gpt-5.6-sol",
  "input": [{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}],
  "store": false,
  "stream": true,
  "include": ["reasoning.encrypted_content"],
  "prompt_cache_key": "native-session"
}"#;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("originator", "codex_cli_rs".parse().unwrap());
        headers.insert(
            reqwest::header::USER_AGENT,
            "codex_cli_rs/0.149.1 (Windows 11; x86_64) vscode"
                .parse()
                .unwrap(),
        );

        let prepared = prepare_codex_subscription_endpoint_request(
            &crate::default_supplier_config(),
            body,
            false,
            "gpt-5.6-sol",
            Some("replacement-must-not-be-injected"),
            Some(&headers),
        )
        .expect("native Codex body");

        assert_eq!(prepared.body, body);
        assert!(prepared.faults.is_empty());
    }

    #[test]
    fn non_native_reasoning_replay_omits_only_fields_rejected_by_codex() {
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": [
                {
                    "type": "reasoning",
                    "status": "completed",
                    "summary": [],
                    "encrypted_content": "opaque-state",
                    "content": [{"type": "reasoning_text", "text": "redundant"}],
                    "future_extension": {"keep": true}
                },
                {
                    "type": "reasoning",
                    "status": "completed",
                    "summary": [],
                    "encrypted_content": " ",
                    "content": [{"type": "reasoning_text", "text": "only-copy"}]
                },
                {
                    "type": "message",
                    "status": "completed",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "continue"}]
                }
            ],
            "store": false
        })
        .to_string();

        let adjustment = adjust_codex_subscription_request(&body).expect("adjusted request");
        let adjusted: serde_json::Value =
            serde_json::from_str(&adjustment.body).expect("adjusted JSON");

        assert!(adjusted["input"][0].get("status").is_none());
        assert!(adjusted["input"][0].get("content").is_none());
        assert_eq!(adjusted["input"][0]["encrypted_content"], "opaque-state");
        assert_eq!(adjusted["input"][0]["future_extension"]["keep"], true);
        assert!(adjusted["input"][1].get("status").is_none());
        assert!(adjusted["input"][1].get("content").is_some());
        assert_eq!(adjusted["input"][2]["status"], "completed");
        assert!(adjustment.actions.iter().any(|action| {
            action.code == "codex_reasoning_status_omitted" && action.path == "$.input[0].status"
        }));
        assert!(adjustment.actions.iter().any(|action| {
            action.code == "codex_redundant_reasoning_content_omitted"
                && action.path == "$.input[0].content"
        }));
    }

    #[test]
    fn native_codex_reasoning_replay_remains_byte_stable() {
        let body = r#"{
  "model": "gpt-5.6-sol",
  "input": [{"type":"reasoning","status":"completed","summary":[],"encrypted_content":"opaque-state","content":[{"type":"reasoning_text","text":"native"}]}],
  "store": false,
  "stream": true
}"#;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("originator", "codex_cli_rs".parse().unwrap());
        headers.insert(
            reqwest::header::USER_AGENT,
            "codex_cli_rs/0.153.4 (Windows 11; x86_64) vscode"
                .parse()
                .unwrap(),
        );

        let prepared = prepare_codex_subscription_endpoint_request(
            &crate::default_supplier_config(),
            body,
            false,
            "gpt-5.6-sol",
            None,
            Some(&headers),
        )
        .expect("native Codex body");

        assert_eq!(prepared.body, body);
        assert!(prepared.faults.is_empty());
    }

    #[test]
    fn native_codex_omits_only_unreplayable_stateless_reasoning_items() {
        let body = serde_json::json!({
            "model": "gpt-5.6-terra",
            "input": [
                {
                    "id": "msg_1",
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "continue"}]
                },
                {
                    "id": "rs_stale",
                    "type": "reasoning",
                    "summary": []
                },
                {
                    "id": "rs_replayable",
                    "type": "reasoning",
                    "summary": [],
                    "encrypted_content": "opaque"
                }
            ],
            "store": false,
            "stream": true
        })
        .to_string();
        assert!(native_codex_stateless_reasoning_may_need_repair(&body));
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("originator", "codex_cli_rs".parse().unwrap());
        headers.insert(
            reqwest::header::USER_AGENT,
            "codex_cli_rs/0.150.1 (Windows 11; x86_64) vscode"
                .parse()
                .unwrap(),
        );

        let prepared = prepare_codex_subscription_endpoint_request(
            &crate::default_supplier_config(),
            &body,
            false,
            "gpt-5.6-terra",
            None,
            Some(&headers),
        )
        .expect("repair stale native reasoning");
        let prepared_body: serde_json::Value =
            serde_json::from_str(&prepared.body).expect("prepared body");
        let input = prepared_body["input"].as_array().expect("input array");

        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["id"], "msg_1");
        assert_eq!(input[1]["id"], "rs_replayable");
        assert_eq!(input[1]["encrypted_content"], "opaque");
        assert_eq!(
            prepared_body["include"],
            serde_json::json!(["reasoning.encrypted_content"])
        );
        assert!(prepared.faults.iter().any(|fault| {
            fault.code == "codex_unreplayable_reasoning_item_omitted"
                && fault.field_path == "$.input[1]"
        }));
    }

    #[test]
    fn native_codex_with_replayable_reasoning_remains_byte_stable() {
        let body = r#"{
  "model": "gpt-5.6-terra",
  "input": [{"id":"rs_valid","type":"reasoning","summary":[],"encrypted_content":"opaque"}],
  "store": false,
  "stream": true,
  "include": ["reasoning.encrypted_content"]
}"#;
        assert!(!native_codex_stateless_reasoning_may_need_repair(body));
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("originator", "codex_cli_rs".parse().unwrap());
        headers.insert(
            reqwest::header::USER_AGENT,
            "codex_cli_rs/0.150.1 (Windows 11; x86_64) vscode"
                .parse()
                .unwrap(),
        );

        let prepared = prepare_codex_subscription_endpoint_request(
            &crate::default_supplier_config(),
            body,
            false,
            "gpt-5.6-terra",
            None,
            Some(&headers),
        )
        .expect("preserve valid native reasoning");

        assert_eq!(prepared.body, body);
        assert!(prepared.faults.is_empty());
    }

    #[test]
    fn non_native_lite_model_uses_the_official_codex_prefix_shape() {
        let body = serde_json::json!({
            "model": "gpt-5.6-luna",
            "instructions": "stable instructions",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }],
            "tools": [
                {
                    "type": "function",
                    "name": "lookup",
                    "description": "look up a value",
                    "parameters": {"type": "object", "properties": {}}
                },
                {"type": "web_search"}
            ],
            "parallel_tool_calls": true,
            "prompt_cache_key": "stable-session",
            "store": false,
            "stream": true
        })
        .to_string();

        let prepared = prepare_codex_subscription_endpoint_request(
            &crate::default_supplier_config(),
            &body,
            false,
            "gpt-5.6-luna",
            Some("must-not-replace-caller-key"),
            None,
        )
        .expect("Responses Lite request");
        let value: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();

        assert_eq!(value["instructions"], "");
        assert!(value.get("tools").is_none());
        assert_eq!(value["input"][0]["type"], "additional_tools");
        assert_eq!(value["input"][0]["tools"][0]["type"], "namespace");
        assert_eq!(value["input"][0]["tools"][0]["name"], "functions");
        assert_eq!(value["input"][0]["tools"][0]["tools"][0]["name"], "lookup");
        assert_eq!(value["input"][0]["tools"][1]["type"], "web_search");
        assert_eq!(value["input"][1]["role"], "developer");
        assert_eq!(
            value["input"][1]["content"][0]["text"],
            "stable instructions"
        );
        assert_eq!(
            value["input"][1]["internal_chat_message_metadata_passthrough"]["content_item_kinds"],
            serde_json::json!(["model.base_instructions"])
        );
        assert_eq!(value["input"][2]["role"], "user");
        assert_eq!(value["parallel_tool_calls"], false);
        assert_eq!(value["reasoning"]["context"], "all_turns");
        assert_eq!(value["prompt_cache_key"], "stable-session");
        let session_id = value["client_metadata"]["session_id"].as_str().unwrap();
        let thread_id = value["client_metadata"]["thread_id"].as_str().unwrap();
        assert_eq!(session_id.len(), 36);
        assert_eq!(thread_id.len(), 36);
        assert_ne!(session_id, "stable-session");
        assert_ne!(session_id, thread_id);
        assert!(body_uses_codex_responses_lite(&value));
        assert!(prepared
            .faults
            .iter()
            .any(|fault| { fault.code == "codex_parallel_tool_calls_disabled_for_lite" }));
    }

    #[test]
    fn explicit_standard_retry_overrides_a_lite_model_hint() {
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "instructions": "stable instructions",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "hello"}]
            }],
            "tools": [{"type": "function", "name": "lookup", "parameters": {}}],
            "store": false,
            "stream": true
        })
        .to_string();

        let prepared = prepare_codex_subscription_endpoint_request_with_lite_override(
            &crate::default_supplier_config(),
            &body,
            false,
            "gpt-5.6-sol",
            None,
            None,
            Some(false),
        )
        .expect("standard dialect retry");
        let value: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();

        assert!(!body_uses_codex_responses_lite(&value));
        assert_eq!(value["instructions"], "stable instructions");
        assert!(value["tools"].is_array());
    }

    #[test]
    fn lite_prefix_ids_are_stable_content_and_thread_scoped() {
        let prepare = |request: &serde_json::Value| -> serde_json::Value {
            let prepared = prepare_codex_subscription_endpoint_request_with_lite_override(
                &crate::default_supplier_config(),
                &request.to_string(),
                false,
                "gpt-6-astra",
                None,
                None,
                Some(true),
            )
            .unwrap();
            serde_json::from_str(&prepared.body).unwrap()
        };
        let request = serde_json::json!({
            "model": "gpt-6-astra",
            "instructions": "stable instructions",
            "tools": [{"type": "function", "name": "lookup", "parameters": {}}],
            "input": [{"role": "user", "content": "hello"}],
            "prompt_cache_key": "stable-session"
        });
        let first = prepare(&request);
        let tools_id = &first["input"][0]["id"];
        let instructions_id = &first["input"][1]["id"];
        assert!(tools_id.as_str().unwrap().starts_with("at_"));
        assert!(instructions_id.as_str().unwrap().starts_with("msg_"));
        assert_ne!(tools_id, instructions_id);
        assert_eq!(prepare(&request), first);

        let mut followup = request.clone();
        // Include the previously generated prefix as continuation replay does.
        followup["input"] = first["input"].clone();
        followup["input"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"role": "user", "content": "continue"}));
        let next = prepare(&followup);
        assert_eq!(&next["input"][0]["id"], tools_id);
        assert_eq!(&next["input"][1]["id"], instructions_id);
        assert_eq!(next["input"].as_array().unwrap().len(), 4);

        let mut changed_tools = request.clone();
        changed_tools["tools"][0]["name"] = "lookup_other".into();
        let changed_tools = prepare(&changed_tools);
        assert_ne!(&changed_tools["input"][0]["id"], tools_id);
        assert_eq!(&changed_tools["input"][1]["id"], instructions_id);

        let mut changed_instructions = request.clone();
        changed_instructions["instructions"] = "different instructions".into();
        let changed_instructions = prepare(&changed_instructions);
        assert_eq!(&changed_instructions["input"][0]["id"], tools_id);
        assert_ne!(&changed_instructions["input"][1]["id"], instructions_id);

        let mut other_thread = request;
        other_thread["prompt_cache_key"] = "other-session".into();
        let other_thread = prepare(&other_thread);
        assert_ne!(&other_thread["input"][0]["id"], tools_id);
        assert_ne!(&other_thread["input"][1]["id"], instructions_id);
    }

    #[test]
    fn native_codex_new_fields_and_lite_ids_remain_byte_stable() {
        let body = r#"{ "model":"gpt-6-astra", "store":false,
            "input":[{"type":"additional_tools","id":"at_native","role":"developer","tools":[]}],
            "reasoning":{"effort":"disabled","context":"all_turns"},
            "access_programs":{"cyber":"standard"}, "future_extension":{"value":1} }"#;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("originator", "codex_cli_rs".parse().unwrap());
        headers.insert(
            "user-agent",
            "codex_cli_rs/0.153.4 (Mac OS; arm64) terminal"
                .parse()
                .unwrap(),
        );
        let prepared = prepare_codex_subscription_endpoint_request(
            &crate::default_supplier_config(),
            body,
            false,
            "gpt-6-astra",
            None,
            Some(&headers),
        )
        .unwrap();
        assert_eq!(prepared.body, body);
        assert!(prepared.faults.is_empty());
    }

    #[test]
    fn non_lite_model_keeps_standard_responses_tools() {
        let body = serde_json::json!({
            "model": "gpt-5.4",
            "input": [{"type": "message", "role": "user", "content": "hello"}],
            "tools": [{"type": "function", "name": "lookup", "parameters": {}}],
            "store": false,
            "stream": true
        })
        .to_string();

        let prepared = prepare_codex_subscription_endpoint_request(
            &crate::default_supplier_config(),
            &body,
            false,
            "gpt-5.4",
            None,
            None,
        )
        .expect("standard Responses request");
        let value: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();

        assert!(value.get("tools").is_some());
        assert!(!body_uses_codex_responses_lite(&value));
        assert!(value["client_metadata"]["session_id"].is_string());
        assert!(value["client_metadata"]["thread_id"].is_string());
    }

    #[test]
    fn derived_codex_session_identity_is_stable_and_preserves_caller_metadata() {
        let request = |input: serde_json::Value| {
            prepare_codex_subscription_endpoint_request(
                &crate::default_supplier_config(),
                &serde_json::json!({
                    "model": "gpt-5.6-luna",
                    "input": input,
                    "prompt_cache_key": "c1_opaque_conversation",
                    "client_metadata": {"x-codex-window-id": "caller-window"},
                    "stream": true
                })
                .to_string(),
                false,
                "gpt-5.6-luna",
                None,
                None,
            )
            .unwrap()
        };
        let first = request(serde_json::json!([{"role": "user", "content": "first"}]));
        let grown = request(serde_json::json!([
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "answer"},
            {"role": "user", "content": "follow up"}
        ]));
        let first: serde_json::Value = serde_json::from_str(&first.body).unwrap();
        let grown: serde_json::Value = serde_json::from_str(&grown.body).unwrap();

        assert_eq!(
            first["client_metadata"]["session_id"],
            grown["client_metadata"]["session_id"]
        );
        assert_eq!(
            first["client_metadata"]["thread_id"],
            grown["client_metadata"]["thread_id"]
        );
        assert_eq!(
            first["client_metadata"]["x-codex-window-id"],
            "caller-window"
        );

        let caller = prepare_codex_subscription_endpoint_request(
            &crate::default_supplier_config(),
            &serde_json::json!({
                "model": "gpt-5.6-luna",
                "input": "hello",
                "prompt_cache_key": "cache-key",
                "client_metadata": {
                    "session_id": "caller-session",
                    "thread_id": "caller-thread"
                }
            })
            .to_string(),
            false,
            "gpt-5.6-luna",
            None,
            None,
        )
        .unwrap();
        let caller: serde_json::Value = serde_json::from_str(&caller.body).unwrap();
        assert_eq!(caller["client_metadata"]["session_id"], "caller-session");
        assert_eq!(caller["client_metadata"]["thread_id"], "caller-thread");
    }

    #[test]
    fn extracted_system_instruction_is_not_sent_twice() {
        let body = serde_json::json!({
            "model": "gpt-5.6-luna",
            "input": [
                {"type": "message", "role": "system", "content": [{"type": "input_text", "text": "system rules"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hello"}]}
            ],
            "store": false,
            "stream": true
        })
        .to_string();

        let prepared = prepare_codex_subscription_endpoint_request(
            &crate::default_supplier_config(),
            &body,
            false,
            "gpt-5.6-luna",
            None,
            None,
        )
        .expect("system extraction");
        let value: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();
        let input = value["input"].as_array().unwrap();

        assert_eq!(input[1]["content"][0]["text"], "system rules");
        assert_eq!(
            input
                .iter()
                .filter(|item| item["role"] == "developer")
                .count(),
            2
        );
        assert_eq!(input[2]["role"], "user");
        assert_eq!(prepared.body.matches("system rules").count(), 1);
    }
}
