const HARNESS_USER_SETTINGS: &str = "\
llm-pi-ai:
  providers:
    user:
      api: openai-completions
      apiKeyEnv: USER_KEY
      baseURL: https://example.test/v1
      models: [{id: user-model}]
agent-default-model:
  provider: user
  model: user-model
  temperature: 0.7
";

const HARNESS_OTHER_PATCH: &str = "\
- id: unrelated-plugin
  config: {theme: dark, enabled: true}
";

#[test]
fn deepseek_harness_patch_comments_and_literal_tag_names_are_not_expressions() {
    let template = "# overrides, disables, and insert lists; `!!js` expressions allowed.\n[]\n";
    assert!(deepseek_harness_patch_entries(template).unwrap().is_empty());
    let raw = "- id: other\n  config: {description: '!!js expression example'} # !!js is allowed\n";
    let rendered = render_deepseek_harness_patch(raw, &serde_yaml::Mapping::new()).unwrap();
    assert_eq!(
        deepseek_harness_patch_entries(&rendered).unwrap()[0]["config"]["description"],
        "!!js expression example"
    );
}

fn harness_test_models(id: &str) -> Vec<ToolModelInfo> {
    vec![ToolModelInfo {
        id: id.into(),
        ..ToolModelInfo::default()
    }]
}

fn harness_test_profile_patch() -> PathBuf {
    deepseek_harness_profile_patch_path("web")
}

fn harness_test_read_settings(path: &Path) -> serde_yaml::Value {
    serde_yaml::from_str(&deepseek_harness_settings_text(path).unwrap()).unwrap()
}

#[test]
fn deepseek_harness_legacy_settings_remain_preferred_before_migration() {
    with_temp_home(|_| {
        let patch = harness_test_profile_patch();
        fs::create_dir_all(patch.parent().unwrap()).unwrap();
        fs::write(&patch, "# Created by Harness\n[]\n").unwrap();
        let (legacy, _) = deepseek_harness_config_paths();
        fs::write(&legacy, HARNESS_USER_SETTINGS).unwrap();
        apply_deepseek_harness_config(
            TOOL_CONFIG_ROOT_URL,
            "test-key",
            &harness_test_models("model-a"),
            ToolProtocol::OpenAiChat,
        )
        .unwrap();
        assert_eq!(
            deepseek_harness_settings_groups().unwrap()[0][0].path,
            legacy
        );
        assert_eq!(
            fs::read_to_string(&patch).unwrap(),
            "# Created by Harness\n[]\n"
        );
        assert!(
            check_deepseek_harness_config(TOOL_CONFIG_ROOT_URL, "test-key")
                .unwrap()
                .already_configured
        );
        remove_additional_tool_config("deepseek-harness").unwrap();
        assert_eq!(fs::read_to_string(legacy).unwrap(), HARNESS_USER_SETTINGS);
    });
}

