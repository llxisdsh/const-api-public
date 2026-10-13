fn empty_zcode_provider_registry() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "config": {
            "providerConfigRules": {"providerRules": []},
            "modelConfigRules": {"providerModelRules": [], "manualProviderModelRules": []},
        },
    })
}

#[test]
fn zcode_versioned_registry_matches_native_decoder_fixture() {
    // This complete file was also decoded/resolved offline with the installed
    // ZCode 3.14.4 provider codec, not only with CONST's own status checker.
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/zcode-provider-config-v1.json")).unwrap();
    let mut model = capability_test_models().remove(0);
    model.id = "gpt-6-luna".into();
    model.reasoning_efforts = vec!["low".into(), "high".into()];
    let mut root = empty_zcode_provider_registry();
    upsert_zcode_provider_registry(
        &mut root,
        TOOL_CONFIG_OPENAI_BASE_URL,
        "test-key",
        Some(&[model]),
    )
    .unwrap();
    assert_eq!(root, expected);
}

#[test]
fn zcode_versioned_registry_takes_precedence_over_configured_legacy_file() {
    with_temp_home(|_| {
        let models = admin_order_models(&["z-first", "a-second"]);
        apply_zcode_config(TOOL_CONFIG_OPENAI_BASE_URL, "test-key", &models).unwrap();
        assert!(
            check_zcode_config(TOOL_CONFIG_OPENAI_BASE_URL, "test-key")
                .unwrap()
                .already_configured
        );
        let legacy_before = fs::read(zcode_config_path()).unwrap();
        let native = zcode_provider_config_path();
        fs::write(
            &native,
            json_text(&empty_zcode_provider_registry()).unwrap(),
        )
        .unwrap();
        let before = fs::read(&native).unwrap();

        assert!(
            !check_zcode_config(TOOL_CONFIG_OPENAI_BASE_URL, "test-key")
                .unwrap()
                .already_configured
        );
        let preview = preview_tool_config_apply(|| {
            apply_zcode_config(TOOL_CONFIG_OPENAI_BASE_URL, "test-key", &models)
        })
        .unwrap();
        assert!(!preview.already_configured && preview.files.is_empty());
        assert_eq!(fs::read(&native).unwrap(), before);

        let applied = apply_zcode_config(TOOL_CONFIG_OPENAI_BASE_URL, "test-key", &models).unwrap();
        assert_eq!(applied.files, vec![native.display().to_string()]);
        assert_eq!(fs::read(zcode_config_path()).unwrap(), legacy_before);
        let root = read_json_or_default(&native, serde_json::json!({})).unwrap();
        let provider = &root["config"]["providerConfigRules"]["providerRules"][0];
        assert_eq!(
            provider["config"]["personalModelIds"],
            serde_json::json!(["z-first", "a-second"])
        );
        assert_eq!(
            provider["config"]["modelOrder"],
            provider["config"]["personalModelIds"]
        );
        assert_eq!(provider["config"]["api"]["type"], "openai-chat-completions");
        assert!(
            check_zcode_config(TOOL_CONFIG_OPENAI_BASE_URL, "test-key")
                .unwrap()
                .already_configured
        );
        assert!(
            !check_zcode_config(TOOL_CONFIG_OPENAI_BASE_URL, "rotated-key")
                .unwrap()
                .already_configured
        );
        let unchanged =
            apply_zcode_config(TOOL_CONFIG_OPENAI_BASE_URL, "test-key", &models).unwrap();
        assert!(
            unchanged.already_configured
                && unchanged.files.is_empty()
                && unchanged.backups.is_empty()
        );

        apply_zcode_config(
            TOOL_CONFIG_OPENAI_BASE_URL,
            "rotated-key",
            &admin_order_models(&["a-second", "new-model"]),
        )
        .unwrap();
        let root = read_json_or_default(&native, serde_json::json!({})).unwrap();
        assert_eq!(
            root["config"]["providerConfigRules"]["providerRules"][0]["config"]["modelOrder"],
            serde_json::json!(["a-second", "new-model"])
        );
        assert!(
            check_zcode_config(TOOL_CONFIG_OPENAI_BASE_URL, "rotated-key")
                .unwrap()
                .already_configured
        );
    });
}

#[test]
fn zcode_versioned_registry_restores_owned_rules_without_losing_user_additions() {
    with_temp_home(|_| {
        let path = zcode_provider_config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut original = empty_zcode_provider_registry();
        let user_provider = serde_json::json!({"providerId":"user-provider", "config":{"personalModelIds":["same-model"]}});
        let user_model = serde_json::json!({"providerId":"user-provider", "modelId":"same-model", "config":{"enabled":true}});
        original["config"]["providerConfigRules"]["providerRules"] =
            serde_json::json!([user_provider]);
        original["config"]["modelConfigRules"]["providerModelRules"] =
            serde_json::json!([user_model]);
        fs::write(&path, json_text(&original).unwrap()).unwrap();
        apply_zcode_config(
            TOOL_CONFIG_OPENAI_BASE_URL,
            "test-key",
            &admin_order_models(&["same-model", "another-model"]),
        )
        .unwrap();
        let mut live = read_json_or_default(&path, serde_json::json!({})).unwrap();
        live["config"]["providerConfigRules"]["providerRules"][1]["providerName"] =
            serde_json::json!("Renamed CONST");
        let later = serde_json::json!({"providerId":"later-provider", "config":{}});
        live["config"]["providerConfigRules"]["providerRules"]
            .as_array_mut()
            .unwrap()
            .push(later.clone());
        live["config"]["modelConfigRules"]["providerModelRules"][1]["config"]["properties"] =
            serde_json::json!({"contextWindow":12345});
        fs::write(&path, json_text(&live).unwrap()).unwrap();
        restore_tool_config_from_manifest("zcode").unwrap();
        let restored = read_json_or_default(&path, serde_json::json!({})).unwrap();
        assert_eq!(
            restored["config"]["providerConfigRules"]["providerRules"],
            serde_json::json!([user_provider, later])
        );
        assert_eq!(
            restored["config"]["modelConfigRules"],
            original["config"]["modelConfigRules"]
        );
    });
}

