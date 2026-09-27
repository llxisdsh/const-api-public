fn catalog_test_model(id: &str) -> ToolModelInfo {
    ToolModelInfo {
        id: id.to_string(),
        tool_call: true,
        context_tokens: Some(65_536),
        output_tokens: Some(8_192),
        input_modalities: vec!["text".into()],
        output_modalities: vec!["text".into()],
        ..Default::default()
    }
}

#[test]
fn tool_model_discovery_priority_reaches_array_based_tool_writers() {
    let models = vec![catalog_test_model("aaa"), ToolModelInfo {
        display_priority: 400, ..catalog_test_model("gpt-future")
    }, catalog_test_model("zzz")];
    let codex = build_codex_model_catalog(&models, &serde_json::json!({})).unwrap();
    assert_eq!(codex["models"][0]["slug"], "gpt-future");
    assert_eq!(codex["models"][0]["priority"], 0);
    assert_eq!(openclaw_model_entries(&models)[0]["id"], "gpt-future");
    assert_eq!(vscode_const_api_provider("http://localhost/v1", "test", &models, ToolProtocol::OpenAiChat).unwrap()["models"][0]["id"], "gpt-future");
    assert_eq!(configured_additional_tool_models("test", &models).unwrap()[0].id, "gpt-future");
    let mut workbuddy = serde_json::json!([]);
    for model in &models {
        let mut entry = serde_json::json!({});
        configure_workbuddy_model_entry(&mut entry, model, "http://localhost/v1/chat/completions", "test");
        workbuddy.as_array_mut().unwrap().push(entry);
    }
    normalize_workbuddy_managed_model_ids(&mut workbuddy, "http://localhost/v1/chat/completions", "test", &["gpt-future".into(), "aaa".into(), "zzz".into()]).unwrap();
    assert_eq!(workbuddy[0]["id"], "gpt-future");
}

#[test]
fn model_discovery_rules_also_filter_generated_claude_desktop_aliases() {
    let mut config = crate::default_config();
    config.allow_model_equivalence = true;
    let model_ids = ["gpt-5.6-sol".to_string()];
    let routes = crate::model_compatibility::anthropic_model_routes(&config, &model_ids);
    assert!(routes.len() > 1);
    let hidden = &routes[0];
    let preferred = &routes[1];
    let models = crate::tool_model_metadata::tool_models_from_response(&serde_json::json!({
        "data":[{"id":"gpt-5.6-sol"}],
        "const_api_model_presentation":{"schema_version":1,"rules":[
            {"pattern":hidden,"priority":0,"hidden":true},
            {"pattern":preferred,"priority":500,"hidden":false}
        ]}
    }));
    let injected = claude_desktop_inference_models(&config, &models);
    assert_eq!(injected[0]["name"], preferred.as_str());
    assert!(!injected.iter().any(|entry| entry["name"] == hidden.as_str()));
    assert!(injected.iter().all(|entry| entry.get("const_api").is_none()));
}

fn apply_test_codex_catalog(
    source: CodexModelSource,
    catalog: Option<&serde_json::Value>,
) -> Result<ToolApplyResult> {
    apply_codex_config_with_catalog_and_progress(
        TOOL_CONFIG_OPENAI_BASE_URL,
        "sk-test",
        Some(source),
        catalog,
        &mut |_, _, _, _| {},
    )
}