#[test]
fn deepseek_harness_settings_migration_preserves_status_reapply_and_restore() {
    for original_exists in [false, true] {
        for reapply in [false, true] {
            for external_default in [false, true] {
                with_temp_home(|_| {
                    let (legacy, _) = deepseek_harness_config_paths();
                    let patch = harness_test_profile_patch();
                    fs::create_dir_all(patch.parent().unwrap()).unwrap();
                    if original_exists {
                        fs::write(&legacy, HARNESS_USER_SETTINGS).unwrap();
                    }
                    apply_deepseek_harness_config(
                        TOOL_CONFIG_ROOT_URL,
                        "test-key",
                        &harness_test_models("model-a"),
                        ToolProtocol::OpenAiChat,
                    )
                    .unwrap();
                    let settings = parse_deepseek_harness_yaml_mapping(
                        &fs::read_to_string(&legacy).unwrap(),
                        "settings",
                    )
                    .unwrap();
                    // Official alpha renames the legacy file, then imports its
                    // sections as plugin config patches in the active profile.
                    fs::rename(&legacy, legacy.with_file_name("settings.yaml.imported")).unwrap();
                    fs::write(
                        &patch,
                        render_deepseek_harness_patch(HARNESS_OTHER_PATCH, &settings).unwrap(),
                    )
                    .unwrap();
                    let manifest_before = fs::read(tool_config_manifest_path()).unwrap();
                    let patch_before = fs::read(&patch).unwrap();
                    assert_eq!(
                        deepseek_harness_settings_groups().unwrap()[0][0].path,
                        patch
                    );
                    assert!(
                        check_deepseek_harness_config(TOOL_CONFIG_ROOT_URL, "test-key")
                            .unwrap()
                            .already_configured
                    );
                    let preview = preview_tool_config_apply(|| {
                        apply_deepseek_harness_config(
                            TOOL_CONFIG_ROOT_URL,
                            "test-key",
                            &harness_test_models("model-a"),
                            ToolProtocol::OpenAiChat,
                        )
                    })
                    .unwrap();
                    assert!(preview.already_configured && preview.files.is_empty());
                    assert_eq!(
                        fs::read(tool_config_manifest_path()).unwrap(),
                        manifest_before
                    );
                    assert_eq!(fs::read(&patch).unwrap(), patch_before);

                    if reapply {
                        apply_deepseek_harness_config(
                            TOOL_CONFIG_ROOT_URL,
                            "test-key",
                            &harness_test_models("model-b"),
                            ToolProtocol::AnthropicMessages,
                        )
                        .unwrap();
                        assert!(
                            check_deepseek_harness_config(TOOL_CONFIG_ROOT_URL, "test-key")
                                .unwrap()
                                .already_configured
                        );
                        assert_eq!(
                            harness_test_read_settings(&patch)["agent-default-model"]["model"],
                            "model-b"
                        );
                    }
                    assert!(!legacy.exists(), "do not recreate an imported legacy file");
                    let manifest = load_tool_config_manifest().unwrap();
                    assert_eq!(
                        manifest.files.len(),
                        2,
                        "one settings owner plus credentials"
                    );
                    let mut entries =
                        deepseek_harness_patch_entries(&fs::read_to_string(&patch).unwrap())
                            .unwrap();
                    for entry in &mut entries {
                        if deepseek_harness_patch_matches(entry, "agent-default-model") {
                            entry["config"]["temperature"] = serde_yaml::to_value(0.9).unwrap();
                            if external_default {
                                entry["config"]["provider"] = yaml_string("later-provider");
                                entry["config"]["model"] = yaml_string("later-model");
                            }
                        }
                    }
                    fs::write(&patch, serde_yaml::to_string(&entries).unwrap()).unwrap();
                    remove_additional_tool_config("deepseek-harness").unwrap();
                    assert!(!legacy.exists());
                    let restored = harness_test_read_settings(&patch);
                    assert!(
                        restored["llm-pi-ai"]["providers"]
                            .get(ADDITIONAL_CONST_API_PROVIDER_ID)
                            .is_none(),
                        "original={original_exists}, reapply={reapply}, user_edit={external_default}: {restored:?}"
                    );
                    if original_exists {
                        let original: serde_yaml::Value =
                            serde_yaml::from_str(HARNESS_USER_SETTINGS).unwrap();
                        assert_eq!(restored["llm-pi-ai"], original["llm-pi-ai"]);
                    }
                    assert_eq!(
                        restored["agent-default-model"]["temperature"],
                        serde_yaml::to_value(0.9).unwrap()
                    );
                    if external_default {
                        assert_eq!(restored["agent-default-model"]["model"], "later-model");
                        assert_eq!(
                            restored["agent-default-model"]["provider"],
                            "later-provider"
                        );
                    } else if original_exists {
                        assert_eq!(restored["agent-default-model"]["model"], "user-model");
                        assert_eq!(restored["agent-default-model"]["provider"], "user");
                    } else {
                        assert!(restored["agent-default-model"].get("model").is_none());
                        assert!(restored["agent-default-model"].get("provider").is_none());
                    }
                    let entries =
                        deepseek_harness_patch_entries(&fs::read_to_string(&patch).unwrap())
                            .unwrap();
                    assert_eq!(
                        entries[0],
                        deepseek_harness_patch_entries(HARNESS_OTHER_PATCH).unwrap()[0]
                    );
                    assert!(load_tool_config_manifest().unwrap().files.is_empty());
                });
            }
        }
    }
}

