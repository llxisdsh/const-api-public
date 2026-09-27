    #[test]
    fn sse_event_boundary_accepts_lf_and_crlf_separators() {
        assert_eq!(find_sse_event_boundary("data: one\n\nrest"), Some((9, 2)));
        assert_eq!(
            find_sse_event_boundary("data: one\r\n\r\nrest"),
            Some((9, 4))
        );
        assert_eq!(find_sse_event_boundary("data: one\r\n"), None);
    }

    #[test]
    fn streaming_converters_accept_crlf_sse_boundaries() {
        let mut responses = crate::protocol::stream::stream_converter(
            crate::protocol::kind::ProtocolKind::OpenAiResponses,
            crate::protocol::kind::ProtocolKind::OpenAiChat,
            "code-cheap",
        );
        let chat = responses
            .push(b"event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\r\n\r\n");
        assert!(String::from_utf8_lossy(&chat).contains("\"content\":\"hello\""));

        let mut chat_converter = crate::protocol::stream::stream_converter(
            crate::protocol::kind::ProtocolKind::OpenAiChat,
            crate::protocol::kind::ProtocolKind::OpenAiResponses,
            "code-cheap",
        );
        let responses = chat_converter.push(
            b"data: {\"id\":\"chatcmpl_1\",\"model\":\"code-cheap\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\r\n\r\n",
        );
        let responses = String::from_utf8_lossy(&responses);
        assert!(responses.contains("event: response.output_text.delta"));
        assert!(responses.contains("\"delta\":\"hi\""));

        let mut gemini =
            AntigravitySseConverter::with_canonical_model("gemini-3.6-flash");
        let gemini_native = gemini.push(
            "data: {\"response\":{\"modelVersion\":\"gemini-3.6-flash-medium\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"gemini\",\"futurePart\":{\"depth\":2}}]}}],\"futureTop\":{\"keep\":true}}}\r\n\r\n",
        );
        assert!(gemini_native.contains("\"modelVersion\":\"gemini-3.6-flash\""));
        assert!(!gemini_native.contains("gemini-3.6-flash-medium"));
        assert!(gemini_native.contains("\"text\":\"gemini\""));
        assert!(gemini_native.contains("\"futurePart\":{\"depth\":2}"));
        assert!(gemini_native.contains("\"futureTop\":{\"keep\":true}"));
    }

    #[test]
    fn chat_stream_tool_calls_convert_to_responses_events() {
        let raw = concat!(
            "data: {\"id\":\"chatcmpl_tool\",\"model\":\"code-cheap\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl_tool\",\"model\":\"code-cheap\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"README.md\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl_tool\",\"model\":\"code-cheap\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let converted = chat_sse_to_responses_sse(raw, "code-cheap");

        assert!(converted.contains("response.output_item.added"));
        assert!(converted.contains("response.function_call_arguments.delta"));
        assert!(converted.contains("\"name\":\"read_file\""));
        assert!(converted.contains("\\\"README.md\\\""));
        assert!(converted.contains("response.completed"));
    }

    #[test]
    fn responses_stream_function_call_converts_to_chat_tool_calls() {
        let raw = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\"{\\\"path\\\":\\\"README.md\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\"}\n\n"
        );
        let converted = responses_sse_to_chat_sse(raw, "code-cheap");

        assert!(converted.contains("\"tool_calls\""));
        assert!(converted.contains("\"name\":\"read_file\""));
        assert!(converted.contains("\\\"README.md\\\""));
        assert!(converted.contains("data: [DONE]"));
    }

    #[test]
    fn responses_stream_output_item_done_does_not_restart_chat_tool_call() {
        let raw = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\"{}\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"{}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"{}\"}]}}\n\n"
        );
        let converted = responses_sse_to_chat_sse(raw, "code-cheap");

        assert_eq!(
            converted.matches("\"name\":\"read_file\"").count(),
            1,
            "{converted}"
        );
        assert_eq!(
            converted.matches("\"id\":\"call_1\"").count(),
            1,
            "{converted}"
        );
        assert_eq!(
            converted.matches("\"arguments\":\"{}\"").count(),
            1,
            "{converted}"
        );
        assert!(
            converted.contains("\"finish_reason\":\"tool_calls\""),
            "{converted}"
        );
        assert!(converted.contains("data: [DONE]"), "{converted}");
    }

    #[test]
    fn responses_stream_completed_output_converts_to_chat_tool_calls() {
        let raw = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}]}}\n\n"
        );
        let converted = responses_sse_to_chat_body(raw, "code-cheap").unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"README.md\"}"
        );
    }

    #[test]
    fn responses_stream_completed_output_converts_to_chat_sse_tool_calls() {
        let raw = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"status\":\"in_progress\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}]}}\n\n"
        );
        let converted = responses_sse_to_chat_sse(raw, "code-cheap");

        assert!(converted.contains("\"tool_calls\""));
        assert!(converted.contains("\"name\":\"read_file\""));
        assert!(converted.contains("\\\"README.md\\\""));
        assert!(converted.contains("\"finish_reason\":\"tool_calls\""));
        assert!(converted.contains("data: [DONE]"));
    }

    #[test]
    fn responses_completed_output_text_and_multiple_tools_convert_to_chat_body() {
        let raw = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[",
            "{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"need tools\"}]},",
            "{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"},",
            "{\"id\":\"fc_2\",\"type\":\"function_call\",\"call_id\":\"call_2\",\"name\":\"list_dir\",\"arguments\":\"{\\\"path\\\":\\\"docs\\\"}\"}",
            "]}}\n\n"
        );
        let converted = responses_sse_to_chat_body(raw, "code-cheap").unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();
        let tool_calls = value["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();

        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(value["choices"][0]["message"]["content"], "need tools");
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0]["function"]["name"], "read_file");
        assert_eq!(tool_calls[1]["function"]["name"], "list_dir");
    }

    #[test]
    fn responses_completed_output_sse_tool_indexes_are_contiguous() {
        let raw = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[",
            "{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"need tools\"}]},",
            "{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"},",
            "{\"id\":\"fc_2\",\"type\":\"function_call\",\"call_id\":\"call_2\",\"name\":\"list_dir\",\"arguments\":\"{\\\"path\\\":\\\"docs\\\"}\"}",
            "]}}\n\n"
        );
        let converted = responses_sse_to_chat_sse(raw, "code-cheap");

        assert!(converted.contains("\"index\":0"));
        assert!(converted.contains("\"index\":1"));
        assert!(!converted.contains("\"index\":2"));
        assert!(converted.contains("\"finish_reason\":\"tool_calls\""));
    }

    #[test]
    fn responses_completed_output_text_converts_to_chat_sse_delta() {
        let raw = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[",
            "{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello from completed\"}]}",
            "]}}\n\n"
        );
        let converted = responses_sse_to_chat_sse(raw, "code-cheap");

        assert!(converted.contains("\"content\":\"hello from completed\""));
        assert!(converted.contains("\"finish_reason\":\"stop\""));
        assert!(converted.contains("data: [DONE]"));
    }

    #[test]
    fn responses_to_chat_omits_unmapped_encrypted_reasoning_without_leaking_it() {
        let body = serde_json::json!({
            "model": "gpt-5.5",
            "input": [
                {
                    "type": "reasoning",
                    "encrypted_content": "opaque-secret"
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hello"}]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "visible"}],
                    "reasoning": {"encrypted_content": "nested-secret"}
                }
            ],
            "reasoning": {"effort": "high", "encrypted_content": "top-secret"}
        })
        .to_string();

        let converted = responses_request_to_chat_body(&body, "gpt-up")
            .expect("supplemental encrypted reasoning should be safely omitted");
        assert!(converted.contains("visible"));
        assert!(!converted.contains("opaque-secret"));
        assert!(!converted.contains("nested-secret"));
        assert!(!converted.contains("top-secret"));
    }

    #[test]
    fn chat_to_responses_maps_reasoning_effort() {
        let body = serde_json::json!({
            "model": "gpt-5.5",
            "messages": [{"role": "user", "content": "hello"}],
            "reasoning_effort": "low"
        })
        .to_string();

        let converted = chat_body_to_responses_body(&body, "gpt-up").expect("convert");
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(value["model"], serde_json::json!("gpt-up"));
        assert_eq!(value["reasoning"]["effort"], serde_json::json!("low"));
        assert!(value.get("reasoning_effort").is_none());
    }

    #[test]
    fn chat_to_responses_keeps_effort_but_omits_unsigned_reasoning_history() {
        for effort in ["low", "medium", "high", "max"] {
            let body = serde_json::json!({
                "model": "gpt-5.6-luna",
                "messages": [
                    {
                        "role": "assistant",
                        "content": "visible answer",
                        "reasoning_content": "unsigned hidden reasoning"
                    },
                    {"role": "user", "content": "continue"}
                ],
                "reasoning_effort": effort
            })
            .to_string();

            let conversion = upstream_request_for_api_format_with_profiles_report(
                "openai_responses",
                "/v1/chat/completions",
                &body,
                "gpt-5.6-luna",
                &[],
            )
            .unwrap_or_else(|error| panic!("{effort}: {error}"));
            let converted: serde_json::Value =
                serde_json::from_str(&conversion.body).expect("Responses JSON");

            assert_eq!(converted["reasoning"]["effort"], effort);
            assert!(converted["input"]
                .as_array()
                .expect("input")
                .iter()
                .all(|item| item["type"] != "reasoning"));
            assert!(converted.to_string().contains("visible answer"));
            assert!(!converted.to_string().contains("unsigned hidden reasoning"));
            assert!(conversion.faults.iter().any(|fault| {
                serde_json::to_value(fault)
                    .ok()
                    .and_then(|value| value.get("code").cloned())
                    == Some(serde_json::json!("reasoning_history_omitted"))
            }));
        }
    }

    #[test]
    fn same_protocol_chat_omits_unsigned_history_without_touching_effort() {
        for effort in [None, Some("medium")] {
            let mut body = serde_json::json!({
                "model": "gpt-5.6-luna",
                "messages": [
                    {
                        "role": "assistant",
                        "content": "visible answer",
                        "reasoning_content": "unsigned hidden reasoning"
                    },
                    {"role": "user", "content": "continue"}
                ]
            });
            if let Some(effort) = effort {
                body["reasoning_effort"] = serde_json::json!(effort);
            }

            let conversion = upstream_request_for_api_format_with_profiles_report(
                "openai_chat",
                "/v1/chat/completions",
                &body.to_string(),
                "gpt-5.6-luna",
                &[],
            )
            .expect("same-protocol Chat request");
            let converted: serde_json::Value =
                serde_json::from_str(&conversion.body).expect("Chat JSON");

            assert_eq!(converted["messages"][0]["content"], "visible answer");
            assert!(converted["messages"][0].get("reasoning_content").is_none());
            match effort {
                Some(effort) => assert_eq!(converted["reasoning_effort"], effort),
                None => assert!(converted.get("reasoning_effort").is_none()),
            }
            assert_eq!(conversion.faults.len(), 1);
        }
    }

    #[test]
    fn same_protocol_route_never_forwards_const_continuation_carrier_upstream() {
        let artifact = crate::protocol::continuation::stream_artifact(
            crate::protocol::ir::ArtifactKind::GeminiThoughtSignature,
            serde_json::json!("gemini-state"),
            crate::protocol::ir::ContentBlockKind::ToolCall,
            Some("gemini-3.6-flash"),
        );
        let carrier = crate::protocol::continuation::encode_carrier(
            crate::protocol::continuation::tool_call_entries("call-state", &[artifact]),
        )
        .unwrap();
        let body = serde_json::json!({
            "model":"client-model",
            "input":[
                {"type":"reasoning","status":"completed","summary":[],"encrypted_content":carrier},
                {"type":"function_call","call_id":"call-state","name":"read_file","arguments":"{}"},
                {"type":"function_call_output","call_id":"call-state","output":"ok"}
            ]
        });
        let conversion = upstream_request_for_api_format_with_profiles_report(
            "openai_responses",
            "/v1/responses",
            &body.to_string(),
            "gpt-target",
            &[],
        )
        .unwrap();
        assert!(!conversion.body.contains("const-api-continuation-v1:"));
        assert!(conversion.body.contains("call-state"));
    }

    #[test]
    fn codex_subscription_folds_responses_system_input_into_instructions_once() {
        let body = serde_json::json!({
            "model": "gpt-5.5",
            "input": [
                {
                    "type": "message",
                    "role": "system",
                    "content": [{"type": "input_text", "text": "system guidance"}]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hi"}]
                }
            ],
            "instructions": "existing instructions",
            "max_output_tokens": 64000
        })
        .to_string();

        let converted = strip_codex_subscription_unsupported_params(&body).unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(value["input"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["input"][0]["role"], "user");
        assert_eq!(
            value["instructions"],
            "system guidance\n\nexisting instructions"
        );
        assert!(!value["input"].to_string().contains("system guidance"));
        assert!(value.get("max_output_tokens").is_none());
    }

    #[test]
    fn codex_subscription_removes_unsupported_reasoning_token_budget() {
        let body = serde_json::json!({
            "model": "gpt-5.6-terra",
            "input": "hello",
            "reasoning": {"max_tokens": 8192, "effort": "high"}
        })
        .to_string();

        let converted = strip_codex_subscription_unsupported_params(&body).unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(value["reasoning"]["effort"], "high");
        assert!(value["reasoning"].get("max_tokens").is_none());

        let budget_only = serde_json::json!({
            "model": "gpt-5.6-terra",
            "input": "hello",
            "reasoning": {"max_tokens": 8192}
        })
        .to_string();
        let converted = strip_codex_subscription_unsupported_params(&budget_only).unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();
        assert!(value.get("reasoning").is_none());
    }

    #[test]
    fn codex_subscription_preserves_cache_identity_and_omits_explicit_controls() {
        let body = serde_json::json!({
            "model": "gpt-5.6-luna",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "stable cache prefix",
                    "prompt_cache_breakpoint": {"mode": "explicit"}
                }]
            }],
            "prompt_cache_key": "cache-session-1",
            "prompt_cache_options": {"mode": "explicit"},
            "prompt_cache_retention": "24h",
            "client_metadata": {
                "session_id": "cache-session-1",
                "thread_id": "cache-thread-1"
            },
            "store": false
        })
        .to_string();

        let adjustment = adjust_codex_subscription_request(&body).unwrap();
        let value: serde_json::Value = serde_json::from_str(&adjustment.body).unwrap();

        assert_eq!(value["prompt_cache_key"], "cache-session-1");
        assert_eq!(value["client_metadata"]["session_id"], "cache-session-1");
        assert_eq!(value["client_metadata"]["thread_id"], "cache-thread-1");
        assert!(value.get("prompt_cache_options").is_none());
        assert!(value.get("prompt_cache_retention").is_none());
        assert!(value["input"][0]["content"][0]
            .get("prompt_cache_breakpoint")
            .is_none());
        let action_paths = adjustment
            .actions
            .iter()
            .filter(|action| action.code == "codex_prompt_cache_control_omitted")
            .map(|action| action.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            action_paths,
            [
                "$.prompt_cache_options",
                "$.prompt_cache_retention",
                "$.input[0].content[0].prompt_cache_breakpoint"
            ]
        );
    }

    #[test]
    fn local_route_affinity_reuses_explicit_and_derived_sessions_across_protocols() {
        let mut headers = warp::http::HeaderMap::new();
        headers.insert(
            "x-conversation-id",
            warp::http::HeaderValue::from_static("shared-conversation"),
        );
        let chat = local_model_route_affinity_key(
            "/v1/chat/completions",
            &headers,
            br#"{"model":"shared-model","messages":[{"role":"user","content":"first"}]}"#,
        );
        let responses = local_model_route_affinity_key(
            "/v1/responses",
            &headers,
            br#"{"model":"shared-model","input":"first"}"#,
        );
        assert_eq!(chat, responses);

        let no_headers = warp::http::HeaderMap::new();
		let derived_chat = local_model_route_affinity_key(
			"/v1/chat/completions",
			&no_headers,
			br#"{"model":"shared-model","messages":[{"role":"system","content":"stable system"},{"role":"user","content":"first question"},{"role":"assistant","content":"answer"},{"role":"user","content":"follow up"}]}"#,
		);
		let derived_responses = local_model_route_affinity_key(
			"/v1/responses",
			&no_headers,
			br#"{"model":"shared-model","instructions":"stable system","input":[{"role":"user","content":[{"type":"input_text","text":"first question"}]}]}"#,
		);
		let derived_anthropic = local_model_route_affinity_key(
			"/anthropic/v1/messages",
			&no_headers,
			br#"{"model":"shared-model","system":"stable system","messages":[{"role":"user","content":[{"type":"text","text":"first question"}]}]}"#,
		);
		assert_eq!(derived_chat, derived_responses);
		assert_eq!(derived_chat, derived_anthropic);

        assert_ne!(
            local_model_route_affinity_key(
                "/v1/chat/completions",
                &no_headers,
                br#"{"model":"shared-model","messages":[]}"#,
            ),
            local_model_route_affinity_key(
                "/v1/responses",
                &no_headers,
                br#"{"model":"shared-model","input":[]}"#,
            )
        );
    }

    #[test]
    fn openai_responses_keeps_cache_controls_until_the_subscription_send_boundary() {
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": "hello",
            "prompt_cache_key": "cache-session-1",
            "prompt_cache_retention": "24h",
            "future_openai_field": {"kept": true},
            "stream": true
        })
        .to_string();

        let (_, converted, inbound, target) = upstream_request_for_api_format(
            "openai_responses",
            "/v1/responses",
            &body,
            "gpt-5.6-sol",
        )
        .unwrap();
        let converted: serde_json::Value = serde_json::from_str(&converted).unwrap();
        assert_eq!(inbound, "openai_responses");
        assert_eq!(target, "openai_responses");
        assert_eq!(converted["prompt_cache_retention"], "24h");
        assert_eq!(converted["future_openai_field"]["kept"], true);

        let channel = openai_subscription_channel(&["gpt-5.6-sol"]);
        let supplier = crate::config::supplier_from_channel(&channel);
        let prepared = prepare_codex_subscription_endpoint_request(
            &supplier,
            &serde_json::to_string(&converted).unwrap(),
            false,
            "gpt-5.6-sol",
            Some("cache-session-1"),
            None,
        )
        .unwrap();
        let prepared_body: serde_json::Value = serde_json::from_str(&prepared.body).unwrap();
        assert!(prepared_body.get("prompt_cache_retention").is_none());
        assert_eq!(prepared_body["prompt_cache_key"], "cache-session-1");
        assert_eq!(prepared_body["future_openai_field"]["kept"], true);

        let faults = serde_json::to_value(prepared.faults).unwrap();
        assert!(faults.as_array().is_some_and(|faults| faults.iter().any(|fault| {
            fault["category"] == "provider_dialect"
                && fault["code"] == "codex_prompt_cache_control_omitted"
                && fault["field_path"] == "$.prompt_cache_retention"
                && fault["resolution"] == "request_adjusted_for_endpoint"
        })));
    }

    #[test]
    fn codex_subscription_dialect_reports_every_compatibility_adjustment() {
        let body = serde_json::json!({
            "model": "gpt-5.6-terra",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "hello",
                    "annotations": []
                }]
            }],
            "store": true,
            "truncation": "disabled",
            "context_management": [{"type": "compaction", "compact_threshold": 12000}],
            "user": "account-1",
            "temperature": 0.3,
            "top_p": 0.9,
            "max_output_tokens": 4096
        })
        .to_string();

        let adjustment = adjust_codex_subscription_request(&body).unwrap();
        let value: serde_json::Value = serde_json::from_str(&adjustment.body).unwrap();
        let actions = adjustment
            .actions
            .iter()
            .map(|action| (action.code, action.path.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(value["store"], false);
        for field in [
            "user",
            "temperature",
            "top_p",
            "max_output_tokens",
            "truncation",
            "context_management",
        ] {
            assert!(value.get(field).is_none(), "{field}");
        }
        assert!(value["input"][0]["content"][0].get("annotations").is_none());
        for expected in [
            ("codex_store_forced_false", "$.store"),
            ("codex_user_identity_omitted", "$.user"),
            ("codex_sampling_hint_omitted", "$.temperature"),
            ("codex_sampling_hint_omitted", "$.top_p"),
            ("codex_token_limit_omitted", "$.max_output_tokens"),
            ("codex_internal_extension_omitted", "$.truncation"),
            ("codex_internal_extension_omitted", "$.context_management"),
            (
                "codex_output_only_field_omitted",
                "$.input[0].content[0].annotations",
            ),
        ] {
            assert!(
                actions.contains(&expected),
                "missing {expected:?} in {actions:?}"
            );
        }
    }

    #[test]
    fn codex_subscription_reports_only_semantic_dialect_losses_as_faults() {
        let body = serde_json::json!({
            "model": "gpt-5.6-terra",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "hello",
                    "annotations": []
                }]
            }],
            "store": true,
            "temperature": 0.3,
            "future_extension": {"kept": true}
        })
        .to_string();

        let adjustment =
            adapt_codex_subscription_params_for_operation(&body, false, "gpt-5.6-terra").unwrap();
        let value: serde_json::Value = serde_json::from_str(&adjustment.body).unwrap();
        let faults = serde_json::to_value(adjustment.faults).unwrap();

        assert_eq!(value["future_extension"]["kept"], true);
        assert!(faults.as_array().is_some_and(|faults| faults
            .iter()
            .any(|fault| fault["code"] == "codex_sampling_hint_omitted"
                && fault["field_path"] == "$.temperature")));
        assert!(faults.as_array().is_some_and(|faults| faults
            .iter()
            .any(|fault| fault["code"] == "codex_store_forced_false"
                && fault["field_path"] == "$.store")));
        assert!(!faults.as_array().is_some_and(|faults| faults
            .iter()
            .any(|fault| fault["field_path"] == "$.input[0].content[0].annotations")));
    }

    #[test]
    fn codex_subscription_normalizes_message_and_tool_audio_without_loss_faults() {
        let body = serde_json::json!({
            "model": "gpt-audio-experimental",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_audio",
                        "data": "AAAA",
                        "format": "mp3"
                    }]
                },
                {
                    "type": "function_call_output",
                    "call_id": "call-1",
                    "output": [{
                        "type": "input_audio",
                        "audio_url": "DATA:audio/x-wav;base64,AAAA"
                    }]
                }
            ],
            "store": false
        })
        .to_string();

        let adjustment =
            adapt_codex_subscription_params_for_operation(&body, false, "gpt-audio-experimental")
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&adjustment.body).unwrap();

        assert_eq!(
            value["input"][0]["content"][0]["audio_url"],
            "data:audio/mpeg;base64,AAAA"
        );
        assert_eq!(
            value["input"][1]["output"][0]["audio_url"],
            "data:audio/wav;base64,AAAA"
        );
        assert!(value["input"][0]["content"][0].get("data").is_none());
        assert!(value["input"][0]["content"][0].get("format").is_none());
        let faults = serde_json::to_value(adjustment.faults).unwrap();
        assert!(!faults.as_array().is_some_and(|faults| faults
            .iter()
            .any(|fault| fault["code"] == "codex_input_audio_normalized")));
    }

    #[test]
    fn codex_subscription_rejects_remote_invalid_and_unsupported_audio() {
        for (audio, expected) in [
            (
                serde_json::json!({
                    "type": "input_audio",
                    "audio_url": "https://example.test/audio.wav"
                }),
                "requires a base64 data URL",
            ),
            (
                serde_json::json!({
                    "type": "input_audio",
                    "data": "not-base64",
                    "format": "wav"
                }),
                "invalid base64",
            ),
            (
                serde_json::json!({
                    "type": "input_audio",
                    "data": "AAAA",
                    "format": "flac"
                }),
                "unsupported format",
            ),
        ] {
            let body = serde_json::json!({
                "model": "gpt-audio-experimental",
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [audio]
                }],
                "store": false
            })
            .to_string();
            let error = strip_codex_subscription_unsupported_params(&body)
                .expect_err("invalid audio must fail before the upstream request");
            assert!(
                error.to_string().contains(expected),
                "error={error}, expected={expected}"
            );
        }
    }

    #[test]
    fn codex_subscription_preserves_encrypted_reasoning_for_native_upstream() {
        let body = serde_json::json!({
            "model": "gpt-5.5",
            "input": [
                {
                    "type": "reasoning",
                    "id": "rs_ephemeral",
                    "encrypted_content": "opaque-secret"
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hello"}]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "visible"}],
                    "reasoning": {"encrypted_content": "nested-secret"}
                }
            ],
            "reasoning": {"effort": "high", "encrypted_content": "top-secret"},
            "max_completion_tokens": 4096
        })
        .to_string();

        let converted = strip_codex_subscription_unsupported_params(&body).unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(value["input"].as_array().unwrap().len(), 3);
        assert!(converted.contains("hello"));
        assert!(converted.contains("visible"));
        assert!(converted.contains("\"effort\":\"high\""));
        assert!(converted.contains("opaque-secret"));
        assert!(converted.contains("nested-secret"));
        assert!(converted.contains("top-secret"));
        assert!(converted.contains("encrypted_content"));
        assert!(value.get("max_completion_tokens").is_none());
        assert!(value["input"][0].get("id").is_none());
        assert_eq!(value["input"][0]["summary"], serde_json::json!([]));
        assert_eq!(
            value["include"],
            serde_json::json!(["reasoning.encrypted_content"])
        );
    }

    #[test]
    fn codex_subscription_keeps_unknown_fields_for_guarded_recovery() {
        let body = serde_json::json!({
            "model": "gpt-5.6-terra",
            "input": "hello",
            "seed": 7,
            "future_extension": {"enabled": true}
        })
        .to_string();
        let converted = strip_codex_subscription_unsupported_params(&body).unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();
        assert_eq!(value["seed"], 7);
        assert_eq!(value["future_extension"]["enabled"], true);
        assert_eq!(
            value["include"],
            serde_json::json!(["reasoning.encrypted_content"])
        );
    }

    #[test]
    fn codex_subscription_always_requests_encrypted_state_without_replacing_other_includes() {
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": [{"type":"message","role":"user","content":[{
                "type":"input_text","text":"use a tool"
            }]}],
            "include": ["web_search_call.action.sources"]
        })
        .to_string();
        let converted = strip_codex_subscription_unsupported_params(&body).unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();
        assert_eq!(
            value["include"],
            serde_json::json!([
                "web_search_call.action.sources",
                "reasoning.encrypted_content"
            ])
        );
    }

    #[test]
    fn codex_subscription_normalizes_string_input_and_preserves_service_tier() {
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": "hello",
            "service_tier": "auto",
            "future_extension": {"kept": true}
        })
        .to_string();

        let adjustment = adjust_codex_subscription_request(&body).unwrap();
        let value: serde_json::Value = serde_json::from_str(&adjustment.body).unwrap();

        assert_eq!(
            value.pointer("/input/0/type").and_then(|v| v.as_str()),
            Some("message")
        );
        assert_eq!(
            value
                .pointer("/input/0/content/0/text")
                .and_then(|v| v.as_str()),
            Some("hello")
        );
        assert_eq!(value["service_tier"], "auto");
        assert_eq!(value["future_extension"]["kept"], true);
        assert!(adjustment.actions.iter().any(|action| {
            action.code == "codex_string_input_normalized" && action.path == "$.input"
        }));
        assert!(!adjustment
            .actions
            .iter()
            .any(|action| action.path == "$.service_tier"));

        let priority = adjust_codex_subscription_request(
            r#"{"model":"gpt-5.6-sol","input":[],"service_tier":"priority"}"#,
        )
        .unwrap();
        let priority: serde_json::Value = serde_json::from_str(&priority.body).unwrap();
        assert_eq!(priority["service_tier"], "priority");

        let flex = adjust_codex_subscription_request(
            r#"{"model":"gpt-5.6-sol","input":[],"service_tier":"flex"}"#,
        )
        .unwrap();
        let flex: serde_json::Value = serde_json::from_str(&flex.body).unwrap();
        assert_eq!(flex["service_tier"], "flex");
    }

    #[test]
    fn codex_subscription_preserves_responses_continuation_state() {
        for stream in [false, true] {
            let body = serde_json::json!({
                "model": "gpt-5.6-sol",
                "stream": stream,
                "previous_response_id": "resp_1",
                "parallel_tool_calls": true,
                "stream_options": {
                    "reasoning_summary_delivery": "sequential_cutoff"
                },
                "client_metadata": {"traceparent": "00-test"},
                "input": [{
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "ok"
                }]
            })
            .to_string();

            let adjustment = adjust_codex_subscription_request(&body).unwrap();
            let value: serde_json::Value = serde_json::from_str(&adjustment.body).unwrap();
            assert_eq!(value["previous_response_id"], "resp_1");
            assert_eq!(value["parallel_tool_calls"], true);
            assert_eq!(
                value["stream_options"]["reasoning_summary_delivery"],
                "sequential_cutoff"
            );
            assert_eq!(value["client_metadata"]["traceparent"], "00-test");
            assert_eq!(value["input"][0]["call_id"], "call_1");
            assert!(!adjustment.actions.iter().any(|action| {
                action.path == "$.previous_response_id"
                    || action.path == "$.parallel_tool_calls"
                    || action.path == "$.stream_options"
            }));
        }
    }

    #[test]
    fn codex_subscription_normalizes_message_ids_and_shortens_overlong_input_ids() {
        let long_id = "你".repeat(80);
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": [
                {"type": "message", "id": long_id, "role": "user", "content": []},
                {"type": "message", "id": "item_74ec40c883248ebb4885ec84", "role": "user", "content": []},
                {"type": "message", "id": "msg-1", "role": "assistant", "content": []},
                {"type": "function_call", "id": "item_call", "call_id": "call-1"}
            ]
        })
        .to_string();

        let first = adjust_codex_subscription_request(&body).unwrap();
        let second = adjust_codex_subscription_request(&body).unwrap();
        let first_value: serde_json::Value = serde_json::from_str(&first.body).unwrap();
        let second_value: serde_json::Value = serde_json::from_str(&second.body).unwrap();
        let shortened = first_value["input"][0]["id"].as_str().unwrap();
        assert_eq!(shortened.chars().count(), 64);
        assert!(shortened.starts_with("msg_"));
        assert_eq!(shortened, second_value["input"][0]["id"]);
        assert_eq!(
            first_value["input"][1]["id"],
            "msg_item_74ec40c883248ebb4885ec84"
        );
        assert_eq!(first_value["input"][2]["id"], "msg-1");
        assert_eq!(first_value["input"][3]["id"], "item_call");
        assert!(first.actions.iter().any(|action| {
            action.code == "codex_message_id_normalized" && action.path == "$.input[0].id"
        }));
        assert!(first.actions.iter().any(|action| {
            action.code == "codex_message_id_normalized" && action.path == "$.input[1].id"
        }));
        assert!(first.actions.iter().any(|action| {
            action.code == "codex_input_id_shortened" && action.path == "$.input[0].id"
        }));
    }

    #[test]
    fn claude_subscription_defaults_preserve_unknown_fields() {
        let body = serde_json::json!({
            "model": "requested",
            "cache_control": {"type": "ephemeral"},
            "thinking": {"type": "enabled", "budget_tokens": 4096},
            "system": [{
                "type": "text",
                "text": "stable prefix",
                "cache_control": {"type": "ephemeral", "ttl": "1h"}
            }],
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "hi",
                    "cache_control": {"type": "ephemeral"}
                }]
            }],
            "future_beta": {"kept": true},
            "tool_choice": {"type": "auto"}
        })
        .to_string();
        let adjusted = ensure_claude_messages_body_with_faults(&body, "claude-opus-4-8").unwrap();
        let value: serde_json::Value = serde_json::from_str(&adjusted.body).unwrap();
        let faults = serde_json::to_value(adjusted.faults).unwrap();
        assert_eq!(value["model"], "claude-opus-4-8");
        assert_eq!(value["max_tokens"], 128000);
        assert!(value.get("temperature").is_none());
        assert_eq!(value["tools"], serde_json::json!([]));
        assert!(value.get("tool_choice").is_none());
        assert_eq!(
            value.pointer("/context_management/edits/0/type"),
            Some(&serde_json::json!("clear_thinking_20251015"))
        );
        assert_eq!(value["future_beta"]["kept"], true);
        assert_eq!(value["cache_control"]["type"], "ephemeral");
        assert_eq!(value["system"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(
            value["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(faults
            .as_array()
            .is_some_and(|faults| faults.iter().any(|fault| fault["code"]
                == "claude_tool_choice_omitted_without_tools"
                && fault["field_path"] == "$.tool_choice")));
    }

    #[test]
    fn claude_subscription_normalizes_only_the_official_oauth_aliases() {
        let cases = [
            ("claude-sonnet-4-5", "claude-sonnet-4-5-20250929"),
            ("claude-opus-4-5", "claude-opus-4-5-20251101"),
            ("claude-haiku-4-5", "claude-haiku-4-5-20251001"),
            ("claude-opus-4-8", "claude-opus-4-8"),
        ];

        for (input, expected) in cases {
            assert_eq!(normalize_claude_subscription_model(input), expected);
            let body = normalize_claude_subscription_count_tokens_body(
                &serde_json::json!({
                    "model": input,
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            )
            .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&body).unwrap()["model"],
                expected
            );
        }
    }

    #[test]
    fn claude_subscription_injects_only_stable_missing_cache_breakpoints() {
        let body = serde_json::json!({
            "model": "requested",
            "system": "stable system",
            "tools": [
                {"name": "read", "input_schema": {"type": "object"}},
                {
                    "name": "deferred_search",
                    "defer_loading": true,
                    "input_schema": {"type": "object"}
                }
            ],
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "one"},
                {"role": "user", "content": [{"type": "text", "text": "second"}]},
                {"role": "assistant", "content": "two"},
                {"role": "user", "content": "latest"}
            ]
        })
        .to_string();

        let adjusted = ensure_claude_messages_body_with_faults(&body, "claude-sonnet-4-5").unwrap();
        let value: serde_json::Value = serde_json::from_str(&adjusted.body).unwrap();
        assert_eq!(value["tools"][0]["cache_control"]["type"], "ephemeral");
        assert!(value["tools"][0]["cache_control"].get("ttl").is_none());
        assert!(value["tools"][1].get("cache_control").is_none());
        assert_eq!(value["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(
            value["messages"][4]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(value["messages"][2]["content"][0]
            .get("cache_control")
            .is_none());
        assert_eq!(value["messages"][0]["content"][0]["text"], "first");
        assert_eq!(value["messages"][4]["content"][0]["text"], "latest");

        let repeated =
            ensure_claude_messages_body_with_faults(&adjusted.body, "claude-sonnet-4-5").unwrap();
        assert_eq!(
            value,
            serde_json::from_str::<serde_json::Value>(&repeated.body).unwrap()
        );
    }

    #[test]
    fn claude_subscription_rolling_cache_uses_the_latest_safe_turn() {
        let cases = [
            (
                serde_json::json!([
                    {"role":"user","content":"first"},
                    {"role":"assistant","content":"assistant prefill"}
                ]),
                1,
            ),
            (
                serde_json::json!([
                    {"role":"user","content":"safe fallback"},
                    {"role":"assistant","content":[
                        {"type":"text","text":"answer"},
                        {"type":"thinking","thinking":"private"}
                    ]}
                ]),
                0,
            ),
        ];

        for (messages, expected_index) in cases {
            let body = serde_json::json!({
                "model":"requested",
                "messages":messages
            })
            .to_string();
            let adjusted =
                ensure_claude_messages_body_with_faults(&body, "claude-sonnet-4-5").unwrap();
            let value: serde_json::Value = serde_json::from_str(&adjusted.body).unwrap();
            assert_eq!(
                value["messages"][expected_index]["content"][0]["cache_control"]["type"],
                "ephemeral",
                "{}",
                adjusted.body
            );
        }
    }

    #[test]
    fn claude_subscription_never_rewrites_caller_cache_anchors() {
        let body = serde_json::json!({
            "model": "requested",
            "system": [{"type": "text", "text": "stable system"}],
            "tools": [{"name": "read", "input_schema": {"type": "object"}}],
            "messages": [
                {
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": "caller anchor",
                        "cache_control": {"type": "ephemeral", "ttl": "1h"}
                    }]
                },
                {"role": "assistant", "content": "one"},
                {"role": "user", "content": "latest"}
            ]
        })
        .to_string();

        let adjusted = ensure_claude_messages_body_with_faults(&body, "claude-sonnet-4-5").unwrap();
        let value: serde_json::Value = serde_json::from_str(&adjusted.body).unwrap();
        assert!(value["tools"][0].get("cache_control").is_none());
        assert!(value["system"][0].get("cache_control").is_none());
        assert_eq!(
            value["messages"][0]["content"][0]["cache_control"]["ttl"],
            "1h"
        );
        assert!(value["messages"][2]["content"].is_string());
    }

    #[test]
    fn claude_subscription_preserves_caller_max_tokens() {
        let adjusted = ensure_claude_messages_body(
            r#"{"model":"requested","max_tokens":4096,"messages":[{"role":"user","content":"hi"}]}"#,
            "claude-3-opus",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&adjusted).unwrap();

        assert_eq!(value["max_tokens"], 4096);
    }

    #[test]
    fn anthropic_tool_history_with_empty_text_is_safe_for_antigravity_claude() {
        for stream in [false, true] {
            let body = serde_json::json!({
                "model": "claude-opus-4-6", "max_tokens": 1024, "stream": stream,
                "messages": [
                    {"role": "user", "content": "read the file"},
                    {"role": "assistant", "content": [
                        {"type": "text", "text": ""},
                        {"type": "tool_use", "id": "call-1", "name": "read_file", "input": {"path": "README.md"}}
                    ]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "call-1", "content": "file contents"}
                    ]}
                ],
                "tools": [{"name": "read_file", "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}}]
            }).to_string();
            let (_, native, _, _) = upstream_request_for_api_format(
                "gemini_native", "/anthropic/v1/messages", &body, "claude-opus-4-6",
            ).unwrap();
            let prepared = crate::antigravity_models::prepare_subscription_request(
                &serde_json::json!({}), "claude-opus-4-6", &body, &native,
            ).unwrap();
            let wrapped = gemini_native_body_to_antigravity_body(
                &prepared.native_body, &prepared.upstream_model, "test-project",
            ).unwrap();
            let actual: serde_json::Value = serde_json::from_str(&wrapped).unwrap();
            let parts = actual["request"]["contents"][1]["parts"].as_array().unwrap();
            assert_eq!(parts.len(), 1, "stream={stream}: {parts:?}");
            assert_eq!(parts[0]["functionCall"]["id"], "call-1");
            assert_eq!(parts[0]["functionCall"]["args"]["path"], "README.md");
            assert_eq!(actual["request"]["contents"][2]["parts"][0]["functionResponse"]["id"], "call-1");
            assert_eq!(actual["request"]["toolConfig"]["functionCallingConfig"]["mode"], "VALIDATED");
        }
    }

    #[test]
    fn antigravity_envelope_preserves_native_body() {
        let body = serde_json::json!({
            "contents": [{"parts": [{"text": "hi", "futurePart": {"kept": true}}]}],
            "generationConfig": {"temperature": 0.4},
            "stream": false
        })
        .to_string();
        let wrapped =
            gemini_native_body_to_antigravity_body(&body, "gemini-2.5-pro", "project-1").unwrap();
        let value: serde_json::Value = serde_json::from_str(&wrapped).unwrap();
        assert_eq!(value["project"], "project-1");
        assert_eq!(value["model"], "gemini-2.5-pro");
        assert_eq!(value["userAgent"], "antigravity");
        assert_eq!(value["requestType"], "agent");
        assert!(value["requestId"]
            .as_str()
            .is_some_and(|request_id| request_id.starts_with("agent-")));
        assert_eq!(
            value.pointer("/request/contents/0/parts/0/futurePart/kept"),
            Some(&serde_json::json!(true))
        );
        assert!(value.pointer("/request/stream").is_none());
    }

    #[test]
    fn antigravity_session_is_stable_for_growing_conversations() {
        let first = serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "first question"}]}]
        })
        .to_string();
        let grown = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "first question"}]},
                {"role": "model", "parts": [{"text": "first answer"}]},
                {"role": "user", "parts": [{"text": "follow up"}]}
            ]
        })
        .to_string();

        let first_wrapped = gemini_native_body_to_antigravity_body_with_cache_identity(
            &first,
            "gemini-2.5-pro",
            "project-1",
            Some("c1_stable"),
        )
        .unwrap();
        let grown_wrapped = gemini_native_body_to_antigravity_body_with_cache_identity(
            &grown,
            "gemini-2.5-pro",
            "project-1",
            Some("c1_stable"),
        )
        .unwrap();
        let first_wrapped: serde_json::Value = serde_json::from_str(&first_wrapped).unwrap();
        let grown_wrapped: serde_json::Value = serde_json::from_str(&grown_wrapped).unwrap();
        assert_eq!(
            first_wrapped.pointer("/request/sessionId"),
            grown_wrapped.pointer("/request/sessionId")
        );

        let derived_first = gemini_native_body_to_antigravity_body(
            &first,
            "gemini-2.5-pro",
            "project-1",
        )
        .unwrap();
        let derived_grown = gemini_native_body_to_antigravity_body(
            &grown,
            "gemini-2.5-pro",
            "project-1",
        )
        .unwrap();
        let derived_first: serde_json::Value = serde_json::from_str(&derived_first).unwrap();
        let derived_grown: serde_json::Value = serde_json::from_str(&derived_grown).unwrap();
        assert_eq!(
            derived_first.pointer("/request/sessionId"),
            derived_grown.pointer("/request/sessionId")
        );

        let explicit = gemini_native_body_to_antigravity_body_with_cache_identity(
            r#"{"sessionId":"caller-session","contents":[{"role":"user","parts":[{"text":"hello"}]}]}"#,
            "gemini-2.5-pro",
            "project-1",
            Some("c1_gateway"),
        )
        .unwrap();
        let explicit: serde_json::Value = serde_json::from_str(&explicit).unwrap();
        assert_eq!(explicit.pointer("/request/sessionId").unwrap(), "caller-session");
    }

    #[test]
    fn responses_output_text_done_does_not_duplicate_streamed_delta() {
        let raw = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
            "event: response.output_text.done\n",
            "data: {\"type\":\"response.output_text.done\",\"text\":\"hello\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );
        let converted = responses_sse_to_chat_sse(raw, "code-cheap");

        assert_eq!(converted.matches("\"content\":\"hello\"").count(), 1);
        assert!(converted.contains("\"finish_reason\":\"stop\""));
        assert!(converted.contains("data: [DONE]"));
    }

    #[test]
    fn responses_chat_body_does_not_repeat_output_text_done() {
        let raw = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
            "event: response.output_text.done\n",
            "data: {\"type\":\"response.output_text.done\",\"text\":\"hello\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );
        let converted = responses_sse_to_chat_body(raw, "code-cheap").unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(value["choices"][0]["message"]["content"], "hello");
        assert_eq!(value["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn anthropic_and_gemini_final_objects_preserve_text_plus_tools() {
        let anthropic_raw = serde_json::json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "need tools"},
                {"type": "tool_use", "id": "call_1", "name": "read_file", "input": {"path": "README.md"}},
                {"type": "tool_use", "id": "call_2", "name": "list_dir", "input": {"path": "docs"}}
            ],
            "usage": {"input_tokens": 3, "output_tokens": 4}
        })
        .to_string();
        let anthropic = anthropic_message_to_chat_body(&anthropic_raw, "m").unwrap();
        let anthropic_value: serde_json::Value = serde_json::from_str(&anthropic).unwrap();
        assert_eq!(anthropic_value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            anthropic_value["choices"][0]["message"]["content"],
            "need tools"
        );
        assert_eq!(
            anthropic_value["choices"][0]["message"]["tool_calls"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let gemini_raw = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "need tools"},
                        {"functionCall": {"name": "read_file", "args": {"path": "README.md"}}},
                        {"functionCall": {"name": "list_dir", "args": {"path": "docs"}}}
                    ]
                }
            }],
            "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 4, "totalTokenCount": 7}
        })
        .to_string();
        let gemini = gemini_native_response_to_chat_body(&gemini_raw, "m").unwrap();
        let gemini_value: serde_json::Value = serde_json::from_str(&gemini).unwrap();
        assert_eq!(gemini_value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            gemini_value["choices"][0]["message"]["content"],
            "need tools"
        );
        assert_eq!(
            gemini_value["choices"][0]["message"]["tool_calls"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn upstream_request_keeps_native_protocol_passthrough() {
        let cases = [
            (
                "openai_responses",
                "/v1/responses",
                r#"{"model":"public","input":"hi","future":{"depth":2}}"#,
                "gpt-responses-up",
                "/future/depth",
                "/v1/responses",
            ),
            (
                "openai_chat",
                "/v1/chat/completions",
                r#"{"model":"public","messages":[{"role":"user","content":"hi"}],"future":{"depth":2}}"#,
                "gpt-chat-up",
                "/future/depth",
                "/v1/chat/completions",
            ),
            (
                "anthropic_messages",
                "/v1/messages",
                r#"{"model":"public","messages":[{"role":"user","content":"hi"}],"future":{"depth":2}}"#,
                "claude-up",
                "/future/depth",
                "/v1/messages",
            ),
            (
                "gemini_native",
                "/v1beta/models/public:generateContent",
                r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"future":{"depth":2}}"#,
                "gemini-up",
                "/future/depth",
                "/v1beta/models/gemini-up:generateContent",
            ),
        ];

        for (format, inbound_path, body, upstream_model, future_path, expected_path) in cases {
            let (path, converted, inbound, target) =
                upstream_request_for_api_format(format, inbound_path, body, upstream_model)
                    .unwrap();

            assert_eq!(path, expected_path, "{format}");
            assert_eq!(inbound, format, "{format}");
            assert_eq!(target, format, "{format}");
            let value: serde_json::Value = serde_json::from_str(&converted).unwrap();
            assert_eq!(value.pointer(future_path), Some(&serde_json::json!(2)));
            if format == "gemini_native" {
                assert!(value.get("model").is_none());
            } else {
                assert_eq!(value["model"], upstream_model, "{format}");
            }
        }
    }

    #[test]
    fn cross_protocol_claude_prefill_is_repaired_only_for_rejecting_models() {
        let source = serde_json::json!({
            "model": "source-model",
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]},
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"prefill"}]}
            ]
        })
        .to_string();

        let repaired = upstream_request_for_api_format_with_profiles_report(
            "anthropic_messages",
            "/v1/responses",
            &source,
            "claude-opus-5",
            &[],
        )
        .unwrap();
        let repaired_body: serde_json::Value = serde_json::from_str(&repaired.body).unwrap();
        assert_eq!(repaired_body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(repaired_body["messages"][0]["role"], "user");
        let faults = serde_json::to_value(&repaired.faults).unwrap();
        assert!(faults.as_array().unwrap().iter().any(|fault| {
            fault["code"] == "anthropic_assistant_prefill_omitted"
                && fault["field_path"] == "$.messages[-1]"
        }));

        let legacy = upstream_request_for_api_format(
            "anthropic_messages",
            "/v1/responses",
            &source,
            "claude-sonnet-4-5",
        )
        .unwrap();
        let legacy_body: serde_json::Value = serde_json::from_str(&legacy.1).unwrap();
        assert_eq!(legacy_body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(legacy_body["messages"][1]["role"], "assistant");
    }

    #[test]
    fn unsupported_only_assistant_prefill_gets_a_minimal_user_turn() {
        let source = serde_json::json!({
            "model": "source-model",
            "input": [
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"prefill"}]}
            ]
        })
        .to_string();
        let (_, converted, _, _) = upstream_request_for_api_format(
            "anthropic_messages",
            "/v1/responses",
            &source,
            "claude-fable-5",
        )
        .unwrap();
        let converted: serde_json::Value = serde_json::from_str(&converted).unwrap();
        let messages = converted["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "text");
    }

    #[test]
    fn native_anthropic_prefill_remains_byte_stable() {
        let body = r#"{
      "model": "claude-opus-5",
      "messages": [{"role":"assistant","content":"native prefill"}],
      "max_tokens": 32,
      "future": {"preserve":true}
    }"#;
        let (_, converted, inbound, target) = upstream_request_for_api_format(
            "anthropic_messages",
            "/anthropic/v1/messages",
            body,
            "claude-opus-5",
        )
        .unwrap();
        assert_eq!(converted, body);
        assert_eq!(inbound, target);
    }

    #[test]
    fn anthropic_message_level_system_history_stays_ordered_for_all_targets() {
        let source = serde_json::json!({
            "model": "claude-source",
            "max_tokens": 64,
            "messages": [
                {"role":"user","content":"first question"},
                {"role":"assistant","content":"first answer"},
                {"role":"system","content":"late reminder"},
                {"role":"user","content":"continue"}
            ]
        })
        .to_string();

        for (format, model) in [
            ("openai_responses", "gpt-5.6-sol"),
            ("openai_chat", "gpt-5.6-sol"),
            ("gemini_native", "gemini-3.6-flash"),
        ] {
            let (_, converted, _, _) =
                upstream_request_for_api_format(format, "/anthropic/v1/messages", &source, model)
                    .unwrap_or_else(|error| panic!("target={format}: {error}"));
            let answer = converted.find("first answer").unwrap();
            let reminder = converted.find("late reminder").unwrap();
            let continuation = converted.find("continue").unwrap();
            assert!(answer < reminder, "target={format}: {converted}");
            assert!(reminder < continuation, "target={format}: {converted}");
        }
    }

    #[test]
    fn subscription_endpoint_request_matrix_covers_all_protocols_and_stream_modes() {
        struct InboundCase {
            name: &'static str,
            path: String,
            body: String,
            carries_identity: bool,
        }
        struct TargetCase {
            name: &'static str,
            api_format: &'static str,
            model: &'static str,
        }

        let targets = [
            TargetCase {
                name: "codex",
                api_format: "openai_responses",
                model: "gpt-5.6-terra",
            },
            TargetCase {
                name: "claude",
                api_format: "anthropic_messages",
                model: "claude-opus-4-8",
            },
            TargetCase {
                name: "gemini",
                api_format: "gemini_native",
                model: "gemini-2.5-pro",
            },
        ];

        for stream in [false, true] {
            let inbound = vec![
                InboundCase {
                    name: "openai_chat",
                    path: "/v1/chat/completions".to_string(),
                    body: serde_json::json!({
                        "model": "source-chat",
                        "messages": [{"role": "user", "content": "hello"}],
                        "user": "matrix-user",
                        "temperature": 0.3,
                        "stream": stream
                    })
                    .to_string(),
                    carries_identity: true,
                },
                InboundCase {
                    name: "openai_responses",
                    path: "/v1/responses".to_string(),
                    body: serde_json::json!({
                        "model": "source-responses",
                        "input": "hello",
                        "store": true,
                        "user": "matrix-user",
                        "temperature": 0.3,
                        "stream": stream
                    })
                    .to_string(),
                    carries_identity: true,
                },
                InboundCase {
                    name: "anthropic_messages",
                    path: "/anthropic/v1/messages?beta=true".to_string(),
                    body: serde_json::json!({
                        "model": "source-claude",
                        "messages": [{"role": "user", "content": "hello"}],
                        "max_tokens": 256,
                        "metadata": {"user_id": "matrix-user"},
                        "temperature": 0.3,
                        "stream": stream
                    })
                    .to_string(),
                    carries_identity: true,
                },
                InboundCase {
                    name: "gemini_native",
                    path: format!(
                        "/v1beta/models/source-gemini:{}",
                        if stream {
                            "streamGenerateContent?alt=sse"
                        } else {
                            "generateContent"
                        }
                    ),
                    body: serde_json::json!({
                        "contents": [{"role": "user", "parts": [{"text": "hello"}]}]
                    })
                    .to_string(),
                    carries_identity: false,
                },
            ];

            for source in &inbound {
                for target in &targets {
                    let label = format!("{} -> {} stream={stream}", source.name, target.name);
                    let (path, converted, inbound_protocol, target_protocol) =
                        upstream_request_for_api_format(
                            target.api_format,
                            &source.path,
                            &source.body,
                            target.model,
                        )
                        .unwrap_or_else(|error| panic!("{label}: {error}"));
                    assert_eq!(target_protocol, target.api_format, "{label}");
                    assert_eq!(inbound_protocol, source.name, "{label}");

                    match target.name {
                        "codex" => {
                            let adjusted =
                                strip_codex_subscription_unsupported_params(&converted).unwrap();
                            let body: serde_json::Value = serde_json::from_str(&adjusted).unwrap();
                            assert_eq!(path, "/v1/responses", "{label}");
                            assert_eq!(body["model"], target.model, "{label}");
                            assert_eq!(body["store"], false, "{label}");
                            assert_eq!(body["stream"], stream, "{label}");
                            for unsupported in ["user", "temperature", "top_p"] {
                                assert!(body.get(unsupported).is_none(), "{label}: {unsupported}");
                            }
                        }
                        "claude" => {
                            let adjusted =
                                ensure_claude_messages_body(&converted, target.model).unwrap();
                            let body: serde_json::Value = serde_json::from_str(&adjusted).unwrap();
                            assert!(path.starts_with("/v1/messages"), "{label}: {path}");
                            assert_eq!(body["model"], target.model, "{label}");
                            assert!(body["messages"].is_array(), "{label}");
                            assert_eq!(body["stream"], stream, "{label}");
                            if source.carries_identity {
                                assert_eq!(
                                    body.pointer("/metadata/user_id"),
                                    Some(&serde_json::json!("matrix-user")),
                                    "{label}"
                                );
                            }
                        }
                        "gemini" => {
                            let wrapped = gemini_native_body_to_antigravity_body(
                                &converted,
                                target.model,
                                "test-project",
                            )
                            .unwrap();
                            let body: serde_json::Value = serde_json::from_str(&wrapped).unwrap();
                            assert_eq!(body["model"], target.model, "{label}");
                            assert_eq!(body["project"], "test-project", "{label}");
                            assert!(body["request"]["contents"].is_array(), "{label}");
                            assert_eq!(path.contains(":streamGenerateContent"), stream, "{label}");
                        }
                        _ => unreachable!(),
                    }
                }
            }
        }
    }

    #[test]
    fn mounted_anthropic_path_with_query_converts_to_openai_responses() {
        let body = r#"{
            "model":"claude-opus-4-8",
            "max_tokens":1024,
            "stream":false,
            "thinking":{"type":"enabled","budget_tokens":512},
            "messages":[{"role":"user","content":[{"type":"text","text":"Reply OK","cache_control":{"type":"ephemeral"}}]}],
            "tools":[{"name":"noop","description":"No operation","input_schema":{"type":"object","properties":{}}}]
        }"#;

        let (path, converted, inbound, target) = upstream_request_for_api_format(
            "openai_responses",
            "/anthropic/v1/messages?beta=true",
            body,
            "gpt-5.6-terra",
        )
        .expect("mounted Anthropic request conversion");

        assert_eq!(path, "/v1/responses");
        assert_eq!(inbound, "anthropic_messages");
        assert_eq!(target, "openai_responses");
        let converted: serde_json::Value = serde_json::from_str(&converted).unwrap();
        assert_eq!(converted["model"], "gpt-5.6-terra");
        assert!(converted["tools"].is_array());
    }

    #[test]
    fn claude_code_compatible_roles_convert_for_buffered_and_streaming_requests() {
        for stream in [false, true] {
            let body = serde_json::json!({
                "model": "claude-opus-4-8",
                "max_tokens": 1024,
                "stream": stream,
                "messages": [
                    {"role": "system", "content": "system history"},
                    {"role": "assistant", "content": [
                        {"type": "thinking", "thinking": "private analysis", "signature": "anthropic-signature"},
                        {"type": "redacted_thinking", "data": "anthropic-redacted"},
                        {
                            "type": "tool_use",
                            "id": "call-1",
                            "name": "read_file",
                            "input": {"path": "README.md"}
                        }
                    ]},
                    {
                        "role": "tool",
                        "tool_call_id": "call-1",
                        "content": "file contents"
                    },
                    {"role": "future_assistant", "content": [{
                        "type": "tool_use",
                        "id": "call-2",
                        "name": "search",
                        "input": {"query": "const api"}
                    }]},
                    {"role": "user", "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call-2",
                        "content": "search result"
                    }]},
                    {"content": "continue"}
                ]
            })
            .to_string();

            let (path, converted, inbound, target) = upstream_request_for_api_format(
                "openai_responses",
                "/anthropic/v1/messages?beta=true",
                &body,
                "gpt-5.6-terra",
            )
            .unwrap_or_else(|error| panic!("stream={stream}: {error}"));

            assert_eq!(path, "/v1/responses");
            assert_eq!(inbound, "anthropic_messages");
            assert_eq!(target, "openai_responses");
            let converted: serde_json::Value = serde_json::from_str(&converted).unwrap();
            assert_eq!(converted["model"], "gpt-5.6-terra");
            assert_eq!(converted["stream"], stream);
            let converted_text = converted.to_string();
            assert!(
                converted_text.contains("system history"),
                "{converted_text}"
            );
            assert!(converted_text.contains("call-1"), "{converted_text}");
            assert!(converted_text.contains("file contents"), "{converted_text}");
            assert!(converted_text.contains("call-2"), "{converted_text}");
            assert!(converted_text.contains("search result"), "{converted_text}");
            assert!(converted_text.contains("continue"), "{converted_text}");
            assert!(
                !converted_text.contains("anthropic-signature"),
                "{converted_text}"
            );
            assert!(
                !converted_text.contains("anthropic-redacted"),
                "{converted_text}"
            );
        }
    }

    #[test]
    fn native_request_preserves_original_json_bytes_except_the_model_value() {
        let unchanged = "{\n  \"model\" : \"gpt-up\",\n  \"future\" : { \"nested\" : [1, {\"x\": true}] },\n  \"messages\" : []\n}";
        let (_, body, inbound, target) = upstream_request_for_api_format(
            "openai_chat",
            "/v1/chat/completions",
            unchanged,
            "gpt-up",
        )
        .unwrap();
        assert_eq!(body, unchanged);
        assert_eq!(inbound, target);

        let original = unchanged.replace("\"gpt-up\"", "\"public-model\"");
        let expected = unchanged;
        let (_, body, _, _) = upstream_request_for_api_format(
            "openai_chat",
            "/v1/chat/completions",
            &original,
            "gpt-up",
        )
        .unwrap();
        assert_eq!(body, expected);
    }

    #[test]
    fn native_protocols_keep_unknown_tools_content_and_extensions() {
        for (protocol, path) in [
            ("openai_responses", "/v1/responses"),
            ("openai_chat", "/v1/chat/completions"),
            ("anthropic_messages", "/v1/messages"),
            ("gemini_native", "/v1beta/models/future-model:generateContent"),
        ] {
            let raw = "{\n  \"model\":\"future-model\", \"input\":[{\"type\":\"additional_tools\",\"tools\":[{\"type\":\"future_tool\"}]}], \"tools\":[{\"type\":\"future_tool\",\"opaque\":true}], \"messages\":[{\"role\":\"user\",\"content\":[{\"type\":\"future_content\"}]}], \"contents\":[{\"role\":\"user\",\"parts\":[{\"future_part\":true}]}], \"future\": {\"large\":9007199254740993123456789,\"keep\":1.2300e+09} }";
            let converted = upstream_request_for_api_format_with_profiles_report(protocol, path, raw, "future-model", &[]).unwrap();
            assert_eq!(converted.body, raw, "{protocol}");
            assert!(converted.tool_mapping.is_empty(), "{protocol}");
            assert!(converted.faults.is_empty(), "{protocol}");
            let response = response_body_from_target_as_inbound_with_report(raw, protocol, protocol, "future-model").unwrap();
            assert_eq!(response.body, raw, "{protocol}");
        }
    }

    #[test]
    fn native_sse_converter_passthrough_preserves_unparseable_bytes() {
        for protocol in [
            "openai_responses",
            "openai_chat",
            "anthropic_messages",
            "gemini_native",
        ] {
            let raw = b"future-event-without-sse-framing\0\xff";
            let mut converter =
                inbound_sse_stream_converter(protocol, protocol, "future-model").unwrap();
            assert_eq!(converter.push(raw), bytes::Bytes::copy_from_slice(raw));
            assert!(converter.finish().is_empty());
        }
    }

    #[test]
    fn native_anthropic_sse_passthrough_preserves_compaction_events_exactly() {
        let raw = br#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"compaction","content":"","encrypted_content":"opaque-state"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"compaction_delta","content":"summary"}}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"compaction"},"usage":{"output_tokens":12}}

"#;
        let split = raw.len() / 2;
        let mut converter = inbound_sse_stream_converter(
            "anthropic_messages",
            "anthropic_messages",
            "claude-sonnet-4-6",
        )
        .unwrap();
        let mut output = Vec::new();
        output.extend_from_slice(&converter.push(&raw[..split]));
        output.extend_from_slice(&converter.push(&raw[split..]));
        output.extend_from_slice(&converter.finish());
        assert_eq!(output, raw);
    }

    #[test]
    fn anthropic_compaction_control_and_replay_cannot_cross_protocols() {
        let requests = [
            serde_json::json!({
                "model": "claude-sonnet-4-6",
                "max_tokens": 64,
                "messages": [{"role": "user", "content": "hello"}],
                "context_management": {
                    "edits": [{"type": "compact_20260112"}]
                }
            }),
            serde_json::json!({
                "model": "claude-sonnet-4-6",
                "max_tokens": 64,
                "messages": [{"role": "assistant", "content": [{
                    "type": "compaction",
                    "content": "summary",
                    "encrypted_content": "opaque-state"
                }]}]
            }),
        ];

        for request in requests {
            let error = upstream_request_for_api_format(
                "gemini_native",
                "/anthropic/v1/messages",
                &request.to_string(),
                "claude-sonnet-4-6",
            )
            .expect_err("compaction must not cross into Gemini");
            let payload: serde_json::Value =
                serde_json::from_str(&error.to_string()).expect("structured capability error");
            assert_eq!(payload["error_kind"], "capability_mismatch");
            assert_eq!(payload["feature"], "server_side_compaction");
            assert_eq!(payload["safe_to_retry_other_channel"], true);
        }
    }

    #[test]
    fn stream_shape_changes_only_for_explicit_verified_shape_evidence() {
        let mut channel = local_model_channel(&["future-model"], "http://127.0.0.1:9/v1");
        channel.capability_profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_chat".to_string(),
            non_stream_json: true,
            stream_sse: true,
            verification_state: "verified".to_string(),
            ..Default::default()
        }];
        let supplier = supplier_from_channel(&channel);

        assert!(!supplier_target_stream_requested(
            &supplier,
            "openai_chat",
            "openai_chat",
            false,
        ));
        assert!(supplier_target_stream_requested(
            &supplier,
            "openai_chat",
            "openai_chat",
            true,
        ));
        assert!(!supplier_target_stream_requested(
            &supplier,
            "anthropic_messages",
            "openai_chat",
            false,
        ));

        let mut explicitly_unsupported = supplier.clone();
        explicitly_unsupported.capability_profiles[0].stream_sse = false;
        explicitly_unsupported.capability_profiles[0].stream_sse_unsupported = true;
        assert!(!supplier_target_stream_requested(
            &explicitly_unsupported,
            "anthropic_messages",
            "openai_chat",
            true,
        ));
        assert!(!supplier_target_stream_requested(
            &explicitly_unsupported,
            "openai_chat",
            "openai_chat",
            true,
        ));

        let mut stream_only = supplier.clone();
        stream_only.capability_profiles[0].non_stream_json = false;
        assert!(supplier_target_stream_requested(
            &stream_only,
            "openai_chat",
            "openai_chat",
            false,
        ));
    }

    #[test]
    fn native_anthropic_request_only_rewrites_model_without_repairing_payload() {
        let body = serde_json::json!({
            "model": "public",
            "messages": [],
            "future_nested": {"level": {"value": 3}}
        })
        .to_string();
        let (path, converted, inbound, target) = upstream_request_for_api_format(
            "anthropic_messages",
            "/v1/messages",
            &body,
            "claude-up",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(path, "/v1/messages");
        assert_eq!(inbound, "anthropic_messages");
        assert_eq!(target, "anthropic_messages");
        assert_eq!(value["model"], "claude-up");
        assert_eq!(value["messages"], serde_json::json!([]));
        assert!(value.get("max_tokens").is_none());
        assert_eq!(value["future_nested"]["level"]["value"], 3);
    }

    #[test]
    fn native_special_operations_preserve_their_operation_path() {
        let cases = [
            (
                "openai_responses",
                "/v1/responses/compact",
                r#"{"model":"public","input":"compact me"}"#,
                "gpt-up",
                "/v1/responses/compact",
            ),
            (
                "anthropic_messages",
                "/anthropic/v1/messages/count_tokens",
                r#"{"model":"public","messages":[{"role":"user","content":"count me"}]}"#,
                "claude-up",
                "/v1/messages/count_tokens",
            ),
            (
                "gemini_native",
                "/gemini/v1beta/models/public:countTokens",
                r#"{"contents":[{"role":"user","parts":[{"text":"count me"}]}]}"#,
                "gemini-up",
                "/v1beta/models/gemini-up:countTokens",
            ),
            (
                "gemini_native",
                "/gemini/v1beta/models/public:embedContent",
                r#"{"content":{"parts":[{"text":"embed me"}]}}"#,
                "gemini-up",
                "/v1beta/models/gemini-up:embedContent",
            ),
        ];

        for (format, inbound_path, body, model, expected_path) in cases {
            let (path, _, inbound, target) =
                upstream_request_for_api_format(format, inbound_path, body, model)
                    .unwrap_or_else(|error| panic!("{inbound_path}: {error}"));
            assert_eq!(path, expected_path, "{inbound_path}");
            assert_eq!(inbound, target, "{inbound_path}");
        }
    }

    #[test]
    fn cross_protocol_special_operation_is_attempted_and_reported() {
        let conversion = upstream_request_for_api_format_with_profiles_report(
            "openai_responses",
            "/anthropic/v1/messages/count_tokens",
            r#"{"model":"claude-public","messages":[{"role":"user","content":"count me"}]}"#,
            "gpt-up",
            &[],
        )
        .expect("availability-first compatibility attempt");

        assert_eq!(conversion.path, "/v1/responses");
        let faults = serde_json::to_value(conversion.faults).unwrap();
        assert!(faults.as_array().is_some_and(|faults| faults
            .iter()
            .any(|fault| fault["code"] == "special_operation_approximated"
                && fault["field_path"] == "/anthropic/v1/messages/count_tokens")));
    }

    #[test]
    fn upstream_request_converts_chat_tools_to_anthropic_target() {
        let body = serde_json::json!({
            "model": "public",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}
                }
            }]
        })
        .to_string();
        let (path, converted, inbound, target) = upstream_request_for_api_format(
            "anthropic_messages",
            "/v1/chat/completions",
            &body,
            "claude-up",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(path, "/v1/messages");
        assert_eq!(inbound, "openai_chat");
        assert_eq!(target, "anthropic_messages");
        assert_eq!(value["model"], "claude-up");
        assert_eq!(value["tools"][0]["name"], "read_file");
    }

    #[test]
    fn upstream_request_converts_chat_stream_to_gemini_sse_target() {
        let body = serde_json::json!({
            "model": "public",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        })
        .to_string();
        let (path, converted, inbound, target) = upstream_request_for_api_format(
            "gemini_native",
            "/v1/chat/completions",
            &body,
            "gemini-2.5-pro",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(
            path,
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
        assert_eq!(inbound, "openai_chat");
        assert_eq!(target, "gemini_native");
        assert_eq!(value["contents"][0]["parts"][0]["text"], "hi");
    }

    #[test]
    fn upstream_request_converts_standard_chat_audio_to_gemini_inline_data() {
        let body = serde_json::json!({
            "model": "public",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "input_audio",
                    "input_audio": {
                        "data": "AAAA",
                        "format": "wav"
                    }
                }]
            }]
        })
        .to_string();
        let (path, converted, inbound, target) = upstream_request_for_api_format(
            "gemini_native",
            "/v1/chat/completions",
            &body,
            "gemini-2.5-flash",
        )
        .expect("Chat audio to Gemini");
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(
            path,
            "/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert_eq!(inbound, "openai_chat");
        assert_eq!(target, "gemini_native");
        assert_eq!(
            value["contents"][0]["parts"][0]["inlineData"],
            serde_json::json!({"mimeType":"audio/wav","data":"AAAA"})
        );
    }

    #[test]
    fn runtime_request_conversion_preserves_native_extensions_and_logs_safe_cross_protocol_omissions(
    ) {
        let native = serde_json::json!({
            "model": "public",
            "messages": [{"role": "user", "content": "hi"}],
            "future_semantic_option": {"mode": "strict"}
        })
        .to_string();
        let (_, converted, _, _) = upstream_request_for_api_format(
            "openai_chat",
            "/v1/chat/completions",
            &native,
            "gpt-up",
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&converted).unwrap()
                ["future_semantic_option"]["mode"],
            "strict"
        );

        let (_, converted, _, _) = upstream_request_for_api_format(
            "anthropic_messages",
            "/v1/chat/completions",
            &native,
            "claude-up",
        )
        .expect("advisory unknown cross-protocol option is omitted");
        assert!(
            serde_json::from_str::<serde_json::Value>(&converted).unwrap()
                ["future_semantic_option"]
                .is_null()
        );
    }

    #[test]
    fn optional_cache_control_does_not_block_cross_protocol_requests() {
        let body = serde_json::json!({
            "model": "claude-source",
            "max_tokens": 64,
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "hi",
                    "cache_control": {"type": "ephemeral"}
                }]
            }]
        })
        .to_string();
        let (_, converted, _, _) =
            upstream_request_for_api_format("openai_chat", "/v1/messages", &body, "gpt-target")
                .expect("Chat omits Anthropic cache control and continues");
        let payload: serde_json::Value = serde_json::from_str(&converted).unwrap();
        assert!(payload["messages"][0]["content"][0]["cache_control"].is_null());
    }

    #[test]
    fn chat_audio_uses_model_gated_codex_responses_extension() {
        let body = serde_json::json!({
            "model": "public-audio-model",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "input_audio",
                    "input_audio": {
                        "data": "AAAA",
                        "format": "wav"
                    }
                }]
            }]
        })
        .to_string();

        upstream_request_for_api_format(
            "openai_responses",
            "/v1/chat/completions",
            &body,
            "gpt-audio-preview",
        )
        .expect_err("public Responses audio stays closed without model evidence");

        let profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_responses".to_string(),
            model_pattern: "gpt-audio-preview".to_string(),
            input_modalities_authoritative: true,
            audio_input: true,
            release_status: "experimental".to_string(),
            verification_state: "declared".to_string(),
            verified_at_unix: 1,
            ..Default::default()
        }];
        let conversion = upstream_request_for_api_format_with_profiles_report(
            "openai_responses",
            "/v1/chat/completions",
            &body,
            "gpt-audio-preview",
            &profiles,
        )
        .expect("catalog-gated Codex audio conversion");
        let dialect = adapt_codex_subscription_params_for_operation(
            &conversion.body,
            false,
            "gpt-audio-preview",
        )
        .expect("Codex subscription dialect");
        let payload: serde_json::Value = serde_json::from_str(&dialect.body).unwrap();
        assert_eq!(
            payload["input"][0]["content"][0],
            serde_json::json!({
                "type": "input_audio",
                "audio_url": "data:audio/wav;base64,AAAA"
            })
        );
        assert!(dialect.faults.is_empty());
    }

    #[test]
    fn native_codex_audio_cannot_bypass_exact_model_capability_gate() {
        let body = serde_json::json!({
            "model": "gpt-text-only",
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_audio",
                    "audio_url": "data:audio/wav;base64,AAAA"
                }]
            }]
        })
        .to_string();
        let profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_responses".to_string(),
            model_pattern: "gpt-text-only".to_string(),
            input_modalities_authoritative: true,
            audio_input: false,
            release_status: "supported".to_string(),
            verification_state: "declared".to_string(),
            verified_at_unix: 1,
            ..Default::default()
        }];
        let error = match upstream_request_for_api_format_with_profiles_report(
            "openai_responses",
            "/v1/responses",
            &body,
            "gpt-text-only",
            &profiles,
        ) {
            Ok(_) => panic!("native private extension must still be model-gated"),
            Err(error) => error,
        };
        let payload: serde_json::Value =
            serde_json::from_str(&error.to_string()).expect("structured mismatch");
        assert_eq!(payload["feature"], "audio_input");
        assert_eq!(payload["protocol"], "openai_responses");
        assert_eq!(payload["target_protocol"], "openai_responses");

        let supported = vec![ChannelCapabilityProfile {
            audio_input: true,
            release_status: "experimental".to_string(),
            ..profiles[0].clone()
        }];
        upstream_request_for_api_format_with_profiles_report(
            "openai_responses",
            "/v1/responses",
            &body,
            "gpt-text-only",
            &supported,
        )
        .expect("exact supported evidence admits the reviewed native extension");
    }

    #[test]
    fn standard_file_input_ignores_client_host_evidence_without_losing_availability() {
        let body = serde_json::json!({
            "model": "gpt-file",
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_file",
                    "file_id": "file_123"
                }]
            }]
        })
        .to_string();
        let client_host_only = vec![ChannelCapabilityProfile {
            protocol: "openai_responses".to_string(),
            capability_layer: "client_host".to_string(),
            file_input: true,
            release_status: "supported".to_string(),
            verification_state: "declared".to_string(),
            ..Default::default()
        }];

        let conversion = upstream_request_for_api_format_with_profiles_report(
            "openai_responses",
            "/v1/responses",
            &body,
            "gpt-file",
            &client_host_only,
        )
        .expect("missing provider evidence remains an availability fallback");
        assert!(conversion.body.contains("\"input_file\""));
        assert_eq!(
            channel_profiles_required_request_features_rank(
                "openai_responses",
                "gpt-file",
                &client_host_only,
                &[crate::protocol::capability::Feature::FileInput],
            ),
            Some(6),
            "client-host support must not improve the provider route rank"
        );

        for layer in ["model_wire", "driver"] {
            let profiles = vec![ChannelCapabilityProfile {
                capability_layer: layer.to_string(),
                ..client_host_only[0].clone()
            }];
            let conversion = upstream_request_for_api_format_with_profiles_report(
                "openai_responses",
                "/v1/responses",
                &body,
                "gpt-file",
                &profiles,
            )
            .unwrap_or_else(|error| panic!("{layer} file evidence should route: {error}"));
            assert!(conversion.body.contains("\"input_file\""));
            assert!(conversion.body.contains("\"file_123\""));
            assert!(
                channel_profiles_required_request_features_rank(
                    "openai_responses",
                    "gpt-file",
                    &profiles,
                    &[crate::protocol::capability::Feature::FileInput],
                )
                .expect("routable positive evidence")
                    < 6
            );
        }

        let catalog_only = vec![ChannelCapabilityProfile {
            protocol: "openai_responses".to_string(),
            capability_layer: "model_wire".to_string(),
            model_pattern: "gpt-file".to_string(),
            image_input: true,
            // A positive catalog declaration is not an exhaustive rejection contract. Missing
            // file evidence therefore stays unknown and retains the lossless availability path.
            input_modalities_authoritative: false,
            verification_state: "declared".to_string(),
            ..Default::default()
        }];
        let conversion = upstream_request_for_api_format_with_profiles_report(
            "openai_responses",
            "/v1/responses",
            &body,
            "gpt-file",
            &catalog_only,
        )
        .expect("catalog omissions do not reject a native Responses file");
        assert!(conversion.body.contains("\"input_file\""));
        assert!(conversion.body.contains("\"file_123\""));
    }

    #[test]
    fn cross_protocol_file_input_uses_lossless_unknown_fallback_and_live_negative_suppression() {
        let body = serde_json::json!({
            "model": "gpt-file",
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_file",
                    "file_data": "data:application/pdf;base64,JVBERi0x"
                }]
            }]
        })
        .to_string();

        upstream_request_for_api_format_with_profiles_report(
            "anthropic_messages",
            "/v1/responses",
            &body,
            "claude-file",
            &[],
        )
        .expect("lossless target wire remains available when provider evidence is missing");

        let profiles = vec![ChannelCapabilityProfile {
            protocol: "anthropic_messages".to_string(),
            capability_layer: "driver".to_string(),
            file_input: true,
            verification_state: "declared".to_string(),
            ..Default::default()
        }];
        upstream_request_for_api_format_with_profiles_report(
            "anthropic_messages",
            "/v1/responses",
            &body,
            "claude-file",
            &profiles,
        )
        .expect("declared driver file support admits lossless conversion");

        let confirmed_negative = vec![ChannelCapabilityProfile {
            protocol: "anthropic_messages".to_string(),
            capability_layer: "driver".to_string(),
            model_pattern: "claude-file".to_string(),
            input_modalities_authoritative: true,
            file_input: false,
            verification_state: "verified".to_string(),
            verified_at_unix: now_unix(),
            ..Default::default()
        }];
        assert!(
            upstream_request_for_api_format_with_profiles_report(
                "anthropic_messages",
                "/v1/responses",
                &body,
                "claude-file",
                &confirmed_negative,
            )
            .is_err(),
            "exact live negative must block a known-bad conversion target"
        );
    }

    #[test]
    fn same_protocol_chat_file_passthrough_blocks_only_a_live_negative() {
        let body = serde_json::json!({
            "model": "chat-file",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "input_file",
                    "file": {"file_id": "file_123"}
                }]
            }]
        })
        .to_string();
        upstream_request_for_api_format_with_profiles_report(
            "openai_chat",
            "/v1/chat/completions",
            &body,
            "chat-file",
            &[],
        )
        .expect("unknown same-protocol fields preserve native availability");

        let profiles = vec![ChannelCapabilityProfile {
            protocol: "openai_chat".to_string(),
            capability_layer: "driver".to_string(),
            file_input: true,
            verification_state: "declared".to_string(),
            ..Default::default()
        }];
        upstream_request_for_api_format_with_profiles_report(
            "openai_chat",
            "/v1/chat/completions",
            &body,
            "chat-file",
            &profiles,
        )
        .expect("explicit compatible-dialect evidence admits Chat file input");

        let confirmed_negative = vec![ChannelCapabilityProfile {
            protocol: "openai_chat".to_string(),
            capability_layer: "driver".to_string(),
            model_pattern: "chat-file".to_string(),
            input_modalities_authoritative: true,
            file_input: false,
            verification_state: "verified".to_string(),
            verified_at_unix: now_unix(),
            ..Default::default()
        }];
        assert!(
            upstream_request_for_api_format_with_profiles_report(
                "openai_chat",
                "/v1/chat/completions",
                &body,
                "chat-file",
                &confirmed_negative,
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn local_proxy_preserves_structured_route_mismatches() {
        for (error_kind, issue_code, feature) in [
            (
                "capability_mismatch",
                "unsupported_required_feature",
                "audio_input",
            ),
            (
                "conversation_state_incompatible",
                "conversation_state_incompatible",
                "protocol_continuation",
            ),
        ] {
            let error = anyhow!(serde_json::json!({
                "error": format!("{issue_code}: route cannot preserve the request"),
                "error_kind": error_kind,
                "issue_code": issue_code,
                "feature": feature,
                "protocol": "openai_responses",
                "target_protocol": "openai_responses",
                "model": "gpt-text-only",
                "safe_to_retry_other_channel": true
            })
            .to_string());
            let response = proxy_execution_error_response(&error);
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(
                response
                    .headers()
                    .get("x-const-error-code")
                    .and_then(|value| value.to_str().ok()),
                Some(error_kind)
            );
            let body = crate::test_body_bytes(response.into_body())
                .await
                .expect("response body");
            let payload: serde_json::Value =
                serde_json::from_slice(&body).expect("structured response");
            assert_eq!(payload["error_kind"], error_kind);
            assert_eq!(payload["feature"], feature);
            assert_eq!(payload["safe_to_retry_other_channel"], true);
        }
    }

    #[tokio::test]
    async fn ambiguous_transport_is_terminal_and_exposes_no_replay_contract() {
        let error = crate::channel_executor::ambiguous_transport_error("outcome unknown");
        assert!(crate::upstream_transport::is_ambiguous_transport_error(
            &error
        ));
        assert!(!local_execution_error_marks_channel_unready(&error));

        let response = proxy_execution_error_response(&error);
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            response
                .headers()
                .get("x-const-error-code")
                .and_then(|value| value.to_str().ok()),
            Some("ambiguous_transport")
        );
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("response body");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("ambiguous response");
        assert_eq!(payload["error_kind"], "ambiguous_transport");
        assert_eq!(payload["safe_to_retry_other_channel"], false);
        assert_eq!(payload["safe_to_retry_same_channel"], false);
    }

    #[test]
    fn unsupported_required_media_returns_structured_retry_boundary() {
        let body = serde_json::json!({
            "contents": [{
                "role": "user",
                "parts": [{
                    "inlineData": {
                        "mimeType": "video/mp4",
                        "data": "AA=="
                    }
                }]
            }]
        })
        .to_string();

        let error = upstream_request_for_api_format(
            "openai_chat",
            "/v1beta/models/source-model:generateContent",
            &body,
            "target-model",
        )
        .expect_err("Gemini video cannot be represented on OpenAI Chat");
        let payload: serde_json::Value =
            serde_json::from_str(&error.to_string()).expect("structured capability mismatch");

        assert_eq!(payload["error_kind"], "capability_mismatch");
        assert_eq!(payload["issue_code"], "unsupported_required_feature");
        assert_eq!(payload["feature"], "video_input");
        assert_eq!(payload["protocol"], "gemini_native");
        assert_eq!(payload["target_protocol"], "openai_chat");
        assert_eq!(payload["safe_to_retry_other_channel"], true);
    }

    #[test]
    fn anthropic_tool_response_converts_to_chat_tool_calls() {
        let raw = serde_json::json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "call_1",
                "name": "read_file",
                "input": {"path": "README.md"}
            }],
            "usage": {"input_tokens": 3, "output_tokens": 4}
        })
        .to_string();
        let converted =
            response_body_from_target_as_inbound(&raw, "openai_chat", "anthropic_messages", "m")
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn chat_tool_response_converts_to_gemini_function_call() {
        let raw = serde_json::json!({
            "id": "chatcmpl_1",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\":\"README.md\"}"}
                    }]
                }
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3}
        })
        .to_string();
        let converted =
            response_body_from_target_as_inbound(&raw, "gemini_native", "openai_chat", "m")
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(
            value["candidates"][0]["content"]["parts"][0]["functionCall"]["name"],
            "read_file"
        );
        assert_eq!(
            value["candidates"][0]["content"]["parts"][0]["functionCall"]["args"]["path"],
            "README.md"
        );
    }

    #[test]
    fn malformed_chat_response_uses_adapter_validation() {
        let value = serde_json::json!({"id":"chatcmpl_invalid","choices":[]});
        let context = crate::protocol::adapters::AdapterContext::legacy_bridge("fallback-model");
        let error = crate::protocol::decode_response(
            crate::protocol::kind::ProtocolKind::OpenAiChat,
            &value,
            &context,
        )
        .unwrap_err();

        assert!(error.to_string().contains("response_choice_missing"));
    }

    #[test]
    fn malformed_responses_response_uses_adapter_validation() {
        let value = serde_json::json!({"id":"resp_invalid","output":{}});
        let context = crate::protocol::adapters::AdapterContext::legacy_bridge("fallback-model");
        let error = crate::protocol::decode_response(
            crate::protocol::kind::ProtocolKind::OpenAiResponses,
            &value,
            &context,
        )
        .unwrap_err();

        assert!(error.to_string().contains("response_output_invalid"));
    }

    #[test]
    fn malformed_anthropic_response_uses_adapter_validation() {
        let value = serde_json::json!({"id":"msg_invalid","content":{}});
        let context = crate::protocol::adapters::AdapterContext::legacy_bridge("fallback-model");
        let error = crate::protocol::decode_response(
            crate::protocol::kind::ProtocolKind::AnthropicMessages,
            &value,
            &context,
        )
        .unwrap_err();

        assert!(error.to_string().contains("response_content_invalid"));
    }

    #[test]
    fn malformed_gemini_response_uses_adapter_validation() {
        let value = serde_json::json!({"candidates":{}});
        let context = crate::protocol::adapters::AdapterContext::legacy_bridge("fallback-model");
        let error = crate::protocol::decode_response(
            crate::protocol::kind::ProtocolKind::GeminiNative,
            &value,
            &context,
        )
        .unwrap_err();

        assert!(error.to_string().contains("candidates_missing"));
    }

    #[test]
    fn anthropic_sse_buffers_to_chat_json_for_non_streaming_inbound() {
        let raw = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n"
        );
        let converted =
            sse_body_from_target_as_inbound(raw, "openai_chat", "anthropic_messages", "claude")
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(value["choices"][0]["message"]["content"], "hello");
        assert_eq!(value["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn gemini_sse_buffers_to_chat_json_for_non_streaming_inbound() {
        let raw = concat!(
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hello\"}]}}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"read_file\",\"args\":{\"path\":\"README.md\"}}}]},\"finishReason\":\"FUNCTION_CALL\"}],\"usageMetadata\":{\"totalTokenCount\":5}}\n\n"
        );
        let converted =
            sse_body_from_target_as_inbound(raw, "openai_chat", "gemini_native", "gemini-2.5-pro")
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(value["choices"][0]["message"]["content"], "hello");
        assert_eq!(
            value["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(value["usage"]["total_tokens"], 5);
    }

    #[test]
    fn canonical_response_converts_anthropic_to_gemini_without_chat_pivot() {
        let raw = serde_json::json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "reading"},
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "read_file",
                    "input": {"path": "README.md"}
                }
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 3, "output_tokens": 4}
        })
        .to_string();

        let converted = response_body_from_target_as_inbound(
            &raw,
            "gemini_native",
            "anthropic_messages",
            "claude",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();

        assert_eq!(
            value["candidates"][0]["content"]["parts"][0]["text"],
            "reading"
        );
        assert_eq!(
            value["candidates"][0]["content"]["parts"][1]["functionCall"]["name"],
            "read_file"
        );
        assert_eq!(
            value["candidates"][0]["content"]["parts"][1]["functionCall"]["args"]["path"],
            "README.md"
        );
    }

    #[test]
    fn cross_protocol_response_omissions_become_improvement_faults() {
        let raw = serde_json::json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "hello"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1},
            "future_response_field": {"semantic": true}
        })
        .to_string();

        let converted = response_body_from_target_as_inbound_with_report(
            &raw,
            "openai_chat",
            "anthropic_messages",
            "claude",
        )
        .unwrap();
        let faults = serde_json::to_value(converted.faults).unwrap();

        assert!(serde_json::from_str::<serde_json::Value>(&converted.body).is_ok());
        assert!(faults.as_array().is_some_and(|faults| faults
            .iter()
            .any(|fault| fault["code"] == "provider_extension_omitted"
                && fault["field_path"] == "$.future_response_field")));
    }

    #[test]
    fn canonical_event_stream_converts_anthropic_to_responses_without_pair_converter() {
        let raw = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":4}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let events =
            canonical_stream_events_from_target_sse(raw, "anthropic_messages", "claude").unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, crate::protocol::ir::CanonicalStreamEvent::TextDelta { text, .. } if text == "hello")));
        assert!(events.iter().any(|event| {
            matches!(event, crate::protocol::ir::CanonicalStreamEvent::BlockStart { block, .. }
                if block.kind == crate::protocol::ir::ContentBlockKind::ToolCall
                    && block.name.as_deref() == Some("read_file"))
        }));

        let converted =
            sse_from_canonical_stream_events(&events, "openai_responses", "claude").unwrap();
        assert!(converted.contains("response.output_text.delta"));
        assert!(converted.contains("response.function_call_arguments.delta"));
        assert!(!converted.contains("chat.completion.chunk"));

        let gemini = sse_from_canonical_stream_events(&events, "gemini_native", "claude").unwrap();
        assert_eq!(gemini.matches("\"functionCall\"").count(), 1, "{gemini}");
        assert!(gemini.contains("\"path\":\"README.md\""), "{gemini}");
    }

    #[test]
    fn canonical_event_stream_converts_gemini_to_chat_responses_and_anthropic() {
        let raw = concat!(
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hello\"}]}}]}\n\n",
            "data: {\"candidates\":[{\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":2,\"totalTokenCount\":3}}\n\n"
        );
        let events =
            canonical_stream_events_from_target_sse(raw, "gemini_native", "gemini").unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, crate::protocol::ir::CanonicalStreamEvent::TextDelta { text, .. } if text == "hello")));

        let chat = sse_from_canonical_stream_events(&events, "openai_chat", "gemini").unwrap();
        assert!(chat.contains("chat.completion.chunk"));
        assert!(chat.contains("hello"));

        let responses =
            sse_from_canonical_stream_events(&events, "openai_responses", "gemini").unwrap();
        assert!(responses.contains("response.output_text.delta"));
        assert!(responses.contains("hello"));

        let anthropic =
            sse_from_canonical_stream_events(&events, "anthropic_messages", "gemini").unwrap();
        assert!(anthropic.contains("content_block_delta"));
        assert!(anthropic.contains("hello"));
    }

    #[test]
    fn canonical_event_stream_renders_chat_responses_and_anthropic_to_gemini() {
        let chat_raw = concat!(
            "data: {\"id\":\"chatcmpl_1\",\"model\":\"gpt\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl_1\",\"model\":\"gpt\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        );
        let chat_events =
            canonical_stream_events_from_target_sse(chat_raw, "openai_chat", "gpt").unwrap();
        let gemini_from_chat =
            sse_from_canonical_stream_events(&chat_events, "gemini_native", "gemini").unwrap();
        assert!(gemini_from_chat.contains("\"candidates\""));
        assert!(gemini_from_chat.contains("\"text\":\"hello\""));
        assert!(gemini_from_chat.contains("\"finishReason\":\"STOP\""));

        let responses_raw = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n"
        );
        let responses_events =
            canonical_stream_events_from_target_sse(responses_raw, "openai_responses", "gpt")
                .unwrap();
        let gemini_from_responses =
            sse_from_canonical_stream_events(&responses_events, "gemini_native", "gemini").unwrap();
        assert!(gemini_from_responses.contains("\"text\":\"hi\""));
        assert!(gemini_from_responses.contains("\"finishReason\":\"STOP\""));

        let anthropic_raw = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hey\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let anthropic_events =
            canonical_stream_events_from_target_sse(anthropic_raw, "anthropic_messages", "claude")
                .unwrap();
        let gemini_from_anthropic =
            sse_from_canonical_stream_events(&anthropic_events, "gemini_native", "gemini").unwrap();
        assert!(gemini_from_anthropic.contains("\"text\":\"hey\""));
        assert!(gemini_from_anthropic.contains("\"finishReason\":\"STOP\""));
    }

    #[test]
    fn responses_sse_to_anthropic_sse_emits_incremental_chunks() {
        let mut converter = crate::protocol::stream::stream_converter(
            crate::protocol::kind::ProtocolKind::OpenAiResponses,
            crate::protocol::kind::ProtocolKind::AnthropicMessages,
            "gpt-5.5",
        );

        let start = converter.push(
            b"event: response.created\n\
             data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5.5\"}}\n\n",
        );
        let start = String::from_utf8_lossy(&start);
        assert!(start.contains("message_start"));

        let delta = converter.push(
            b"event: response.output_text.delta\n\
             data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
        );
        let delta = String::from_utf8_lossy(&delta);
        assert!(delta.contains("content_block_delta"));
        assert!(delta.contains("hello"));

        let done = converter.push(
            b"event: response.completed\n\
             data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5.5\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n",
        );
        let done = String::from_utf8_lossy(&done);
        assert!(done.contains("message_stop"));
        assert!(converter.finish().is_empty());
    }

    #[test]
    fn responses_sse_to_anthropic_sse_does_not_repeat_completed_output_text() {
        let mut converter = crate::protocol::stream::stream_converter(
            crate::protocol::kind::ProtocolKind::OpenAiResponses,
            crate::protocol::kind::ProtocolKind::AnthropicMessages,
            "gpt-5.5",
        );
        let text = "Hi! What would you like to work on?";
        let raw = format!(
            "event: response.output_text.delta\n\
             data: {{\"type\":\"response.output_text.delta\",\"delta\":{}}}\n\n\
             event: response.completed\n\
             data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"resp_1\",\"model\":\"gpt-5.5\",\"output\":[{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":{}}}]}}],\"usage\":{{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}}}}\n\n",
            serde_json::to_string(text).expect("delta text json"),
            serde_json::to_string(text).expect("completed text json")
        );

        let converted = converter.push(raw.as_bytes());
        let converted = String::from_utf8_lossy(&converted);
        assert_eq!(converted.matches(text).count(), 1, "{converted}");
        assert!(converted.contains("message_stop"), "{converted}");
        assert!(converter.finish().is_empty());
    }

    #[test]
    fn responses_sse_to_anthropic_sse_does_not_repeat_completed_tool_arguments() {
        let mut converter = crate::protocol::stream::stream_converter(
            crate::protocol::kind::ProtocolKind::OpenAiResponses,
            crate::protocol::kind::ProtocolKind::AnthropicMessages,
            "gpt-5.5",
        );
        let raw = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"item_id\":\"fc_1\",\"delta\":\"{\\\"path\\\":\\\"README.md\\\"}\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5.5\",\"output\":[{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}],\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n"
        );

        let converted = converter.push(raw.as_bytes());
        let converted = String::from_utf8_lossy(&converted);
        assert_eq!(
            converted.matches("\\\"path\\\":\\\"README.md\\\"").count(),
            1,
            "{converted}"
        );
        assert!(converted.contains("message_stop"), "{converted}");
        assert!(converter.finish().is_empty());
    }

    #[test]
    fn protocol_fallback_never_replays_post_after_an_ambiguous_http3_error() {
        let error = anyhow!("HTTP/3 connection was lost after request start");
        assert!(!http3_error_is_safe_to_fallback(
            &warp::http::Method::POST,
            &error
        ));
        assert!(http3_error_is_safe_to_fallback(
            &warp::http::Method::GET,
            &error
        ));
        let guarded = ambiguous_http3_request_error(error);
        assert!(is_ambiguous_http3_request_error(&guarded));
    }

    #[tokio::test]
    async fn platform_output_read_ahead_keeps_bursts_and_errors_before_tool_reads() {
        let read = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = read.clone();
        let source = futures_util::stream::iter(
            (0..4096).map(|index| Ok::<_, std::io::Error>(bytes::Bytes::from(index.to_string()))),
        )
        .inspect(move |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
        .chain(futures_util::stream::once(async {
            Err(std::io::Error::other("terminal transport failure"))
        }));
        let output = buffered_platform_output(source);
        futures_util::pin_mut!(output);
        tokio::time::timeout(Duration::from_secs(3), async {
            while read.load(std::sync::atomic::Ordering::Relaxed) != 4096 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("output should reach local buffer without tool reads");
        for index in 0..4096 {
            assert_eq!(output.next().await.unwrap().unwrap(), index.to_string());
        }
        assert_eq!(
            output.next().await.unwrap().unwrap_err().to_string(),
            "terminal transport failure"
        );
        assert!(output.next().await.is_none());
    }

    #[tokio::test]
    async fn platform_output_read_ahead_stops_idle_source_when_tool_disconnects() {
        let (source, incoming) =
            tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(1);
        let source_stream = futures_util::stream::unfold(incoming, |mut incoming| async move {
            incoming.recv().await.map(|item| (item, incoming))
        });
        let output = buffered_platform_output(source_stream);
        drop(output);
        tokio::time::timeout(Duration::from_secs(2), source.closed())
            .await
            .expect("cancel idle read-ahead after tool disconnect");
    }

    #[tokio::test]
    async fn platform_sse_transport_error_becomes_a_clean_terminal_event() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("text/event-stream"),
        );
        let stream = futures_util::stream::iter(vec![
            Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from_static(
                b"event: response.output_text.delta\ndata: {\"delta\":\"partial\"}\n\n",
            )),
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "simulated HTTP/3 idle close",
            )),
        ]);

        let response =
            response_from_platform_event_stream(warp::http::StatusCode::OK, &headers, stream)
                .expect("streaming response");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("clean downstream body");
        let body = String::from_utf8_lossy(&body);

        assert!(body.contains("partial"), "{body}");
        assert!(body.contains("platform_stream_interrupted"), "{body}");
        assert_eq!(body.matches("platform_stream_interrupted").count(), 1);
        assert_eq!(platform_request_timeout(true), None);
        assert_eq!(
            platform_request_timeout(false),
            Some(crate::MODEL_REQUEST_TIMEOUT)
        );
        assert!(platform_request_is_stream(
            "/v1/responses",
            "",
            br#"{"stream":true}"#
        ));
        assert!(platform_request_is_stream(
            "/gemini/v1beta/models/test:streamGenerateContent",
            "alt=sse",
            b"{}"
        ));
    }

    #[tokio::test]
    async fn platform_sse_transport_error_resumes_exactly_once_from_byte_offset() {
        let first = b"event: response.output_text.delta\ndata: {\"delta\":\"first\"}\n\n";
        let second = b"event: response.completed\ndata: {\"status\":\"completed\"}\n\n";
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind resumable platform");
        let address = listener.local_addr().expect("resumable platform address");
        let (captures_tx, captures_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut initial, _) = listener.accept().await.expect("initial request");
            let initial_request = read_raw_http_request(&mut initial).await;
            let stream_id = raw_header_values(&initial_request.headers, PLATFORM_STREAM_ID_HEADER)
                .into_iter()
                .next()
                .expect("initial stream id")
                .to_string();
            let initial_response_head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Const-Stream-Id: {stream_id}\r\nX-Const-Stream-Resumable: 1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            );
            initial
                .write_all(initial_response_head.as_bytes())
                .await
                .expect("write initial headers");
            initial
                .write_all(format!("{:x}\r\n", first.len()).as_bytes())
                .await
                .expect("write first chunk size");
            initial.write_all(first).await.expect("write first chunk");
            initial
                .write_all(b"\r\n")
                .await
                .expect("write first chunk trailer");
            initial
                .shutdown()
                .await
                .expect("interrupt initial response");

            let (mut resumed, _) = listener.accept().await.expect("resume request");
            let resumed_request = read_raw_http_request(&mut resumed).await;
            let resumed_response_head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Const-Stream-Id: {stream_id}\r\nX-Const-Stream-Resumable: 1\r\nX-Const-Stream-Resumed: 1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            );
            resumed
                .write_all(resumed_response_head.as_bytes())
                .await
                .expect("write resumed headers");
            resumed
                .write_all(format!("{:x}\r\n", second.len()).as_bytes())
                .await
                .expect("write second chunk size");
            resumed.write_all(second).await.expect("write second chunk");
            resumed
                .write_all(b"\r\n0\r\n\r\n")
                .await
                .expect("complete resumed response");
            captures_tx
                .send((initial_request, resumed_request))
                .expect("send captures");
        });

        let cfg = default_config();
        let endpoint = Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{address}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        };
        let response = forward_once(
            warp::http::Method::POST,
            "/v1/responses",
            "",
            false,
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"gpt-test","stream":true}"#),
            &Client::new(),
            &cfg,
            &endpoint,
            "sk-platform",
        )
        .await
        .expect("initial platform response");
        // The tool has not read any body bytes yet. Recovery must still retain
        // both portions locally and request only the missing suffix once.
        let (initial_request, resumed_request) =
            tokio::time::timeout(Duration::from_secs(3), captures_rx)
                .await
                .expect("read-ahead resume timeout")
                .expect("capture channel");
        let body = crate::test_body_bytes(response.into_body())
            .await
            .expect("resumed response body");

        let mut expected = first.to_vec();
        expected.extend_from_slice(second);
        assert_eq!(body.as_ref(), expected.as_slice());
        let initial_ids = raw_header_values(&initial_request.headers, PLATFORM_STREAM_ID_HEADER);
        let resumed_ids = raw_header_values(&resumed_request.headers, PLATFORM_STREAM_ID_HEADER);
        assert_eq!(initial_ids.len(), 1, "{}", initial_request.headers);
        assert_eq!(resumed_ids, initial_ids);
        let expected_offset = first.len().to_string();
        assert_eq!(
            raw_header_values(&resumed_request.headers, PLATFORM_STREAM_OFFSET_HEADER),
            vec![expected_offset.as_str()]
        );
        assert_eq!(initial_request.body, resumed_request.body);
    }

    async fn check_platform_resume_rejection(status: &str, code: &str, permanent: bool) {
        let first = b"event: response.output_text.delta\ndata: {\"delta\":\"first\"}\n\n";
        let last = b"event: response.completed\ndata: {}\n\n";
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let rejection = format!(
            "HTTP/1.1 {status}\r\nX-Const-Error-Code: {code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let initial = read_raw_http_request(&mut socket).await;
            let stream_id = raw_header_values(&initial.headers, PLATFORM_STREAM_ID_HEADER)[0].to_string();
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Const-Stream-Id: {stream_id}\r\nX-Const-Stream-Resumable: 1\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
                first.len()
            ).as_bytes()).await.unwrap();
            socket.write_all(first).await.unwrap();
            socket.write_all(b"\r\n").await.unwrap();
            socket.shutdown().await.unwrap(); // Deliberately omit the final chunk.
            let mut requests = vec![initial];
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_raw_http_request(&mut socket).await);
            socket.write_all(rejection.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            if !permanent {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.push(read_raw_http_request(&mut socket).await);
                socket.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Const-Stream-Id: {stream_id}\r\nX-Const-Stream-Resumable: 1\r\nX-Const-Stream-Resumed: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    last.len()
                ).as_bytes()).await.unwrap();
                socket.write_all(last).await.unwrap();
            }
            requests
        });
        let endpoint = Endpoint {
            server_id: String::new(),
            name: "platform".to_string(),
            base_url: format!("http://{address}"),
            supplier_ws_url: String::new(),
            supplier_quic_url: String::new(),
            enabled: true,
        };
        let response = forward_once(
            warp::http::Method::POST, "/v1/responses", "", false, HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"gpt-test","stream":true}"#),
            &Client::new(), &default_config(), &endpoint, "sk-platform",
        ).await.unwrap();
        let body = tokio::time::timeout(Duration::from_secs(8), crate::test_body_bytes(response.into_body()))
            .await.expect("permanent refusal must not wait through the 120-second resume window")
            .unwrap();
        let mut expected = first.to_vec();
        if permanent {
            expected.extend_from_slice(&platform_stream_resume_rejected_event(code));
        } else {
            expected.extend_from_slice(last);
        }
        assert_eq!(body.as_ref(), expected.as_slice());
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), if permanent { 2 } else { 3 });
        for retry in &requests[1..] {
            assert_eq!(retry.body, requests[0].body);
            assert_eq!(raw_header_values(&retry.headers, PLATFORM_STREAM_ID_HEADER),
                raw_header_values(&requests[0].headers, PLATFORM_STREAM_ID_HEADER));
            assert_eq!(raw_header_values(&retry.headers, PLATFORM_STREAM_OFFSET_HEADER),
                vec![first.len().to_string()]);
        }
    }

    #[test]
    fn responses_lite_tools_preserve_native_bytes_and_convert_function_declarations() {
        let body = r#"{ "model" : "gpt-up", "stream":true, "input":[{"type":"additional_tools","role":"developer","id":"at_stable","tools":[{"type":"function","name":"lookup","parameters":{"type":"object","properties":{}}}]},{"role":"user","content":"hello"}] }"#;
        let (_, native, _, _) = upstream_request_for_api_format("openai_responses", "/v1/responses", body, "gpt-up").unwrap();
        assert_eq!(native, body);
        for target in ["openai_chat", "anthropic_messages", "gemini_native"] {
            let (_, converted, _, _) = upstream_request_for_api_format(target, "/v1/responses", body, "target-model").unwrap();
            assert!(converted.contains("lookup"), "{target}: {converted}");
            assert!(!converted.contains("additional_tools"), "{target}: {converted}");
        }
    }

    #[test]
    fn responses_lite_custom_namespace_roundtrips_json_stream_and_history() {
        let body = serde_json::json!({"model":"gpt-test","input":[
            {"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions","tools":[{"type":"custom","name":"apply_patch","format":{"type":"grammar","syntax":"lark","definition":"start: /.+/"}}]}]},
            {"role":"user","content":"hello"}
        ]}).to_string();
        let input = "*** Begin Patch\n*** Add File: 测试.txt\n+hello \\\"world\\\"\n*** End Patch";
        for target in ["openai_chat", "anthropic_messages", "gemini_native"] {
            let conversion = upstream_request_for_api_format_with_profiles_report(target,"/v1/responses",&body,"target-model", &[]).unwrap();
            assert!(conversion.body.contains("ns9_functions_apply_patch"), "{target}");
            let args = serde_json::json!({"input":input});
            let upstream = match target {
                "openai_chat" => serde_json::json!({"id":"resp_tool","choices":[{"message":{"role":"assistant","tool_calls":[{"id":"call_patch","type":"function","function":{"name":"ns9_functions_apply_patch","arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]}),
                "anthropic_messages" => serde_json::json!({"id":"msg_tool","type":"message","role":"assistant","content":[{"type":"tool_use","id":"call_patch","name":"ns9_functions_apply_patch","input":args}],"stop_reason":"tool_use"}),
                _ => serde_json::json!({"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"id":"call_patch","name":"ns9_functions_apply_patch","args":args}}]},"finishReason":"STOP"}]}),
            }.to_string();
            let converted = response_body_from_target_as_inbound_with_report_with_tools(&upstream, "openai_responses", target, "target-model", &conversion.tool_mapping).unwrap();
            let response: serde_json::Value = serde_json::from_str(&converted.body).unwrap();
            let call = response["output"].as_array().unwrap().iter().find(|item| item["type"] == "custom_tool_call").unwrap();
            assert_eq!(call["name"], "apply_patch", "{target}");
            assert_eq!(call["namespace"], "functions", "{target}");
            assert_eq!(call["input"], input, "{target}");
            let mut next: serde_json::Value = serde_json::from_str(&body).unwrap();
            next["input"].as_array_mut().unwrap().extend([
                call.clone(), serde_json::json!({"type":"custom_tool_call_output","call_id":call["call_id"],"output":"Done"}),
            ]);
            let next = upstream_request_for_api_format_with_profiles_report(target, "/v1/responses", &next.to_string(), "target-model", &[]).unwrap();
            assert!(next.body.contains("ns9_functions_apply_patch"), "history {target}");
            assert!(!next.body.contains("custom_tool_call_output"), "history {target}");

            let source_sse = sse_body_from_target_body_as_inbound(&upstream, target, target, "target-model").unwrap();
            let mut converter = inbound_sse_stream_converter("openai_responses", target, "target-model").unwrap().with_tool_mapping(conversion.tool_mapping.clone());
            let mut output = Vec::new();
            for chunk in source_sse.as_bytes().chunks(7) { output.extend_from_slice(&converter.push(chunk)); }
            output.extend_from_slice(&converter.finish());
            assert!(converter.failure().is_none(), "{target}: {:?}", converter.failure());
            let events = crate::protocol::stream::decode_sse_frames(&output).unwrap();
            let done = events.iter().filter_map(|(_, data)| serde_json::from_str::<serde_json::Value>(data).ok())
                .find(|event| event["type"] == "response.completed").unwrap();
            let streamed_call = done["response"]["output"].as_array().unwrap().iter().find(|item| item["type"] == "custom_tool_call").unwrap();
            assert_eq!(streamed_call["input"], input, "stream {target}");
            assert_eq!(streamed_call["namespace"], "functions", "stream {target}");

            let malformed = upstream.replace(&serde_json::to_string(input).unwrap(), "42");
            // A malformed wrapper must not be emitted as an executable raw tool call.
            if malformed != upstream {
                assert!(response_body_from_target_as_inbound_with_report_with_tools(&malformed, "openai_responses", target, "target-model", &conversion.tool_mapping).is_err());
            }
        }
    }

    #[tokio::test]
    async fn platform_sse_resume_stops_when_replay_state_is_unavailable() {
        check_platform_resume_rejection("409 Conflict", "stream_resume_unavailable", true).await;
    }

    #[test]
    fn platform_resume_rejection_classification_preserves_transient_and_legacy_retries() {
        for (status, code, stop) in [
            (409, "stream_resume_unavailable", true),
            (409, "stream_resume_offset_unavailable", true),
            (400, "invalid_stream_resume_offset", true),
            (401, "", true), (403, "", true),
            (409, "", false), (400, "unknown_future_code", false),
            (408, "", false), (429, "", false),
            (503, "runtime_store_unavailable", false),
        ] {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(PLATFORM_ERROR_CODE_HEADER, code.parse().unwrap());
            assert_eq!(platform_resume_rejection_reason(reqwest::StatusCode::from_u16(status).unwrap(), &headers).is_some(), stop, "{status} {code}");
        }
    }

    #[tokio::test]
    async fn platform_sse_resume_retries_temporary_store_failure_without_reexecuting() {
        check_platform_resume_rejection("503 Service Unavailable", "runtime_store_unavailable", false).await;
    }

    #[test]
    fn lan_share_metering_uses_the_route_billing_class() {
        let model_body = br#"{"model":"gpt-test","input":"hello"}"#;

        assert!(lan_share_metering_for_request(
            &warp::http::Method::POST,
            "/v1/responses",
            model_body,
        )
        .is_some());
        assert!(lan_share_metering_for_request(
            &warp::http::Method::POST,
            "/v1/embeddings",
            model_body,
        )
        .is_some());
        assert!(lan_share_metering_for_request(
            &warp::http::Method::POST,
            "/v1/responses/input_tokens",
            model_body,
        )
        .is_none());
        assert!(lan_share_metering_for_request(
            &warp::http::Method::POST,
            "/anthropic/v1/messages/count_tokens",
            model_body,
        )
        .is_none());
        assert!(lan_share_metering_for_request(
            &warp::http::Method::POST,
            "/gemini/v1beta/models/gemini-test:countTokens",
            br#"{"contents":[]}"#,
        )
        .is_none());
    }
