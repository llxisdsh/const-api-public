fn protocol_test_models() -> Vec<ToolModelInfo> {
    crate::tool_model_metadata::tool_models_from_response(&serde_json::json!({"data":[
        {"id":"native-responses","const_api":{"native_protocols":["openai_responses"]}},
        {"id":"native-messages","const_api":{"native_protocols":["anthropic_messages"],"reasoning":true,"reasoning_efforts":["high"]}},
        {"id":"native-gemini","const_api":{"native_protocols":["gemini_native"]}},
        {"id":"old-catalog-model"}
    ]}))
}

#[test]
fn per_model_protocol_uses_metadata_not_names_and_respects_tool_capabilities() {
    let mut model = ToolModelInfo {
        id: "claude-name-does-not-prove-protocol".into(),
        ..Default::default()
    };
    assert_eq!(
        tool_model_protocol("vscode", &model, ToolProtocol::OpenAiChat),
        ToolProtocol::OpenAiChat
    );
    model.native_protocols = vec!["openai_responses".into(), "anthropic_messages".into()];
    model.preferred_protocol = Some("anthropic_messages".into());
    assert_eq!(
        tool_model_protocol("vscode", &model, ToolProtocol::OpenAiResponses),
        ToolProtocol::AnthropicMessages
    );
    assert_eq!(
        tool_model_protocol("trae-cn", &model, ToolProtocol::OpenAiChat),
        ToolProtocol::AnthropicMessages
    );
    model.preferred_protocol = Some("future-wire".into());
    assert_eq!(
        tool_model_protocol("vscode", &model, ToolProtocol::OpenAiResponses),
        ToolProtocol::OpenAiResponses
    );
    model.native_protocols = vec!["gemini_native".into()];
    assert_eq!(
        tool_model_protocol("vscode", &model, ToolProtocol::OpenAiChat),
        ToolProtocol::OpenAiChat
    );
    assert_eq!(
        tool_model_protocol("opencode", &model, ToolProtocol::OpenAiResponses),
        ToolProtocol::GeminiNative
    );
}

#[test]
fn all_per_model_tools_prefer_catalog_identity_only_with_native_support() {
    let models = crate::tool_model_metadata::tool_models_from_response(
        &serde_json::json!({"data":[
            {"id":"claude-opus-4-6","const_api":{"native_protocols":["openai_chat","openai_responses","anthropic_messages"],"preferred_protocol":"openai_responses"}},
            {"id":"gpt-6-luna","const_api":{"native_protocols":["openai_chat","openai_responses"],"preferred_protocol":"openai_chat"}},
            {"id":"gemini-3.1-pro-preview","const_api":{"native_protocols":["openai_chat","gemini_native"],"preferred_protocol":"openai_chat"}}
        ]}),
    );
    let find = |id| models.iter().find(|model| model.id == id).unwrap();
    for tool in [
        "opencode",
        "mimocode",
        "openscience",
        "openclaw",
        "pi",
        "vscode",
        "copilot-desktop",
        "grok-build",
        "trae",
        "trae-cn",
        "trae-work",
    ] {
        let fallback = ToolProtocol::OpenAiChat;
        let claude_protocol = if tool_supports_protocol(tool, ToolProtocol::AnthropicMessages) {
            ToolProtocol::AnthropicMessages
        } else {
            ToolProtocol::OpenAiResponses
        };
        assert_eq!(
            tool_model_protocol(tool, find("claude-opus-4-6"), fallback),
            claude_protocol,
            "{tool}"
        );
        assert_eq!(
            tool_model_protocol(tool, find("gpt-6-luna"), fallback),
            ToolProtocol::OpenAiResponses,
            "{tool}"
        );
        let google = if tool_supports_protocol(tool, ToolProtocol::GeminiNative) {
            ToolProtocol::GeminiNative
        } else {
            fallback
        };
        assert_eq!(
            tool_model_protocol(tool, find("gemini-3.1-pro-preview"), fallback),
            google,
            "{tool}"
        );
        let mut constrained = find("claude-opus-4-6").clone();
        constrained.native_protocols = vec!["openai_chat".into()];
        assert_eq!(
            tool_model_protocol(tool, &constrained, fallback),
            fallback,
            "catalog identity cannot invent native support: {tool}"
        );
        constrained.native_protocols.clear();
        assert_eq!(tool_model_protocol(tool, &constrained, fallback), fallback);
    }
    let disguised = ToolModelInfo {
        id: "claude-opus-future".into(),
        native_protocols: vec!["openai_chat".into(), "anthropic_messages".into()],
        preferred_protocol: Some("openai_chat".into()),
        ..Default::default()
    };
    assert_eq!(
        tool_model_protocol("trae", &disguised, ToolProtocol::OpenAiChat),
        ToolProtocol::OpenAiChat,
        "unknown names are not vendor evidence"
    );
}