#[test]
fn deepseek_harness_native_patch_supports_all_protocols_and_preserves_other_entries() {
    for protocol in [
        ToolProtocol::OpenAiChat,
        ToolProtocol::OpenAiResponses,
        ToolProtocol::AnthropicMessages,
    ] {
        with_temp_home(|_| {
            let (legacy, _) = deepseek_harness_config_paths();
            let patch = harness_test_profile_patch();
            fs::create_dir_all(patch.parent().unwrap()).unwrap();
            let settings =
                parse_deepseek_harness_yaml_mapping(HARNESS_USER_SETTINGS, "settings").unwrap();
            let original = render_deepseek_harness_patch(HARNESS_OTHER_PATCH, &settings).unwrap();
            fs::write(&patch, &original).unwrap();
            apply_deepseek_harness_config(
                TOOL_CONFIG_ROOT_URL,
                "test-key",
                &harness_test_models("model-a"),
                protocol,
            )
            .unwrap();
            assert!(!legacy.exists());
            let checked = check_deepseek_harness_config(TOOL_CONFIG_ROOT_URL, "test-key").unwrap();
            assert!(checked.already_configured);
            let actual = harness_test_read_settings(&patch);
            let provider = &actual["llm-pi-ai"]["providers"][ADDITIONAL_CONST_API_PROVIDER_ID];
            assert_eq!(provider["api"], deepseek_harness_api(protocol).unwrap());
            assert_eq!(
                provider["baseURL"],
                tool_surface_url(TOOL_CONFIG_ROOT_URL, protocol.surface())
            );
            let appended = format!(
                "{}\n- id: later-plugin\n  config: {{value: keep}}\n",
                fs::read_to_string(&patch).unwrap()
            );
            fs::write(&patch, appended).unwrap();
            remove_additional_tool_config("deepseek-harness").unwrap();
            assert_eq!(
                harness_test_read_settings(&patch),
                serde_yaml::Value::Mapping(settings)
            );
            let entries =
                deepseek_harness_patch_entries(&fs::read_to_string(&patch).unwrap()).unwrap();
            assert_eq!(entries.last().unwrap()["config"]["value"], "keep");
        });
    }
}

#[test]
fn deepseek_harness_home_patch_updates_only_the_effective_sections() {
    with_temp_home(|_| {
        let patch = harness_test_profile_patch();
        fs::create_dir_all(patch.parent().unwrap()).unwrap();
        let settings =
            parse_deepseek_harness_yaml_mapping(HARNESS_USER_SETTINGS, "settings").unwrap();
        let profile = render_deepseek_harness_patch(HARNESS_OTHER_PATCH, &settings).unwrap();
        fs::write(&patch, &profile).unwrap();
        let home_patch = deepseek_harness_config_paths()
            .0
            .with_file_name("cordis.patch.yml");
        let original =
            "- id: agent-default-model\n  config: {provider: home-user, model: home-model}\n";
        fs::write(&home_patch, original).unwrap();
        apply_deepseek_harness_config(
            TOOL_CONFIG_ROOT_URL,
            "test-key",
            &harness_test_models("model-a"),
            ToolProtocol::OpenAiChat,
        )
        .unwrap();
        assert_eq!(deepseek_harness_settings_groups().unwrap()[0].len(), 2);
        assert!(
            check_deepseek_harness_config(TOOL_CONFIG_ROOT_URL, "test-key")
                .unwrap()
                .already_configured
        );
        let home = harness_test_read_settings(&home_patch);
        assert!(
            home.get("llm-pi-ai").is_none(),
            "do not shadow inherited provider settings"
        );
        let actual = harness_test_read_settings(&patch);
        assert!(actual["llm-pi-ai"]["providers"].get("user").is_some());
        assert!(
            actual["llm-pi-ai"]["providers"]
                .get(ADDITIONAL_CONST_API_PROVIDER_ID)
                .is_some()
        );
        assert_eq!(actual["agent-default-model"]["model"], "user-model");
        remove_additional_tool_config("deepseek-harness").unwrap();
        assert_eq!(fs::read_to_string(&home_patch).unwrap(), original);
        assert_eq!(fs::read_to_string(patch).unwrap(), profile);
    });
}

