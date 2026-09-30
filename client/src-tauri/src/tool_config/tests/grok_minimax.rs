#[test]
fn grok_build_catalog_preserves_user_config_and_selects_each_models_protocol() {
    with_temp_home(|_| {
        let path = grok_build_config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "# user's config\n[models]\ndefault = 'personal'\n[model.personal]\nmodel = 'kept'\n[mcp.servers.test]\ncommand = 'keep-me'\n";
        fs::write(&path, original).unwrap();
        let models = protocol_test_models();
        configure_grok_build(
            TOOL_CONFIG_ROOT_URL,
            "mock-key",
            Some(&models),
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let doc = raw.parse::<DocumentMut>().unwrap();
        assert_eq!(doc["model"]["personal"]["model"].as_str(), Some("kept"));
        assert_eq!(
            doc["mcp"]["servers"]["test"]["command"].as_str(),
            Some("keep-me")
        );
        assert_eq!(
            doc["model"]["const-api/native-messages"]["api_backend"].as_str(),
            Some("messages")
        );
        assert_eq!(
            doc["model"]["const-api/native-messages"]["base_url"].as_str(),
            Some("http://127.0.0.1:38787/anthropic/v1")
        );
        assert_eq!(
            doc["model"]["const-api/native-responses"]["api_backend"].as_str(),
            Some("responses")
        );
        assert_eq!(
            doc["model"]["const-api/native-gemini"]["api_backend"].as_str(),
            Some("responses")
        );
        assert_eq!(
            doc["models"]["session_summary"].as_str(),
            doc["models"]["default"].as_str()
        );
        assert_eq!(
            doc["models"]["prompt_suggestion"].as_str(),
            doc["models"]["default"].as_str()
        );
        assert!(
            check_additional_tool_config("grok-build", TOOL_CONFIG_ROOT_URL, "mock-key")
                .unwrap()
                .already_configured
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        assert!(
            configure_grok_build(
                TOOL_CONFIG_ROOT_URL,
                "mock-key",
                Some(&models),
                ToolProtocol::OpenAiResponses
            )
            .unwrap()
            .files
            .is_empty()
        );
        assert!(
            !check_additional_tool_config("grok-build", TOOL_CONFIG_ROOT_URL, "changed")
                .unwrap()
                .already_configured
        );
        restore_tool_config_from_manifest("grok-build").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    });
}

#[test]
fn grok_minimax_check_and_preview_never_write_user_files() {
    with_temp_home(|_| {
        for tool in ["grok-build", "minimax-code"] {
            let protocol = default_tool_protocol(tool).unwrap();
            assert!(
                !check_additional_tool_config(tool, TOOL_CONFIG_ROOT_URL, "mock")
                    .unwrap()
                    .already_configured
            );
            preview_tool_config_apply(|| {
                apply_additional_tool_config_with_model_info_for_protocol(
                    tool,
                    TOOL_CONFIG_ROOT_URL,
                    "mock",
                    &protocol_test_models(),
                    protocol,
                )
            })
            .unwrap();
        }
        assert!(!grok_build_config_path().exists());
        assert!(!minimax_code_config_path().exists());
    });
}

#[test]
fn minimax_code_catalog_protocols_and_restore_preserve_other_providers() {
    with_temp_home(|_| {
        let path = minimax_code_config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "theme: dark\ndefaultModel: personal/old\ndefaultLightModel: personal/small\ndefaultModelThinking: high\ncustom_provider:\n  personal:\n    models:\n      old: {}\n";
        fs::write(&path, original).unwrap();
        let mut models = protocol_test_models();
        models[0].context_tokens = Some(128000);
        models[0].output_tokens = Some(8192);
        models[0].input_modalities = vec!["text".into(), "image".into()];
        for protocol in [
            ToolProtocol::AnthropicMessages,
            ToolProtocol::OpenAiResponses,
            ToolProtocol::OpenAiChat,
        ] {
            configure_minimax_code(TOOL_CONFIG_ROOT_URL, "mock-key", Some(&models), protocol)
                .unwrap();
            let raw = fs::read_to_string(&path).unwrap();
            let root: serde_json::Value = serde_yaml::from_str(&raw).unwrap();
            let provider = &root["custom_provider"]["const-api"];
            assert_eq!(provider["api"], pi_sdk_api_mode(protocol));
            assert_eq!(
                provider["options"]["baseURL"],
                pi_sdk_base_url(TOOL_CONFIG_ROOT_URL, protocol)
            );
            assert_eq!(
                provider["models"][&models[0].id]["limit"]["context"],
                128000
            );
            assert_eq!(
                provider["models"][&models[0].id]["capabilities"]["support_image"],
                true
            );
            assert!(root["custom_provider"]["personal"]["models"]["old"].is_object());
            assert!(root.get("defaultLightModel").is_none());
            assert!(root.get("defaultModelThinking").is_none());
            assert!(
                check_additional_tool_config("minimax-code", TOOL_CONFIG_ROOT_URL, "mock-key")
                    .unwrap()
                    .already_configured
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
            assert!(
                configure_minimax_code(TOOL_CONFIG_ROOT_URL, "mock-key", Some(&models), protocol)
                    .unwrap()
                    .files
                    .is_empty()
            );
        }
        let mut current: serde_json::Value =
            serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        current["theme"] = serde_json::json!("light");
        fs::write(&path, serde_yaml::to_string(&current).unwrap()).unwrap();
        remove_additional_tool_config("minimax-code").unwrap();
        let restored: serde_json::Value =
            serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored["theme"], "light");
        assert_eq!(restored["defaultModel"], "personal/old");
        assert_eq!(restored["defaultLightModel"], "personal/small");
        assert_eq!(restored["defaultModelThinking"], "high");
        assert!(restored["custom_provider"].get("const-api").is_none());
    });
}

#[test]
fn grok_minimax_reject_invalid_existing_config_and_empty_catalog() {
    with_temp_home(|_| {
        for (tool, path, raw) in [
            ("grok-build", grok_build_config_path(), "model = 123\n"),
            (
                "minimax-code",
                minimax_code_config_path(),
                "custom_provider: []\n",
            ),
        ] {
            let protocol = default_tool_protocol(tool).unwrap();
            assert!(
                apply_additional_tool_config_with_model_info_for_protocol(
                    tool,
                    TOOL_CONFIG_ROOT_URL,
                    "mock",
                    &[],
                    protocol
                )
                .is_err()
            );
            assert!(!path.exists());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, raw).unwrap();
            assert!(
                apply_additional_tool_config_with_model_info_for_protocol(
                    tool,
                    TOOL_CONFIG_ROOT_URL,
                    "mock",
                    &protocol_test_models(),
                    protocol
                )
                .is_err()
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        }
    });
}