#[test]
fn codex_catalog_filters_deduplicates_and_preserves_exact_native_metadata() {
    let mut native = generic_codex_catalog_model(&catalog_test_model("gpt-test"));
    native["base_instructions"] = serde_json::json!("native instruction template");
    native["apply_patch_tool_type"] = serde_json::json!("freeform");
    native["context_window"] = serde_json::json!(32_768);
    let cached = serde_json::json!({"models": [native, {"slug": "bad-cache"}]});
    let models = vec![
        catalog_test_model("vendor/Z-model"),
        catalog_test_model("Gpt-test"),
        ToolModelInfo {
            context_tokens: Some(1_000_000),
            ..catalog_test_model("z-model")
        },
        ToolModelInfo {
            tool_call: false,
            ..catalog_test_model("no-tools")
        },
        ToolModelInfo {
            output_modalities: vec!["audio".into()],
            ..catalog_test_model("voice")
        },
        catalog_test_model("bad-cache"),
    ];
    let catalog = build_codex_model_catalog(&models, &cached).unwrap();
    let entries = catalog["models"].as_array().unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["slug"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["bad-cache", "gpt-test", "z-model"]
    );
    assert_eq!(entries[0]["shell_type"], "default");
    assert_eq!(
        entries[1]["base_instructions"],
        "native instruction template"
    );
    assert_eq!(entries[1]["apply_patch_tool_type"], "freeform");
    assert_eq!(entries[1]["context_window"], 32_768);
    assert_eq!(entries[2]["context_window"], 65_536);
    assert_eq!(entries[2]["priority"], 2);
    assert!(entries[2]["apply_patch_tool_type"].is_null());
    assert_eq!(entries[2]["supports_reasoning_summary_parameter"], false);
    assert_eq!(entries[2]["auto_compact_token_limit"], 57_344);
}