#[test]
fn deepseek_harness_migrated_empty_patch_restores_to_valid_empty_sequence() {
    with_temp_home(|_| {
        apply_deepseek_harness_config(
            TOOL_CONFIG_ROOT_URL,
            "test-key",
            &harness_test_models("model-a"),
            ToolProtocol::OpenAiChat,
        )
        .unwrap();
        let (legacy, _) = deepseek_harness_config_paths();
        let settings =
            parse_deepseek_harness_yaml_mapping(&fs::read_to_string(&legacy).unwrap(), "settings")
                .unwrap();
        let patch = harness_test_profile_patch();
        fs::create_dir_all(patch.parent().unwrap()).unwrap();
        fs::rename(&legacy, legacy.with_file_name("settings.yaml.imported")).unwrap();
        fs::write(
            &patch,
            render_deepseek_harness_patch("[]", &settings).unwrap(),
        )
        .unwrap();
        remove_additional_tool_config("deepseek-harness").unwrap();
        assert_eq!(
            deepseek_harness_patch_entries(&fs::read_to_string(patch).unwrap())
                .unwrap()
                .len(),
            0
        );
        assert!(!legacy.exists());
    });
}

#[test]
fn deepseek_harness_unsupported_patch_fails_without_writing_config() {
    for raw in [
        "{not: a-sequence}",
        "- id: llm-pi-ai\n  config: bad-shape\n",
        "- id: llm-pi-ai\n  config: !!js someExpression\n",
        "- id: other-plugin\n  config: {value: !!js someExpression}\n",
        "- id: other-plugin\n  config: {value: !<tag:yaml.org,2002:js> someExpression}\n",
        "%TAG !dsh! tag:yaml.org,2002:\n---\n- id: other\n  config: !dsh!js someExpression\n",
    ] {
        with_temp_home(|_| {
            let patch = harness_test_profile_patch();
            fs::create_dir_all(patch.parent().unwrap()).unwrap();
            fs::write(&patch, raw).unwrap();
            assert!(
                apply_deepseek_harness_config(
                    TOOL_CONFIG_ROOT_URL,
                    "test-key",
                    &harness_test_models("model-a"),
                    ToolProtocol::OpenAiChat,
                )
                .is_err()
            );
            assert_eq!(fs::read_to_string(&patch).unwrap(), raw);
            let (legacy, credentials) = deepseek_harness_config_paths();
            assert!(!legacy.exists() && !credentials.exists());
            assert!(!tool_config_manifest_path().exists());
        });
    }
}

