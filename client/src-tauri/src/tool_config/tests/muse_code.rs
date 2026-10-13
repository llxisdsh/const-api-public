fn muse_test_models() -> Vec<ToolModelInfo> {
    vec![
        ToolModelInfo {
            id: "claude-sonnet-4-6".into(),
            context_tokens: Some(200_000),
            output_tokens: Some(64_000),
            ..Default::default()
        },
        ToolModelInfo {
            id: "gpt-6-luna".into(),
            display_name: "GPT-6 Luna".into(),
            display_priority: 100,
            context_tokens: Some(400_000),
            output_tokens: Some(32_000),
            reasoning: true,
            reasoning_efforts: vec![
                "none".into(),
                "low".into(),
                "high".into(),
                "ultra".into(),
                "invalid".into(),
            ],
            input_modalities: vec!["text".into(), "image".into()],
            tool_call: true,
            ..Default::default()
        },
    ]
}

#[test]
fn muse_code_catalog_uses_native_fields_order_and_closed_effort_enum() {
    with_temp_home(|_| {
        let models = muse_test_models();
        apply_additional_tool_config_with_model_info_for_protocol(
            "muse-code",
            TOOL_CONFIG_ROOT_URL,
            "mock-key",
            &models,
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        let (settings_path, auth_path) = muse_code_paths();
        let root = read_muse_code_document(&settings_path).unwrap();
        assert_eq!(root["provider"], "meta");
        assert_eq!(
            root["endpoint_transport"]["base_url"],
            format!("{TOOL_CONFIG_ROOT_URL}/v1")
        );
        assert_eq!(root["endpoint_transport"]["auth"], "bearer");
        assert_eq!(root["model"], "gpt-6-luna");
        let rows = root["model_catalog"].as_array().unwrap();
        assert_eq!(rows[0]["model_id"], "gpt-6-luna");
        assert_eq!(rows[0]["display_label"], "GPT-6 Luna (CONST API)");
        assert_eq!(rows[0]["display_order"], 0);
        assert_eq!(rows[0]["context_limit"], 400_000);
        assert_eq!(rows[0]["output_limit"], 32_000);
        assert_eq!(
            rows[0]["reasoning_effort_variants"],
            serde_json::json!([{"tier":"low"},{"tier":"high"},{"tier":"ultra"}])
        );
        assert_eq!(rows[1]["reasoning_effort_variants"], serde_json::json!([]));
        assert!(
            rows[0].get("supports_images").is_none(),
            "no invented capability fields"
        );
        assert!(
            root.get("reasoning_effort").is_none(),
            "do not force a global tier"
        );
        assert_eq!(
            read_muse_code_document(&auth_path).unwrap()["providers"]["meta"]["api_key"],
            "mock-key"
        );
        let bytes = fs::read(&settings_path).unwrap();
        assert!(
            check_additional_tool_config("muse-code", TOOL_CONFIG_ROOT_URL, "mock-key")
                .unwrap()
                .already_configured
        );
        assert!(
            !check_additional_tool_config("muse-code", TOOL_CONFIG_ROOT_URL, "new-key")
                .unwrap()
                .already_configured
        );
        assert_eq!(fs::read(&settings_path).unwrap(), bytes);
        assert!(
            configure_muse_code(TOOL_CONFIG_ROOT_URL, "mock-key", Some(&models))
                .unwrap()
                .files
                .is_empty()
        );
        remove_additional_tool_config("muse-code").unwrap();
        assert!(!settings_path.exists());
        assert!(!auth_path.exists());
    });
}

#[test]
fn muse_code_remove_preserves_user_models_auth_and_later_changes() {
    with_temp_home(|_| {
        let (settings_path, auth_path) = muse_code_paths();
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        let original_row = serde_json::json!({"provider_id":"meta","model_id":"gpt-6-luna","display_label":"My Luna","context_limit":16384});
        let original = serde_json::json!({"schema_version":1,"model":"personal","provider":"local","theme":"dark","model_catalog":[original_row.clone()]});
        fs::write(&settings_path, serde_json::to_vec(&original).unwrap()).unwrap();
        fs::write(&auth_path, r#"{"schema_version":1,"providers":{"meta":{"api_key":"personal-key","account":"keep"},"local":{"api_key":"other-key"}}}"#).unwrap();
        configure_muse_code(TOOL_CONFIG_ROOT_URL, "mock-key", Some(&muse_test_models())).unwrap();
        let mut settings = read_muse_code_document(&settings_path).unwrap();
        settings["theme"] = serde_json::json!("light");
        settings["model"] = serde_json::json!("user-added");
        let rows = settings["model_catalog"].as_array_mut().unwrap();
        rows.iter_mut()
            .find(|r| r["model_id"] == "claude-sonnet-4-6")
            .unwrap()["runtime_extra"] = serde_json::json!(true);
        let personal = serde_json::json!({"provider_id":"meta","model_id":"user-added","display_label":"User"});
        rows.push(personal.clone());
        fs::write(&settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();
        remove_additional_tool_config("muse-code").unwrap();
        let settings = read_muse_code_document(&settings_path).unwrap();
        assert_eq!(settings["theme"], "light");
        assert_eq!(settings["model"], "user-added");
        assert_eq!(settings["provider"], "local");
        assert_eq!(
            settings["model_catalog"],
            serde_json::json!([original_row, personal])
        );
        assert!(settings.get("endpoint_transport").is_none());
        let auth = read_muse_code_document(&auth_path).unwrap();
        assert_eq!(auth["providers"]["meta"]["api_key"], "personal-key");
        assert_eq!(auth["providers"]["meta"]["account"], "keep");
        assert_eq!(auth["providers"]["local"]["api_key"], "other-key");
    });
}

#[test]
fn muse_code_resync_removes_only_previous_owned_models_and_detects_metadata_change() {
    with_temp_home(|_| {
        let mut models = muse_test_models();
        configure_muse_code(TOOL_CONFIG_ROOT_URL, "mock-key", Some(&models)).unwrap();
        let (path, _) = muse_code_paths();
        let mut settings = read_muse_code_document(&path).unwrap();
        settings["model_catalog"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"provider_id":"meta","model_id":"personal"}));
        fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
        models[1].context_tokens = Some(1_000_000);
        let preview = preview_tool_config_apply(|| {
            configure_muse_code(TOOL_CONFIG_ROOT_URL, "mock-key", Some(&models))
        })
        .unwrap();
        assert!(!preview.already_configured);
        configure_muse_code(TOOL_CONFIG_ROOT_URL, "mock-key", Some(&models[1..])).unwrap();
        let settings = read_muse_code_document(&path).unwrap();
        let rows = settings["model_catalog"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r["model_id"] == "personal"));
        assert!(!rows.iter().any(|r| r["model_id"] == "claude-sonnet-4-6"));
        assert_eq!(rows[1]["context_limit"], 1_000_000);
        remove_additional_tool_config("muse-code").unwrap();
        let remaining: serde_json::Value =
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            remaining["model_catalog"],
            serde_json::json!([{"provider_id":"meta","model_id":"personal"}])
        );
    });
}