#[test]
fn opencode_fork_protocols_match_their_real_reader_and_restore() {
    with_temp_home(|_| {
        let models = protocol_test_models();
        for (tool, path) in [
            ("mimocode", mimocode_config_path().unwrap()),
            ("openscience", openscience_config_path().unwrap()),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = r#"{"provider":{"mine":{"name":"Keep"}},"keep":true}"#;
            fs::write(&path, original).unwrap();
            apply_additional_tool_config_with_model_info_for_protocol(
                tool,
                TOOL_CONFIG_ROOT_URL,
                "mock",
                &models,
                ToolProtocol::OpenAiChat,
            )
            .unwrap();
            let raw = fs::read_to_string(&path).unwrap();
            let root: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let provider = &root["provider"]["const-api"];
            assert!(provider["options"].get("baseURL").is_none());
            assert_eq!(
                provider["models"]["native-responses"]["provider"]["npm"],
                "@ai-sdk/openai"
            );
            if tool == "mimocode" {
                assert_eq!(
                    provider["models"]["native-messages"]["provider"]["api"],
                    "http://127.0.0.1:38787/anthropic/v1"
                );
                assert_eq!(
                    provider["models"]["native-gemini"]["provider"]["npm"],
                    "@ai-sdk/google"
                );
            } else {
                assert_eq!(
                    provider["models"]["native-messages"]["provider"]["npm"],
                    "@ai-sdk/openai-compatible"
                );
                assert!(
                    provider["models"]["native-responses"]["provider"]
                        .get("api")
                        .is_none()
                );
            }
            assert!(
                check_additional_tool_config(tool, TOOL_CONFIG_ROOT_URL, "mock")
                    .unwrap()
                    .already_configured
            );
            assert!(
                !check_additional_tool_config(tool, "http://127.0.0.1:40000", "mock")
                    .unwrap()
                    .already_configured
            );
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                raw,
                "checks are read-only"
            );
            assert!(
                apply_additional_tool_config_with_model_info_for_protocol(
                    tool,
                    TOOL_CONFIG_ROOT_URL,
                    "mock",
                    &models,
                    ToolProtocol::OpenAiChat
                )
                .unwrap()
                .files
                .is_empty()
            );
            restore_tool_config_from_manifest(tool).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
    });
}

