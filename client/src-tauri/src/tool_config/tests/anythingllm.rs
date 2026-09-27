#[test]
fn anythingllm_config_preserves_knowledge_base_and_restores_user_settings() {
    with_temp_home(|_| {
        let path = anythingllm_config_path();
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let original = "# user settings\nLLM_PROVIDER=ollama\nEMBEDDING_ENGINE=native\nVECTOR_DB=lancedb\nGENERIC_OPEN_AI_MAX_TOKENS=2048\n";
        fs::write(&path, original).expect("seed");
        let model = ToolModelInfo {
            id: "model-a".into(),
            context_tokens: Some(128_000),
            output_tokens: Some(16_384),
            ..Default::default()
        };
        apply_anythingllm_config(TOOL_CONFIG_ROOT_URL, "test#key", &[model.clone()])
            .expect("apply");
        let current = env_text_to_status_json(&fs::read_to_string(&path).expect("read"));
        assert_eq!(current["LLM_PROVIDER"], "generic-openai");
        assert_eq!(
            current["GENERIC_OPEN_AI_BASE_PATH"],
            TOOL_CONFIG_OPENAI_BASE_URL
        );
        assert_eq!(current["GENERIC_OPEN_AI_API_KEY"], "test#key");
        assert_eq!(current["GENERIC_OPEN_AI_MODEL_TOKEN_LIMIT"], "128000");
        assert_eq!(current["GENERIC_OPEN_AI_MAX_TOKENS"], "2048");
        assert_eq!(current["EMBEDDING_ENGINE"], "native");
        assert_eq!(current["VECTOR_DB"], "lancedb");
        assert!(
            check_anythingllm_config(TOOL_CONFIG_ROOT_URL, "test#key")
                .expect("check")
                .already_configured
        );
        assert!(
            apply_anythingllm_config(TOOL_CONFIG_ROOT_URL, "test#key", &[model])
                .expect("reapply")
                .files
                .is_empty()
        );
        let edited = format!(
            "{}\nUSER_ADDED=kept\n",
            fs::read_to_string(&path).expect("read")
        );
        fs::write(&path, edited).expect("user edit");
        remove_additional_tool_config("anythingllm").expect("restore");
        let restored = env_text_to_status_json(&fs::read_to_string(&path).expect("read"));
        assert_eq!(restored["LLM_PROVIDER"], "ollama");
        assert_eq!(restored["USER_ADDED"], "kept");
        assert!(restored.get("GENERIC_OPEN_AI_API_KEY").is_none());
    });
}

#[test]
fn anythingllm_preserves_upstream_default_output_budget() {
    let model = ToolModelInfo {
        id: "model-a".into(),
        output_tokens: Some(16_384),
        ..Default::default()
    };
    let configured =
        upsert_anythingllm_env("", TOOL_CONFIG_ROOT_URL, "key", &model).expect("configure");
    assert_eq!(
        env_text_to_status_json(&configured)["GENERIC_OPEN_AI_MAX_TOKENS"],
        "1024"
    );
}

#[test]
fn anythingllm_config_targets_desktop_storage_and_rejects_env_injection() {
    with_temp_home(|_| {
        let path = anythingllm_config_path();
        assert!(
            path.ends_with(
                Path::new("anythingllm-desktop")
                    .join("storage")
                    .join(".env")
            )
        );
        #[cfg(target_os = "windows")]
        assert!(path.starts_with(home_dir().join("AppData").join("Roaming")));
        #[cfg(target_os = "macos")]
        assert!(path.starts_with(home_dir().join("Library").join("Application Support")));
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        assert!(path.starts_with(home_dir().join(".config")));
        let model = ToolModelInfo {
            id: "model\nOTHER=value".into(),
            ..Default::default()
        };
        assert!(upsert_anythingllm_env("", TOOL_CONFIG_ROOT_URL, "key", &model).is_err());
        assert!(!path.exists());
    });
}
