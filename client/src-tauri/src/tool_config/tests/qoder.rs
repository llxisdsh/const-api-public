#[test]
fn qoder_cleanup_recognizes_its_old_provider_and_preserves_an_active_other_profile() {
    with_temp_home(|_| {
        let models = capability_test_models();
        for tool in ["qoder", "qoder-cn"] {
            let path = qoder_settings_path(tool).unwrap();
            let empty = ToolConfigManifest::default();
            apply_qoder_config(
                tool,
                TOOL_CONFIG_ROOT_URL,
                "fixture-one",
                &models,
                ToolProtocol::OpenAiResponses,
            )
            .unwrap();
            let first = load_tool_config_manifest().unwrap();
            let mut saved = read_json_or_default(&path, serde_json::Value::Null).unwrap();
            saved["providers"]["personal"] =
                serde_json::json!({"type":"custom","baseUrl":"https://example.invalid"});
            fs::write(&path, json_text(&saved).unwrap()).unwrap();
            save_tool_config_manifest(&empty).unwrap();
            remove_qoder_config(tool, TOOL_CONFIG_ROOT_URL, "fixture-one").unwrap();
            let cleaned = read_json_or_default(&path, serde_json::Value::Null).unwrap();
            assert!(cleaned["providers"].get("const-api").is_none());
            assert_eq!(
                cleaned["providers"]["personal"],
                saved["providers"]["personal"]
            );

            // A newer profile can reuse the same key in the providers map.
            saved["providers"]["const-api"]["apiKey"] = serde_json::json!("fixture-two");
            for local_key in ["fixture-one", "rotated-local-key"] {
                fs::write(&path, json_text(&saved).unwrap()).unwrap();
                save_tool_config_manifest(&first).unwrap();
                remove_qoder_config(tool, TOOL_CONFIG_ROOT_URL, local_key).unwrap();
                assert_eq!(
                    read_json_or_default(&path, serde_json::Value::Null).unwrap(),
                    saved,
                    "releasing the old receipt must preserve the active provider even after a key reset"
                );
            }
        }
    });
}

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

#[test]
fn qoder_reapply_repairs_a_provider_with_one_unsupported_model_effort() {
    with_temp_home(|_| {
        let path = qoder_settings_path("qoder").unwrap();
        let old = serde_json::json!({
            "providers": {"const-api": {
                "type": "openai-compatible", "protocol": "openai-responses", "authType": "bearer",
                "baseUrl": "http://127.0.0.1:38787/v1", "apiKey": "fixture",
                "model": "first", "models": [
                    {"model": "first"},
                    {"model": "second", "capabilities": {"thinking": {
                        "modes": ["enabled"], "supportsEffort": true,
                        "supportedEffortLevels": ["minimal", "high"]
                    }}}
                ]
            }}
        });
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, json_text(&old).unwrap()).unwrap();
        let check = || check_qoder_config("qoder", TOOL_CONFIG_ROOT_URL, "fixture").unwrap();
        let stale = check();
        assert!(!stale.already_configured);
        assert_eq!(stale.details["has_managed_config"], "true");
        assert_eq!(
            read_json_or_default(&path, serde_json::Value::Null).unwrap(),
            old
        );

        let models = [
            ToolModelInfo {
                id: "first".into(),
                reasoning: true,
                reasoning_efforts: vec!["minimal".into()],
                ..Default::default()
            },
            ToolModelInfo {
                id: "second".into(),
                reasoning: true,
                reasoning_efforts: vec!["minimal".into(), "high".into()],
                ..Default::default()
            },
        ];
        for protocol in [ToolProtocol::OpenAiResponses, ToolProtocol::OpenAiChat] {
            apply_additional_tool_config_with_model_info_for_protocol(
                "qoder",
                TOOL_CONFIG_ROOT_URL,
                "fixture",
                &models,
                protocol,
            )
            .unwrap();
            let settings = read_json_or_default(&path, serde_json::Value::Null).unwrap();
            let written = settings["providers"]["const-api"]["models"]
                .as_array()
                .unwrap();
            assert_eq!(written.len(), 2);
            assert_eq!(written[0]["model"], "first");
            let first = &written[0]["capabilities"]["thinking"];
            assert_eq!(first["modes"], serde_json::json!(["enabled"]));
            assert_eq!(first["supportsEffort"], false);
            assert_eq!(first["supportedEffortLevels"], serde_json::json!([]));
            assert_eq!(written[1]["model"], "second");
            let second = &written[1]["capabilities"]["thinking"];
            assert_eq!(second["supportsEffort"], true);
            assert_eq!(second["supportedEffortLevels"], serde_json::json!(["high"]));
            assert!(check().already_configured);
        }
    });
}