#[test]
fn kimi_uses_only_its_supported_anthropic_model_override() {
    with_temp_home(|_| {
        for fallback in [ToolProtocol::OpenAiResponses, ToolProtocol::OpenAiChat] {
            apply_kimicode_config(
                TOOL_CONFIG_ROOT_URL,
                "mock",
                &protocol_test_models(),
                fallback,
            )
            .unwrap();
            let raw = fs::read_to_string(kimicode_config_path()).unwrap();
            let doc = raw.parse::<DocumentMut>().unwrap();
            assert_eq!(
                doc["providers"]["const-api"]["type"].as_str(),
                Some(kimi_provider_type(fallback).unwrap())
            );
            let entry = &doc["models"]["const-api/native-messages"];
            assert_eq!(entry["protocol"].as_str(), Some("anthropic"));
            assert_eq!(
                entry["base_url"].as_str(),
                Some("http://127.0.0.1:38787/anthropic")
            );
            for id in ["native-responses", "native-gemini", "old-catalog-model"] {
                assert!(
                    doc["models"][&format!("const-api/{id}")]
                        .get("protocol")
                        .is_none()
                );
            }
            assert!(
                check_kimicode_config(TOOL_CONFIG_ROOT_URL, "mock")
                    .unwrap()
                    .already_configured
            );
            assert!(
                !check_kimicode_config("http://127.0.0.1:40000", "mock")
                    .unwrap()
                    .already_configured
            );
            assert_eq!(fs::read_to_string(kimicode_config_path()).unwrap(), raw);
            restore_tool_config_from_manifest("kimicode").unwrap();
        }
    });
}

