#[test]
fn open_interpreter_modern_and_legacy_configs_restore_independently() {
    with_temp_home(|_| {
        let modern = open_interpreter_config_path().expect("modern path");
        let legacy = open_interpreter_profile_path();
        let original_toml = "# user preference\nmodel = 'old-model'\nmodel_provider = 'other'\n[model_providers.other]\nname = 'Existing'\nwire_api = 'chat'\n";
        let original_yaml =
            "auto_run: false\nllm:\n  model: openai/old-model\n  temperature: 0.4\n";
        fs::create_dir_all(modern.parent().expect("parent")).expect("mkdir");
        fs::create_dir_all(legacy.parent().expect("parent")).expect("mkdir");
        fs::write(&modern, original_toml).expect("seed TOML");
        fs::write(&legacy, original_yaml).expect("seed YAML");
        let models = tool_models_from_ids(&["test-model".to_string()]);
        apply_open_interpreter_config(TOOL_CONFIG_ROOT_URL, "test-local-key", &models)
            .expect("apply");
        let doc = fs::read_to_string(&modern)
            .expect("read")
            .parse::<DocumentMut>()
            .expect("TOML");
        assert_eq!(doc["model"].as_str(), Some("test-model"));
        assert_eq!(
            doc["model_providers"]["CONST_API"]["wire_api"].as_str(),
            Some("chat")
        );
        assert_eq!(
            doc["model_providers"]["CONST_API"]["base_url"].as_str(),
            Some(TOOL_CONFIG_OPENAI_BASE_URL)
        );
        assert_eq!(
            doc["model_providers"]["other"]["name"].as_str(),
            Some("Existing")
        );
        assert!(
            check_open_interpreter_config(TOOL_CONFIG_ROOT_URL, "test-local-key")
                .expect("check")
                .already_configured
        );
        assert!(
            apply_open_interpreter_config(TOOL_CONFIG_ROOT_URL, "test-local-key", &models)
                .expect("reapply")
                .files
                .is_empty()
        );
        let mut edited = doc;
        edited["personality"] = toml_value("friendly");
        fs::write(&modern, edited.to_string()).expect("user edit");
        remove_additional_tool_config("open-interpreter").expect("remove");
        let restored = fs::read_to_string(&modern)
            .expect("read")
            .parse::<DocumentMut>()
            .expect("TOML");
        assert_eq!(restored["model"].as_str(), Some("old-model"));
        assert_eq!(restored["personality"].as_str(), Some("friendly"));
        assert!(restored["model_providers"].get("CONST_API").is_none());
        assert_eq!(
            fs::read_to_string(&legacy).expect("restored YAML"),
            original_yaml
        );
        assert!(!home_dir().join(".codex").exists());
    });
}

#[test]
fn open_interpreter_legacy_only_install_needs_modern_config_repair() {
    with_temp_home(|_| {
        let path = open_interpreter_profile_path();
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(
            &path,
            upsert_open_interpreter_yaml("", TOOL_CONFIG_ROOT_URL, "test-key", "model")
                .expect("YAML"),
        )
        .expect("write");
        assert!(
            !check_open_interpreter_config(TOOL_CONFIG_ROOT_URL, "test-key")
                .expect("check")
                .already_configured
        );
        apply_open_interpreter_config(
            TOOL_CONFIG_ROOT_URL,
            "test-key",
            &tool_models_from_ids(&["model".into()]),
        )
        .expect("repair");
        assert!(
            check_open_interpreter_config(TOOL_CONFIG_ROOT_URL, "test-key")
                .expect("check")
                .already_configured
        );
    });
}

#[test]
fn open_interpreter_rejects_malformed_user_config_without_writing() {
    with_temp_home(|_| {
        let path = open_interpreter_config_path().expect("path");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, "model_providers = 42\n").expect("write");
        assert!(
            apply_open_interpreter_config(
                TOOL_CONFIG_ROOT_URL,
                "test-key",
                &tool_models_from_ids(&["model".into()])
            )
            .is_err()
        );
        assert_eq!(
            fs::read_to_string(path).expect("read"),
            "model_providers = 42\n"
        );
        assert!(!open_interpreter_profile_path().exists());
    });
}
#[test]
fn open_interpreter_legacy_path_matches_platformdirs() {
    with_temp_home(|home| {
        #[cfg(target_os = "windows")]
        let expected =
            home.join("AppData/Local/open-interpreter/open-interpreter/profiles/default.yaml");
        #[cfg(target_os = "macos")]
        let expected =
            home.join("Library/Application Support/open-interpreter/profiles/default.yaml");
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let expected = home.join(".config/open-interpreter/profiles/default.yaml");
        assert_eq!(open_interpreter_profile_path(), expected);
    });
}

#[test]
fn open_interpreter_legacy_profile_declares_schema_without_overwriting_existing_version() {
    for (raw, expected) in [
        ("", "0.2.5"),
        ("version: 'future-version'\n", "future-version"),
    ] {
        let configured =
            upsert_open_interpreter_yaml(raw, TOOL_CONFIG_ROOT_URL, "test-key", "model")
                .expect("configure");
        let profile: serde_yaml::Value = serde_yaml::from_str(&configured).expect("parse");
        assert_eq!(profile["version"].as_str(), Some(expected));
    }
    assert!(
        upsert_open_interpreter_yaml(
            "- invalid-shape\n",
            TOOL_CONFIG_ROOT_URL,
            "test-key",
            "model"
        )
        .is_err()
    );
}