#[test]
fn muse_code_check_preview_and_bad_schema_never_write() {
    with_temp_home(|_| {
        let (settings, auth) = muse_code_paths();
        assert!(
            !configure_muse_code(TOOL_CONFIG_ROOT_URL, "mock", None)
                .unwrap()
                .already_configured
        );
        preview_tool_config_apply(|| {
            configure_muse_code(TOOL_CONFIG_ROOT_URL, "mock", Some(&muse_test_models()))
        })
        .unwrap();
        assert!(!settings.exists() && !auth.exists());
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(&settings, r#"{"schema_version":99,"personal":true}"#).unwrap();
        let before = fs::read(&settings).unwrap();
        assert!(
            configure_muse_code(TOOL_CONFIG_ROOT_URL, "mock", Some(&muse_test_models())).is_err()
        );
        assert_eq!(fs::read(settings).unwrap(), before);
        assert!(!auth.exists());
    });
}

#[test]
fn muse_code_registration_uses_shared_catalog_and_cli_policy() {
    assert_eq!(
        tool_model_sync_policy("muse-code"),
        ToolModelSyncPolicy::Catalog
    );
    assert_eq!(
        default_tool_protocol("muse-code").unwrap(),
        ToolProtocol::OpenAiResponses
    );
    assert!(manifest_manages_tool("muse-code"));
    assert!(is_tool_owned_program_path(
        "muse-code",
        Path::new("/bin/muse.cmd")
    ));
    assert!(!is_tool_owned_program_path(
        "muse-code",
        Path::new("/bin/not-muse.exe")
    ));
    assert_eq!(
        tool_profile("muse-code").unwrap().command_launch,
        ToolCommandLaunch::Terminal
    );
    assert!(
        defined_tool_environment_variables_with("muse-code", |name| name == "META_API_KEY")
            .contains(&"META_API_KEY")
    );
}

#[test]
fn muse_code_versioned_runtime_ownership_does_not_claim_neighbor_programs() {
    with_temp_home(|home| {
        let bin = home.join(".local/bin");
        fs::create_dir_all(&bin).unwrap();
        let launcher = bin.join(if cfg!(windows) { "muse.cmd" } else { "muse" });
        fs::write(&launcher, "fixture").unwrap();
        let native = bin.join(if cfg!(windows) {
            "muse-bin-1.4.7-R6101.1.exe"
        } else {
            "muse-bin-1.4.7-R6101.1"
        });
        fs::write(&native, "fixture").unwrap();
        for name in ["node.exe", "muse-bin-unrelated.exe", "muse-other"] {
            fs::write(bin.join(name), "fixture").unwrap();
        }
        fs::create_dir(bin.join("muse-bin-9.9.9")).unwrap();
        assert_eq!(
            muse_code_runtime_executables(&launcher),
            vec![native.clone()]
        );
        assert!(
            tool_process_owner_paths("muse-code")
                .unwrap()
                .contains(&native)
        );
        assert!(
            tool_process_match_names("muse-code")
                .unwrap()
                .contains(&normalized_process_name(
                    native.file_name().unwrap().to_str().unwrap()
                ))
        );
        #[cfg(windows)]
        {
            let names = tool_process_match_names("muse-code").unwrap();
            let scope = windows_tool_process_scope("muse-code", &names).unwrap();
            assert!(
                scope
                    .exact_executable_paths
                    .contains(&normalized_windows_process_path(&native))
            );
            assert!(
                scope.install_roots.is_empty(),
                "do not own the shared bin directory"
            );
        }
    });
}

#[test]
fn muse_code_keeps_unknown_limits_and_efforts_unknown_and_rejects_bad_catalogs() {
    let row = muse_code_model_row(
        &ToolModelInfo {
            id: "new-model".into(),
            reasoning: true,
            ..Default::default()
        },
        0,
        true,
    );
    assert!(row["context_limit"].is_null());
    assert!(row["output_limit"].is_null());
    assert_eq!(row["reasoning_effort_variants"], serde_json::json!([]));
    with_temp_home(|_| {
        let (path, auth) = muse_code_paths();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for catalog in [
            serde_json::json!([{"name":"incomplete"}]),
            serde_json::json!([
                {"provider_id":"meta","model_id":"duplicate"},
                {"provider_id":"meta","model_id":"duplicate"}
            ]),
        ] {
            let content = serde_json::to_vec(
                &serde_json::json!({"schema_version":1,"model_catalog":catalog}),
            )
            .unwrap();
            fs::write(&path, &content).unwrap();
            assert!(
                configure_muse_code(TOOL_CONFIG_ROOT_URL, "mock-key", Some(&muse_test_models()))
                    .is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), content);
            assert!(!auth.exists());
        }
    });
}