#[test]
fn codex_catalog_context_and_reasoning_are_bounded_without_guessing_capabilities() {
    let mut model = catalog_test_model("custom-model");
    model.context_tokens = None;
    model.reasoning_efforts = vec!["medium".into(), "future-level".into()];
    let entry = generic_codex_catalog_model(&model);
    assert_eq!(entry["context_window"], 32_768);
    assert_eq!(entry["default_reasoning_level"], "medium");
    assert_eq!(
        entry["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    model.context_tokens = Some(u64::MAX);
    assert!(
        generic_codex_catalog_model(&model)["auto_compact_token_limit"]
            .as_u64()
            .unwrap()
            <= i64::MAX as u64
    );
    assert!(build_codex_model_catalog(&[], &serde_json::Value::Null).is_err());
}

#[test]
fn codex_catalog_matches_offline_codex_smoke_fixture() {
    let catalog = build_codex_model_catalog(
        &[catalog_test_model("const-test")],
        &serde_json::Value::Null,
    )
    .unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/codex-models.json")).unwrap();
    assert_eq!(catalog, fixture);
}

#[test]
fn codex_catalog_switch_restores_custom_directory_and_does_not_change_selected_model() {
    with_temp_home(|home| {
        let config_path = home.join(".codex/config.toml");
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            "model = 'my-chosen-model'\nmodel_catalog_json = 'C:/custom/models.json'\nweb_search = 'live'\n",
        )
        .unwrap();
        let catalog = build_codex_model_catalog(
            &[catalog_test_model("third-party")],
            &serde_json::Value::Null,
        )
        .unwrap();
        apply_test_codex_catalog(CodexModelSource::Const, Some(&catalog)).unwrap();
        let checked = check_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert!(checked.already_configured);
        assert_eq!(checked.details["codex_model_source"], "const");
        assert_eq!(checked.details["model_sync_policy"], "catalog");
        assert_eq!(
            read_text_or_empty(&config_path)
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap()["web_search"]
                .as_str(),
            Some("disabled")
        );
        // Legacy calls must not silently disable a selected custom directory.
        apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert_eq!(
            current_codex_model_source().unwrap(),
            CodexModelSource::Const
        );
        let repeated = apply_test_codex_catalog(CodexModelSource::Const, Some(&catalog)).unwrap();
        assert!(repeated.files.is_empty());
        apply_test_codex_catalog(CodexModelSource::Codex, None).unwrap();
        let config = read_text_or_empty(&config_path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(config["model"].as_str(), Some("my-chosen-model"));
        assert_eq!(config["web_search"].as_str(), Some("live"));
        assert_eq!(
            config["model_catalog_json"].as_str(),
            Some("C:/custom/models.json")
        );
        assert_eq!(
            current_codex_model_source().unwrap(),
            CodexModelSource::Codex
        );
        // Generated file is retained while connected for reuse, and normal
        // manifest removal cleans it up without touching the user's directory.
        remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert!(!codex_managed_catalog_path().exists());
    });
}

#[test]
fn codex_catalog_empty_apply_does_not_write_configuration() {
    with_temp_home(|home| {
        let error = apply_test_codex_catalog(
            CodexModelSource::Const,
            Some(&serde_json::json!({"models": []})),
        )
        .unwrap_err();
        assert!(error.to_string().contains("MODELS_UNAVAILABLE"));
        assert!(!home.join(".codex/config.toml").exists());
        assert!(!home.join(".codex/auth.json").exists());
        assert!(!codex_managed_catalog_path().exists());
    });
}

#[test]
fn codex_catalog_disable_without_manifest_and_remove_both_modes() {
    with_temp_home(|home| {
        let config_path = home.join(".codex/config.toml");
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let mut doc = DocumentMut::new();
        doc["model_catalog_json"] =
            toml_value(codex_managed_catalog_path().to_string_lossy().as_ref());
        fs::write(&config_path, doc.to_string()).unwrap();
        apply_test_codex_catalog(CodexModelSource::Codex, None).unwrap();
        assert!(!read_text_or_empty(&config_path)
            .unwrap()
            .contains("model_catalog_json"));
        remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
    });
    for mode in [
        ToolConfigRemoveMode::RestorePreConst,
        ToolConfigRemoveMode::NativeRoute,
    ] {
        with_temp_home(|home| {
            let catalog = build_codex_model_catalog(
                &[catalog_test_model("custom")],
                &serde_json::Value::Null,
            )
            .unwrap();
            apply_test_codex_catalog(CodexModelSource::Const, Some(&catalog)).unwrap();
            remove_codex_config_with_progress_and_mode(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                mode,
                &mut |_, _, _, _| {},
            )
            .unwrap();
            assert!(!codex_managed_catalog_path().exists());
            assert!(!read_text_or_empty(&home.join(".codex/config.toml"))
                .unwrap()
                .contains("model_catalog_json"));
        });
    }
}

#[test]
fn codex_catalog_disable_preserves_later_user_edits() {
    with_temp_home(|home| {
        let catalog =
            build_codex_model_catalog(&[catalog_test_model("custom")], &serde_json::Value::Null)
                .unwrap();
        apply_test_codex_catalog(CodexModelSource::Const, Some(&catalog)).unwrap();
        let path = home.join(".codex/config.toml");
        let mut doc = read_text_or_empty(&path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        doc["model_catalog_json"] = toml_value("C:/user/edited.json");
        fs::write(&path, doc.to_string()).unwrap();
        apply_test_codex_catalog(CodexModelSource::Codex, None).unwrap();
        remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert!(read_text_or_empty(&path)
            .unwrap()
            .contains("C:/user/edited.json"));
    });
}

#[test]
fn codex_catalog_failed_config_write_rolls_back_directory_and_manifest() {
    with_temp_home(|home| {
        let auth_path = home.join(".codex").join("auth.json");
        let mut builder = ToolApplyBuilder::default();
        write_json_with_backup(
            &auth_path,
            &serde_json::json!({"OPENAI_API_KEY": "other-key"}),
            "claude",
            &mut builder,
        )
        .unwrap();
        let before = fs::read(tool_config_manifest_path()).unwrap();
        let catalog =
            build_codex_model_catalog(&[catalog_test_model("custom")], &serde_json::Value::Null)
                .unwrap();
        assert!(apply_test_codex_catalog(CodexModelSource::Const, Some(&catalog)).is_err());
        assert!(!codex_managed_catalog_path().exists());
        assert!(!home.join(".codex/config.toml").exists());
        assert_eq!(fs::read(tool_config_manifest_path()).unwrap(), before);
    });
}