#[test]
fn deepseek_harness_desktop_and_web_preserve_separate_settings_and_restore() {
    for protocol in [
        ToolProtocol::OpenAiChat,
        ToolProtocol::OpenAiResponses,
        ToolProtocol::AnthropicMessages,
    ] {
        for home_override in [false, true] {
            with_temp_home(|_| {
                let mut originals = Vec::new();
                for profile in ["desktop", "web"] {
                    let path = deepseek_harness_profile_patch_path(profile);
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    let settings = parse_deepseek_harness_yaml_mapping(
                        &HARNESS_USER_SETTINGS
                            .replace("user-model", &format!("{profile}-user-model")),
                        "test",
                    )
                    .unwrap();
                    let original =
                        render_deepseek_harness_patch(HARNESS_OTHER_PATCH, &settings).unwrap();
                    fs::write(&path, &original).unwrap();
                    originals.push((path, original));
                }
                let home_patch = deepseek_harness_config_paths()
                    .0
                    .with_file_name("cordis.patch.yml");
                if home_override {
                    let original = "- id: agent-default-model\n  config: {provider: home-user, model: home-model}\n";
                    fs::write(&home_patch, original).unwrap();
                    originals.push((home_patch.clone(), original.into()));
                }
                let models = admin_order_models(&["model-z", "model-a"]);
                let preview = preview_tool_config_apply(|| {
                    apply_deepseek_harness_config(
                        TOOL_CONFIG_ROOT_URL,
                        "test-key",
                        &models,
                        protocol,
                    )
                })
                .unwrap();
                assert!(!preview.already_configured);
                assert!(!tool_config_manifest_path().exists());
                for (path, original) in &originals {
                    assert_eq!(fs::read_to_string(path).unwrap(), *original);
                }
                apply_deepseek_harness_config(TOOL_CONFIG_ROOT_URL, "test-key", &models, protocol)
                    .unwrap();
                assert_eq!(deepseek_harness_settings_groups().unwrap().len(), 2);
                assert!(
                    check_deepseek_harness_config(TOOL_CONFIG_ROOT_URL, "test-key")
                        .unwrap()
                        .already_configured
                );
                for (profile, files) in ["desktop", "web"]
                    .into_iter()
                    .zip(deepseek_harness_settings_groups().unwrap())
                {
                    let effective = parse_deepseek_harness_yaml_mapping(
                        &deepseek_harness_effective_settings(&files).unwrap(),
                        "test",
                    )
                    .unwrap();
                    assert_eq!(deepseek_harness_protocol(&effective), protocol);
                    assert_eq!(
                        deepseek_harness_model_ids(&effective),
                        ["model-z", "model-a"]
                    );
                    assert_eq!(
                        effective["llm-pi-ai"]["providers"]["user"]["models"][0]["id"],
                        format!("{profile}-user-model")
                    );
                }
                assert!(
                    apply_deepseek_harness_config(
                        TOOL_CONFIG_ROOT_URL,
                        "test-key",
                        &models,
                        protocol
                    )
                    .unwrap()
                    .already_configured
                );
                let manifest = load_tool_config_manifest().unwrap();
                assert_eq!(manifest.files.len(), if home_override { 4 } else { 3 });
                remove_additional_tool_config("deepseek-harness").unwrap();
                for (path, original) in originals {
                    assert_eq!(fs::read_to_string(path).unwrap(), original);
                }
                assert!(!deepseek_harness_config_paths().1.exists());
                assert!(load_tool_config_manifest().unwrap().files.is_empty());
            });
        }
    }
}

#[test]
fn deepseek_harness_legacy_ownership_follows_desktop_import_not_empty_web_profile() {
    with_temp_home(|_| {
        let (legacy, _) = deepseek_harness_config_paths();
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, HARNESS_USER_SETTINGS).unwrap();
        apply_deepseek_harness_config(
            TOOL_CONFIG_ROOT_URL,
            "test-key",
            &harness_test_models("model-a"),
            ToolProtocol::OpenAiChat,
        )
        .unwrap();
        let settings =
            parse_deepseek_harness_yaml_mapping(&fs::read_to_string(&legacy).unwrap(), "test")
                .unwrap();
        fs::rename(&legacy, legacy.with_file_name("settings.yaml.imported")).unwrap();
        let desktop = deepseek_harness_profile_patch_path("desktop");
        let web = deepseek_harness_profile_patch_path("web");
        for path in [&desktop, &web] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        fs::write(
            &desktop,
            render_deepseek_harness_patch(HARNESS_OTHER_PATCH, &settings).unwrap(),
        )
        .unwrap();
        fs::write(&web, "# web template\n[]\n").unwrap();
        let manifest = load_tool_config_manifest().unwrap();
        assert!(manifest.files.contains_key(&manifest_file_key(&desktop)));
        assert!(!manifest.files.contains_key(&manifest_file_key(&web)));
        assert!(!manifest.files.contains_key(&manifest_file_key(&legacy)));
        apply_deepseek_harness_config(
            TOOL_CONFIG_ROOT_URL,
            "test-key",
            &harness_test_models("model-b"),
            ToolProtocol::OpenAiResponses,
        )
        .unwrap();
        remove_additional_tool_config("deepseek-harness").unwrap();
        assert_eq!(
            harness_test_read_settings(&desktop),
            serde_yaml::from_str::<serde_yaml::Value>(HARNESS_USER_SETTINGS).unwrap()
        );
        assert_eq!(fs::read_to_string(&web).unwrap(), "# web template\n[]\n");
        assert!(!legacy.exists());
        assert!(load_tool_config_manifest().unwrap().files.is_empty());
    });
}