#[test]
fn zcode_versioned_registry_preserves_native_manual_model_overrides() {
    let mut root = empty_zcode_provider_registry();
    let manual = serde_json::json!({"providerId":"const-api", "modelId":"manual", "config":{"enabled":false,"properties":{"contextWindow":64000}}});
    root["config"]["modelConfigRules"]["manualProviderModelRules"] = serde_json::json!([manual]);
    let mut models = capability_test_models();
    models[0].id = "manual".into();
    upsert_zcode_provider_registry(
        &mut root,
        TOOL_CONFIG_OPENAI_BASE_URL,
        "test-key",
        Some(&models),
    )
    .unwrap();
    assert_eq!(
        root["config"]["modelConfigRules"]["manualProviderModelRules"],
        serde_json::json!([manual])
    );
    let rules = root["config"]["modelConfigRules"]["providerModelRules"]
        .as_array()
        .unwrap();
    assert!(rules.iter().all(|rule| rule["modelId"] != "manual"));
    let config = &rules[0]["config"];
    assert_eq!(config["properties"]["contextWindow"], 272000);
    assert_eq!(config["properties"]["inputFormat"]["supportsImage"], true);
    assert_eq!(config["properties"]["supportsToolCall"], true);
    assert_eq!(config["optionSpecs"]["maxOutputTokens"]["max"], 128000);
    assert_eq!(
        config["optionSpecs"]["reasoningLevel"]["values"],
        serde_json::json!(["low", "medium", "high", "xhigh", "max", "ultra"])
    );
    assert!(rules.last().unwrap()["config"]["optionSpecs"]["reasoningLevel"].is_null());
}

#[test]
fn zcode_unknown_registry_version_fails_without_changing_either_file() {
    with_temp_home(|_| {
        let path = zcode_provider_config_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut root = empty_zcode_provider_registry();
        root["schemaVersion"] = serde_json::json!(2);
        let before = json_text(&root).unwrap();
        fs::write(&path, &before).unwrap();
        assert!(check_zcode_config(TOOL_CONFIG_OPENAI_BASE_URL, "test-key").is_err());
        assert!(
            apply_zcode_config(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "test-key",
                &admin_order_models(&["model"])
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert!(!zcode_config_path().exists());
    });
}

#[test]
fn zcode_registry_updates_capabilities_without_losing_unmanaged_settings() {
    let mut root = empty_zcode_provider_registry();
    let mut model = capability_test_models().remove(0);
    upsert_zcode_provider_registry(
        &mut root,
        TOOL_CONFIG_OPENAI_BASE_URL,
        "test-key",
        Some(&[model.clone()]),
    )
    .unwrap();
    let existing = &mut root["config"]["modelConfigRules"]["providerModelRules"][0]["config"];
    existing["properties"]["requiresMfjsToolSchema"] = serde_json::json!(true);
    existing["optionSpecs"]["temperature"] = serde_json::json!({"default": 0.4});
    model.tool_call = false;
    model.reasoning = false;
    model.input_modalities = vec!["text".into(), "pdf".into()];
    model.context_tokens = Some(64000);
    model.output_tokens = Some(4096);
    upsert_zcode_provider_registry(
        &mut root,
        TOOL_CONFIG_OPENAI_BASE_URL,
        "test-key",
        Some(&[model]),
    )
    .unwrap();
    let config = &root["config"]["modelConfigRules"]["providerModelRules"][0]["config"];
    assert_eq!(config["properties"]["requiresMfjsToolSchema"], true);
    assert_eq!(config["optionSpecs"]["temperature"]["default"], 0.4);
    assert_eq!(config["properties"]["supportsToolCall"], false);
    assert_eq!(config["properties"]["inputFormat"]["supportsImage"], false);
    assert_eq!(config["properties"]["inputFormat"]["supportsPdf"], true);
    assert_eq!(config["properties"]["contextWindow"], 64000);
    assert_eq!(config["optionSpecs"]["maxOutputTokens"]["max"], 4096);
    assert!(config["optionSpecs"]["reasoningLevel"].is_null());
}

#[test]
fn zcode_rejects_conflicting_smart_and_manual_rules() {
    let mut root = empty_zcode_provider_registry();
    let duplicate =
        serde_json::json!({"providerId":"user-provider", "modelId":"same-model", "config":{}});
    root["config"]["modelConfigRules"]["providerModelRules"] = serde_json::json!([duplicate]);
    root["config"]["modelConfigRules"]["manualProviderModelRules"] = serde_json::json!([duplicate]);
    let original = root.clone();
    assert!(
        upsert_zcode_provider_registry(
            &mut root,
            TOOL_CONFIG_OPENAI_BASE_URL,
            "test-key",
            Some(&capability_test_models())
        )
        .is_err()
    );
    assert_eq!(root, original);
}
