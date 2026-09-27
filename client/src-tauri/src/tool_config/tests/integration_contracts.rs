#[test]
fn cline_provider_timestamp_is_valid_and_stable() {
    let models = tool_models_from_ids(&["test-model".to_string()]);
    for timestamp in [
        serde_json::Value::Null,
        serde_json::json!("invalid"),
        serde_json::json!("2026-09-19T00:00:00Z"),
    ] {
        let mut providers = serde_json::json!({"providers": {
            "user": {"keep": true},
            "const-api": {"updatedAt": timestamp}
        }});
        let mut catalog = serde_json::json!({});
        upsert_cline_configs(
            &mut providers,
            &mut catalog,
            TOOL_CONFIG_ROOT_URL,
            "mock-key",
            Some(&models),
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        let timestamp = providers["providers"]["const-api"]["updatedAt"]
            .as_str()
            .unwrap();
        assert!(timestamp.ends_with('Z'));
        time::OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
            .unwrap();
        assert_eq!(providers["providers"]["user"]["keep"], true);
        let before = providers.clone();
        upsert_cline_configs(
            &mut providers,
            &mut catalog,
            TOOL_CONFIG_ROOT_URL,
            "mock-key",
            None,
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        assert_eq!(providers, before, "checking must not change updatedAt");
    }
}

#[test]
fn opencode_environment_paths_follow_upstream_merge_precedence() {
    with_temp_home(|home| {
        let default = home.join("xdg/opencode");
        let directory = home.join("custom-dir");
        let file = home.join("explicit.json");
        assert_eq!(
            opencode_config_path_from_sources(default.clone(), None, None).unwrap(),
            default.join("opencode.json")
        );
        assert_eq!(
            opencode_config_path_from_sources(default.clone(), None, Some(file.clone())).unwrap(),
            file
        );
        assert_eq!(
            opencode_config_path_from_sources(default, Some(directory.clone()), Some(file))
                .unwrap(),
            directory.join("opencode.json")
        );
    });
}

#[test]
fn opencode_uses_effective_jsonc_and_restores_original_contents() {
    with_temp_home(|home| {
        let root = home.join(".config/opencode");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("opencode.json"), "{\"keepJson\":true}").unwrap();
        let original = "{// user comment\n\"keepJsonc\":true}";
        fs::write(root.join("opencode.jsonc"), original).unwrap();
        assert_eq!(opencode_config_path().unwrap(), root.join("opencode.jsonc"));
        apply_opencode_config_with_models(TOOL_CONFIG_ROOT_URL, "mock-key", &["test-model".into()])
            .unwrap();
        assert!(
            check_opencode_config(TOOL_CONFIG_ROOT_URL, "mock-key")
                .unwrap()
                .already_configured
        );
        assert_eq!(
            fs::read_to_string(root.join("opencode.json")).unwrap(),
            "{\"keepJson\":true}"
        );
        remove_opencode_config(TOOL_CONFIG_ROOT_URL, "mock-key").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("opencode.jsonc")).unwrap(),
            original
        );
    });
}

#[test]
fn open_science_targets_ai4s_runtime_not_similarly_named_cli() {
    with_temp_home(|home| {
        let old = home.join(".config/openscience/openscience.json");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, "{\"keep\":true}").unwrap();
        let path = openscience_config_path().unwrap();
        assert!(path.ends_with("com.ai4s.workbench/runtime/xdg-config/opencode/opencode.json"));
        let models = tool_models_from_ids(&["test-model".into()]);
        apply_additional_tool_config_with_model_info_for_protocol(
            "openscience",
            TOOL_CONFIG_ROOT_URL,
            "mock-key",
            &models,
            ToolProtocol::OpenAiChat,
        )
        .unwrap();
        assert!(path.exists());
        assert!(
            check_additional_tool_config("openscience", TOOL_CONFIG_ROOT_URL, "mock-key")
                .unwrap()
                .already_configured
        );
        assert_eq!(fs::read_to_string(&old).unwrap(), "{\"keep\":true}");
        remove_additional_tool_config("openscience").unwrap();
        assert!(!path.exists());
        assert!(old.exists());
    });
    for path in [
        "/Applications/Open Science.app",
        "C:/Tools/ai4s-workbench.exe",
        "/usr/bin/ai4s-workbench",
    ] {
        assert!(is_tool_owned_program_path("openscience", Path::new(path)));
    }
    for path in ["/usr/bin/openscience", "/usr/bin/osd"] {
        assert!(!is_tool_owned_program_path("openscience", Path::new(path)));
    }
}

#[test]
fn open_design_desktop_config_matches_platform_and_selected_release_channel() {
    for platform in ["win", "mac", "linux"] {
        for (app, channel) in [
            ("Open Design", "stable"),
            ("Open Design Beta", "beta"),
            ("Open Design Prerelease", "prerelease"),
            ("Open Design Preview", "preview"),
        ] {
            for suffix in ["", ".exe", ".app"] {
                let program = PathBuf::from(format!("/apps/{app}{suffix}"));
                assert_eq!(
                    open_design_desktop_config_path(Path::new("config"), Some(&program), platform),
                    PathBuf::from("config")
                        .join(app)
                        .join("namespaces")
                        .join(format!("release-{channel}-{platform}"))
                        .join("data/app-config.json")
                );
            }
        }
    }
}

