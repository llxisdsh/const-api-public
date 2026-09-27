    use super::*;

    #[test]
    fn claude_auto_context_hint_passes_offline_check_without_hiding_real_changes() {
        with_temp_home(|_| {
            let mut cfg = crate::default_config();
            cfg.allow_model_equivalence = false;
            let models = crate::tool_model_metadata::tool_models_from_response(&serde_json::json!({"data":[
                {"id":"claude-sonnet-4-6","const_api":{"context_tokens":1_000_000,"output_tokens":128_000}}
            ]}));
            let choices = ClaudeModelSettings { main:"claude-sonnet-4-6".into(), opus:CLAUDE_MODEL_FOLLOW_MAIN.into(), sonnet:CLAUDE_MODEL_FOLLOW_MAIN.into(), haiku:CLAUDE_MODEL_FOLLOW_MAIN.into() };
            let effective = claude_settings_with_context(&cfg, &choices, &models);
            apply_claude_config_with_model_info(TOOL_CONFIG_ROOT_URL, "mock-key", &effective, &models, &cfg).unwrap();
            let path = home_dir().join(".claude/settings.json");
            let before = fs::read(&path).unwrap();
            for settings in [&choices, &effective] {
                assert!(check_claude_config_with_model_settings(TOOL_CONFIG_ROOT_URL, "mock-key", settings).unwrap().already_configured);
            }
            assert_eq!(before, fs::read(&path).unwrap(), "status check must never write settings");
            let changed = ClaudeModelSettings { main:"claude-opus-4-6".into(), ..choices.clone() };
            assert!(!check_claude_config_with_model_settings(TOOL_CONFIG_ROOT_URL, "mock-key", &changed).unwrap().already_configured);
            assert!(!check_claude_config_with_model_settings(TOOL_CONFIG_ROOT_URL, "wrong-key", &choices).unwrap().already_configured);
            apply_claude_config_with_model_settings(TOOL_CONFIG_ROOT_URL, "mock-key", &choices).unwrap();
            assert!(!check_claude_config_with_model_settings(TOOL_CONFIG_ROOT_URL, "mock-key", &effective).unwrap().already_configured, "an explicit [1m] selection must not match an unhinted file");
        });
    }

    fn rewrite_tool_config_without_semantic_change(path: &Path) {
        let raw = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let rewritten = match tool_config_format(path) {
            ToolConfigFormat::Json => {
                let value = json5::from_str::<serde_json::Value>(&raw)
                    .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
                serde_json::to_string(&value).expect("compact JSON")
            }
            ToolConfigFormat::Env => env_text_to_status_json(&raw)
                .as_object()
                .expect("env values")
                .iter()
                .map(|(key, value)| {
                    format!("export {key}=\"{}\"", value.as_str().expect("env string"))
                })
                .collect::<Vec<_>>()
                .join("\n"),
            ToolConfigFormat::Toml | ToolConfigFormat::Yaml => {
                format!("# reformatted by tool\n{}", raw.trim_end())
            }
            ToolConfigFormat::Text => raw.trim_end().to_string(),
        };
        fs::write(path, rewritten)
            .unwrap_or_else(|error| panic!("rewrite {}: {error}", path.display()));
    }

    #[test]
    fn const_api_brand_display_name_uses_space_not_underscore() {
        assert_eq!(CONST_API_DISPLAY_NAME, "CONST API");
        assert_eq!(CODEX_CONST_API_PROVIDER_ID, "CONST_API");
        assert_eq!(CONST_API_LEGACY_DISPLAY_NAME, "Const API");
    }

    #[test]
    fn remove_mode_defaults_to_restore_and_rejects_native_route_without_canonical_target() {
        assert_eq!(
            ToolConfigRemoveMode::parse(None).expect("default remove mode"),
            ToolConfigRemoveMode::RestorePreConst
        );
        let error = ToolConfigRemoveMode::NativeRoute
            .validate_for_tool("vscode")
            .expect_err("VS Code has additive providers, not one canonical native route");
        assert!(error
            .to_string()
            .contains("TOOL_CONFIG_NATIVE_ROUTE_UNSUPPORTED"));
    }

    fn workbuddy_test_models() -> Vec<ToolModelInfo> {
        vec![
            ToolModelInfo {
                id: "gpt-5.6-sol".to_string(),
                display_name: "GPT 5.6 Sol".to_string(),
                reasoning: true,
                reasoning_efforts: vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                    "xhigh".to_string(),
                    "max".to_string(),
                    "ultra".to_string(),
                ],
                tool_call: true,
                context_tokens: Some(372_000),
                output_tokens: Some(128_000),
                input_modalities: vec!["text".to_string(), "image".to_string()],
                ..Default::default()
            },
            ToolModelInfo {
                id: "gpt-5.6-terra".to_string(),
                display_name: "GPT 5.6 Terra".to_string(),
                tool_call: true,
                input_modalities: vec!["text".to_string()],
                ..Default::default()
            },
        ]
    }

    #[test]
    fn workbuddy_context_budgets_follow_observed_limits_and_preserve_valid_defaults() {
        let mut model = workbuddy_test_models()[0].clone();
        model.context_tokens = Some(1_000_000);
        let mut entry = serde_json::json!({});
        configure_workbuddy_model_entry(&mut entry, &model, "http://localhost/v1/chat/completions", "test");
        assert_eq!(entry["contextWindow"], serde_json::json!({
            "supportedLengths": [200_000, 1_000_000], "defaultLength": 1_000_000
        }));
        entry["contextWindow"] = serde_json::json!({
            "supportedLengths": [1_000_000, 128_000, 128_000, 0, -1, "bad", 2_000_000],
            "defaultLength": 128_000, "userField": "keep"
        });
        model.context_tokens = Some(272_000);
        configure_workbuddy_model_entry(&mut entry, &model, "http://localhost/v1/chat/completions", "test");
        assert_eq!(entry["maxInputTokens"], 272_000);
        assert_eq!(entry["contextWindow"]["supportedLengths"], serde_json::json!([128_000, 200_000, 272_000]));
        assert_eq!(entry["contextWindow"]["defaultLength"], 128_000);
        assert_eq!(entry["contextWindow"]["userField"], "keep");
        model.context_tokens = Some(64_000);
        configure_workbuddy_model_entry(&mut entry, &model, "http://localhost/v1/chat/completions", "test");
        assert_eq!(entry["maxInputTokens"], 64_000);
        assert!(entry.get("contextWindow").is_none());
        model.context_tokens = None;
        model.output_tokens = None;
        configure_workbuddy_model_entry(&mut entry, &model, "http://localhost/v1/chat/completions", "test");
        assert!(entry.get("maxInputTokens").is_none());
        assert!(entry.get("maxOutputTokens").is_none());
        assert!(entry.get("contextWindow").is_none());
    }

    #[test]
    fn cline_declares_each_models_actual_capabilities() {
        let mut models = workbuddy_test_models();
        models[0].input_modalities.push("file".into());
        models[1].tool_call = false;
        let mut providers = serde_json::json!({});
        let mut catalog = serde_json::json!({});
        upsert_cline_configs(&mut providers, &mut catalog, TOOL_CONFIG_OPENAI_BASE_URL, "test", Some(&models), ToolProtocol::OpenAiChat).unwrap();
        let entries = &catalog["providers"]["const-api"]["models"];
        assert_eq!(entries["gpt-5.6-sol"]["capabilities"], serde_json::json!(["streaming", "tools", "reasoning", "images", "files"]));
        assert_eq!(entries["gpt-5.6-terra"]["capabilities"], serde_json::json!(["streaming"]));
        assert_eq!(entries["gpt-5.6-sol"]["contextWindow"], 372_000);
        assert_eq!(entries["gpt-5.6-sol"]["maxTokens"], 128_000);
    }

    #[test]
    fn pi_declares_context_and_input_capabilities_from_shared_model_metadata() {
        let models = crate::tool_model_metadata::tool_models_from_response(&serde_json::json!({"data": [
            {"id": "gpt-5.6-sol", "const_api": {
                "context_tokens": 272_000, "output_tokens": 64_000,
                "vision": true, "reasoning": true
            }},
            {"id": "plain-model", "const_api": {
                "context_tokens": 32_000, "output_tokens": 4_000,
                "vision": false, "reasoning": false
            }}
        ]}));
        let mut root = serde_json::json!({"providers": {
            "user-provider": {"apiKey": "keep"},
            "const-api": {"models": [{"id": "stale"}]}
        }});
        upsert_pi_config(
            &mut root, TOOL_CONFIG_OPENAI_BASE_URL, "test", Some(&models),
            ToolProtocol::OpenAiResponses,
        ).unwrap();
        let entries = root["providers"]["const-api"]["models"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        let rich = entries.iter().find(|model| model["id"] == "gpt-5.6-sol").unwrap();
        assert_eq!(rich["contextWindow"], 272_000);
        assert_eq!(rich["maxTokens"], 64_000);
        assert_eq!(rich["reasoning"], true);
        assert_eq!(rich["input"], serde_json::json!(["text", "image"]));
        let plain = entries.iter().find(|model| model["id"] == "plain-model").unwrap();
        assert_eq!(plain["contextWindow"], 32_000);
        assert_eq!(plain["reasoning"], false);
        assert_eq!(plain["input"], serde_json::json!(["text"]));
        assert_eq!(root["providers"]["user-provider"]["apiKey"], "keep");
    }

    #[test]
    fn workbuddy_sync_removes_stale_const_models_but_preserves_other_connections_and_restore() {
        for legacy_object in [false, true] {
            with_temp_home(|_| {
                let path = workbuddy_models_path();
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                let endpoint = "http://127.0.0.1:38787/v1/chat/completions";
                let entries = serde_json::json!([
                    {"id":"Vendor/claude-fable-5", "name":"Vendor/claude-fable-5", "vendor":"Custom", "url":endpoint, "apiKey":"sk-test"},
                    {"id":"same-model", "vendor":"Custom", "url":endpoint, "apiKey":"sk-test"},
                    {"id":"same-model", "vendor":"Custom", "url":"https://user.example/v1/chat/completions", "apiKey":"keep"},
                    {"id":"another-key", "vendor":"Custom", "url":endpoint, "apiKey":"keep"}
                ]);
                let original = if legacy_object {
                    serde_json::json!({"models":entries,"availableModels":["Vendor/claude-fable-5","same-model","another-key","builtin"]})
                } else { entries };
                fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();
                let live = workbuddy_test_models();
                apply_workbuddy_config_with_model_info_for_protocol(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test", &live, ToolProtocol::OpenAiChat).unwrap();
                let current = read_json_or_default(&path, serde_json::json!([])).unwrap();
                let models = workbuddy_models(&current).unwrap();
                assert!(!models.iter().any(|m| m["id"].as_str().is_some_and(|id| id.contains("fable"))));
                assert_eq!(models.iter().filter(|m| workbuddy_model_uses_connection(m, endpoint, "sk-test")).count(), live.len());
                assert_eq!(models.iter().filter(|m| m["apiKey"] == "keep").count(), 2);
                if legacy_object {
                    let available = current["availableModels"].as_array().unwrap();
                    assert!(!available.iter().any(|id| id.as_str().is_some_and(|id| id.contains("fable"))));
                    assert!(available.contains(&serde_json::json!("same-model")));
                    assert!(available.contains(&serde_json::json!("builtin")));
                }
                assert!(apply_workbuddy_config_with_model_info_for_protocol(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test", &live, ToolProtocol::OpenAiChat).unwrap().already_configured);
                assert!(apply_workbuddy_config_with_model_info_for_protocol(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test", &[], ToolProtocol::OpenAiChat).is_err());
                assert_eq!(read_json_or_default(&path, serde_json::json!([])).unwrap(), current, "failed/empty refresh must not clear a working configuration");
                remove_workbuddy_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
                assert_eq!(read_json_or_default(&path, serde_json::json!([])).unwrap(), original);
            });
        }
    }

    #[test]
    fn workbuddy_switches_const_profiles_without_mixing_their_models() {
        with_temp_home(|_| {
            let path = workbuddy_models_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let prod_endpoint = "http://127.0.0.1:38787/v1/chat/completions";
            let dev_endpoint = "http://127.0.0.1:38788/v1/chat/completions";
            let original = serde_json::json!({
                "models": [
                    {"id":"gemini-3.8-flash", "name":"gemini-3.8-flash", "vendor":"Custom", "url":prod_endpoint, "apiKey":"prod-key"},
                    {"id":"gpt-5.6-sol", "name":"gpt-5.6-sol", "vendor":"Custom", "url":dev_endpoint, "apiKey":"dev-key"},
                    {"id":"user-model", "name":"user-model", "vendor":"Custom", "url":"https://user.example/v1/chat/completions", "apiKey":"user-key"}
                ],
                "availableModels": ["gemini-3.8-flash", "gpt-5.6-sol", "user-model"]
            });
            fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();

            let live = workbuddy_test_models();
            apply_workbuddy_config_with_model_info_for_protocol(
                "http://127.0.0.1:38788/v1",
                "dev-key",
                &live,
                ToolProtocol::OpenAiChat,
            )
            .unwrap();

            let current = read_json_or_default(&path, serde_json::json!([])).unwrap();
            let models = workbuddy_models(&current).unwrap();
            assert!(!models.iter().any(|entry| entry["url"] == prod_endpoint));
            assert!(models.iter().any(|entry| entry["id"] == "user-model"));
            assert!(models
                .iter()
                .filter(|entry| entry["url"] == dev_endpoint)
                .all(|entry| entry["constApiManaged"] == true));
            assert!(models
                .iter()
                .find(|entry| entry["id"] == "user-model")
                .unwrap()
                .get("constApiManaged")
                .is_none());
            assert_eq!(
                models
                    .iter()
                    .filter(|entry| entry["url"] == dev_endpoint)
                    .map(|entry| entry["id"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                live.iter().map(|model| model.id.as_str()).collect::<Vec<_>>()
            );
            assert!(!current["availableModels"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("gemini-3.8-flash")));

            let prod_models = tool_models_from_ids(&["claude-sonnet-4-6".to_string()]);
            apply_workbuddy_config_with_model_info_for_protocol(
                "http://127.0.0.1:38787/v1",
                "new-prod-key",
                &prod_models,
                ToolProtocol::OpenAiChat,
            )
            .unwrap();

            let switched = read_json_or_default(&path, serde_json::json!([])).unwrap();
            let models = workbuddy_models(&switched).unwrap();
            assert!(!models.iter().any(|entry| entry["url"] == dev_endpoint));
            assert!(models.iter().any(|entry| entry["id"] == "user-model"));
            assert_eq!(
                models
                    .iter()
                    .filter(|entry| entry["url"] == prod_endpoint)
                    .map(|entry| entry["id"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec!["claude-sonnet-4-6"]
            );
            assert!(models
                .iter()
                .filter(|entry| entry["url"] == prod_endpoint)
                .all(|entry| entry["constApiManaged"] == true));

            remove_workbuddy_config("http://127.0.0.1:38787/v1", "new-prod-key").unwrap();
            assert_eq!(
                read_json_or_default(&path, serde_json::json!([])).unwrap(),
                original
            );
        });
    }

    #[test]
    fn workbuddy_models_path_matches_runtime_directory_precedence() {
        let home = Path::new("home");

        assert_eq!(
            workbuddy_models_path_from_config_dirs(home, None, None),
            home.join(".workbuddy").join("models.json")
        );
        assert_eq!(
            workbuddy_models_path_from_config_dirs(
                home,
                Some("  workbuddy-config  "),
                Some("codebuddy-config"),
            ),
            PathBuf::from("workbuddy-config").join("models.json")
        );
        assert_eq!(
            workbuddy_models_path_from_config_dirs(
                home,
                Some("  "),
                Some("  codebuddy-config  "),
            ),
            PathBuf::from("codebuddy-config").join("models.json")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn reasonix_uses_its_current_windows_roaming_config_home() {
        with_temp_home(|home| {
            let expected = home.join("AppData/Roaming/reasonix");
            assert_eq!(reasonix_home_path(), expected);
            assert_eq!(reasonix_config_paths().0, expected.join("config.toml"));
            assert_eq!(reasonix_config_paths().1, expected.join(".env"));
        });
    }

    #[test]
    fn workbuddy_pretty_name_is_normalized_to_avoid_duplicate_selector_label() {
        let endpoint_url = "http://127.0.0.1:38787/v1/chat/completions";
        let mut entry = serde_json::json!({
            "id": "gpt-5.6-sol",
            "name": "GPT 5.6 Sol",
            "vendor": "Custom",
            "url": endpoint_url,
            "apiKey": "sk-test",
            "supportsToolCall": true,
            "supportsImages": true,
            "supportsReasoning": true,
            "useCustomProtocol": false
        });

        assert!(!workbuddy_model_is_configured(
            &entry,
            endpoint_url,
            "sk-test"
        ));
        configure_workbuddy_model_entry(
            &mut entry,
            &workbuddy_test_models()[0],
            endpoint_url,
            "sk-test",
        );

        assert_eq!(entry["name"], entry["id"]);
        assert!(workbuddy_model_is_configured(
            &entry,
            endpoint_url,
            "sk-test"
        ));
    }

    #[test]
    fn workbuddy_reasoning_effort_selector_follows_model_metadata() {
        let endpoint_url = "http://127.0.0.1:38787/v1/chat/completions";
        let mut workbuddy_model = workbuddy_test_models()[0].clone();
        workbuddy_model
            .reasoning_efforts
            .insert(0, "minimal".to_string());
        workbuddy_model
            .reasoning_efforts
            .push("future-tier".to_string());
        let mut entry = serde_json::json!({
            "reasoning": {
                "defaultEffort": "ultra",
                "supportedEfforts": ["ultra"]
            }
        });
        configure_workbuddy_model_entry(
            &mut entry,
            &workbuddy_model,
            endpoint_url,
            "sk-test",
        );

        assert_eq!(
            entry["reasoning"]["supportedEfforts"],
            serde_json::json!(["low", "medium", "high", "xhigh", "max"])
        );
        assert!(entry["reasoning"].get("defaultEffort").is_none());

        let mut disable_capable = workbuddy_test_models()[0].clone();
        disable_capable.reasoning_efforts = vec!["none".to_string()];
        configure_workbuddy_model_entry(
            &mut entry,
            &disable_capable,
            endpoint_url,
            "sk-test",
        );
        assert_eq!(
            entry["reasoning"],
            serde_json::json!({"canDisableThinking": true})
        );

        let mut reasoning_without_efforts = workbuddy_test_models()[0].clone();
        reasoning_without_efforts.reasoning_efforts.clear();
        configure_workbuddy_model_entry(
            &mut entry,
            &reasoning_without_efforts,
            endpoint_url,
            "sk-test",
        );
        assert_eq!(entry["supportsReasoning"], true);
        assert!(entry.get("reasoning").is_none());
    }

    #[test]
    fn vscode_reasoning_effort_selector_uses_protocol_shape_and_supported_levels() {
        let mut model = workbuddy_test_models()[0].clone();
        model.reasoning_efforts.push("none".to_string());
        model.reasoning_efforts.push("future-tier".to_string());
        let responses = vscode_model_entry(
            &model,
            "http://127.0.0.1:38787/v1/responses",
            "sk-test",
            ToolProtocol::OpenAiResponses,
        );
        assert_eq!(
            responses["supportsReasoningEffort"],
            serde_json::json!([
                "low",
                "medium",
                "high",
                "xhigh",
                "max",
                "ultra",
                "none",
                "future-tier"
            ])
        );
        assert_eq!(responses["reasoningEffortFormat"], "responses");
        assert_eq!(responses["contextWindow"], 372_000);
        assert_eq!(responses["maxInputTokens"], 244_000);
        assert_eq!(responses["maxOutputTokens"], 128_000);

        let chat = vscode_model_entry(
            &model,
            "http://127.0.0.1:38787/v1/chat/completions",
            "sk-test",
            ToolProtocol::OpenAiChat,
        );
        assert_eq!(chat["reasoningEffortFormat"], "chat-completions");

        let no_efforts = vscode_model_entry(
            &workbuddy_test_models()[1],
            "http://127.0.0.1:38787/v1/responses",
            "sk-test",
            ToolProtocol::OpenAiResponses,
        );
        assert!(no_efforts.get("supportsReasoningEffort").is_none());
        assert!(no_efforts.get("reasoningEffortFormat").is_none());
    }

    #[test]
    fn opencode_model_variants_and_limits_follow_shared_metadata() {
        let mut model_info = workbuddy_test_models();
        model_info[0].reasoning_efforts.push("none".to_string());
        model_info[0]
            .reasoning_efforts
            .push("future-tier".to_string());
        let models = opencode_models_object(&model_info);
        let sol = &models["gpt-5.6-sol"];

        assert_eq!(
            sol["variants"],
            serde_json::json!({
                "low": {"reasoningEffort": "low"},
                "medium": {"reasoningEffort": "medium"},
                "high": {"reasoningEffort": "high"},
                "xhigh": {"reasoningEffort": "xhigh"},
                "max": {"reasoningEffort": "max"},
                "ultra": {"reasoningEffort": "ultra"},
                "none": {"reasoningEffort": "none"},
                "future-tier": {"reasoningEffort": "future-tier"}
            })
        );
        assert_eq!(
            sol["limit"],
            serde_json::json!({
                "context": 372_000,
                "input": 244_000,
                "output": 128_000
            })
        );
        assert_eq!(
            sol["modalities"]["input"],
            serde_json::json!(["text", "image"])
        );
        assert!(models["gpt-5.6-terra"].get("variants").is_none());
    }

    #[test]
    fn shared_short_model_catalog_feeds_all_tool_format_builders() {
        let models = crate::tool_model_metadata::tool_models_from_ids(&[
            "Vendor/Zulu".into(), "First/MiXeD:free".into(), "Other/mixed:free".into(),
            "Vendor/Alpha[1M]".into(),
        ]);
        let expected = ["alpha", "mixed:free", "zulu"];
        let opencode = opencode_models_object(&models);
        assert_eq!(opencode.keys().map(String::as_str).collect::<Vec<_>>(), expected);
        let openclaw = openclaw_model_entries(&models);
        assert_eq!(openclaw.iter().map(|entry| entry["id"].as_str().unwrap()).collect::<Vec<_>>(), expected);
        for (model, id) in models.iter().zip(expected) {
            let vscode = vscode_model_entry(model, "http://127.0.0.1:1/v1/chat/completions", "sk-test", ToolProtocol::OpenAiChat);
            assert_eq!(vscode["id"], id);
            assert_eq!(vscode["name"], id);
            let mut workbuddy = serde_json::json!({});
            configure_workbuddy_model_entry(&mut workbuddy, model, "http://127.0.0.1:1/v1/chat/completions", "sk-test");
            assert_eq!(workbuddy["id"], id);
            assert_eq!(workbuddy["name"], id);
        }
    }

    #[test]
    fn openclaw_model_catalog_follows_shared_metadata_and_status_preserves_it() {
        with_temp_home(|_| {
            let mut models = workbuddy_test_models();
            models[0].reasoning_efforts.push("none".to_string());
            models[0]
                .reasoning_efforts
                .push("future-tier".to_string());
            apply_openclaw_config_with_model_info_for_protocol(
                TOOL_CONFIG_ROOT_URL,
                "sk-test",
                &models,
                ToolProtocol::OpenAiResponses,
            )
            .expect("apply OpenClaw with model metadata");

            let path = home_dir().join(".openclaw").join("openclaw.json");
            let root = read_json_or_default(&path, serde_json::json!({}))
                .expect("read OpenClaw config");
            let provider = &root["models"]["providers"][CODEX_CONST_API_PROVIDER_ID];
            let models = provider["models"].as_array().expect("OpenClaw models");
            let sol = models
                .iter()
                .find(|model| model["id"] == "gpt-5.6-sol")
                .expect("Sol model");
            assert_eq!(sol["reasoning"], true);
            assert_eq!(sol["input"], serde_json::json!(["text", "image"]));
            assert_eq!(sol["contextWindow"], 372_000);
            assert_eq!(sol["maxTokens"], 128_000);
            assert_eq!(sol["compat"]["supportsTools"], true);
            assert_eq!(sol["compat"]["supportsReasoningEffort"], true);
            assert_eq!(
                sol["compat"]["supportedReasoningEfforts"],
                serde_json::json!([
                    "low",
                    "medium",
                    "high",
                    "xhigh",
                    "max",
                    "ultra",
                    "none",
                    "future-tier"
                ])
            );
            assert_eq!(
                sol["compat"]["reasoningEffortMap"],
                serde_json::json!({"off": "none", "none": "none"})
            );

            assert!(
                check_openclaw_config(TOOL_CONFIG_ROOT_URL, "sk-test")
                    .expect("check OpenClaw")
                    .already_configured
            );
            let checked = read_json_or_default(&path, serde_json::json!({}))
                .expect("read checked OpenClaw config");
            assert_eq!(
                checked["models"]["providers"][CODEX_CONST_API_PROVIDER_ID]["models"],
                provider["models"]
            );
        });
    }

    #[test]
    fn workbuddy_current_array_config_is_managed_checked_and_restored() {
        with_temp_home(|_| {
            let path = workbuddy_models_path();
            fs::create_dir_all(path.parent().expect("WorkBuddy config parent")).expect("mkdir");
            let original = serde_json::json!([
                {
                    "id": "other-model",
                    "name": "other-model",
                    "vendor": "Custom",
                    "url": "https://example.com/v1/chat/completions",
                    "apiKey": "keep"
                },
                {
                    "id": "gpt-5.6-sol",
                    "name": "gpt-5.6-sol",
                    "vendor": "Custom",
                    "url": "https://user.example/v1/chat/completions",
                    "apiKey": "user-key",
                    "reasoning": {"effort": "high"},
                    "userField": "preserve"
                }
            ]);
            fs::write(
                &path,
                serde_json::to_string_pretty(&original).expect("serialize WorkBuddy config")
                    + "\n",
            )
            .expect("seed WorkBuddy config");

            let applied = apply_workbuddy_config_with_model_info_for_protocol(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                &workbuddy_test_models(),
                ToolProtocol::OpenAiChat,
            )
            .expect("apply WorkBuddy");
            assert!(!applied.already_configured);
            assert_eq!(
                applied.details.get("tool_protocol").map(String::as_str),
                Some("openai_chat")
            );

            let configured =
                read_json_or_default(&path, serde_json::json!([])).expect("read WorkBuddy config");
            let entries = configured.as_array().expect("WorkBuddy model array");
            assert_eq!(entries.len(), 3);
            assert!(entries.iter().any(|entry| entry["name"] == "other-model"));
            let sol = entries
                .iter()
                .find(|entry| entry["id"] == "gpt-5.6-sol")
                .expect("configured Sol model");
            assert_eq!(
                sol["url"],
                "http://127.0.0.1:38787/v1/chat/completions"
            );
            assert_eq!(sol["apiKey"], "sk-test");
            assert_eq!(sol["name"], "gpt-5.6-sol");
            assert_eq!(sol["vendor"], "Custom");
            assert_eq!(sol["supportsToolCall"], true);
            assert_eq!(sol["supportsImages"], true);
            assert_eq!(sol["supportsReasoning"], true);
            assert_eq!(sol["useCustomProtocol"], false);
            assert_eq!(
                sol["reasoning"],
                serde_json::json!({
                    "effort": "high",
                    "supportedEfforts": ["low", "medium", "high", "xhigh", "max"]
                })
            );
            assert_eq!(sol["maxInputTokens"], 372_000);
            assert_eq!(sol["maxOutputTokens"], 128_000);
            assert_eq!(sol["userField"], "preserve");

            assert!(
                check_workbuddy_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                    .expect("check WorkBuddy")
                    .already_configured
            );
            assert!(
                !check_workbuddy_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-other")
                    .expect("check WorkBuddy with changed key")
                    .already_configured
            );

            remove_workbuddy_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("remove WorkBuddy");
            let restored =
                read_json_or_default(&path, serde_json::json!([])).expect("read restored config");
            assert_eq!(restored, original);
        });
    }

    #[test]
    fn workbuddy_migrates_managed_paths_and_duplicates_without_touching_other_connections() {
        with_temp_home(|_| {
            let path = workbuddy_models_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let endpoint = "http://127.0.0.1:38787/v1/chat/completions";
            let original = serde_json::json!({"availableModels": ["Vendor/Zulu", "First/MiXeD:free", "mixed:free", "Keep/Manual"], "models": [
                {"id": "Vendor/Zulu", "name": "Vendor/Zulu", "vendor": "Custom", "url": endpoint, "apiKey": "sk-test"},
                {"id": "Keep/Manual", "name": "Keep/Manual", "vendor": "Custom", "url": "https://user.example/v1/chat/completions", "apiKey": "keep"},
                {"id": "First/MiXeD:free", "name": "First/MiXeD:free", "vendor": "Custom", "url": endpoint, "apiKey": "sk-test", "userField": "first"},
                {"id": "mixed:free", "name": "mixed:free", "vendor": "Custom", "url": endpoint, "apiKey": "sk-test", "userField": "duplicate"}
            ]});
            fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();
            let models = crate::tool_model_metadata::tool_models_from_ids(&["Vendor/Zulu".into(), "First/MiXeD:free".into()]);
            apply_workbuddy_config_with_model_info_for_protocol(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test", &models, ToolProtocol::OpenAiChat).unwrap();
            let configured = read_json_or_default(&path, serde_json::json!({})).unwrap();
            let entries = configured["models"].as_array().unwrap();
            let managed = entries.iter().filter(|entry| workbuddy_model_uses_connection(entry, endpoint, "sk-test")).collect::<Vec<_>>();
            assert_eq!(managed.iter().map(|entry| entry["id"].as_str().unwrap()).collect::<Vec<_>>(), ["mixed:free", "zulu"]);
            assert_eq!(managed[0]["userField"], "first");
            assert_eq!(entries[1], original["models"][1]);
            assert_eq!(configured["availableModels"], serde_json::json!(["mixed:free", "zulu", "Keep/Manual"]));
            assert!(apply_workbuddy_config_with_model_info_for_protocol(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test", &models, ToolProtocol::OpenAiChat).unwrap().already_configured);
            remove_workbuddy_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
            assert_eq!(read_json_or_default(&path, serde_json::json!({})).unwrap(), original);
        });
    }

    #[test]
    fn workbuddy_legacy_object_config_is_supported_and_restored() {
        with_temp_home(|_| {
            let path = workbuddy_models_path();
            fs::create_dir_all(path.parent().expect("WorkBuddy config parent")).expect("mkdir");
            let original = serde_json::json!({
                "schemaVersion": 1,
                "availableModels": ["existing"],
                "models": [{
                    "id": "existing",
                    "name": "existing",
                    "vendor": "Custom",
                    "url": "https://example.com/v1/chat/completions",
                    "apiKey": "keep"
                }]
            });
            fs::write(
                &path,
                serde_json::to_string_pretty(&original).expect("serialize legacy WorkBuddy config")
                    + "\n",
            )
            .expect("seed legacy WorkBuddy config");

            apply_workbuddy_config_with_model_info_for_protocol(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                &workbuddy_test_models()[..1],
                ToolProtocol::OpenAiChat,
            )
            .expect("apply legacy WorkBuddy config");
            let configured =
                read_json_or_default(&path, serde_json::json!({})).expect("read configured object");
            assert_eq!(configured["schemaVersion"], 1);
            assert_eq!(configured["models"].as_array().map(Vec::len), Some(2));
            assert_eq!(
                configured["availableModels"],
                serde_json::json!(["existing", "gpt-5.6-sol"])
            );
            assert!(
                check_workbuddy_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                    .expect("check legacy WorkBuddy config")
                    .already_configured
            );

            remove_workbuddy_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("remove legacy WorkBuddy config");
            assert_eq!(
                read_json_or_default(&path, serde_json::json!({}))
                    .expect("read restored legacy object"),
                original
            );
        });
    }

    #[test]
    fn workbuddy_cancel_removes_only_matching_unmanaged_local_models() {
        with_temp_home(|_| {
            let path = workbuddy_models_path();
            fs::create_dir_all(path.parent().expect("WorkBuddy config parent")).expect("mkdir");
            fs::write(
                &path,
                serde_json::to_string_pretty(&serde_json::json!([
                    {
                        "id": "gpt-5.6-sol",
                        "name": "gpt-5.6-sol",
                        "vendor": "Custom",
                        "url": "http://127.0.0.1:38787/v1/chat/completions",
                        "apiKey": "sk-test",
                        "supportsToolCall": true,
                        "supportsImages": true,
                        "supportsReasoning": true,
                        "useCustomProtocol": false
                    },
                    {
                        "id": "keep",
                        "name": "keep",
                        "vendor": "Custom",
                        "url": "https://example.com/v1/chat/completions",
                        "apiKey": "keep"
                    }
                ]))
                .expect("serialize unmanaged WorkBuddy config")
                    + "\n",
            )
            .expect("seed unmanaged WorkBuddy config");

            let removed = remove_workbuddy_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("remove unmanaged WorkBuddy config");
            assert_eq!(
                removed.details.get("manifest_entries_restored").map(String::as_str),
                Some("0")
            );
            let restored =
                read_json_or_default(&path, serde_json::json!([])).expect("read cleaned config");
            assert_eq!(restored.as_array().map(Vec::len), Some(1));
            assert_eq!(restored[0]["name"], "keep");
        });
    }

    #[test]
    fn vscode_custom_endpoint_preserves_other_groups_and_restores_owned_provider() {
        with_temp_home(|_| {
            let path = vscode_chat_language_models_path();
            let settings_path = vscode_user_settings_path();
            fs::create_dir_all(path.parent().expect("VS Code config parent")).expect("mkdir");
            let original = serde_json::json!([
                {
                    "name": "Existing",
                    "vendor": "customendpoint",
                    "apiType": "chat-completions",
                    "apiKey": "keep",
                    "models": []
                },
                {
                    "name": "CONST API",
                    "vendor": "customendpoint",
                    "apiType": "chat-completions",
                    "apiKey": "old",
                    "models": []
                }
            ]);
            fs::write(
                &path,
                serde_json::to_string_pretty(&original).expect("serialize original") + "\n",
            )
            .expect("seed VS Code config");
            let original_settings = serde_json::json!({
                "editor.fontSize": 15,
                "chat.byokUtilityModelDefault": "none"
            });
            fs::write(
                &settings_path,
                serde_json::to_string_pretty(&original_settings)
                    .expect("serialize original settings")
                    + "\n",
            )
            .expect("seed VS Code settings");
            let models = tool_models_from_ids(&[
                "gpt-5.6-terra".to_string(),
                "gpt-5.6-sol".to_string(),
            ]);

            let applied = apply_vscode_config_with_model_info_for_protocol(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                &models,
                ToolProtocol::OpenAiResponses,
            )
            .expect("apply VS Code");
            assert!(!applied.already_configured);
            let configured = read_json_or_default(&path, serde_json::json!([]))
                .expect("read configured VS Code");
            let providers = configured.as_array().expect("provider array");
            assert_eq!(providers.len(), 2);
            assert!(providers.iter().any(|provider| provider["name"] == "Existing"));
            let provider = providers
                .iter()
                .find(|provider| provider["name"] == "CONST API")
                .expect("CONST API provider");
            assert_eq!(provider["apiType"], "responses");
            assert_eq!(provider["apiKey"], "sk-test");
            assert_eq!(provider["models"].as_array().map(Vec::len), Some(2));
            assert!(provider["models"]
                .as_array()
                .expect("models")
                .iter()
                .all(|model| model["url"] == "http://127.0.0.1:38787/v1/responses"));
            assert!(provider["models"]
                .as_array()
                .expect("models")
                .iter()
                .all(|model| model["requestHeaders"]["Authorization"] == "Bearer sk-test"));
            assert!(provider["models"]
                .as_array()
                .expect("models")
                .iter()
                .all(|model| model["reasoningEffortFormat"] == "responses"));
            assert!(provider["models"]
                .as_array()
                .expect("models")
                .iter()
                .all(|model| model["supportsReasoningEffort"]
                    .as_array()
                    .is_some_and(|efforts| !efforts.is_empty())));
            let settings = read_json_or_default(&settings_path, serde_json::json!({}))
                .expect("read configured VS Code settings");
            assert_eq!(settings["chat.byokUtilityModelDefault"], "mainAgent");
            assert_eq!(settings["editor.fontSize"], 15);

            let checked = check_vscode_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("check VS Code");
            assert!(checked.already_configured);
            assert_eq!(
                checked.details.get("tool_protocol").map(String::as_str),
                Some("openai_responses")
            );

            let configured_bytes = fs::read(&path).expect("read VS Code bytes before preview");
            let unchanged = preview_tool_config_apply(|| {
                apply_vscode_config_with_model_info_for_protocol(
                    TOOL_CONFIG_OPENAI_BASE_URL,
                    "sk-test",
                    &models,
                    ToolProtocol::OpenAiResponses,
                )
            })
            .expect("preview unchanged VS Code configuration");
            assert!(unchanged.already_configured);
            assert!(unchanged.files.is_empty());
            assert_eq!(
                fs::read(&path).expect("VS Code preview leaves bytes unchanged"),
                configured_bytes
            );

            let changed_models = tool_models_from_ids(&["gpt-5.6-sol".to_string()]);
            let changed = preview_tool_config_apply(|| {
                apply_vscode_config_with_model_info_for_protocol(
                    TOOL_CONFIG_OPENAI_BASE_URL,
                    "sk-test",
                    &changed_models,
                    ToolProtocol::OpenAiResponses,
                )
            })
            .expect("preview changed VS Code model catalog");
            assert!(!changed.already_configured);
            assert!(changed.files.is_empty());
            assert_eq!(
                fs::read(&path).expect("changed preview leaves VS Code bytes unchanged"),
                configured_bytes
            );

            apply_vscode_config_with_model_info_for_protocol(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                &models,
                ToolProtocol::OpenAiChat,
            )
            .expect("switch VS Code protocol");
            let switched = read_json_or_default(&path, serde_json::json!([]))
                .expect("read switched VS Code");
            let provider = switched
                .as_array()
                .and_then(|providers| providers.iter().find(|provider| provider["name"] == "CONST API"))
                .expect("switched provider");
            assert_eq!(provider["apiType"], "chat-completions");
            assert!(provider["models"]
                .as_array()
                .expect("models")
                .iter()
                .all(|model| model["url"] == "http://127.0.0.1:38787/v1/chat/completions"));
            assert!(provider["models"]
                .as_array()
                .expect("models")
                .iter()
                .all(|model| model["reasoningEffortFormat"] == "chat-completions"));

            remove_vscode_config().expect("remove VS Code");
            let restored = read_json_or_default(&path, serde_json::json!([]))
                .expect("read restored VS Code");
            assert_eq!(restored, original);
            let restored_settings = read_json_or_default(&settings_path, serde_json::json!({}))
                .expect("read restored VS Code settings");
            assert_eq!(restored_settings, original_settings);
        });
    }

    #[test]
    fn vscode_cancel_removes_unmanaged_legacy_const_api_provider() {
        with_temp_home(|_| {
            let path = vscode_chat_language_models_path();
            let settings_path = vscode_user_settings_path();
            fs::create_dir_all(path.parent().expect("VS Code config parent")).expect("mkdir");
            let existing = serde_json::json!([
                {
                    "name": "Existing",
                    "vendor": "customendpoint",
                    "apiType": "chat-completions",
                    "apiKey": "keep",
                    "models": []
                },
                {
                    "name": "CONST API",
                    "vendor": "customendpoint",
                    "apiType": "responses",
                    "apiKey": "sk-test",
                    "models": [{
                        "id": "gpt-5.6-sol",
                        "name": "gpt-5.6-sol",
                        "url": "http://127.0.0.1:38787/v1/responses",
                        "streaming": true,
                        "requestHeaders": {
                            "Authorization": "Bearer sk-test"
                        }
                    }]
                }
            ]);
            fs::write(
                &path,
                serde_json::to_string_pretty(&existing).expect("serialize VS Code config") + "\n",
            )
            .expect("seed legacy VS Code config");
            fs::write(
                &settings_path,
                serde_json::to_string_pretty(&serde_json::json!({
                    "chat.byokUtilityModelDefault": "mainAgent"
                }))
                .expect("serialize VS Code settings")
                    + "\n",
            )
            .expect("seed legacy VS Code settings");

            assert!(
                check_vscode_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                    .expect("check legacy VS Code config")
                    .already_configured
            );

            let removed = remove_vscode_config().expect("remove legacy VS Code config");
            assert_eq!(
                removed.details.get("manifest_entries_restored").map(String::as_str),
                Some("0")
            );
            let restored =
                read_json_or_default(&path, serde_json::json!([])).expect("read cleaned VS Code");
            let providers = restored.as_array().expect("provider array");
            assert_eq!(providers.len(), 1);
            assert_eq!(providers[0]["name"], "Existing");
            assert!(
                !check_vscode_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                    .expect("check removed VS Code config")
                    .already_configured
            );
        });
    }

    #[test]
    fn dynamic_model_catalog_does_not_control_local_tool_status() {
        with_temp_home(|_| {
            let models = tool_models_from_ids(&[
                "gpt-5.6-terra".to_string(),
                "gpt-5.6-sol".to_string(),
            ]);
            apply_vscode_config_with_model_info_for_protocol(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                &models,
                ToolProtocol::OpenAiResponses,
            )
            .expect("apply VS Code with models");

            let checked = check_vscode_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("check VS Code without a live model snapshot");

            assert!(checked.already_configured);
            assert!(
                !check_vscode_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-other")
                    .expect("check VS Code with a changed key")
                    .already_configured
            );

            apply_opencode_config_with_model_info_for_protocol(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                &models,
                ToolProtocol::OpenAiResponses,
            )
            .expect("apply OpenCode with models");
            let checked = check_opencode_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("check OpenCode without a live model snapshot");
            assert!(checked.already_configured);
            assert!(
                !check_opencode_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-other")
                    .expect("check OpenCode with a changed key")
                    .already_configured
            );

            let model_ids = models
                .iter()
                .map(|model| model.id.clone())
                .collect::<Vec<_>>();
            apply_claude_desktop_config_with_models(
                TOOL_CONFIG_ANTHROPIC_BASE_URL,
                "sk-test",
                &model_ids,
            )
            .expect("apply Claude Desktop with models");
            let checked =
                check_claude_desktop_config(TOOL_CONFIG_ANTHROPIC_BASE_URL, "sk-test")
                    .expect("check Claude Desktop without a live model snapshot");
            assert!(checked.already_configured);
            assert!(
                !check_claude_desktop_config(TOOL_CONFIG_ANTHROPIC_BASE_URL, "sk-other")
                    .expect("check Claude Desktop with a changed key")
                    .already_configured
            );
        });
    }

    #[test]
    fn claude_tools_use_detected_capacity_and_restore_original_configuration() {
        with_temp_home(|home| {
            let mut cfg = crate::default_config();
            cfg.allow_model_equivalence = false;
            let models = crate::tool_model_metadata::tool_models_from_response(&serde_json::json!({"data":[
                {"id":"claude-sonnet-4-6","const_api":{"context_tokens":1000000,"output_tokens":128000}},
                {"id":"claude-haiku-4-5","const_api":{"context_tokens":200000,"output_tokens":64000}}
            ]}));
            let path = home.join(".claude/settings.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = "{\"modelPicker\":{\"options\":[{\"model\":\"user-model\"}]},\"permissions\":{\"allow\":[\"Read\"]}}";
            fs::write(&path, original).unwrap();
            let choices = ClaudeModelSettings { main:"claude-sonnet-4-6".into(), haiku:"claude-haiku-4-5[1m]".into(), ..Default::default() };
            let settings = claude_settings_with_context(&cfg, &choices, &models);
            assert_eq!(settings.main, "claude-sonnet-4-6[1m]");
            assert_eq!(settings.haiku, "claude-haiku-4-5");
            let non_claude = crate::tool_model_metadata::tool_models_from_response(&serde_json::json!({"data":[
                {"id":"gemini-3.7-flash","const_api":{"context_tokens":1048576}}
            ]}));
            assert_eq!(claude_model_with_context(&cfg, "gemini-3.7-flash", &non_claude), "gemini-3.7-flash");
            apply_claude_config_with_model_info(TOOL_CONFIG_ROOT_URL, "sk-test", &settings, &models, &cfg).unwrap();
            let written = read_json_or_default(&path, serde_json::json!({})).unwrap();
            assert_eq!(written["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "claude-sonnet-4-6[1m]");
            assert!(written["env"].get("DISABLE_COMPACT").is_none());
            assert!(written["modelPicker"]["options"].as_array().unwrap().iter().any(|m| m["model"] == "claude-sonnet-4-6[1m]" && m["label"] == "claude-sonnet-4-6"));
            apply_claude_desktop_config_with_model_info(TOOL_CONFIG_ROOT_URL, "sk-test", &models, &cfg).unwrap();
            let desktop = read_json_or_default(&claude_desktop_paths().profile_path, serde_json::json!({})).unwrap();
            let entries = desktop["inferenceModels"].as_array().unwrap();
            assert!(entries.iter().any(|m| m["name"] == "claude-sonnet-4-6" && m["supports1m"] == true && m["prefer1m"] == true));
            assert!(entries.iter().any(|m| m["name"] == "claude-haiku-4-5" && m["supports1m"] == false));
            assert!(!desktop.to_string().contains("[1m]"));
            let lowered = crate::tool_model_metadata::tool_models_from_response(&serde_json::json!({"data":[{"id":"claude-sonnet-4-6","const_api":{"context_tokens":128000}}]}));
            let settings = claude_settings_with_context(&cfg, &choices, &lowered);
            apply_claude_config_with_model_info(TOOL_CONFIG_ROOT_URL, "sk-test", &settings, &lowered, &cfg).unwrap();
            assert!(!read_json_or_default(&path, serde_json::json!({})).unwrap().to_string().contains("[1m]"));
            remove_claude_config(TOOL_CONFIG_ROOT_URL, "sk-test").unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        });
    }

    #[test]
    fn claude_desktop_empty_model_refresh_preserves_existing_inference_models() {
        with_temp_home(|_| {
            apply_claude_desktop_config_with_models(
                TOOL_CONFIG_ANTHROPIC_BASE_URL,
                "sk-test",
                &["gpt-5.6-sol".to_string()],
            )
            .expect("apply Claude Desktop with a non-empty model catalog");
            let profile_path = claude_desktop_paths().profile_path;
            let before = read_json_or_default(&profile_path, serde_json::json!({}))
                .expect("read initial Claude Desktop profile");
            let expected_models = before["inferenceModels"]
                .as_array()
                .filter(|models| !models.is_empty())
                .cloned()
                .expect("initial inference models");

            apply_claude_desktop_config_with_models(
                TOOL_CONFIG_ANTHROPIC_BASE_URL,
                "sk-test",
                &[],
            )
            .expect("reapply Claude Desktop with an empty model catalog");
            let after = read_json_or_default(&profile_path, serde_json::json!({}))
                .expect("read reapplied Claude Desktop profile");

            assert_eq!(
                after["inferenceModels"],
                serde_json::Value::Array(expected_models)
            );
        });
    }

    #[test]
    fn claude_desktop_keeps_3p_mode_without_hiding_claude_ai_sign_in() {
        with_temp_home(|_| {
            let paths = claude_desktop_paths();
            fs::create_dir_all(paths.profile_path.parent().expect("profile parent"))
                .expect("create Claude Desktop profile directory");
            fs::write(
                &paths.profile_path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "disableDeploymentModeChooser": true,
                    "userOwned": true
                }))
                .expect("serialize original Claude Desktop profile"),
            )
            .expect("seed original Claude Desktop profile");

            apply_claude_desktop_config(TOOL_CONFIG_ANTHROPIC_BASE_URL, "sk-test")
                .expect("apply Claude Desktop");
            let profile = read_json_or_default(&paths.profile_path, serde_json::json!({}))
                .expect("read Claude Desktop profile");

            assert_eq!(profile["inferenceProvider"], "gateway");
            assert!(profile.get("disableDeploymentModeChooser").is_none());
            for path in [&paths.normal_config_path, &paths.threep_config_path] {
                let config = read_json_or_default(path, serde_json::json!({}))
                    .expect("read Claude Desktop deployment mode");
                assert_eq!(config["deploymentMode"], "3p");
            }
            let meta = read_json_or_default(&paths.meta_path, serde_json::json!({}))
                .expect("read Claude Desktop profile metadata");
            assert_eq!(
                meta["appliedId"],
                serde_json::json!(CLAUDE_DESKTOP_PROFILE_ID)
            );
            assert!(
                check_claude_desktop_config(TOOL_CONFIG_ANTHROPIC_BASE_URL, "sk-test")
                    .expect("check Claude Desktop")
                    .already_configured
            );

            remove_claude_desktop_config(TOOL_CONFIG_ANTHROPIC_BASE_URL, "sk-test")
                .expect("cancel Claude Desktop");
            let restored = read_json_or_default(&paths.profile_path, serde_json::json!({}))
                .expect("read restored Claude Desktop profile");
            assert_eq!(restored["disableDeploymentModeChooser"], true);
            assert_eq!(restored["userOwned"], true);
        });
    }

    #[test]
    fn every_tool_status_check_uses_semantic_config_comparison() {
        with_temp_home(|_| {
            let model_info = tool_models_from_ids(&["gpt-5.6-sol".to_string()]);
            for tool in [
                "codex",
                "claude",
                "claude-desktop",
                "claude-science",
                "gemini",
                "opencode",
                "openclaw",
                "hermes",
                "vscode",
                "workbuddy",
                "kimicode",
                "mimocode",
                "qwencode",
                "openscience",
                "vibe-trading",
                "zcode",
                "copilot",
                "raven",
                "pi",
                "cline",
                "reasonix",
                "deepseek-harness",
                "open-interpreter",
                "anythingllm",
                "goose",
                "mistral-vibe",
                "open-design",
            ] {
                let applied = match tool {
                    "vscode" => apply_vscode_config_with_model_info_for_protocol(
                        TOOL_CONFIG_OPENAI_BASE_URL,
                        "sk-test",
                        &model_info,
                        ToolProtocol::OpenAiResponses,
                    ),
                    "workbuddy" => apply_workbuddy_config_with_model_info_for_protocol(
                        TOOL_CONFIG_OPENAI_BASE_URL,
                        "sk-test",
                        &model_info,
                        ToolProtocol::OpenAiChat,
                    ),
                    "kimicode" | "mimocode" | "qwencode" | "openscience"
                    | "vibe-trading" | "zcode" | "copilot" | "raven" | "pi"
                    | "cline" | "reasonix" | "deepseek-harness" | "open-interpreter" | "goose"
                    | "mistral-vibe" | "open-design" | "anythingllm" => apply_additional_tool_config_with_model_info_for_protocol(
                        tool,
                        TOOL_CONFIG_OPENAI_BASE_URL,
                        "sk-test",
                        &model_info,
                        resolve_tool_protocol(tool, None).expect("additional tool protocol"),
                    ),
                    _ => apply_tool_config_by_name(
                        tool,
                        TOOL_CONFIG_OPENAI_BASE_URL,
                        TOOL_CONFIG_ROOT_URL,
                        "sk-test",
                    ),
                }
                .unwrap_or_else(|error| panic!("apply {tool}: {error:#}"));

                for status in &applied.file_statuses {
                    rewrite_tool_config_without_semantic_change(Path::new(&status.path));
                }

                if tool != "codex" {
                    let preview = preview_tool_config_apply(|| match tool {
                        "vscode" => apply_vscode_config_with_model_info_for_protocol(
                            TOOL_CONFIG_OPENAI_BASE_URL,
                            "sk-test",
                            &model_info,
                            ToolProtocol::OpenAiResponses,
                        ),
                        "workbuddy" => apply_workbuddy_config_with_model_info_for_protocol(
                            TOOL_CONFIG_OPENAI_BASE_URL,
                            "sk-test",
                            &model_info,
                            ToolProtocol::OpenAiChat,
                        ),
                        "kimicode" | "mimocode" | "qwencode" | "openscience"
                        | "vibe-trading" | "zcode" | "copilot" | "raven" | "pi"
                        | "cline" | "reasonix" | "deepseek-harness" | "open-interpreter"
                        | "goose" | "mistral-vibe" | "open-design" | "anythingllm" => {
                            apply_additional_tool_config_with_model_info_for_protocol(
                                tool,
                                TOOL_CONFIG_OPENAI_BASE_URL,
                                "sk-test",
                                &model_info,
                                resolve_tool_protocol(tool, None)
                                    .expect("additional tool protocol"),
                            )
                        }
                        _ => apply_tool_config_by_name(
                            tool,
                            TOOL_CONFIG_OPENAI_BASE_URL,
                            TOOL_CONFIG_ROOT_URL,
                            "sk-test",
                        ),
                    })
                    .unwrap_or_else(|error| panic!("preview reformatted {tool}: {error:#}"));
                    assert!(
                        preview.already_configured,
                        "{tool} preview must ignore formatting-only rewrites: {preview:#?}"
                    );
                    assert!(preview.files.is_empty(), "{tool} preview must not write");
                    assert!(preview.backups.is_empty(), "{tool} preview must not back up");
                }

                let checked = check_tool_config_by_name(
                    tool,
                    TOOL_CONFIG_OPENAI_BASE_URL,
                    TOOL_CONFIG_ROOT_URL,
                    "sk-test",
                )
                .unwrap_or_else(|error| panic!("check reformatted {tool}: {error:#}"));
                assert!(
                    checked.already_configured,
                    "{tool} must ignore formatting-only rewrites: {checked:#?}"
                );

                let changed_key = check_tool_config_by_name(
                    tool,
                    TOOL_CONFIG_OPENAI_BASE_URL,
                    TOOL_CONFIG_ROOT_URL,
                    "sk-other",
                )
                .unwrap_or_else(|error| panic!("check changed key for {tool}: {error:#}"));
                assert!(
                    !changed_key.already_configured,
                    "{tool} must still detect a changed managed credential"
                );
            }
        });
    }

    #[test]
    fn additional_tool_configs_apply_check_and_restore_original_files() {
        with_temp_home(|home| {
            let base_url = TOOL_CONFIG_OPENAI_BASE_URL;
            let api_key = "sk-test";
            let models = tool_models_from_ids(&[
                "gpt-5.6-sol".to_string(),
                "claude-sonnet-4-6".to_string(),
            ]);
            let originals = vec![
                (
                    kimicode_config_path(),
                    "theme = \"dark\"\n\n[providers.user]\ntype = \"openai\"\napi_key = \"sk-user\"\n".to_string(),
                ),
                (
                    mimocode_config_path().expect("MiMo path"),
                    "{\n  // user setting\n  \"keep\": {\"value\": 1}\n}\n".to_string(),
                ),
                (
                    qwencode_config_path(),
                    "{\n  \"keep\": {\"value\": 2}\n}\n".to_string(),
                ),
                (
                    openscience_config_path().expect("OpenScience path"),
                    "{\n  // user setting\n  \"keep\": {\"value\": 3}\n}\n".to_string(),
                ),
                (
                    vibe_trading_config_path(),
                    "KEEP_ME=yes\n".to_string(),
                ),
                (
                    zcode_config_path(),
                    concat!(
                        "{\n",
                        "  \"keep\": {\"value\": 4},\n",
                        "  \"model\": \"user/model\",\n",
                        "  \"provider\": {\n",
                        "    \"user\": {\n",
                        "      \"name\": \"User\",\n",
                        "      \"kind\": \"openai-compatible\",\n",
                        "      \"options\": {\"baseURL\": \"https://example.test/v1\", \"apiKey\": \"user-key\"},\n",
                        "      \"models\": {\"model\": {\"name\": \"Model\"}}\n",
                        "    }\n",
                        "  }\n",
                        "}\n"
                    )
                    .to_string(),
                ),
            ];
            for (path, content) in &originals {
                fs::create_dir_all(path.parent().expect("config parent"))
                    .expect("create config parent");
                fs::write(path, content).expect("seed additional tool config");
            }

            for tool in [
                "kimicode",
                "mimocode",
                "qwencode",
                "openscience",
                "vibe-trading",
                "zcode",
            ] {
                let protocol = resolve_tool_protocol(tool, None).expect("tool protocol");
                apply_additional_tool_config_with_model_info_for_protocol(
                    tool, base_url, api_key, &models, protocol,
                )
                .unwrap_or_else(|error| panic!("apply {tool}: {error:#}"));
                let checked = check_additional_tool_config(tool, base_url, api_key)
                    .unwrap_or_else(|error| panic!("check {tool}: {error:#}"));
                assert!(checked.already_configured, "{tool}: {checked:#?}");
                assert_eq!(
                    checked.details.get("tool_protocol").map(String::as_str),
                    Some(protocol.as_str())
                );
            }

            let kimi = fs::read_to_string(kimicode_config_path()).expect("read Kimi config");
            assert!(kimi.contains("[providers.const-api]"));
            assert!(kimi.contains("type = \"openai_responses\""));
            assert!(kimi.contains("capabilities = [\"tool_use\""));
            let mimo = read_json5_or_default(
                &mimocode_config_path().expect("MiMo path"),
                serde_json::json!({}),
            )
            .expect("read MiMo config");
            assert_eq!(mimo["keep"]["value"], 1);
            assert_eq!(
                mimo["provider"][ADDITIONAL_CONST_API_PROVIDER_ID]["options"]["baseURL"],
                base_url
            );
            let qwen = read_json_or_default(&qwencode_config_path(), serde_json::json!({}))
                .expect("read Qwen config");
            assert_eq!(qwen["keep"]["value"], 2);
            assert_eq!(
                qwen["providerProtocol"][ADDITIONAL_CONST_API_PROVIDER_ID],
                "openai"
            );
            let openscience = read_json5_or_default(
                &openscience_config_path().expect("OpenScience path"),
                serde_json::json!({}),
            )
            .expect("read OpenScience config");
            assert_eq!(openscience["keep"]["value"], 3);
            let vibe = fs::read_to_string(vibe_trading_config_path()).expect("read Vibe config");
            assert!(vibe.contains("KEEP_ME=yes"));
            assert!(vibe.contains("LANGCHAIN_PROVIDER=openai"));
            let zcode = read_json_or_default(&zcode_config_path(), serde_json::json!({}))
                .expect("read ZCode config");
            assert_eq!(zcode["keep"]["value"], 4);
            assert_eq!(
                zcode["provider"][ADDITIONAL_CONST_API_PROVIDER_ID]["kind"],
                "openai-compatible"
            );
            assert_eq!(
                zcode["provider"][ADDITIONAL_CONST_API_PROVIDER_ID]["options"]["baseURL"],
                base_url
            );
            assert_eq!(
                zcode["model"],
                format!("{ADDITIONAL_CONST_API_PROVIDER_ID}/claude-sonnet-4-6")
            );

            for tool in [
                "kimicode",
                "mimocode",
                "qwencode",
                "openscience",
                "vibe-trading",
                "zcode",
            ] {
                remove_additional_tool_config(tool)
                    .unwrap_or_else(|error| panic!("remove {tool}: {error:#}"));
                assert!(
                    !check_additional_tool_config(tool, base_url, api_key)
                        .expect("check removed additional tool")
                        .already_configured,
                    "{tool} should be unconfigured after restore"
                );
            }
            for (path, original) in originals {
                assert_eq!(
                    fs::read_to_string(&path)
                        .unwrap_or_else(|error| panic!("read restored {}: {error}", path.display())),
                    original,
                    "{} must restore the original file",
                    path.display()
                );
            }

            assert!(home.join(".kimi-code/config.toml").exists());
        });
    }

    #[test]
    fn p0_p1_tool_configs_apply_check_and_restore_original_files() {
        with_temp_home(|_| {
            let base_url = TOOL_CONFIG_OPENAI_BASE_URL;
            let api_key = "sk-test";
            let models = tool_models_from_ids(&[
                "gpt-5.6-sol".to_string(),
                "claude-sonnet-4-6".to_string(),
            ]);
            let (cline_providers_path, cline_models_path) =
                cline_provider_paths().expect("Cline paths");
            let (reasonix_config_path, reasonix_env_path) = reasonix_config_paths();
            let (goose_provider_path, goose_env_path) = goose_config_paths();
            let (mistral_config_path, mistral_env_path) = mistral_vibe_config_paths();
            let originals = vec![
                (copilot_env_path(), "KEEP_COPILOT=yes\n".to_string()),
                (
                    raven_config_path(),
                    concat!(
                        "{\n",
                        "  \"keep\": {\"value\": 1},\n",
                        "  \"providers\": {\"other\": {\"apiKey\": \"user-key\"}},\n",
                        "  \"agents\": {\"other\": {\"enabled\": true}}\n",
                        "}\n"
                    )
                    .to_string(),
                ),
                (
                    pi_models_path(),
                    concat!(
                        "{\n",
                        "  \"keep\": {\"value\": 2},\n",
                        "  \"providers\": {\"user\": {\"baseUrl\": \"https://example.test\"}}\n",
                        "}\n"
                    )
                    .to_string(),
                ),
                (
                    cline_providers_path.clone(),
                    concat!(
                        "{\n",
                        "  \"version\": 1,\n",
                        "  \"keep\": {\"value\": 3},\n",
                        "  \"providers\": {\"user\": {\"settings\": {\"model\": \"user-model\"}}}\n",
                        "}\n"
                    )
                    .to_string(),
                ),
                (
                    cline_models_path.clone(),
                    concat!(
                        "{\n",
                        "  \"version\": 1,\n",
                        "  \"keep\": {\"value\": 4},\n",
                        "  \"providers\": {\"user\": {\"provider\": {\"name\": \"User\"}, \"models\": {}}}\n",
                        "}\n"
                    )
                    .to_string(),
                ),
                (
                    reasonix_config_path.clone(),
                    concat!(
                        "theme = \"dark\"\n\n",
                        "[[providers]]\n",
                        "name = \"user\"\n",
                        "kind = \"openai\"\n",
                        "base_url = \"https://example.test/v1\"\n",
                        "models = [\"user-model\"]\n",
                        "default = \"user-model\"\n",
                        "api_key_env = \"USER_KEY\"\n"
                    )
                    .to_string(),
                ),
                (
                    reasonix_env_path.clone(),
                    "KEEP_REASONIX=yes\nUSER_KEY=user-secret\n".to_string(),
                ),
                (
                    open_interpreter_profile_path(),
                    "keep: true\nllm:\n  temperature: 0.3\n".to_string(),
                ),
                (
                    goose_provider_path.clone(),
                    "{\n  \"keep\": {\"value\": 5},\n  \"headers\": {\"x-user\": \"keep\"}\n}\n"
                        .to_string(),
                ),
                (goose_env_path.clone(), "KEEP_GOOSE=yes\n".to_string()),
                (
                    mistral_config_path.clone(),
                    concat!(
                        "theme = \"dark\"\n\n",
                        "[[providers]]\n",
                        "name = \"user\"\n",
                        "api_base = \"https://example.test/v1\"\n",
                        "api_key_env_var = \"USER_KEY\"\n",
                        "api_style = \"openai\"\n",
                        "backend = \"generic\"\n\n",
                        "[[models]]\n",
                        "name = \"user-model\"\n",
                        "provider = \"user\"\n",
                        "alias = \"user/user-model\"\n"
                    )
                    .to_string(),
                ),
                (
                    mistral_env_path.clone(),
                    "KEEP_VIBE=yes\nUSER_KEY=user-secret\n".to_string(),
                ),
                (
                    open_design_config_path().expect("Open Design path"),
                    concat!(
                        "{\n",
                        "  \"keep\": {\"value\": 6},\n",
                        "  \"agentId\": \"claude\",\n",
                        "  \"agentModels\": {\"claude\": {\"model\": \"user-model\"}},\n",
                        "  \"agentCliEnv\": {\"claude\": {\"USER\": \"keep\"}}\n",
                        "}\n"
                    )
                    .to_string(),
                ),
            ];
            for (path, content) in &originals {
                fs::create_dir_all(path.parent().expect("P0/P1 config parent"))
                    .expect("create P0/P1 config parent");
                fs::write(path, content).expect("seed P0/P1 tool config");
            }

            let tools = [
                "copilot",
                "raven",
                "pi",
                "cline",
                "reasonix",
                "open-interpreter",
                "goose",
                "mistral-vibe",
                "open-design",
            ];
            for tool in tools {
                let protocol = resolve_tool_protocol(tool, None).expect("P0/P1 protocol");
                apply_additional_tool_config_with_model_info_for_protocol(
                    tool, base_url, api_key, &models, protocol,
                )
                .unwrap_or_else(|error| panic!("apply {tool}: {error:#}"));
                let checked = check_additional_tool_config(tool, base_url, api_key)
                    .unwrap_or_else(|error| panic!("check {tool}: {error:#}"));
                assert!(checked.already_configured, "{tool}: {checked:#?}");
                assert_eq!(
                    checked.details.get("tool_protocol").map(String::as_str),
                    Some(protocol.as_str())
                );
                assert!(
                    !check_additional_tool_config(tool, base_url, "sk-other")
                        .unwrap_or_else(|error| panic!("check changed key for {tool}: {error:#}"))
                        .already_configured,
                    "{tool} must detect a changed managed credential"
                );
            }

            let raven = read_json_or_default(&raven_config_path(), serde_json::json!({}))
                .expect("read Raven config");
            assert_eq!(raven["keep"]["value"], 1);
            assert_eq!(raven["agents"]["other"]["enabled"], true);
            assert_eq!(raven["agents"]["defaults"]["provider"], "custom");
            assert_eq!(
                raven["agents"]["defaults"]["model"],
                raven["providers"]["custom"]["models"][0]
            );
            assert!(raven["providers"]["custom"]["models"]
                .as_array()
                .expect("Raven model list")
                .iter()
                .all(|model| model.as_str().is_some_and(|id| id.starts_with("custom/"))));

            let pi = read_json_or_default(&pi_models_path(), serde_json::json!({}))
                .expect("read Pi config");
            assert_eq!(pi["keep"]["value"], 2);
            assert_eq!(pi["providers"][ADDITIONAL_CONST_API_PROVIDER_ID]["api"], "openai-responses");

            let cline_providers =
                read_json_or_default(&cline_providers_path, serde_json::json!({}))
                    .expect("read Cline providers");
            let cline_models = read_json_or_default(&cline_models_path, serde_json::json!({}))
                .expect("read Cline models");
            assert_eq!(cline_providers["keep"]["value"], 3);
            assert_eq!(cline_models["keep"]["value"], 4);
            assert_eq!(
                cline_providers["providers"][ADDITIONAL_CONST_API_PROVIDER_ID]["settings"]["model"],
                "claude-sonnet-4-6"
            );

            let reasonix = fs::read_to_string(&reasonix_config_path).expect("read Reasonix config");
            assert!(reasonix.contains("theme = \"dark\""));
            assert!(reasonix.contains("name = \"user\""));
            assert!(reasonix.contains("name = \"const-api\""));
            assert!(fs::read_to_string(&reasonix_env_path)
                .expect("read Reasonix env")
                .contains("KEEP_REASONIX=yes"));

            let interpreter = fs::read_to_string(open_interpreter_profile_path())
                .expect("read Open Interpreter profile");
            let interpreter: serde_yaml::Value =
                serde_yaml::from_str(&interpreter).expect("parse Open Interpreter profile");
            assert_eq!(interpreter["keep"], true);
            assert_eq!(interpreter["llm"]["model"], "openai/claude-sonnet-4-6");

            let goose = read_json_or_default(&goose_provider_path, serde_json::json!({}))
                .expect("read Goose provider");
            assert_eq!(goose["keep"]["value"], 5);
            assert_eq!(goose["headers"]["x-user"], "keep");
            assert_eq!(goose["engine"], "openai");
            assert!(fs::read_to_string(&goose_env_path)
                .expect("read Goose env")
                .contains("KEEP_GOOSE=yes"));

            let mistral = fs::read_to_string(&mistral_config_path).expect("read Vibe config");
            assert!(mistral.contains("theme = \"dark\""));
            assert!(mistral.contains("name = \"user\""));
            assert!(mistral.contains("name = \"const-api\""));
            assert!(fs::read_to_string(&mistral_env_path)
                .expect("read Vibe env")
                .contains("KEEP_VIBE=yes"));

            let open_design =
                read_json_or_default(&open_design_config_path().expect("Open Design path"), serde_json::json!({}))
                    .expect("read Open Design config");
            assert_eq!(open_design["keep"]["value"], 6);
            assert_eq!(open_design["agentModels"]["claude"]["model"], "user-model");
            assert_eq!(open_design["agentCliEnv"]["claude"]["USER"], "keep");
            assert_eq!(open_design["agentId"], "codex");

            for tool in tools {
                remove_additional_tool_config(tool)
                    .unwrap_or_else(|error| panic!("remove {tool}: {error:#}"));
                assert!(
                    !check_additional_tool_config(tool, base_url, api_key)
                        .unwrap_or_else(|error| panic!("check removed {tool}: {error:#}"))
                        .already_configured,
                    "{tool} should be unconfigured after restore"
                );
            }
            for (path, original) in originals {
                assert_eq!(
                    fs::read_to_string(&path)
                        .unwrap_or_else(|error| panic!("read restored {}: {error}", path.display())),
                    original,
                    "{} must restore the original file",
                    path.display()
                );
            }
        });
    }

    #[test]
    fn deepseek_harness_config_applies_checks_and_three_way_restores() {
        with_temp_home(|_| {
            let base_url = TOOL_CONFIG_OPENAI_BASE_URL;
            let api_key = "sk-harness";
            let (settings_path, credentials_path) = deepseek_harness_config_paths();
            fs::create_dir_all(settings_path.parent().expect("Harness config parent"))
                .expect("create Harness config parent");
            fs::write(
                &settings_path,
                concat!(
                    "ui:\n",
                    "  theme: dark\n",
                    "llm-pi-ai:\n",
                    "  providers:\n",
                    "    user:\n",
                    "      apiKeyEnv: USER_KEY\n",
                    "      api: openai-completions\n",
                    "      baseURL: https://example.test/v1\n",
                    "      models:\n",
                    "        - id: user-model\n",
                    "agent-default-model:\n",
                    "  provider: user\n",
                    "  model: user-model\n"
                ),
            )
            .expect("seed Harness settings");
            fs::write(&credentials_path, "USER_KEY: user-secret\n")
                .expect("seed Harness credentials");

            let models = vec![
                ToolModelInfo {
                    id: "model-a".to_string(),
                    display_name: "Model A".to_string(),
                    reasoning: true,
                    reasoning_efforts: vec![
                        "none".to_string(),
                        "high".to_string(),
                        "ultra".to_string(),
                    ],
                    tool_call: true,
                    attachment: true,
                    context_tokens: Some(200_000),
                    output_tokens: Some(16_384),
                    input_modalities: vec!["text".to_string(), "image".to_string()],
                    output_modalities: vec!["text".to_string()],
                    ..ToolModelInfo::default()
                },
                ToolModelInfo {
                    id: "model-b".to_string(),
                    display_name: "Model B".to_string(),
                    ..ToolModelInfo::default()
                },
                ToolModelInfo {
                    id: "model-c".to_string(),
                    display_name: "Model C".to_string(),
                    reasoning: true,
                    reasoning_efforts: vec!["none".to_string()],
                    ..ToolModelInfo::default()
                },
            ];
            apply_additional_tool_config_with_model_info_for_protocol(
                "deepseek-harness",
                base_url,
                api_key,
                &models,
                ToolProtocol::OpenAiChat,
            )
            .expect("apply DeepSeek Harness config");

            let settings: serde_yaml::Value = serde_yaml::from_str(
                &fs::read_to_string(&settings_path).expect("read Harness settings"),
            )
            .expect("parse Harness settings");
            let provider = &settings["llm-pi-ai"]["providers"][ADDITIONAL_CONST_API_PROVIDER_ID];
            assert_eq!(settings["ui"]["theme"], "dark");
            assert_eq!(provider["displayName"], CONST_API_DISPLAY_NAME);
            assert_eq!(provider["apiKeyEnv"], DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY);
            assert_eq!(provider["api"], "openai-completions");
            assert_eq!(provider["baseURL"], TOOL_CONFIG_OPENAI_BASE_URL);
            assert_eq!(provider["models"][0]["id"], "model-a");
            assert_eq!(provider["models"][0]["contextWindow"], 200_000);
            assert_eq!(provider["models"][0]["maxTokens"], 16_384);
            assert_eq!(provider["models"][0]["input"][1], "image");
            assert_eq!(provider["models"][0]["reasoningEfforts"]["off"], "none");
            assert_eq!(provider["models"][0]["reasoningEfforts"]["high"], "high");
            assert_eq!(provider["models"][0]["reasoningEfforts"]["max"], "ultra");
            assert_eq!(provider["models"][1]["reasoningEfforts"], false);
            assert_eq!(provider["models"][2]["id"], "model-c");
            assert!(provider["models"][2]
                .as_mapping()
                .expect("Harness model mapping")
                .get(yaml_key("reasoningEfforts"))
                .is_none());
            assert_eq!(settings["agent-default-model"]["provider"], "const-api");
            assert_eq!(settings["agent-default-model"]["model"], "model-a");

            let credentials: serde_yaml::Value = serde_yaml::from_str(
                &fs::read_to_string(&credentials_path).expect("read Harness credentials"),
            )
            .expect("parse Harness credentials");
            assert_eq!(credentials["USER_KEY"], "user-secret");
            assert_eq!(credentials[DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY], api_key);

            let checked = check_additional_tool_config("deepseek-harness", base_url, api_key)
                .expect("check Harness config");
            assert!(checked.already_configured, "{checked:#?}");
            assert_eq!(
                checked.details.get("tool_protocol").map(String::as_str),
                Some("openai_chat")
            );
            assert!(
                !check_additional_tool_config("deepseek-harness", base_url, "sk-other")
                    .expect("check changed Harness credential")
                    .already_configured
            );

            let mut settings: serde_yaml::Value = serde_yaml::from_str(
                &fs::read_to_string(&settings_path).expect("re-read Harness settings"),
            )
            .expect("parse Harness settings for external edit");
            settings["ui"]["locale"] = yaml_string("zh");
            fs::write(
                &settings_path,
                serde_yaml::to_string(&settings).expect("serialize external Harness edit"),
            )
            .expect("write external Harness settings edit");
            let mut credentials: serde_yaml::Value = serde_yaml::from_str(
                &fs::read_to_string(&credentials_path).expect("re-read Harness credentials"),
            )
            .expect("parse Harness credentials for external edit");
            credentials["LATER_KEY"] = yaml_string("keep-later");
            fs::write(
                &credentials_path,
                serde_yaml::to_string(&credentials)
                    .expect("serialize external Harness credential edit"),
            )
            .expect("write external Harness credential edit");

            remove_additional_tool_config("deepseek-harness")
                .expect("remove DeepSeek Harness config");
            let restored_settings: serde_yaml::Value = serde_yaml::from_str(
                &fs::read_to_string(&settings_path).expect("read restored Harness settings"),
            )
            .expect("parse restored Harness settings");
            assert_eq!(restored_settings["ui"]["theme"], "dark");
            assert_eq!(restored_settings["ui"]["locale"], "zh");
            assert_eq!(restored_settings["llm-pi-ai"]["providers"]["user"]["models"][0]["id"], "user-model");
            assert!(restored_settings["llm-pi-ai"]["providers"]
                .get(ADDITIONAL_CONST_API_PROVIDER_ID)
                .is_none());
            assert_eq!(restored_settings["agent-default-model"]["provider"], "user");
            assert_eq!(restored_settings["agent-default-model"]["model"], "user-model");

            let restored_credentials: serde_yaml::Value = serde_yaml::from_str(
                &fs::read_to_string(&credentials_path)
                    .expect("read restored Harness credentials"),
            )
            .expect("parse restored Harness credentials");
            assert_eq!(restored_credentials["USER_KEY"], "user-secret");
            assert_eq!(restored_credentials["LATER_KEY"], "keep-later");
            assert!(restored_credentials
                .get(DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY)
                .is_none());
            assert!(
                !check_additional_tool_config("deepseek-harness", base_url, api_key)
                    .expect("check removed Harness config")
                    .already_configured
            );
        });
    }

    #[test]
    fn deepseek_harness_versioned_credentials_preview_apply_check_and_restore() {
        with_temp_home(|_| {
            let (settings_path, credentials_path) = deepseek_harness_config_paths();
            fs::create_dir_all(credentials_path.parent().unwrap()).unwrap();
            let original = concat!(
                "version: 1\n",
                "refs:\n  USER_KEY: user-secret\n",
                "records:\n  client-connection/browser-session:\n",
                "    kind: grant\n    payload: {token: test-browser-token}\n",
            );
            fs::write(&credentials_path, original).unwrap();
            let models = [ToolModelInfo {
                id: "model-a".to_string(),
                ..ToolModelInfo::default()
            }];
            let apply = || {
                apply_deepseek_harness_config(
                    TOOL_CONFIG_OPENAI_BASE_URL,
                    "sk-harness",
                    &models,
                    ToolProtocol::OpenAiChat,
                )
            };
            let preview = preview_tool_config_apply(apply).unwrap();
            assert!(!preview.already_configured);
            assert!(preview.files.is_empty() && preview.backups.is_empty());
            assert!(!settings_path.exists());
            assert!(!tool_config_manifest_path().exists());
            assert_eq!(fs::read_to_string(&credentials_path).unwrap(), original);

            apply().unwrap();
            let mut actual: serde_yaml::Value =
                serde_yaml::from_str(&fs::read_to_string(&credentials_path).unwrap()).unwrap();
            assert_eq!(actual["version"], 1);
            assert!(actual.get(DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY).is_none());
            assert_eq!(actual["refs"][DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY], "sk-harness");
            let mut expected: serde_yaml::Value = serde_yaml::from_str(original).unwrap();
            assert_eq!(actual["records"], expected["records"]);
            assert!(check_deepseek_harness_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-harness")
                .unwrap().already_configured);
            assert!(!check_deepseek_harness_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-other")
                .unwrap().already_configured);
            assert!(preview_tool_config_apply(apply).unwrap().already_configured);

            // Harness/user changes made after configuration survive cancellation.
            actual["refs"]["LATER_KEY"] = yaml_string("later-secret");
            actual["records"]["client-connection/browser-session"]["payload"]["token"] =
                yaml_string("rotated-test-token");
            expected["refs"]["LATER_KEY"] = actual["refs"]["LATER_KEY"].clone();
            expected["records"] = actual["records"].clone();
            fs::write(&credentials_path, serde_yaml::to_string(&actual).unwrap()).unwrap();
            remove_additional_tool_config("deepseek-harness").unwrap();
            let restored: serde_yaml::Value =
                serde_yaml::from_str(&fs::read_to_string(&credentials_path).unwrap()).unwrap();
            assert_eq!(restored, expected);
        });
    }

    #[test]
    fn deepseek_harness_restore_follows_upstream_credentials_migration() {
        for previous_key in [None, Some("original-key")] {
            for reapply in [false, true] {
                for external_edit in [false, true] {
                    with_temp_home(|_| {
                        let (_, credentials_path) = deepseek_harness_config_paths();
                        fs::create_dir_all(credentials_path.parent().unwrap()).unwrap();
                        let mut original = serde_json::json!({"USER_KEY": "user-secret"});
                        if let Some(key) = previous_key {
                            original[DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY] = key.into();
                        }
                        fs::write(&credentials_path, serde_yaml::to_string(&original).unwrap())
                            .unwrap();
                        let models = [ToolModelInfo {
                            id: "model-a".to_string(),
                            ..ToolModelInfo::default()
                        }];
                        let apply = |key| {
                            apply_deepseek_harness_config(
                                TOOL_CONFIG_OPENAI_BASE_URL,
                                key,
                                &models,
                                ToolProtocol::OpenAiChat,
                            )
                        };
                        apply("sk-before-migration").unwrap();
                        let refs: serde_json::Value = serde_yaml::from_str(
                            &fs::read_to_string(&credentials_path).unwrap(),
                        ).unwrap();
                        // Simulate Harness's official flat-layout migration and
                        // the browser login record it creates after first launch.
                        let mut migrated = serde_json::json!({
                            "version": 1,
                            "refs": refs,
                            "records": {"client-connection/browser-session": {
                                "kind": "grant", "payload": {"token": "test-token"}
                            }}
                        });
                        fs::write(&credentials_path, serde_yaml::to_string(&migrated).unwrap())
                            .unwrap();
                        assert!(preview_tool_config_apply(|| apply("sk-before-migration"))
                            .unwrap().already_configured);
                        if reapply {
                            apply("sk-after-migration").unwrap();
                        }
                        if external_edit {
                            migrated = serde_yaml::from_str(
                                &fs::read_to_string(&credentials_path).unwrap(),
                            ).unwrap();
                            migrated["refs"][DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY] =
                                "user-edited-key".into();
                            original[DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY] =
                                "user-edited-key".into();
                            fs::write(&credentials_path, serde_yaml::to_string(&migrated).unwrap())
                                .unwrap();
                        }
                        remove_additional_tool_config("deepseek-harness").unwrap();
                        let restored: serde_json::Value = serde_yaml::from_str(
                            &fs::read_to_string(&credentials_path).unwrap(),
                        ).unwrap();
                        assert_eq!(restored, serde_json::json!({
                            "version": 1, "refs": original, "records": migrated["records"],
                        }));
                    });
                }
            }
        }
    }

    #[test]
    fn deepseek_harness_versioned_credentials_accept_empty_refs() {
        for raw in ["version: 1\n", "version: 1\nrefs:\n", "version: 1\nrefs: {}\n"] {
            let updated = upsert_deepseek_harness_credentials(raw, "sk-test").unwrap();
            let value: serde_yaml::Value = serde_yaml::from_str(&updated).unwrap();
            assert_eq!(value["version"], 1);
            assert_eq!(value["refs"][DEEPSEEK_HARNESS_CREDENTIAL_ENV_KEY], "sk-test");
        }
    }

    #[test]
    fn deepseek_harness_invalid_credentials_do_not_modify_files() {
        for raw in [
            "version: 2\nrefs: {USER_KEY: test-secret}\n",
            "version: 1\nrefs: test-secret\n",
            "version: 1\nrefs: [test-secret]\n",
            "version: 1\nrefs: [\n",
            "- test-secret\n",
        ] {
            with_temp_home(|_| {
                let (settings_path, credentials_path) = deepseek_harness_config_paths();
                fs::create_dir_all(credentials_path.parent().unwrap()).unwrap();
                let settings = "ui: {theme: dark}\n";
                fs::write(&settings_path, settings).unwrap();
                fs::write(&credentials_path, raw).unwrap();
                let models = [ToolModelInfo {
                    id: "model-a".to_string(),
                    ..ToolModelInfo::default()
                }];
                let apply = || apply_deepseek_harness_config(
                    TOOL_CONFIG_OPENAI_BASE_URL, "sk-test", &models, ToolProtocol::OpenAiChat,
                );
                assert!(preview_tool_config_apply(apply).is_err());
                assert!(apply().is_err());
                assert_eq!(fs::read_to_string(&settings_path).unwrap(), settings);
                assert_eq!(fs::read_to_string(&credentials_path).unwrap(), raw);
            });
        }
    }

    #[test]
    fn deepseek_harness_protocol_choice_controls_api_surface_and_check() {
        with_temp_home(|_| {
            let api_key = "sk-harness-protocols";
            let models = vec![ToolModelInfo {
                id: "model-a".to_string(),
                display_name: "Model A".to_string(),
                ..ToolModelInfo::default()
            }];
            let cases = [
                (
                    ToolProtocol::OpenAiChat,
                    "openai-completions",
                    TOOL_CONFIG_OPENAI_BASE_URL,
                ),
                (
                    ToolProtocol::OpenAiResponses,
                    "openai-responses",
                    TOOL_CONFIG_OPENAI_BASE_URL,
                ),
                (
                    ToolProtocol::AnthropicMessages,
                    "anthropic-messages",
                    TOOL_CONFIG_ANTHROPIC_BASE_URL,
                ),
            ];

            for (protocol, expected_api, expected_base_url) in cases {
                apply_additional_tool_config_with_model_info_for_protocol(
                    "deepseek-harness",
                    TOOL_CONFIG_OPENAI_BASE_URL,
                    api_key,
                    &models,
                    protocol,
                )
                .unwrap_or_else(|error| panic!("apply Harness {protocol:?}: {error:#}"));

                let settings: serde_yaml::Value = serde_yaml::from_str(
                    &fs::read_to_string(deepseek_harness_config_paths().0)
                        .expect("read Harness protocol settings"),
                )
                .expect("parse Harness protocol settings");
                let provider =
                    &settings["llm-pi-ai"]["providers"][ADDITIONAL_CONST_API_PROVIDER_ID];
                assert_eq!(provider["api"], expected_api);
                assert_eq!(provider["baseURL"], expected_base_url);

                let checked = check_additional_tool_config(
                    "deepseek-harness",
                    TOOL_CONFIG_OPENAI_BASE_URL,
                    api_key,
                )
                .unwrap_or_else(|error| panic!("check Harness {protocol:?}: {error:#}"));
                assert!(checked.already_configured, "{checked:#?}");
                assert_eq!(
                    checked.details.get("tool_protocol").map(String::as_str),
                    Some(protocol.as_str())
                );
            }
        });
    }

    #[test]
    fn claude_science_runtime_config_applies_checks_and_restores_original_file() {
        with_temp_home(|_| {
            let path = claude_science_env_path();
            fs::create_dir_all(path.parent().expect("Claude Science config parent"))
                .expect("create Claude Science config parent");
            let original = "KEEP_SCIENCE_SETTING=1\n";
            fs::write(&path, original).expect("seed Claude Science config");

            let applied = apply_claude_science_config(
                TOOL_CONFIG_ANTHROPIC_BASE_URL,
                "sk-science",
            )
            .expect("apply Claude Science");
            assert!(applied.file_statuses.iter().any(|status| status.changed));
            assert_eq!(
                applied.details.get("configuration_scope").map(String::as_str),
                Some("const_api_launch_environment")
            );
            let configured = fs::read_to_string(&path).expect("read Claude Science config");
            assert_eq!(
                env_file_value(&configured, "ANTHROPIC_BASE_URL"),
                Some(TOOL_CONFIG_ANTHROPIC_BASE_URL)
            );
            assert_eq!(
                env_file_value(&configured, "ANTHROPIC_AUTH_TOKEN"),
                Some("sk-science")
            );
            assert_eq!(env_file_value(&configured, "KEEP_SCIENCE_SETTING"), Some("1"));

            assert!(check_claude_science_config(
                TOOL_CONFIG_ANTHROPIC_BASE_URL,
                "sk-science",
            )
            .expect("check Claude Science")
            .already_configured);
            assert!(!check_claude_science_config(
                TOOL_CONFIG_ANTHROPIC_BASE_URL,
                "sk-other",
            )
            .expect("check Claude Science changed key")
            .already_configured);

            remove_claude_science_config().expect("remove Claude Science");
            assert_eq!(
                fs::read_to_string(&path).expect("read restored Claude Science config"),
                original
            );
        });
    }

    #[test]
    fn codex_provider_migrates_owned_lowercase_id_to_uppercase() {
        let output = upsert_codex_toml(
            concat!(
                "model_provider = \"const_api\"\n\n",
                "[model_providers.const_api]\n",
                "name = \"CONST API\"\n",
                "base_url = \"http://127.0.0.1:38787/v1\"\n",
                "wire_api = \"responses\"\n",
                "experimental_bearer_token = \"sk-test\"\n",
            ),
            TOOL_CONFIG_OPENAI_BASE_URL,
            "sk-test",
        )
        .expect("upsert Codex config");
        let doc = output.parse::<DocumentMut>().expect("parse Codex config");

        assert_eq!(
            doc.get("model_provider").and_then(Item::as_str),
            Some("CONST_API")
        );
        let providers = doc
            .get("model_providers")
            .and_then(Item::as_table)
            .expect("provider table");
        assert!(providers.contains_key("CONST_API"));
        assert!(!providers.contains_key("const_api"));
        assert_eq!(
            doc.get("experimental_realtime_ws_base_url")
                .and_then(Item::as_str),
            Some(TOOL_CONFIG_OPENAI_BASE_URL)
        );
        assert_eq!(
            doc.get("experimental_realtime_webrtc_call_base_url")
                .and_then(Item::as_str),
            Some(TOOL_CONFIG_OPENAI_BASE_URL)
        );
        assert_eq!(
            providers["CONST_API"]
                .get("supports_websockets")
                .and_then(Item::as_bool),
            Some(true)
        );
    }

    #[test]
    fn codex_writer_preserves_user_model_preferences_and_does_not_add_defaults() {
        let output = upsert_codex_toml(
            concat!(
                "model_reasoning_effort = \"medium\"\n",
                "disable_response_storage = false\n",
            ),
            TOOL_CONFIG_OPENAI_BASE_URL,
            "sk-test",
        )
        .expect("upsert Codex config with user preferences");
        let doc = output.parse::<DocumentMut>().expect("parse Codex config");
        assert_eq!(
            doc.get("model_reasoning_effort").and_then(Item::as_str),
            Some("medium")
        );
        assert_eq!(
            doc.get("disable_response_storage").and_then(Item::as_bool),
            Some(false)
        );

        let output = upsert_codex_toml("", TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
            .expect("upsert Codex config without user preferences");
        let doc = output.parse::<DocumentMut>().expect("parse Codex config");
        assert!(doc.get("model_reasoning_effort").is_none());
        assert!(doc.get("disable_response_storage").is_none());
    }

    #[test]
    fn codex_reapply_retires_legacy_preferences_with_three_way_restore() {
        with_temp_home(|home| {
            let config_path = home.join(".codex").join("config.toml");
            fs::create_dir_all(config_path.parent().expect("Codex config parent"))
                .expect("create Codex config parent");
            let original = concat!(
                "model_provider = \"openai\"\n",
                "model_reasoning_effort = \"medium\"\n",
                "disable_response_storage = false\n",
            );
            fs::write(&config_path, original).expect("seed original Codex config");

            let legacy_applied = concat!(
                "model_provider = \"CONST_API\"\n",
                "model_reasoning_effort = \"high\"\n",
                "disable_response_storage = true\n\n",
                "[model_providers.CONST_API]\n",
                "name = \"CONST API\"\n",
                "base_url = \"http://127.0.0.1:38787/v1\"\n",
                "wire_api = \"responses\"\n",
                "requires_openai_auth = true\n",
                "experimental_bearer_token = \"sk-test\"\n",
            );
            let mut legacy_result = ToolApplyBuilder::default();
            write_text_with_backup(
                &config_path,
                legacy_applied,
                "codex",
                &mut legacy_result,
            )
            .expect("simulate legacy Codex apply");

            let mut user_edited = fs::read_to_string(&config_path)
                .expect("read legacy Codex config")
                .parse::<DocumentMut>()
                .expect("parse legacy Codex config");
            user_edited["model_reasoning_effort"] = toml_value("xhigh");
            fs::write(&config_path, user_edited.to_string())
                .expect("simulate later user preference edit");

            apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("reapply Codex config");
            let applied = fs::read_to_string(&config_path)
                .expect("read reapplied Codex config")
                .parse::<DocumentMut>()
                .expect("parse reapplied Codex config");
            assert_eq!(
                applied
                    .get("model_reasoning_effort")
                    .and_then(Item::as_str),
                Some("xhigh"),
                "a later user edit must survive reapply"
            );
            assert_eq!(
                applied
                    .get("disable_response_storage")
                    .and_then(Item::as_bool),
                Some(false),
                "an untouched legacy CONST value must restore the pre-CONST value"
            );

            remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("cancel Codex config");
            let restored = fs::read_to_string(&config_path)
                .expect("read restored Codex config")
                .parse::<DocumentMut>()
                .expect("parse restored Codex config");
            assert_eq!(
                restored.get("model_provider").and_then(Item::as_str),
                Some("openai")
            );
            assert_eq!(
                restored
                    .get("model_reasoning_effort")
                    .and_then(Item::as_str),
                Some("xhigh")
            );
            assert_eq!(
                restored
                    .get("disable_response_storage")
                    .and_then(Item::as_bool),
                Some(false)
            );
        });
    }

    #[test]
    fn tool_configs_write_uppercase_provider_ids() {
        with_temp_home(|home| {
            apply_opencode_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").expect("apply OpenCode");
            let opencode = read_json_or_default(
                &home.join(".config").join("opencode").join("opencode.json"),
                serde_json::json!({}),
            )
            .expect("read OpenCode config");
            assert!(opencode["provider"].get("CONST_API").is_some());
            assert!(opencode["provider"].get("const_api").is_none());

            apply_openclaw_config(TOOL_CONFIG_ROOT_URL, "sk-test").expect("apply OpenClaw");
            let openclaw = read_json_or_default(
                &home.join(".openclaw").join("openclaw.json"),
                serde_json::json!({}),
            )
            .expect("read OpenClaw config");
            assert!(openclaw["models"]["providers"].get("CONST_API").is_some());
            assert!(openclaw["models"]["providers"].get("const_api").is_none());

            let hermes = upsert_hermes_sections_for_protocol(
                "custom_providers:\n- name: const_api\n  base_url: http://127.0.0.1:38787/v1\n  api_key: sk-test\n  api_mode: chat_completions\n",
                TOOL_CONFIG_ROOT_URL,
                "sk-test",
                ToolProtocol::OpenAiChat,
            )
            .expect("upsert Hermes config");
            let root =
                serde_yaml::from_str::<serde_yaml::Value>(&hermes).expect("parse Hermes config");
            let provider = root["custom_providers"][0]["name"]
                .as_str()
                .expect("Hermes provider name");
            assert_eq!(provider, "CONST_API");
            assert_eq!(root["model"]["provider"].as_str(), Some("custom:CONST_API"));
        });
    }

    #[test]
    fn cancellation_removes_owned_lowercase_and_uppercase_provider_aliases() {
        with_temp_home(|home| {
            let codex_path = home.join(".codex").join("config.toml");
            fs::create_dir_all(codex_path.parent().unwrap()).expect("Codex dir");
            fs::write(
                &codex_path,
                concat!(
                    "model_provider = \"const_api\"\n\n",
                    "[model_providers.const_api]\n",
                    "name = \"CONST API\"\n",
                    "base_url = \"http://127.0.0.1:38787/v1\"\n",
                    "wire_api = \"responses\"\n",
                    "experimental_bearer_token = \"sk-test\"\n",
                ),
            )
            .expect("legacy Codex config");
            apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").expect("apply Codex");
            remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").expect("cancel Codex");
            let codex = fs::read_to_string(&codex_path).unwrap_or_default();
            assert!(!codex.contains("const_api"));
            assert!(!codex.contains("CONST_API"));

            let opencode_path = home.join(".config/opencode/opencode.json");
            fs::create_dir_all(opencode_path.parent().unwrap()).expect("OpenCode dir");
            fs::write(
                &opencode_path,
                r#"{"provider":{"const_api":{"name":"CONST API","npm":"@ai-sdk/openai-compatible","options":{"baseURL":"http://127.0.0.1:38787/v1","apiKey":"sk-test"}}}}"#,
            )
            .expect("legacy OpenCode config");
            apply_opencode_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").expect("apply OpenCode");
            remove_opencode_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("cancel OpenCode");
            let opencode =
                read_json_or_default(&opencode_path, serde_json::json!({})).expect("read OpenCode");
            assert!(opencode["provider"].get("const_api").is_none());
            assert!(opencode["provider"].get("CONST_API").is_none());

            let openclaw_path = home.join(".openclaw/openclaw.json");
            fs::create_dir_all(openclaw_path.parent().unwrap()).expect("OpenClaw dir");
            fs::write(
                &openclaw_path,
                r#"{"models":{"providers":{"const_api":{"baseUrl":"http://127.0.0.1:38787/v1","apiKey":"sk-test","api":"openai-completions"}}}}"#,
            )
            .expect("legacy OpenClaw config");
            apply_openclaw_config(TOOL_CONFIG_ROOT_URL, "sk-test").expect("apply OpenClaw");
            remove_openclaw_config(TOOL_CONFIG_ROOT_URL, "sk-test").expect("cancel OpenClaw");
            let openclaw =
                read_json_or_default(&openclaw_path, serde_json::json!({})).expect("read OpenClaw");
            assert!(openclaw["models"]["providers"].get("const_api").is_none());
            assert!(openclaw["models"]["providers"].get("CONST_API").is_none());

            let hermes_path = hermes_config_paths()
                .into_iter()
                .next()
                .expect("Hermes path");
            fs::create_dir_all(hermes_path.parent().unwrap()).expect("Hermes dir");
            fs::write(
                &hermes_path,
                "model:\n  provider: custom:const_api\n  base_url: http://127.0.0.1:38787/v1\n  api_key: sk-test\n  api_mode: chat_completions\ncustom_providers:\n- name: const_api\n  base_url: http://127.0.0.1:38787/v1\n  api_key: sk-test\n  api_mode: chat_completions\n",
            )
            .expect("legacy Hermes config");
            apply_hermes_config(TOOL_CONFIG_ROOT_URL, "sk-test").expect("apply Hermes");
            remove_hermes_config(TOOL_CONFIG_ROOT_URL, "sk-test").expect("cancel Hermes");
            let hermes = fs::read_to_string(&hermes_path).unwrap_or_default();
            assert!(!hermes.contains("custom:const_api"));
            assert!(!hermes.contains("custom:CONST_API"));
            assert!(!hermes.contains("name: const_api"));
            assert!(!hermes.contains("name: CONST_API"));
        });
    }

    #[test]
    fn identical_backup_content_is_not_recorded_twice() {
        with_temp_home(|home| {
            let path = home.join("config.toml");
            fs::write(&path, "value = 1\n").expect("write source");
            let mut backups = Vec::new();

            backup_if_exists(&path, "codex", &mut backups).expect("first backup");
            backup_if_exists(&path, "codex", &mut backups).expect("second backup");

            assert_eq!(backups.len(), 1);
            assert_eq!(
                fs::read_dir(backup_root().join("codex")).unwrap().count(),
                1
            );
        });
    }

    #[test]
    fn same_named_config_files_use_distinct_backup_series() {
        with_temp_home(|home| {
            let first = home.join("first").join("claude_desktop_config.json");
            let second = home.join("second").join("claude_desktop_config.json");
            fs::create_dir_all(first.parent().expect("first parent")).expect("mkdir first");
            fs::create_dir_all(second.parent().expect("second parent")).expect("mkdir second");
            fs::write(&first, b"first").expect("write first");
            fs::write(&second, b"second").expect("write second");
            let mut backups = Vec::new();

            backup_if_exists(&first, "claude-desktop", &mut backups).expect("backup first");
            backup_if_exists(&second, "claude-desktop", &mut backups).expect("backup second");

            assert_eq!(backups.len(), 2);
            let contents = backups
                .iter()
                .map(|path| fs::read_to_string(path).expect("backup content"))
                .collect::<HashSet<_>>();
            assert_eq!(
                contents,
                HashSet::from(["first".to_string(), "second".to_string()])
            );
        });
    }

    #[test]
    fn backup_retention_is_kept_per_source_path_without_dropping_legacy_series() {
        with_temp_home(|home| {
            let config = home.join("config.toml");
            let database = home.join("state_5.sqlite");
            fs::write(&config, "latest-config").expect("write config");
            fs::write(&database, "latest-database").expect("write database");
            let config_dir = backup_root().join("codex");
            let database_dir = backup_root().join("codex-state");
            fs::create_dir_all(&config_dir).expect("config backup dir");
            fs::create_dir_all(&database_dir).expect("database backup dir");
            for stamp in 1..=7 {
                fs::write(
                    config_dir.join(format!("{stamp}-config.toml")),
                    stamp.to_string(),
                )
                .expect("old config backup");
                fs::write(
                    database_dir.join(format!("{stamp}-state_5.sqlite")),
                    stamp.to_string(),
                )
                .expect("old database backup");
            }
            let mut backups = Vec::new();

            backup_if_exists(&config, "codex", &mut backups).expect("config backup");
            backup_if_exists(&database, "codex-state", &mut backups).expect("database backup");

            // Five bounded legacy backups remain, plus the first path-scoped
            // backup. The two series are intentionally not merged because old
            // same-named backups cannot be attributed to a source path.
            assert_eq!(fs::read_dir(config_dir).unwrap().count(), 6);
            assert_eq!(fs::read_dir(database_dir).unwrap().count(), 3);
        });
    }

    #[test]
    fn startup_backup_cleanup_prunes_existing_series_without_creating_a_backup() {
        with_temp_home(|_| {
            let config_dir = backup_root().join("codex");
            let database_dir = backup_root().join("codex-state");
            fs::create_dir_all(&config_dir).expect("config backup dir");
            fs::create_dir_all(&database_dir).expect("database backup dir");
            for stamp in 1..=7 {
                fs::write(
                    config_dir.join(format!("{stamp}-config.toml")),
                    stamp.to_string(),
                )
                .expect("old config backup");
                fs::write(
                    database_dir.join(format!("{stamp}-state_5.sqlite")),
                    stamp.to_string(),
                )
                .expect("old database backup");
            }

            cleanup_backup_retention().expect("startup cleanup");

            assert_eq!(fs::read_dir(config_dir).unwrap().count(), 5);
            assert_eq!(fs::read_dir(database_dir).unwrap().count(), 2);
        });
    }

    #[test]
    fn prior_loopback_tool_urls_remain_owned_after_listen_port_changes() {
        assert!(is_current_or_const_api_local_url(
            "http://127.0.0.1:8787/v1",
            TOOL_CONFIG_OPENAI_BASE_URL,
        ));
        assert!(is_current_or_const_api_local_url(
            "http://127.0.0.1:8787/anthropic",
            TOOL_CONFIG_ANTHROPIC_BASE_URL,
        ));
        assert!(is_current_or_const_api_local_url(
            "http://127.0.0.1:19432/gemini",
            "http://127.0.0.1:52109/gemini",
        ));
        assert!(!is_current_or_const_api_local_url(
            "http://127.0.0.1:19432/v1",
            "http://127.0.0.1:52109/anthropic",
        ));
        assert!(!is_current_or_const_api_local_url(
            "http://localhost:8787/v1",
            TOOL_CONFIG_OPENAI_BASE_URL,
        ));
    }

    #[test]
    fn tool_surface_urls_never_nest_canonical_mounts() {
        for (root, port) in [
            ("http://127.0.0.1:38787", 38787),
            ("http://127.0.0.1:38787/v1", 38787),
            ("http://127.0.0.1:38787/anthropic", 38787),
            ("http://127.0.0.1:38787/gemini", 38787),
            ("http://0.0.0.0:52109", 52109),
            ("http://localhost:43127", 43127),
            ("http://127.0.0.1:19432", 19432),
            ("http://[::1]:30241/v1", 30241),
        ] {
            let expected_root = format!("http://127.0.0.1:{port}");
            assert_eq!(tool_surface_url(root, "v1"), format!("{expected_root}/v1"));
            assert_eq!(
                tool_surface_url(root, "anthropic"),
                format!("{expected_root}/anthropic")
            );
            assert_eq!(
                tool_surface_url(root, "gemini"),
                format!("{expected_root}/gemini")
            );
        }
    }

    #[test]
    fn codex_writer_uses_the_supplied_port_on_loopback() {
        with_temp_home(|home| {
            apply_codex_config("http://localhost:19432/v1", "sk-test").expect("apply Codex config");

            let config =
                fs::read_to_string(home.join(".codex/config.toml")).expect("read Codex config");
            assert!(config.contains("http://127.0.0.1:19432/v1"), "{config}");
            assert!(!config.contains("localhost:19432"), "{config}");
        });
    }

    #[test]
    fn codex_realtime_overrides_are_owned_and_restored_with_the_provider() {
        with_temp_home(|home| {
            let path = home.join(".codex/config.toml");
            fs::create_dir_all(path.parent().expect("Codex config parent"))
                .expect("create Codex config parent");
            let original = concat!(
                "model_provider = \"openai\"\n",
                "experimental_realtime_ws_base_url = \"https://voice.example/v1\"\n",
                "experimental_realtime_webrtc_call_base_url = \"https://calls.example/v1\"\n",
                "[realtime]\ntransport = \"webrtc\"\nvoice = \"sage\"\n",
            );
            fs::write(&path, original).expect("seed Codex realtime config");

            apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("apply Codex config");
            let applied = fs::read_to_string(&path).expect("read applied config");
            assert!(applied.contains("transport = \"webrtc\""));
            assert!(applied.contains("voice = \"sage\""));
            assert!(applied.contains(&format!(
                "experimental_realtime_ws_base_url = \"{TOOL_CONFIG_OPENAI_BASE_URL}\""
            )));
            assert!(applied.contains(&format!(
                "experimental_realtime_webrtc_call_base_url = \"{TOOL_CONFIG_OPENAI_BASE_URL}\""
            )));

            remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("restore Codex config");
            assert_eq!(fs::read_to_string(path).expect("read restored config"), original);
        });
    }

    #[test]
    fn every_tool_writer_uses_the_supplied_port_on_loopback() {
        with_temp_home(|home| {
            for supplied_root in [
                "http://0.0.0.0:52109",
                "http://localhost:43127",
                "http://127.0.0.1:19432",
            ] {
                apply_claude_config(supplied_root, "sk-test").expect("apply Claude config");
                apply_claude_desktop_config(supplied_root, "sk-test")
                    .expect("apply Claude Desktop config");
                apply_gemini_config(supplied_root, "sk-test").expect("apply Gemini config");
                apply_opencode_config(supplied_root, "sk-test").expect("apply OpenCode config");
                apply_openclaw_config(supplied_root, "sk-test").expect("apply OpenClaw config");
                apply_hermes_config(supplied_root, "sk-test").expect("apply Hermes config");
                apply_workbuddy_config_with_model_info_for_protocol(
                    supplied_root,
                    "sk-test",
                    &workbuddy_test_models()[..1],
                    ToolProtocol::OpenAiChat,
                )
                .expect("apply WorkBuddy config");

                let port = local_tool_url_port(supplied_root).expect("local tool URL port");
                let expected_root = format!("http://127.0.0.1:{port}");
                let outputs = [
                    (
                        home.join(".claude/settings.json"),
                        format!("{expected_root}/anthropic"),
                    ),
                    (
                        claude_desktop_paths().profile_path,
                        format!("{expected_root}/anthropic"),
                    ),
                    (
                        home.join(".gemini/.env"),
                        format!("{expected_root}/gemini"),
                    ),
                    (
                        home.join(".config/opencode/opencode.json"),
                        format!("{expected_root}/v1"),
                    ),
                    (
                        home.join(".openclaw/openclaw.json"),
                        format!("{expected_root}/v1"),
                    ),
                    (
                        home.join(".hermes/config.yaml"),
                        format!("{expected_root}/v1"),
                    ),
                    (
                        home.join(".workbuddy/models.json"),
                        format!("{expected_root}/v1/chat/completions"),
                    ),
                ];
                for (path, expected_url) in outputs {
                    let output = fs::read_to_string(&path)
                        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
                    assert!(
                        output.contains(&expected_url),
                        "{}: {output}",
                        path.display()
                    );
                    if !supplied_root.starts_with("http://127.0.0.1:") {
                        assert!(
                            !output.contains(supplied_root),
                            "{} retained non-loopback root {supplied_root}: {output}",
                            path.display()
                        );
                    }
                }
            }
        });
    }

    #[test]
    fn tool_protocol_matrix_only_exposes_supported_choices() {
        assert_eq!(
            resolve_tool_protocol("codex", None).expect("codex default"),
            ToolProtocol::OpenAiResponses
        );
        assert_eq!(
            resolve_tool_protocol("claude", None).expect("claude default"),
            ToolProtocol::AnthropicMessages
        );
        assert_eq!(
            resolve_tool_protocol("gemini", None).expect("gemini default"),
            ToolProtocol::GeminiNative
        );
        assert_eq!(
            resolve_tool_protocol("opencode", None).expect("opencode default"),
            ToolProtocol::OpenAiResponses
        );
        assert_eq!(
            resolve_tool_protocol("openclaw", None).expect("openclaw default"),
            ToolProtocol::OpenAiResponses
        );
        assert_eq!(
            resolve_tool_protocol("hermes", None).expect("hermes default"),
            ToolProtocol::OpenAiChat
        );
        assert_eq!(
            resolve_tool_protocol("workbuddy", None).expect("WorkBuddy default"),
            ToolProtocol::OpenAiChat
        );
        assert_eq!(
            resolve_tool_protocol("kimicode", None).expect("Kimi Code default"),
            ToolProtocol::OpenAiResponses
        );
        assert_eq!(
            resolve_tool_protocol("kimicode", Some("openai_chat")).expect("Kimi Code chat"),
            ToolProtocol::OpenAiChat
        );
        for tool in ["pi", "cline"] {
            assert_eq!(
                resolve_tool_protocol(tool, None).expect("dual-protocol tool default"),
                ToolProtocol::OpenAiResponses
            );
            assert_eq!(
                resolve_tool_protocol(tool, Some("openai_chat"))
                    .expect("dual-protocol tool chat"),
                ToolProtocol::OpenAiChat
            );
        }
        assert_eq!(
            resolve_tool_protocol("open-design", None).expect("Open Design default"),
            ToolProtocol::OpenAiResponses
        );
        assert!(resolve_tool_protocol("open-design", Some("openai_chat")).is_err());
        assert_eq!(
            resolve_tool_protocol("deepseek-harness", None)
                .expect("DeepSeek Harness default"),
            ToolProtocol::OpenAiChat
        );
        for (name, protocol) in [
            ("chat", ToolProtocol::OpenAiChat),
            ("responses", ToolProtocol::OpenAiResponses),
            ("messages", ToolProtocol::AnthropicMessages),
        ] {
            assert_eq!(
                resolve_tool_protocol("deepseek-harness", Some(protocol.as_str()))
                    .unwrap_or_else(|error| panic!("DeepSeek Harness {name}: {error:#}")),
                protocol
            );
        }
        assert!(resolve_tool_protocol("deepseek-harness", Some("gemini_native")).is_err());
        for tool in [
            "copilot",
            "raven",
            "reasonix",
            "open-interpreter",
            "goose",
            "mistral-vibe",
            "mimocode",
            "qwencode",
            "openscience",
            "vibe-trading",
            "zcode",
        ] {
            assert_eq!(
                resolve_tool_protocol(tool, None).expect("additional tool default"),
                ToolProtocol::OpenAiChat
            );
            assert!(resolve_tool_protocol(tool, Some("openai_responses")).is_err());
        }
        assert_eq!(
            resolve_tool_protocol("opencode", Some("openai_chat")).expect("opencode chat"),
            ToolProtocol::OpenAiChat
        );
        assert!(resolve_tool_protocol("opencode", Some("anthropic_messages")).is_err());
        assert!(resolve_tool_protocol("opencode", Some("gemini_native")).is_err());
        assert_eq!(
            resolve_tool_protocol("openclaw", Some("anthropic_messages"))
                .expect("openclaw messages"),
            ToolProtocol::AnthropicMessages
        );
        assert_eq!(
            resolve_tool_protocol("hermes", Some("openai_chat")).expect("hermes chat"),
            ToolProtocol::OpenAiChat
        );
        assert!(resolve_tool_protocol("hermes", Some("openai_responses")).is_err());
        assert!(resolve_tool_protocol("hermes", Some("anthropic_messages")).is_err());
        assert!(resolve_tool_protocol("hermes", Some("gemini_native")).is_err());
        assert!(resolve_tool_protocol("workbuddy", Some("openai_responses")).is_err());
        assert!(resolve_tool_protocol("workbuddy", Some("anthropic_messages")).is_err());
        assert!(resolve_tool_protocol("workbuddy", Some("gemini_native")).is_err());
        assert!(resolve_tool_protocol("codex", Some("openai_chat")).is_err());
    }

    #[test]
    fn opencode_protocol_choice_controls_package_and_surface() {
        let cases = [
            (
                ToolProtocol::OpenAiResponses,
                "@ai-sdk/openai",
                "http://127.0.0.1:38787/v1",
            ),
            (
                ToolProtocol::OpenAiChat,
                "@ai-sdk/openai-compatible",
                "http://127.0.0.1:38787/v1",
            ),
        ];
        for (protocol, package, base_url) in cases {
            let provider =
                opencode_const_api_provider("http://127.0.0.1:38787/v1", "sk-test", &[], protocol);
            assert_eq!(provider["npm"], package);
            assert_eq!(provider["options"]["baseURL"], base_url);
            assert!(provider["options"].get("setCacheKey").is_none());
        }
    }

    #[test]
    fn opencode_legacy_cache_disable_is_removed_without_overwriting_opt_in() {
        let mut legacy = serde_json::json!({
            "options": {"setCacheKey": false, "custom": "kept"}
        });
        remove_opencode_legacy_cache_disable(&mut legacy);
        assert!(legacy["options"].get("setCacheKey").is_none());
        assert_eq!(legacy["options"]["custom"], "kept");

        let mut explicit = serde_json::json!({"options": {"setCacheKey": true}});
        remove_opencode_legacy_cache_disable(&mut explicit);
        assert_eq!(explicit["options"]["setCacheKey"], true);
    }

    #[test]
    fn multi_protocol_tool_configs_round_trip_and_cancel_without_selection() {
        with_temp_home(|_| {
            let root_url = "http://127.0.0.1:38787";
            let base_url = "http://127.0.0.1:38787/v1";
            let api_key = "sk-test";

            for protocol in [ToolProtocol::OpenAiResponses, ToolProtocol::OpenAiChat] {
                apply_opencode_config_with_model_info_for_protocol(
                    base_url,
                    api_key,
                    &[],
                    protocol,
                )
                .expect("apply OpenCode protocol");
                let checked = check_opencode_config(base_url, api_key)
                    .expect("check OpenCode protocol");
                assert!(checked.already_configured);
                assert_eq!(
                    checked.details.get("tool_protocol").map(String::as_str),
                    Some(protocol.as_str())
                );
                remove_opencode_config(base_url, api_key).expect("cancel OpenCode protocol");
            }

            for protocol in [
                ToolProtocol::OpenAiResponses,
                ToolProtocol::OpenAiChat,
                ToolProtocol::AnthropicMessages,
                ToolProtocol::GeminiNative,
            ] {
                apply_openclaw_config_for_protocol(root_url, api_key, protocol)
                    .expect("apply OpenClaw protocol");
                let checked =
                    check_openclaw_config(root_url, api_key).expect("check OpenClaw protocol");
                assert!(checked.already_configured);
                assert_eq!(
                    checked.details.get("tool_protocol").map(String::as_str),
                    Some(protocol.as_str())
                );
                remove_openclaw_config(root_url, api_key).expect("cancel OpenClaw protocol");
            }

            for protocol in [ToolProtocol::OpenAiChat] {
                apply_hermes_config_for_protocol(root_url, api_key, protocol)
                    .expect("apply Hermes protocol");
                let checked =
                    check_hermes_config(root_url, api_key).expect("check Hermes protocol");
                assert!(checked.already_configured);
                assert_eq!(
                    checked.details.get("tool_protocol").map(String::as_str),
                    Some(protocol.as_str())
                );
                remove_hermes_config(root_url, api_key).expect("cancel Hermes protocol");
            }
        });
    }

    #[test]
    fn cancel_claude_config_restores_manifest_original_bytes() {
        with_temp_home(|home| {
            let path = home.join(".claude/settings.json");
            let onboarding_path = home.join(".claude.json");
            fs::create_dir_all(path.parent().expect("claude parent")).expect("mkdir claude");
            let original = concat!(
                "{\n",
                "  \"env\": {\"ANTHROPIC_BASE_URL\": \"https://api.anthropic.com\", \"ANTHROPIC_AUTH_TOKEN\": \"sk-user\", \"ANTHROPIC_API_KEY\": \"sk-conflict\", \"CLAUDE_CODE_USE_VERTEX\": \"true\"},\n",
                "  \"permissions\": {\"allow\": [\"Read\"]}\n",
                "}\n"
            );
            let onboarding_original =
                "{\n  \"hasCompletedOnboarding\": false,\n  \"keep\": \"yes\"\n}\n";
            fs::write(&path, original).expect("seed claude");
            fs::write(&onboarding_path, onboarding_original).expect("seed claude onboarding");

            apply_claude_config("http://127.0.0.1:38787", "sk-test").expect("apply claude");
            let applied_settings: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&path).expect("read applied Claude settings"),
            )
            .expect("parse applied Claude settings");
            assert!(applied_settings["env"].get("ANTHROPIC_API_KEY").is_none());
            assert!(applied_settings["env"]
                .get("CLAUDE_CODE_USE_VERTEX")
                .is_none());
            let applied_onboarding: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&onboarding_path).expect("read applied onboarding"),
            )
            .expect("parse applied onboarding");
            assert_eq!(applied_onboarding["hasCompletedOnboarding"], true);
            remove_claude_config("http://127.0.0.1:38787", "sk-test").expect("cancel claude");

            assert_eq!(fs::read_to_string(&path).expect("read claude"), original);
            assert_eq!(
                fs::read_to_string(&onboarding_path).expect("read claude onboarding"),
                onboarding_original
            );
        });
    }

    #[test]
    fn cancel_gemini_config_restores_manifest_original_bytes() {
        with_temp_home(|home| {
            let path = home.join(".gemini/.env");
            let settings_path = home.join(".gemini/settings.json");
            fs::create_dir_all(path.parent().expect("gemini parent")).expect("mkdir gemini");
            let original = "GOOGLE_GEMINI_BASE_URL=https://generativelanguage.googleapis.com\nGEMINI_API_KEY=sk-user\nGOOGLE_GENAI_USE_VERTEXAI=true\nGEMINI_CLI_HOME=/tmp/other-gemini-home\nKEEP_ME=yes\n";
            let settings_original = concat!(
                "{\n",
                "  \"security\": {\"auth\": {\"selectedType\": \"oauth-personal\"}},\n",
                "  \"keep\": \"yes\"\n",
                "}\n"
            );
            fs::write(&path, original).expect("seed gemini");
            fs::write(&settings_path, settings_original).expect("seed gemini settings");

            apply_gemini_config("http://127.0.0.1:38787", "sk-test").expect("apply gemini");
            let applied_env = env_text_to_owned_json(
                &fs::read_to_string(&path).expect("read applied Gemini env"),
            );
            assert!(applied_env.get("GOOGLE_GENAI_USE_VERTEXAI").is_none());
            assert!(applied_env.get("GEMINI_CLI_HOME").is_none());
            let applied_settings: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&settings_path).expect("read applied settings"),
            )
            .expect("parse applied settings");
            assert_eq!(
                applied_settings["security"]["auth"]["selectedType"],
                "gemini-api-key"
            );
            remove_gemini_config("http://127.0.0.1:38787", "sk-test").expect("cancel gemini");

            assert_eq!(fs::read_to_string(&path).expect("read gemini"), original);
            assert_eq!(
                fs::read_to_string(&settings_path).expect("read gemini settings"),
                settings_original
            );
        });
    }

    #[test]
    fn native_codex_remove_uses_builtin_provider_and_preserves_official_login() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).expect("mkdir codex");
            let config_path = codex_dir.join("config.toml");
            let auth_path = codex_dir.join("auth.json");
            fs::write(
                &config_path,
                concat!(
                    "model_provider = \"third_party\"\n",
                    "model = \"user-model\"\n",
                    "approval_policy = \"on-request\"\n\n",
                    "[model_providers.third_party]\n",
                    "name = \"User provider\"\n",
                    "base_url = \"https://provider.example/v1\"\n",
                ),
            )
            .expect("seed Codex config");
            fs::write(
                &auth_path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "tokens": {"access_token": "official-token"},
                    "OPENAI_API_KEY": "sk-previous-provider",
                    "keep": "yes"
                }))
                .expect("encode Codex auth"),
            )
            .expect("seed Codex auth");

            apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("apply Codex");
            let mut ignore_progress =
                |_: &str, _: Option<u64>, _: Option<u64>, _: Option<f64>| {};
            let result = remove_codex_config_with_progress_and_mode(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                ToolConfigRemoveMode::NativeRoute,
                &mut ignore_progress,
            )
            .expect("switch Codex to native route");

            let config = fs::read_to_string(&config_path).expect("read native Codex config");
            let doc = config.parse::<DocumentMut>().expect("parse native Codex config");
            assert!(doc.get("model_provider").is_none());
            assert_eq!(doc.get("model").and_then(Item::as_str), Some("user-model"));
            assert_eq!(
                doc.get("approval_policy").and_then(Item::as_str),
                Some("on-request")
            );
            assert!(doc
                .get("model_providers")
                .and_then(|providers| providers.get("third_party"))
                .is_some());
            assert!(doc
                .get("model_providers")
                .and_then(|providers| providers.get(CODEX_CONST_API_PROVIDER_ID))
                .is_none());

            let auth = read_json_or_default(&auth_path, serde_json::json!({}))
                .expect("read native Codex auth");
            assert_eq!(auth["tokens"]["access_token"], "official-token");
            assert_eq!(auth["keep"], "yes");
            assert!(auth.get("OPENAI_API_KEY").is_none());
            assert_eq!(
                result.details.get("remove_mode").map(String::as_str),
                Some("native_route")
            );
            assert_eq!(
                result
                    .details
                    .get("session_target_provider")
                    .map(String::as_str),
                Some("openai")
            );
        });
    }

    #[test]
    fn native_codex_remove_without_configuration_is_a_write_free_noop() {
        with_temp_home(|home| {
            let mut ignore_progress =
                |_: &str, _: Option<u64>, _: Option<u64>, _: Option<f64>| {};
            let result = remove_codex_config_with_progress_and_mode(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                ToolConfigRemoveMode::NativeRoute,
                &mut ignore_progress,
            )
            .expect("native Codex removal without configuration");

            assert!(result.files.is_empty());
            assert!(result.backups.is_empty());
            assert!(result.file_statuses.is_empty());
            assert_eq!(
                result.details.get("manifest_entries_restored").map(String::as_str),
                Some("0")
            );
            assert_eq!(
                result.details.get("remove_mode").map(String::as_str),
                Some("native_route")
            );
            assert!(!home.join(".codex").exists());
            assert!(!home.join(".const-api/tool-config-manifest.json").exists());
            assert!(!home.join(".const-api/codex-session-sync.json").exists());
        });
    }

    #[test]
    fn native_codex_remove_keeps_native_cleanup_when_session_index_cannot_run() {
        with_temp_home(|home| {
            let codex_dir = home.join(".codex");
            fs::create_dir_all(&codex_dir).expect("mkdir Codex");
            let config_path = codex_dir.join("config.toml");
            fs::write(
                &config_path,
                concat!(
                    "model_provider = \"third_party\"\n",
                    "sqlite_home = \"relative-state\"\n",
                    "model = \"user-model\"\n\n",
                    "[model_providers.third_party]\n",
                    "name = \"User provider\"\n",
                    "base_url = \"https://provider.example/v1\"\n",
                ),
            )
            .expect("seed Codex config");
            let session_path = codex_dir.join("sessions/2026/08/session.jsonl");
            fs::create_dir_all(session_path.parent().expect("session parent"))
                .expect("create session dir");
            fs::write(
                &session_path,
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"session\",\"model_provider\":\"CONST_API\"}}\n",
            )
            .expect("seed CONST API session");

            let mut ignore_progress =
                |_: &str, _: Option<u64>, _: Option<u64>, _: Option<f64>| {};
            let result = remove_codex_config_with_progress_and_mode(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                ToolConfigRemoveMode::NativeRoute,
                &mut ignore_progress,
            )
            .expect("native Codex cleanup must finish before session repair");

            let doc = fs::read_to_string(&config_path)
                .expect("read native Codex config")
                .parse::<DocumentMut>()
                .expect("parse native Codex config");
            assert!(doc.get("model_provider").is_none());
            assert_eq!(doc.get("model").and_then(Item::as_str), Some("user-model"));
            assert!(result
                .details
                .get("session_migrated_sqlite_error")
                .is_some_and(|error| error.contains("CODEX_SQLITE_HOME_INVALID")));
            assert!(!result.details.contains_key("session_sync_error"));
            assert_eq!(
                read_codex_session_provider(&session_path)
                    .expect("read restored session")
                    .as_deref(),
                Some("openai")
            );
            assert_eq!(
                result
                    .details
                    .get("session_migrated_files")
                    .map(String::as_str),
                Some("1")
            );
        });
    }

    #[test]
    fn native_claude_remove_clears_route_auth_but_preserves_models_and_settings() {
        with_temp_home(|home| {
            let settings_path = home.join(".claude/settings.json");
            fs::create_dir_all(settings_path.parent().expect("Claude parent"))
                .expect("mkdir Claude");
            fs::write(
                &settings_path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": "https://third-party.example/anthropic",
                        "ANTHROPIC_AUTH_TOKEN": "third-party-token",
                        "ANTHROPIC_API_KEY": "third-party-key",
                        "ANTHROPIC_CUSTOM_HEADERS": "X-Gateway: yes",
                        "ANTHROPIC_FOUNDRY_BASE_URL": "https://foundry.example/anthropic",
                        "CLAUDE_CODE_USE_VERTEX": "true",
                        "CLAUDE_CODE_USE_MANTLE": "true",
                        "ANTHROPIC_MODEL": "user-model",
                        "KEEP_ENV": "yes"
                    },
                    "apiKeyHelper": "third-party-key-helper",
                    "awsAuthRefresh": "third-party-aws-login",
                    "awsCredentialExport": "third-party-aws-export",
                    "model": "user-default-model",
                    "permissions": {"allow": ["Read"]}
                }))
                .expect("encode Claude settings"),
            )
            .expect("seed Claude settings");

            apply_claude_config(TOOL_CONFIG_ROOT_URL, "sk-test").expect("apply Claude");
            let result = remove_claude_config_with_mode(
                TOOL_CONFIG_ROOT_URL,
                "sk-test",
                ToolConfigRemoveMode::NativeRoute,
            )
            .expect("switch Claude to native route");

            let settings = read_json_or_default(&settings_path, serde_json::json!({}))
                .expect("read native Claude settings");
            let env = settings["env"].as_object().expect("Claude env object");
            for name in CLAUDE_CODE_EXTERNAL_ENVIRONMENT_NAMES {
                assert!(env.get(*name).is_none(), "{name} should be removed");
            }
            assert_eq!(env.get("ANTHROPIC_MODEL").and_then(serde_json::Value::as_str), Some("user-model"));
            assert_eq!(env.get("KEEP_ENV").and_then(serde_json::Value::as_str), Some("yes"));
            for setting in CLAUDE_CODE_NATIVE_ROUTE_SETTING_NAMES {
                assert!(settings.get(*setting).is_none(), "{setting} should be removed");
            }
            assert_eq!(settings["model"], "user-default-model");
            assert_eq!(settings["permissions"]["allow"][0], "Read");
            assert_eq!(
                result.details.get("native_route_kind").map(String::as_str),
                Some("claude_first_party_login")
            );
        });
    }

    #[test]
    fn native_claude_desktop_remove_sets_first_party_mode_and_preserves_other_profiles() {
        with_temp_home(|_| {
            let paths = claude_desktop_paths();
            for path in [&paths.normal_config_path, &paths.threep_config_path] {
                fs::create_dir_all(path.parent().expect("Claude Desktop config parent"))
                    .expect("mkdir Claude Desktop config");
                fs::write(
                    path,
                    serde_json::to_vec_pretty(&serde_json::json!({
                        "deploymentMode": "3p",
                        "keep": "yes"
                    }))
                    .expect("encode Claude Desktop config"),
                )
                .expect("seed Claude Desktop config");
            }
            fs::create_dir_all(paths.meta_path.parent().expect("Claude Desktop meta parent"))
                .expect("mkdir Claude Desktop library");
            fs::write(
                &paths.legacy_profile_path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "inferenceGatewayApiKey": "sk-test",
                    "inferenceGatewayBaseUrl": TOOL_CONFIG_ANTHROPIC_BASE_URL,
                    "inferenceProvider": "gateway"
                }))
                .expect("encode CC Switch legacy profile"),
            )
            .expect("seed CC Switch legacy profile");
            fs::write(
                &paths.meta_path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "appliedId": "other-profile",
                    "entries": [
                        {"id": "other-profile", "name": "Other"},
                        {"id": CLAUDE_DESKTOP_LEGACY_PROFILE_ID, "name": "CC Switch"}
                    ],
                    "keep": "yes"
                }))
                .expect("encode Claude Desktop meta"),
            )
            .expect("seed Claude Desktop meta");

            apply_claude_desktop_config(TOOL_CONFIG_ANTHROPIC_BASE_URL, "sk-test")
                .expect("apply Claude Desktop");
            let result = remove_claude_desktop_config_with_mode(
                TOOL_CONFIG_ANTHROPIC_BASE_URL,
                "sk-test",
                ToolConfigRemoveMode::NativeRoute,
            )
            .expect("switch Claude Desktop to native route");

            for path in [&paths.normal_config_path, &paths.threep_config_path] {
                let config = read_json_or_default(path, serde_json::json!({}))
                    .expect("read native Claude Desktop config");
                assert_eq!(config["deploymentMode"], "1p");
                assert_eq!(config["keep"], "yes");
            }
            assert!(!paths.profile_path.exists());
            assert!(paths.legacy_profile_path.exists());
            let meta = read_json_or_default(&paths.meta_path, serde_json::json!({}))
                .expect("read native Claude Desktop meta");
            assert_eq!(meta["appliedId"], "other-profile");
            assert_eq!(meta["entries"][0]["id"], "other-profile");
            assert_eq!(meta["entries"][1]["name"], "CC Switch");
            assert_eq!(meta["keep"], "yes");
            assert_eq!(
                result.details.get("native_route_kind").map(String::as_str),
                Some("claude_desktop_first_party")
            );
        });
    }

    #[test]
    fn native_gemini_remove_selects_personal_oauth_and_preserves_model() {
        with_temp_home(|home| {
            let gemini_dir = home.join(".gemini");
            fs::create_dir_all(&gemini_dir).expect("mkdir Gemini");
            let env_path = gemini_dir.join(".env");
            let settings_path = gemini_dir.join("settings.json");
            fs::write(
                &env_path,
                concat!(
                    "GOOGLE_GEMINI_BASE_URL=https://third-party.example/gemini\n",
                    "GEMINI_API_KEY=third-party-key\n",
                    "GOOGLE_APPLICATION_CREDENTIALS=/third-party/credentials.json\n",
                    "GOOGLE_API_KEY=google-key\n",
                    "GOOGLE_GENAI_USE_VERTEXAI=true\n",
                    "GOOGLE_VERTEX_BASE_URL=https://vertex-proxy.example\n",
                    "GEMINI_MODEL=user-model\n",
                    "KEEP_ME=yes\n",
                ),
            )
            .expect("seed Gemini env");
            fs::write(
                &settings_path,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "security": {"auth": {"selectedType": "vertex-ai"}},
                    "keep": "yes"
                }))
                .expect("encode Gemini settings"),
            )
            .expect("seed Gemini settings");

            apply_gemini_config(TOOL_CONFIG_ROOT_URL, "sk-test").expect("apply Gemini");
            let result = remove_gemini_config_with_mode(
                TOOL_CONFIG_ROOT_URL,
                "sk-test",
                ToolConfigRemoveMode::NativeRoute,
            )
            .expect("switch Gemini to native route");

            let env = env_text_to_owned_json(
                &fs::read_to_string(&env_path).expect("read native Gemini env"),
            );
            for name in GEMINI_CLI_EXTERNAL_ENVIRONMENT_NAMES {
                assert!(env.get(*name).is_none(), "{name} should be removed");
            }
            assert_eq!(env.get("GEMINI_MODEL").and_then(serde_json::Value::as_str), Some("user-model"));
            assert_eq!(env.get("KEEP_ME").and_then(serde_json::Value::as_str), Some("yes"));
            let settings = read_json_or_default(&settings_path, serde_json::json!({}))
                .expect("read native Gemini settings");
            assert_eq!(
                settings["security"]["auth"]["selectedType"],
                "oauth-personal"
            );
            assert_eq!(settings["keep"], "yes");
            assert_eq!(
                result.details.get("native_route_kind").map(String::as_str),
                Some("google_personal_oauth")
            );
        });
    }

    #[test]
    fn cancel_config_restores_owned_fields_and_preserves_unrelated_changes() {
        with_temp_home(|home| {
            let path = home.join(".gemini/.env");
            fs::create_dir_all(path.parent().expect("gemini parent")).expect("mkdir gemini");
            fs::write(&path, "KEEP_ME=yes\n").expect("seed gemini");
            apply_gemini_config("http://127.0.0.1:38787", "sk-test").expect("apply gemini");
            let changed = format!(
                "{}USER_EDIT=yes\n",
                fs::read_to_string(&path).expect("read")
            );
            fs::write(&path, &changed).expect("user edit");

            remove_gemini_config("http://127.0.0.1:38787", "sk-test")
                .expect("unrelated user edit must survive field restore");

            let restored = fs::read_to_string(&path).expect("after gemini");
            let restored = env_text_to_owned_json(&restored);
            assert_eq!(
                restored.get("KEEP_ME").and_then(serde_json::Value::as_str),
                Some("yes")
            );
            assert_eq!(
                restored
                    .get("USER_EDIT")
                    .and_then(serde_json::Value::as_str),
                Some("yes")
            );
            assert!(restored.get("GOOGLE_GEMINI_BASE_URL").is_none());
            assert!(restored.get("GEMINI_API_KEY").is_none());
        });
    }

    #[test]
    fn claude_transaction_rollback_preserves_other_tool_manifest_entries() {
        with_temp_home(|home| {
            apply_gemini_config("http://127.0.0.1:38787", "sk-test")
                .expect("apply Gemini");
            let gemini_path = home.join(".gemini/.env");
            let gemini_before = fs::read(&gemini_path).expect("read Gemini");
            let transaction_paths = claude_desktop_transaction_paths();

            let failed: Result<()> = with_tool_config_file_transaction(
                "claude-desktop",
                &transaction_paths,
                || {
                    apply_claude_desktop_config(
                        "http://127.0.0.1:38787",
                        "sk-test",
                    )?;
                    Err(anyhow!("injected failure"))
                },
            );
            assert!(failed
                .expect_err("transaction must fail")
                .to_string()
                .contains("injected failure"));

            assert_eq!(
                fs::read(&gemini_path).expect("read Gemini after rollback"),
                gemini_before
            );
            let manifest = load_tool_config_manifest().expect("load manifest");
            assert!(manifest
                .files
                .values()
                .any(|ownership| ownership.tool == "gemini"));
            assert!(!manifest
                .files
                .values()
                .any(|ownership| ownership.tool == "claude-desktop"));
            let paths = claude_desktop_paths();
            assert!(!paths.profile_path.exists());
            assert!(!paths.meta_path.exists());
        });
    }

    #[test]
    fn cancel_preserves_user_changes_to_new_managed_json_fields() {
        with_temp_home(|home| {
            apply_claude_config("http://127.0.0.1:38787", "sk-test")
                .expect("apply Claude");
            apply_gemini_config("http://127.0.0.1:38787", "sk-test")
                .expect("apply Gemini");

            let claude_path = home.join(".claude.json");
            let mut claude: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&claude_path).expect("read Claude onboarding"),
            )
            .expect("parse Claude onboarding");
            claude["hasCompletedOnboarding"] = serde_json::json!(false);
            fs::write(
                &claude_path,
                serde_json::to_vec_pretty(&claude).expect("encode Claude onboarding"),
            )
            .expect("edit Claude onboarding");

            let gemini_path = home.join(".gemini/settings.json");
            let mut gemini: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&gemini_path).expect("read Gemini settings"),
            )
            .expect("parse Gemini settings");
            gemini["security"]["auth"]["selectedType"] = serde_json::json!("vertex-ai");
            fs::write(
                &gemini_path,
                serde_json::to_vec_pretty(&gemini).expect("encode Gemini settings"),
            )
            .expect("edit Gemini settings");

            remove_claude_config("http://127.0.0.1:38787", "sk-test")
                .expect("cancel Claude");
            remove_gemini_config("http://127.0.0.1:38787", "sk-test")
                .expect("cancel Gemini");

            let claude_after: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&claude_path).expect("read Claude after cancel"),
            )
            .expect("parse Claude after cancel");
            assert_eq!(claude_after["hasCompletedOnboarding"], false);

            let gemini_after: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(&gemini_path).expect("read Gemini after cancel"),
            )
            .expect("parse Gemini after cancel");
            assert_eq!(
                gemini_after["security"]["auth"]["selectedType"],
                "vertex-ai"
            );
        });
    }

    #[test]
    fn reconfigure_overwrites_changed_owned_fields_and_cancel_still_succeeds() {
        with_temp_home(|home| {
            let path = home.join(".gemini/.env");
            fs::create_dir_all(path.parent().expect("gemini parent")).expect("mkdir gemini");
            fs::write(&path, "KEEP_ME=yes\n").expect("seed gemini");

            apply_gemini_config("http://127.0.0.1:38787", "sk-first")
                .expect("first Gemini apply");
            fs::write(
                &path,
                concat!(
                    "GOOGLE_GEMINI_BASE_URL=https://external.example\n",
                    "GEMINI_API_KEY=sk-external\n",
                    "KEEP_ME=yes\n",
                    "TOOL_REFRESH=keep\n",
                ),
            )
            .expect("simulate tool-owned field refresh");

            apply_gemini_config("http://127.0.0.1:38787", "sk-second")
                .expect("explicit reconfigure must overwrite managed fields");
            let configured = env_text_to_owned_json(
                &fs::read_to_string(&path).expect("read reconfigured Gemini config"),
            );
            assert_eq!(
                configured
                    .get("GOOGLE_GEMINI_BASE_URL")
                    .and_then(serde_json::Value::as_str),
                Some(tool_surface_url("http://127.0.0.1:38787", "gemini").as_str())
            );
            assert_eq!(
                configured
                    .get("GEMINI_API_KEY")
                    .and_then(serde_json::Value::as_str),
                Some("sk-second")
            );
            assert_eq!(
                configured
                    .get("TOOL_REFRESH")
                    .and_then(serde_json::Value::as_str),
                Some("keep")
            );

            remove_gemini_config("http://127.0.0.1:38787", "sk-second")
                .expect("cancel after external changes must complete");
            let restored = env_text_to_owned_json(
                &fs::read_to_string(&path).expect("read cancelled Gemini config"),
            );
            assert_eq!(
                restored.get("KEEP_ME").and_then(serde_json::Value::as_str),
                Some("yes")
            );
            assert_eq!(
                restored
                    .get("TOOL_REFRESH")
                    .and_then(serde_json::Value::as_str),
                Some("keep")
            );
            assert!(restored.get("GOOGLE_GEMINI_BASE_URL").is_none());
            assert!(restored.get("GEMINI_API_KEY").is_none());
        });
    }

    #[test]
    fn cancel_opencode_config_restores_manifest_original_bytes() {
        with_temp_home(|home| {
            let path = home.join(".config/opencode/opencode.json");
            fs::create_dir_all(path.parent().expect("opencode parent")).expect("mkdir opencode");
            let original = concat!(
                "{\n",
                "  \"provider\": {\"const_api\": {\"name\": \"Personal\", \"npm\": \"@ai-sdk/openai-compatible\", \"options\": {\"baseURL\": \"https://provider.example/v1\", \"apiKey\": \"sk-user\"}}},\n",
                "  \"theme\": \"system\"\n",
                "}\n"
            );
            fs::write(&path, original).expect("seed opencode");

            apply_opencode_config_with_model_info_for_protocol(
                "http://127.0.0.1:38787/v1",
                "sk-test",
                &[],
                ToolProtocol::OpenAiChat,
            )
            .expect("apply opencode");
            remove_opencode_config("http://127.0.0.1:38787/v1", "sk-test")
                .expect("cancel opencode");

            assert_eq!(fs::read_to_string(&path).expect("read opencode"), original);
        });
    }

    #[test]
    fn cancel_hermes_config_restores_manifest_original_bytes() {
        with_temp_home(|home| {
            let path = home.join(".hermes/config.yaml");
            fs::create_dir_all(path.parent().expect("hermes parent")).expect("mkdir hermes");
            let original = concat!(
                "model:\n",
                "  provider: openai\n",
                "  base_url: https://api.openai.com/v1\n",
                "  api_key: sk-user\n",
                "ui:\n",
                "  theme: dark\n"
            );
            fs::write(&path, original).expect("seed hermes");

            apply_hermes_config_for_protocol(
                "http://127.0.0.1:38787",
                "sk-test",
                ToolProtocol::OpenAiChat,
            )
            .expect("apply hermes");
            remove_hermes_config("http://127.0.0.1:38787", "sk-test").expect("cancel hermes");

            assert_eq!(fs::read_to_string(&path).expect("read hermes"), original);
        });
    }

    #[test]
    fn hermes_runtime_model_refresh_does_not_block_reconfigure_or_cancel() {
        with_temp_home(|home| {
            let path = home.join(".hermes/config.yaml");
            fs::create_dir_all(path.parent().expect("hermes parent")).expect("mkdir hermes");
            let original = concat!(
                "model:\n",
                "  default: keep-default\n",
                "custom_providers:\n",
                "- base_url: https://legacy.example/v1\n",
                "  models:\n",
                "  - legacy-model\n",
                "ui:\n",
                "  theme: dark\n",
            );
            fs::write(&path, original).expect("seed Hermes config");

            apply_hermes_config_for_protocol(
                "http://127.0.0.1:38787",
                "sk-first",
                ToolProtocol::OpenAiChat,
            )
            .expect("first Hermes apply");

            let mut runtime = serde_yaml::from_str::<serde_yaml::Value>(
                &fs::read_to_string(&path).expect("read applied Hermes config"),
            )
            .expect("parse applied Hermes config");
            let providers = runtime
                .as_mapping_mut()
                .and_then(|root| root.get_mut(yaml_key("custom_providers")))
                .and_then(serde_yaml::Value::as_sequence_mut)
                .expect("Hermes providers");
            let provider = providers
                .iter_mut()
                .find(|provider| provider["name"].as_str() == Some(CODEX_CONST_API_PROVIDER_ID))
                .and_then(serde_yaml::Value::as_mapping_mut)
                .expect("CONST API Hermes provider");
            provider.insert(yaml_key("model"), yaml_string("discovered-a"));
            provider.insert(
                yaml_key("models"),
                serde_yaml::Value::Sequence(vec![
                    yaml_string("discovered-a"),
                    yaml_string("discovered-b"),
                ]),
            );
            provider.insert(yaml_key("api_key"), yaml_string("sk-external"));
            fs::write(
                &path,
                serde_yaml::to_string(&runtime).expect("serialize Hermes runtime refresh"),
            )
            .expect("simulate Hermes runtime refresh");

            apply_hermes_config_for_protocol(
                "http://127.0.0.1:38787",
                "sk-second",
                ToolProtocol::OpenAiChat,
            )
            .expect("Hermes reconfigure after runtime refresh");
            let configured = serde_yaml::from_str::<serde_yaml::Value>(
                &fs::read_to_string(&path).expect("read reconfigured Hermes config"),
            )
            .expect("parse reconfigured Hermes config");
            let provider = configured["custom_providers"]
                .as_sequence()
                .and_then(|providers| {
                    providers.iter().find(|provider| {
                        provider["name"].as_str() == Some(CODEX_CONST_API_PROVIDER_ID)
                    })
                })
                .expect("reconfigured CONST API provider");
            assert_eq!(provider["api_key"].as_str(), Some("sk-second"));
            assert_eq!(provider["model"].as_str(), Some("discovered-a"));
            assert_eq!(provider["models"].as_sequence().map(Vec::len), Some(2));
            assert!(configured["custom_providers"]
                .as_sequence()
                .expect("configured providers")
                .iter()
                .any(|provider| provider["base_url"].as_str()
                    == Some("https://legacy.example/v1")));

            remove_hermes_config("http://127.0.0.1:38787", "sk-second")
                .expect("Hermes cancel after runtime refresh");
            let restored = serde_yaml::from_str::<serde_yaml::Value>(
                &fs::read_to_string(&path).expect("read cancelled Hermes config"),
            )
            .expect("parse cancelled Hermes config");
            let expected =
                serde_yaml::from_str::<serde_yaml::Value>(original).expect("parse original Hermes");
            assert_eq!(restored, expected);
        });
    }
