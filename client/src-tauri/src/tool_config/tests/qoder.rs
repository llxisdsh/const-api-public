#[test]
fn qoder_editions_use_separate_settings_and_emit_native_protocol_labels() {
    with_temp_home(|_| {
        let models = capability_test_models();
        let international_before = serde_json::json!({
            "enabledPlugins": {"example": true},
            "providers": {"user-provider": {"baseUrl": "https://example.invalid"}}
        });
        fs::create_dir_all(qoder_settings_path("qoder").unwrap().parent().unwrap()).unwrap();
        fs::write(
            qoder_settings_path("qoder").unwrap(),
            json_text(&international_before).unwrap(),
        )
        .unwrap();

        let applied = apply_qoder_config(
            "qoder",
            TOOL_CONFIG_ROOT_URL,
            "mock-key",
            &models,
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        assert_eq!(applied.details.get("tool_protocol").map(String::as_str), Some("openai_responses"));
        let international = read_json_or_default(
            &qoder_settings_path("qoder").unwrap(),
            serde_json::json!({}),
        )
        .unwrap();
        let provider = &international["providers"]["const-api"];
        let international_model = provider["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["model"] == "gpt-6-astra")
            .unwrap();
        assert_eq!(provider["baseUrl"], "http://127.0.0.1:38787/v1");
        assert_eq!(provider["type"], "openai-compatible");
        assert_eq!(provider["protocol"], "openai-responses");
        assert_eq!(provider["authType"], "bearer");
        assert_eq!(international_model["contextWindow"], 272_000);
        assert_eq!(international_model["maxOutputTokens"], 128_000);
        assert_eq!(international_model["capabilities"]["vision"], true);
        assert_eq!(international_model["capabilities"]["thinking"]["modes"], serde_json::json!(["enabled"]));
        assert_eq!(
            international_model["capabilities"]["thinking"]["supportedEffortLevels"],
            serde_json::json!(["low", "medium", "high", "xhigh", "max"])
        );
        assert!(check_qoder_config("qoder", TOOL_CONFIG_ROOT_URL, "mock-key")
            .unwrap()
            .already_configured);
        assert!(!check_qoder_config("qoder", TOOL_CONFIG_ROOT_URL, "rotated-key")
            .unwrap()
            .already_configured);

        let cn_models = models[..2].to_vec();
        apply_qoder_config(
            "qoder-cn",
            TOOL_CONFIG_ROOT_URL,
            "cn-key",
            &cn_models,
            ToolProtocol::AnthropicMessages,
        )
        .unwrap();
        let cn = read_json_or_default(
            &qoder_settings_path("qoder-cn").unwrap(),
            serde_json::json!({}),
        )
        .unwrap();
        let cn_provider = &cn["providers"]["const-api"];
        let cn_model = cn_provider["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["model"] == "gpt-6-astra")
            .unwrap();
        assert_eq!(cn_provider["baseUrl"], "http://127.0.0.1:38787/anthropic");
        assert_eq!(cn_provider["protocol"], "anthropic");
        assert_eq!(cn_provider["authType"], "api-key");
        assert_eq!(cn_model["capabilities"]["thinking"]["modes"], serde_json::json!(["adaptive"]));
        assert_eq!(
            cn_model["capabilities"]["thinking"]["supportedEffortLevels"],
            serde_json::json!(["low", "medium", "high", "xhigh", "max"])
        );
        assert!(qoder_settings_path("qoder").unwrap() != qoder_settings_path("qoder-cn").unwrap());

        restore_tool_config_from_manifest("qoder").unwrap();
        assert_eq!(
            read_json_or_default(&qoder_settings_path("qoder").unwrap(), serde_json::json!({}))
                .unwrap(),
            international_before
        );
    });
}

#[test]
fn qoder_preview_is_write_free_and_preserves_existing_provider_identity() {
    with_temp_home(|_| {
        let path = qoder_settings_path("qoder").unwrap();
        let existing = serde_json::json!({
            "providers": {
                "qoder-custom-user": {
                    "baseUrl": "http://127.0.0.1:38787/v1",
                    "apiKey": "old-key",
                    "type": "openai-compatible",
                    "protocol": "openai-responses",
                    "authType": "bearer",
                    "model": "old-model",
                    "models": [{"model": "old-model"}],
                    "userField": "keep"
                }
            }
        });
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, json_text(&existing).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        let preview = preview_tool_config_apply(|| {
            apply_qoder_config(
                "qoder",
                TOOL_CONFIG_ROOT_URL,
                "new-key",
                &capability_test_models()[..2],
                ToolProtocol::OpenAiResponses,
            )
        })
        .unwrap();
        assert!(preview.files.is_empty());
        assert_eq!(fs::read(&path).unwrap(), before);

        apply_qoder_config(
            "qoder",
            TOOL_CONFIG_ROOT_URL,
            "new-key",
            &capability_test_models()[..2],
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        let current = read_json_or_default(&path, serde_json::json!({})).unwrap();
        assert!(current["providers"]["qoder-custom-user"].is_object());
        assert_eq!(current["providers"]["qoder-custom-user"]["apiKey"], "new-key");
        assert_eq!(current["providers"]["qoder-custom-user"]["userField"], "keep");
    });
}