#[test]
fn vscode_mixed_protocols_remain_configured_without_flattening_models() {
    let models = protocol_test_models();
    let provider = vscode_const_api_provider(
        TOOL_CONFIG_ROOT_URL,
        "mock-key",
        &models,
        ToolProtocol::OpenAiResponses,
    )
    .unwrap();
    let find = |id: &str| {
        provider["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .unwrap()
    };
    assert_eq!(find("native-messages")["apiType"], "messages");
    assert_eq!(
        find("native-messages")["url"],
        "http://127.0.0.1:38787/anthropic/v1/messages"
    );
    assert_eq!(
        find("native-responses")["url"],
        "http://127.0.0.1:38787/v1/responses"
    );
    assert_eq!(
        find("native-gemini")["apiType"],
        "responses",
        "unsupported native protocol uses the selected fallback"
    );
    let (checked, configured) = vscode_const_api_provider_for_status(
        Some(&provider),
        TOOL_CONFIG_ROOT_URL,
        "mock-key",
        ToolProtocol::OpenAiResponses,
    )
    .unwrap();
    assert!(configured);
    assert_eq!(provider, checked);
    assert!(
        vscode_model_entry(
            &models[0],
            "http://127.0.0.1",
            "mock-key",
            ToolProtocol::GeminiNative
        )
        .is_err()
    );
}

#[test]
fn model_writers_use_their_own_sdk_url_contracts() {
    let models = protocol_test_models();
    let provider = opencode_const_api_provider(
        TOOL_CONFIG_ROOT_URL,
        "mock-key",
        &models,
        ToolProtocol::OpenAiResponses,
    );
    assert!(provider["options"].get("baseURL").is_none());
    assert_eq!(
        provider["models"]["native-messages"]["variants"]["high"],
        serde_json::json!({"effort":"high"})
    );
    assert_eq!(
        provider["models"]["native-messages"]["provider"],
        serde_json::json!({"npm":"@ai-sdk/anthropic","api":"http://127.0.0.1:38787/anthropic/v1"})
    );
    assert_eq!(
        provider["models"]["native-gemini"]["provider"],
        serde_json::json!({"npm":"@ai-sdk/google","api":"http://127.0.0.1:38787/gemini/v1beta"})
    );
    let mut root = serde_json::json!({});
    upsert_pi_config(
        &mut root,
        TOOL_CONFIG_ROOT_URL,
        "mock-key",
        Some(&models),
        ToolProtocol::OpenAiResponses,
    )
    .unwrap();
    let entries = root["providers"]["const-api"]["models"].as_array().unwrap();
    let anthropic = entries
        .iter()
        .find(|model| model["id"] == "native-messages")
        .unwrap();
    assert_eq!(anthropic["api"], "anthropic-messages");
    assert_eq!(anthropic["baseUrl"], "http://127.0.0.1:38787/anthropic");
    let gemini = entries
        .iter()
        .find(|model| model["id"] == "native-gemini")
        .unwrap();
    assert_eq!(gemini["baseUrl"], "http://127.0.0.1:38787/gemini/v1beta");
    let before = root.clone();
    upsert_pi_config(
        &mut root,
        TOOL_CONFIG_ROOT_URL,
        "mock-key",
        None,
        ToolProtocol::OpenAiResponses,
    )
    .unwrap();
    assert_eq!(root, before);
    let claw = openclaw_model_entries(&models, TOOL_CONFIG_ROOT_URL, ToolProtocol::OpenAiResponses);
    let anthropic = claw
        .iter()
        .find(|model| model["id"] == "native-messages")
        .unwrap();
    assert_eq!(anthropic["api"], "anthropic-messages");
    assert_eq!(anthropic["baseUrl"], "http://127.0.0.1:38787/anthropic");
}

#[test]
fn opencode_mixed_protocols_keep_preview_and_cancel_stable() {
    with_temp_home(|_| {
        let path = opencode_config_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "{\"provider\":{\"user-provider\":{\"name\":\"Keep me\"}}}";
        fs::write(&path, original).unwrap();
        apply_opencode_config_with_model_info_for_protocol(
            TOOL_CONFIG_ROOT_URL,
            "mock-key",
            &protocol_test_models(),
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        assert!(
            check_opencode_config(TOOL_CONFIG_ROOT_URL, "mock-key")
                .unwrap()
                .already_configured
        );
        assert!(
            !check_opencode_config("http://127.0.0.1:40000", "mock-key")
                .unwrap()
                .already_configured
        );
        remove_opencode_config(TOOL_CONFIG_ROOT_URL, "mock-key").unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    });
}

#[test]
fn versioned_sdk_urls_keep_status_and_cancel_stable_after_key_rotation() {
    with_temp_home(|_| {
        for protocol in [ToolProtocol::AnthropicMessages, ToolProtocol::GeminiNative] {
            apply_opencode_config_with_model_info_for_protocol(
                TOOL_CONFIG_ROOT_URL,
                "old-key",
                &protocol_test_models(),
                protocol,
            )
            .unwrap();
            assert!(
                check_opencode_config(TOOL_CONFIG_ROOT_URL, "old-key")
                    .unwrap()
                    .already_configured
            );
            remove_opencode_config(TOOL_CONFIG_ROOT_URL, "new-key").unwrap();
            let root =
                read_json_or_default(&opencode_config_path().unwrap(), serde_json::json!({}))
                    .unwrap();
            assert!(root["provider"].get(CODEX_CONST_API_PROVIDER_ID).is_none());

            apply_openclaw_config_with_model_info_for_protocol(
                TOOL_CONFIG_ROOT_URL,
                "old-key",
                &protocol_test_models(),
                protocol,
            )
            .unwrap();
            let root =
                read_json_or_default(&openclaw_config_path().unwrap(), serde_json::json!({}))
                    .unwrap();
            let models = root["models"]["providers"][CODEX_CONST_API_PROVIDER_ID]["models"]
                .as_array()
                .unwrap();
            let gemini = models
                .iter()
                .find(|model| model["id"] == "native-gemini")
                .unwrap();
            assert_eq!(gemini["baseUrl"], "http://127.0.0.1:38787/gemini/v1beta");
            let messages = models
                .iter()
                .find(|model| model["id"] == "native-messages")
                .unwrap();
            assert_eq!(messages["baseUrl"], "http://127.0.0.1:38787/anthropic");
            assert!(
                check_openclaw_config(TOOL_CONFIG_ROOT_URL, "old-key")
                    .unwrap()
                    .already_configured
            );
            remove_openclaw_config(TOOL_CONFIG_ROOT_URL, "new-key").unwrap();
            let root =
                read_json_or_default(&openclaw_config_path().unwrap(), serde_json::json!({}))
                    .unwrap();
            assert!(
                root["models"]["providers"]
                    .get(CODEX_CONST_API_PROVIDER_ID)
                    .is_none()
            );
        }
    });
}