#[test]
    fn tool_model_sync_policy_covers_every_model_persisting_tool() {
    for tool in [
        "copilot",
        "open-design",
        "open-interpreter",
        "anythingllm",
        "vibe-trading",
    ] {
        assert_eq!(
            tool_model_sync_policy(tool),
            ToolModelSyncPolicy::Selected,
            "tool={tool}"
        );
    }
    for tool in [
        "claude-desktop",
        "cline",
        "deepseek-harness",
        "goose",
        "kimicode",
        "mimocode",
        "mistral-vibe",
        "openclaw",
        "opencode",
        "openscience",
        "pi",
        "qwencode",
        "raven",
        "reasonix",
        "vscode",
        "workbuddy",
        "zcode",
    ] {
        assert_eq!(
            tool_model_sync_policy(tool),
            ToolModelSyncPolicy::Catalog,
            "tool={tool}"
        );
    }
    for tool in ["codex", "claude", "claude-science", "gemini", "hermes"] {
        assert_eq!(
            tool_model_sync_policy(tool),
            ToolModelSyncPolicy::None,
            "tool={tool}"
        );
        }
    }

    #[test]
    fn tool_profiles_are_unique_and_self_consistent() {
        let mut ids = std::collections::BTreeSet::new();
        assert_eq!(TOOL_PROFILES.len(), 27);
        for profile in TOOL_PROFILES {
            assert!(ids.insert(profile.id), "duplicate tool={}", profile.id);
            assert!(!profile.display_name.trim().is_empty(), "tool={}", profile.id);
            assert!(!profile.protocols.is_empty(), "tool={}", profile.id);
            assert!(
                profile.protocols.contains(&profile.default_protocol),
                "default protocol is unsupported for tool={}",
                profile.id
            );
        }
    }

    #[test]
    fn tool_config_preview_is_semantic_and_write_free() {
        with_temp_home(|home| {
            let path = home.join(".gemini/.env");
            let preview = preview_tool_config_apply(|| {
                apply_gemini_config(TOOL_CONFIG_ROOT_URL, "sk-test")
            })
            .expect("preview missing Gemini configuration");

            assert!(!preview.already_configured);
            assert!(preview.files.is_empty());
            assert!(preview.backups.is_empty());
            assert!(!path.exists(), "preview must not create the target file");
            assert!(
                !crate::client_data_root().join(TOOL_CONFIG_MANIFEST).exists(),
                "preview must not create the ownership manifest"
            );

            let preview_error = preview_tool_config_apply(|| {
                with_tool_config_file_transaction("gemini", std::slice::from_ref(&path), || {
                    let mut result = ToolApplyBuilder::default();
                    write_text_with_backup(
                        &path,
                        "GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:38787\n",
                        "gemini",
                        &mut result,
                    )?;
                    Err::<(), _>(anyhow!("preview failure"))
                })
            })
            .expect_err("preview transaction error");
            assert!(preview_error.to_string().contains("preview failure"));
            assert!(!path.exists(), "failed preview must not touch the target file");
            assert!(
                !crate::client_data_root().join(TOOL_CONFIG_MANIFEST).exists(),
                "failed preview must not roll back or write the ownership manifest"
            );

            apply_gemini_config(TOOL_CONFIG_ROOT_URL, "sk-test")
                .expect("apply Gemini configuration");
            let before = fs::read(&path).expect("read applied configuration");
            let preview = preview_tool_config_apply(|| {
                apply_gemini_config(TOOL_CONFIG_ROOT_URL, "sk-test")
            })
            .expect("preview applied Gemini configuration");

            assert!(preview.already_configured);
            assert!(preview.files.is_empty());
            assert_eq!(fs::read(path).expect("configuration remains"), before);
        });
    }
