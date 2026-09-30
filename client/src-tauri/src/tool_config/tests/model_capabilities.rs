fn capability_test_models() -> Vec<ToolModelInfo> {
    ["gpt-6-astra", "gpt-5.6-sol", "budget-only", "no-reasoning"]
        .into_iter()
        .map(|id| ToolModelInfo {
            id: id.into(),
            display_name: id.into(),
            reasoning: id != "no-reasoning",
            reasoning_efforts: if id.starts_with("gpt-") {
                ["low", "medium", "high", "xhigh", "max", "ultra"]
                    .map(str::to_string)
                    .into()
            } else {
                vec![]
            },
            tool_call: true,
            context_tokens: Some(272_000),
            output_tokens: Some(128_000),
            input_modalities: vec!["text".into(), "image".into()],
            output_modalities: vec!["text".into()],
            ..Default::default()
        })
        .collect()
}

#[test]
fn tool_image_flags_do_not_confuse_audio_or_pdf_attachments_with_vision() {
    for image in [false, true] {
        let model = ToolModelInfo {
            id: "capability-boundary".into(),
            attachment: true,
            input_modalities: if image {
                vec!["text".into(), "image".into()]
            } else {
                vec!["text".into(), "audio".into(), "pdf".into()]
            },
            context_tokens: Some(64000),
            output_tokens: Some(8192),
            ..Default::default()
        };
        let kimi = upsert_kimicode_toml(
            "",
            TOOL_CONFIG_ROOT_URL,
            "mock",
            &[model.clone()],
            ToolProtocol::OpenAiResponses,
        )
        .unwrap()
        .parse::<DocumentMut>()
        .unwrap();
        assert_eq!(
            kimi["models"]["const-api/capability-boundary"]["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str() == Some("image_in")),
            image
        );
        let qwen = qwen_model_entries(&[model.clone()], TOOL_CONFIG_ROOT_URL);
        assert_eq!(
            qwen[0]
                .pointer("/capabilities/vision")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            image
        );
        let harness = deepseek_harness_model_entry(&model);
        assert_eq!(
            harness["input"]
                .as_sequence()
                .unwrap()
                .iter()
                .any(|v| v.as_str() == Some("image")),
            image
        );
        let vibe = upsert_mistral_vibe_toml("", TOOL_CONFIG_ROOT_URL, &[model.clone()])
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            vibe["models"].as_array_of_tables().unwrap().get(0).unwrap()["supports_images"]
                .as_bool(),
            Some(image)
        );
        let vscode = vscode_model_entry(
            &model,
            "http://127.0.0.1/v1/responses",
            "mock",
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        assert_eq!(vscode["vision"], image);
        assert_eq!(vscode["toolCalling"], false);
        let mut buddy = serde_json::json!({});
        configure_workbuddy_model_entry(
            &mut buddy,
            &model,
            "http://127.0.0.1/v1/chat/completions",
            "mock",
        );
        assert_eq!(buddy["supportsImages"], image);
        assert_eq!(buddy["supportsToolCall"], false);
    }
}

#[test]
fn injected_capabilities_keep_positive_and_negative_values_per_model() {
    with_temp_home(|_| {
        let mut enabled = capability_test_models().remove(0);
        enabled.id = "enabled".into();
        let disabled = ToolModelInfo {
            id: "disabled".into(),
            context_tokens: Some(64000),
            output_tokens: Some(8192),
            input_modalities: vec!["text".into()],
            output_modalities: vec!["text".into()],
            ..Default::default()
        };
        let models = vec![enabled, disabled];
        for (tool, path) in [
            ("mimocode", mimocode_config_path().unwrap()),
            ("openscience", openscience_config_path().unwrap()),
            ("zcode", zcode_config_path()),
        ] {
            apply_order_test_catalog(tool, &models);
            let root: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            let entries = &root["provider"]["const-api"]["models"];
            for (id, enabled) in [("enabled", true), ("disabled", false)] {
                assert_eq!(entries[id]["reasoning"], enabled, "{tool}");
                assert_eq!(entries[id]["tool_call"], enabled, "{tool}");
                assert_eq!(
                    entries[id]["modalities"]["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v == "image"),
                    enabled
                );
            }
            assert_eq!(entries["disabled"]["limit"]["context"], 64000);
            assert_eq!(entries["disabled"]["limit"]["output"], 8192);
            assert!(entries["disabled"].get("variants").is_none());
        }
        configure_minimax_code(
            TOOL_CONFIG_ROOT_URL,
            "mock",
            Some(&models),
            ToolProtocol::AnthropicMessages,
        )
        .unwrap();
        let mm: serde_json::Value =
            serde_yaml::from_str(&fs::read_to_string(minimax_code_config_path()).unwrap()).unwrap();
        let (mut providers, mut catalog) = (serde_json::json!({}), serde_json::json!({}));
        upsert_cline_configs(
            &mut providers,
            &mut catalog,
            TOOL_CONFIG_ROOT_URL,
            "mock",
            Some(&models),
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        let claw =
            openclaw_model_entries(&models, TOOL_CONFIG_ROOT_URL, ToolProtocol::OpenAiResponses);
        let mut pi = serde_json::json!({});
        upsert_pi_config(
            &mut pi,
            TOOL_CONFIG_ROOT_URL,
            "mock",
            Some(&models),
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        for (id, enabled) in [("enabled", true), ("disabled", false)] {
            let entry = &mm["custom_provider"]["const-api"]["models"][id];
            assert_eq!(entry["reasoning"], enabled);
            assert_eq!(entry["tool_call"], enabled);
            assert_eq!(entry["capabilities"]["support_image"], enabled);
            let caps = catalog["providers"]["const-api"]["models"][id]["capabilities"]
                .as_array()
                .unwrap();
            for flag in ["reasoning", "tools", "images"] {
                assert_eq!(caps.iter().any(|v| v == flag), enabled);
            }
            let entry = claw.iter().find(|m| m["id"] == id).unwrap();
            assert_eq!(entry["reasoning"], enabled);
            assert_eq!(entry["compat"]["supportsTools"], enabled);
            assert_eq!(
                entry["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "image"),
                enabled
            );
            let entry = pi["providers"]["const-api"]["models"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["id"] == id)
                .unwrap();
            assert_eq!(entry["reasoning"], enabled);
            assert_eq!(
                entry["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "image"),
                enabled
            );
        }
    });
}

#[test]
fn vibe_preserves_user_effort_and_does_not_force_high_for_new_models() {
    let original = "[[models]]\nname='gpt-6-astra'\nalias='const-api/gpt-6-astra'\nprovider='const-api'\nthinking='low'\nauto_compact_threshold=42000\n";
    let text = upsert_mistral_vibe_toml(original, TOOL_CONFIG_ROOT_URL, &capability_test_models())
        .unwrap();
    let doc = text.parse::<DocumentMut>().unwrap();
    let models = doc["models"].as_array_of_tables().unwrap();
    assert_eq!(models.get(0).unwrap()["thinking"].as_str(), Some("low"));
    assert_eq!(
        models.get(0).unwrap()["auto_compact_threshold"].as_integer(),
        Some(42000)
    );
    assert!(models.get(1).unwrap().get("thinking").is_none());
    assert_eq!(
        upsert_mistral_vibe_toml(&text, TOOL_CONFIG_ROOT_URL, &mistral_vibe_models(&text)).unwrap(),
        text
    );
}

#[test]
fn minimax_capabilities_expose_effort_and_safe_context_choices_in_every_protocol() {
    with_temp_home(|_| {
        let models = capability_test_models();
        for protocol in [
            ToolProtocol::OpenAiResponses,
            ToolProtocol::OpenAiChat,
            ToolProtocol::AnthropicMessages,
        ] {
            configure_minimax_code(TOOL_CONFIG_ROOT_URL, "mock", Some(&models), protocol).unwrap();
            let raw = fs::read_to_string(minimax_code_config_path()).unwrap();
            let root: serde_json::Value = serde_yaml::from_str(&raw).unwrap();
            let entries = &root["custom_provider"]["const-api"]["models"];
            for id in ["gpt-6-astra", "gpt-5.6-sol"] {
                assert_eq!(
                    entries[id]["thinking"]["effortOptions"],
                    serde_json::json!(models[0].reasoning_efforts)
                );
                assert_eq!(
                    entries[id]["contextWindowOptions"],
                    serde_json::json!([200_000, 272_000])
                );
                assert_eq!(entries[id]["limit"]["context"], 272_000);
                assert!(
                    entries[id]["thinking"].get("defaultEffort").is_none(),
                    "do not force a new cost/depth default"
                );
            }
            for id in ["budget-only", "no-reasoning"] {
                assert!(
                    entries[id].get("thinking").is_none(),
                    "reasoning alone does not establish depth choices"
                );
            }
            assert!(
                configure_minimax_code(TOOL_CONFIG_ROOT_URL, "mock", Some(&models), protocol)
                    .unwrap()
                    .files
                    .is_empty()
            );
            assert_eq!(fs::read_to_string(minimax_code_config_path()).unwrap(), raw);
        }
    });
}

#[test]
fn minimax_capability_refresh_preserves_advanced_settings_and_bounds_choices() {
    with_temp_home(|_| {
        let path = minimax_code_config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = serde_json::json!({
            "custom_provider":{"const-api":{"models":{"gpt-6-astra":{
                "thinking":{"effortOptions":["low","high"],"defaultEffort":"high"},
                "contextWindowOptions":[100_000,160_000,200_000,1_000_000],
                "options":{"temperature":0.3}, "headers":{"x-custom":"keep"},
                "capabilities":{"support_files_api":true,"max_attachments_count":3}
            }}}}
        });
        let raw = serde_yaml::to_string(&original).unwrap();
        fs::write(&path, &raw).unwrap();
        let mut models = capability_test_models();
        configure_minimax_code(
            TOOL_CONFIG_ROOT_URL,
            "mock",
            Some(&models),
            ToolProtocol::AnthropicMessages,
        )
        .unwrap();
        let root: serde_json::Value =
            serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let entry = &root["custom_provider"]["const-api"]["models"]["gpt-6-astra"];
        assert_eq!(entry["thinking"]["defaultEffort"], "high");
        assert_eq!(
            entry["contextWindowOptions"],
            serde_json::json!([160_000, 200_000, 272_000])
        );
        assert_eq!(entry["options"]["temperature"], 0.3);
        assert_eq!(entry["headers"]["x-custom"], "keep");
        assert_eq!(entry["capabilities"]["support_image"], true);
        assert_eq!(entry["capabilities"]["support_files_api"], true);
        assert_eq!(entry["capabilities"]["max_attachments_count"], 3);
        models[0].reasoning_efforts = vec!["low".into()];
        models[0].context_tokens = Some(150_000);
        configure_minimax_code(
            TOOL_CONFIG_ROOT_URL,
            "mock",
            Some(&models),
            ToolProtocol::AnthropicMessages,
        )
        .unwrap();
        let root: serde_json::Value =
            serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let entry = &root["custom_provider"]["const-api"]["models"]["gpt-6-astra"];
        assert!(entry["thinking"].get("defaultEffort").is_none());
        assert_eq!(entry["contextWindowOptions"], serde_json::json!([150_000]));
        restore_tool_config_from_manifest("minimax-code").unwrap();
        let restored: serde_json::Value =
            serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored, original);
    });
}

#[test]
fn opencode_capabilities_use_protocol_native_variants_without_guessing_budget_models() {
    let models = capability_test_models();
    for protocol in [
        ToolProtocol::OpenAiChat,
        ToolProtocol::OpenAiResponses,
        ToolProtocol::AnthropicMessages,
        ToolProtocol::GeminiNative,
    ] {
        let provider = opencode_const_api_provider(TOOL_CONFIG_ROOT_URL, "mock", &models, protocol);
        let entries = &provider["models"];
        let high = &entries["gpt-6-astra"]["variants"]["high"];
        let expected = match protocol {
            ToolProtocol::OpenAiChat | ToolProtocol::OpenAiResponses => {
                serde_json::json!({"reasoningEffort":"high"})
            }
            ToolProtocol::AnthropicMessages => {
                serde_json::json!({"effort":"high"})
            }
            ToolProtocol::GeminiNative => {
                serde_json::json!({"thinkingConfig":{"includeThoughts":true,"thinkingLevel":"high"}})
            }
        };
        assert_eq!(*high, expected);
        assert_eq!(
            entries["gpt-6-astra"]["variants"],
            entries["gpt-5.6-sol"]["variants"]
        );
        for id in ["budget-only", "no-reasoning"] {
            assert!(entries[id].get("variants").is_none());
        }
        assert_eq!(entries["gpt-6-astra"]["limit"]["context"], 272_000);
        if matches!(
            protocol,
            ToolProtocol::AnthropicMessages | ToolProtocol::GeminiNative
        ) {
            assert!(entries["gpt-6-astra"]["variants"].get("ultra").is_none());
        }
        if protocol == ToolProtocol::GeminiNative {
            assert!(entries["gpt-6-astra"]["variants"].get("xhigh").is_none());
            assert!(entries["gpt-6-astra"]["variants"].get("max").is_none());
        }
    }
}

#[test]
fn opencode_forks_keep_declared_depths_including_new_model_names() {
    with_temp_home(|_| {
        let models = capability_test_models();
        for (tool, path) in [
            ("mimocode", mimocode_config_path().unwrap()),
            ("openscience", openscience_config_path().unwrap()),
            ("zcode", zcode_config_path()),
        ] {
            apply_order_test_catalog(tool, &models);
            let root: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
            let entries = &root["provider"]["const-api"]["models"];
            assert_eq!(
                entries["gpt-6-astra"]["variants"]["ultra"]["reasoningEffort"], "ultra",
                "{tool}"
            );
            assert_eq!(
                entries["gpt-6-astra"]["variants"], entries["gpt-5.6-sol"]["variants"],
                "{tool}"
            );
            assert_eq!(entries["gpt-6-astra"]["limit"]["input"], 144_000, "{tool}");
        }
    });
}

#[test]
fn cline_capabilities_emit_its_closed_reasoning_options_schema() {
    let models = capability_test_models();
    for protocol in [ToolProtocol::OpenAiResponses, ToolProtocol::OpenAiChat] {
        let (mut providers, mut catalog) = (serde_json::json!({}), serde_json::json!({}));
        upsert_cline_configs(
            &mut providers,
            &mut catalog,
            TOOL_CONFIG_ROOT_URL,
            "mock",
            Some(&models),
            protocol,
        )
        .unwrap();
        let entries = &catalog["providers"]["const-api"]["models"];
        assert_eq!(
            entries["gpt-6-astra"]["reasoningOptions"],
            serde_json::json!([{"type":"effort","values":["low","medium","high","xhigh","max"]}])
        );
        assert!(
            entries["gpt-6-astra"]["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "reasoning-effort")
        );
        assert_eq!(entries["gpt-6-astra"]["contextWindow"], 272_000);
        for id in ["budget-only", "no-reasoning"] {
            assert!(entries[id].get("reasoningOptions").is_none());
        }
    }
}

#[test]
fn pi_capabilities_map_declared_levels_and_disable_guessed_levels() {
    let mut model = capability_test_models().remove(0);
    let levels = pi_thinking_level_map(&model).unwrap();
    assert!(levels["off"].is_null());
    assert!(levels["minimal"].is_null());
    assert_eq!(levels["max"], "max");
    assert_eq!(levels["xhigh"], "xhigh");
    model.reasoning_efforts = vec!["none".into(), "low".into(), "ultra".into()];
    let levels = pi_thinking_level_map(&model).unwrap();
    assert_eq!(levels["off"], "none");
    assert_eq!(levels["max"], "ultra");
    assert!(levels["high"].is_null());
    model.reasoning = false;
    assert!(pi_thinking_level_map(&model).is_none());
}

#[test]
fn grok_capabilities_use_the_declared_closed_effort_menu() {
    with_temp_home(|_| {
        configure_grok_build(
            TOOL_CONFIG_ROOT_URL,
            "mock",
            Some(&capability_test_models()),
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        let raw = fs::read_to_string(grok_build_config_path()).unwrap();
        let doc = raw.parse::<DocumentMut>().unwrap();
        let entry = &doc["model"]["const-api/gpt-6-astra"];
        let efforts = entry["reasoning_efforts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(entry["supports_reasoning_effort"].as_bool(), Some(true));
        assert_eq!(
            doc["model"]["const-api/no-reasoning"]["supports_reasoning_effort"].as_bool(),
            Some(false)
        );
    });
}

#[test]
fn reasonix_capabilities_remain_per_model_and_checks_do_not_erase_them() {
    with_temp_home(|_| {
        let (path, _) = reasonix_config_paths();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "[[providers]]\nname = 'const-api'\nno_proxy = true\nmodel_overrides = { 'gpt-6-astra' = {default_effort='high', vision=false} }\n";
        fs::write(&path, original).unwrap();
        apply_reasonix_config(TOOL_CONFIG_ROOT_URL, "mock", &capability_test_models()).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let doc = raw.parse::<DocumentMut>().unwrap();
        let provider = doc["providers"]
            .as_array_of_tables()
            .unwrap()
            .iter()
            .find(|p| p["name"].as_str() == Some("const-api"))
            .unwrap();
        assert_eq!(provider["no_proxy"].as_bool(), Some(true));
        let entry = &provider["model_overrides"]["gpt-6-astra"];
        assert_eq!(entry["context_window"].as_integer(), Some(272_000));
        assert_eq!(entry["max_output_tokens"].as_integer(), Some(128_000));
        assert_eq!(entry["default_effort"].as_str(), Some("high"));
        assert_eq!(entry["vision"].as_bool(), Some(true));
        assert_eq!(entry["supported_efforts"].as_array().unwrap().len(), 6);
        assert_eq!(
            provider["model_overrides"]["no-reasoning"]["reasoning_protocol"].as_str(),
            Some("none")
        );
        assert!(
            check_reasonix_config(TOOL_CONFIG_ROOT_URL, "mock")
                .unwrap()
                .already_configured
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        assert!(
            apply_reasonix_config(TOOL_CONFIG_ROOT_URL, "mock", &capability_test_models())
                .unwrap()
                .files
                .is_empty()
        );
        restore_tool_config_from_manifest("reasonix").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    });
}

#[test]
fn goose_capabilities_do_not_lose_the_reasoning_flag() {
    let mut root = serde_json::json!({});
    upsert_goose_provider_config(&mut root, TOOL_CONFIG_ROOT_URL, &capability_test_models());
    assert_eq!(root["models"][0]["reasoning"], true);
    assert_eq!(root["models"][0]["context_limit"], 272_000);
    assert_eq!(root["models"][3]["reasoning"], false);
}

#[test]
fn minimax_capability_refresh_clears_only_invalid_runtime_choices() {
    with_temp_home(|_| {
        let path = minimax_code_config_path();
        let mut models = capability_test_models();
        configure_minimax_code(
            TOOL_CONFIG_ROOT_URL,
            "mock",
            Some(&models),
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        let mut root: serde_json::Value =
            serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        root["defaultModel"] = serde_json::json!("custom_provider:const-api/gpt-6-astra");
        root["defaultModelThinking"] = serde_json::json!({"effort":"high"});
        root["defaultModelContextWindow"] = serde_json::json!(200_000);
        fs::write(&path, serde_yaml::to_string(&root).unwrap()).unwrap();
        configure_minimax_code(
            TOOL_CONFIG_ROOT_URL,
            "mock",
            Some(&models),
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        let kept: serde_json::Value =
            serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(kept["defaultModelThinking"], root["defaultModelThinking"]);
        assert_eq!(kept["defaultModelContextWindow"], 200_000);
        models[0].context_tokens = Some(150_000);
        models[0].reasoning_efforts = vec!["low".into()];
        configure_minimax_code(
            TOOL_CONFIG_ROOT_URL,
            "mock",
            Some(&models),
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        let changed: serde_json::Value =
            serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(changed.get("defaultModelThinking").is_none());
        assert!(changed.get("defaultModelContextWindow").is_none());
        assert_eq!(changed["defaultModel"], root["defaultModel"]);
    });
}