#[test]
fn copilot_isolates_registry_and_preserves_user_registry_on_remove() {
    let env = copilot_cli_child_environment(
        "http://127.0.0.1:38787/v1",
        "mock-key",
        "chosen-model",
        "/private/providers.json",
    );
    assert!(env.contains(&("COPILOT_MODEL", "chosen-model")));
    assert!(env.contains(&("COPILOT_PROVIDERS_CONFIG", "/private/providers.json")));
    assert!(env.contains(&("COPILOT_PROVIDER_WIRE_API", "completions")));
    assert!(env.contains(&("COPILOT_PROVIDER_TRANSPORT", "http")));
    for name in [
        "COPILOT_PROVIDER_API_KEY_COMMAND",
        "COPILOT_PROVIDER_BEARER_TOKEN",
        "COPILOT_PROVIDER_HEADERS",
        "COPILOT_PROVIDER_WIRE_MODEL",
    ] {
        assert!(COPILOT_CONFLICTING_ENVIRONMENT.contains(&name));
    }
    with_temp_home(|_| {
        let user_registry = copilot_env_path().with_file_name("providers.json");
        fs::create_dir_all(user_registry.parent().unwrap()).unwrap();
        let original = "{\"providers\":{\"user-provider\":{\"keep\":true}}}";
        fs::write(&user_registry, original).unwrap();
        let models = tool_models_from_ids(&["test-model".into()]);
        apply_copilot_config(TOOL_CONFIG_ROOT_URL, "mock-key", &models).unwrap();
        assert!(
            check_copilot_config(TOOL_CONFIG_ROOT_URL, "mock-key")
                .unwrap()
                .already_configured
        );
        let registry =
            read_json_or_default(&copilot_providers_path(), serde_json::Value::Null).unwrap();
        assert_eq!(registry, serde_json::json!({"providers": {}, "models": {}}));
        fs::remove_file(copilot_providers_path()).unwrap();
        assert!(
            !check_copilot_config(TOOL_CONFIG_ROOT_URL, "mock-key")
                .unwrap()
                .already_configured
        );
        apply_copilot_config(TOOL_CONFIG_ROOT_URL, "mock-key", &models).unwrap();
        remove_additional_tool_config("copilot").unwrap();
        assert!(!copilot_providers_path().exists());
        assert_eq!(fs::read_to_string(user_registry).unwrap(), original);
    });
}

#[test]
fn empty_provider_registry_ownership_preserves_later_user_additions() {
    with_temp_home(|_| {
        let models = tool_models_from_ids(&["test-model".into()]);
        apply_copilot_config(TOOL_CONFIG_ROOT_URL, "mock-key", &models).unwrap();
        fs::write(
            copilot_providers_path(),
            r#"{"providers":{"user":{"keep":true}},"models":{}}"#,
        )
        .unwrap();
        remove_additional_tool_config("copilot").unwrap();
        let remaining =
            read_json_or_default(&copilot_providers_path(), serde_json::Value::Null).unwrap();
        assert_eq!(remaining["providers"]["user"]["keep"], true);
        assert!(remaining.get("models").is_none());
    });
}

#[test]
fn unix_terminal_script_quotes_arguments_and_sets_environment_in_terminal() {
    assert_eq!(
        selected_path_kind("raven", "/custom/not-in-path/raven"),
        "command"
    );
    let handshake = CommandLaunchHandshake {
        id: "fixture".into(),
        started: "start file".into(),
        exited: "exit file".into(),
    };
    let script = unix_terminal_launch_script(
        Path::new("/tools/my cli"),
        &["hello'world", "$(not-executed)"],
        &[("TEST_KEY", "value'with$chars")],
        &["OTHER_PROVIDER"],
        &handshake,
    );
    assert!(script.contains("unset OTHER_PROVIDER\n"));
    assert!(script.contains("export TEST_KEY='value'\\''with$chars'\n"));
    assert!(script.contains("'/tools/my cli' 'hello'\\''world' '$(not-executed)'"));
    assert!(script.contains("printf started > 'start file'"));
    assert!(script.contains("printf 'exit=%s' \"$status\" > 'exit file'"));
    assert_eq!(
        linux_terminal_candidates()[0],
        ("x-terminal-emulator", &["-e"][..])
    );
    assert!(linux_terminal_candidates().contains(&("gnome-terminal", &["--"][..])));
}

#[test]
fn linux_desktop_launchers_are_distinct_from_interactive_commands() {
    for (tool, program) in [
        ("vscode", "code"),
        ("workbuddy", "workbuddy"),
        ("anythingllm", "anythingllm-desktop"),
        ("openscience", "ai4s-workbench"),
        ("reasonix", "reasonix-launcher"),
        ("reasonix", "reasonix-desktop"),
    ] {
        assert!(
            is_linux_desktop_program_path(tool, program),
            "{tool}: {program}"
        );
    }
    for (tool, program) in [
        ("opencode", "opencode"),
        ("goose", "goose"),
        ("reasonix", "reasonix"),
        ("claude", "claude"),
        ("copilot", "copilot"),
    ] {
        assert!(
            !is_linux_desktop_program_path(tool, program),
            "{tool}: {program}"
        );
        assert_eq!(
            tool_profile(tool).unwrap().command_launch,
            ToolCommandLaunch::Terminal
        );
    }
    let desktop = ToolProgramCandidate {
        path: "/usr/bin/reasonix-desktop".into(),
        kind: "linux_desktop".into(),
        exists: true,
        ..Default::default()
    };
    let cli = ToolProgramCandidate {
        path: "/usr/bin/reasonix".into(),
        kind: "command".into(),
        exists: true,
        ..Default::default()
    };
    assert!(is_desktop_program_candidate(&desktop));
    assert!(
        tool_program_candidate_priority("reasonix", &desktop)
            > tool_program_candidate_priority("reasonix", &cli)
    );
}
