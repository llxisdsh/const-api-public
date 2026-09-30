fn admin_order_models(ids: &[&str]) -> Vec<ToolModelInfo> {
    let mut models = crate::tool_model_metadata::tool_models_from_response(&serde_json::json!({
        "data": ids.iter().map(|id| serde_json::json!({"id":id,"const_api":{
            "tool_call":true,"context_tokens":65536,"output_tokens":8192,
            "input_modalities":["text"],"output_modalities":["text"]
        }})).collect::<Vec<_>>(),
        "const_api_model_presentation":{"schema_version":1,"rules":ids.iter().map(|id|
            serde_json::json!({"pattern":id,"hidden":false})).collect::<Vec<_>>()}
    }));
    models.reverse(); // Writers must use policy rank, not incidental input order.
    models
}

fn apply_order_test_catalog(tool: &str, models: &[ToolModelInfo]) {
    let protocol = default_tool_protocol(tool).unwrap();
    match tool {
        "opencode" => apply_opencode_config_with_model_info_for_protocol(
            TOOL_CONFIG_ROOT_URL,
            "mock",
            models,
            protocol,
        ),
        "workbuddy" => apply_workbuddy_config_with_model_info_for_protocol(
            TOOL_CONFIG_ROOT_URL,
            "mock",
            models,
            protocol,
        ),
        _ => apply_additional_tool_config_with_model_info_for_protocol(
            tool,
            TOOL_CONFIG_ROOT_URL,
            "mock",
            models,
            protocol,
        ),
    }
    .unwrap_or_else(|error| panic!("{tool}: {error:#}"));
}

#[test]
fn model_dictionaries_write_admin_order_on_initial_and_repeat_configuration() {
    with_temp_home(|_| {
        for (tool, path, collection, provider) in [
            (
                "mimocode",
                mimocode_config_path().unwrap(),
                "provider",
                "const-api",
            ),
            (
                "openscience",
                openscience_config_path().unwrap(),
                "provider",
                "const-api",
            ),
            ("zcode", zcode_config_path(), "provider", "const-api"),
            (
                "opencode",
                opencode_config_path().unwrap(),
                "provider",
                CODEX_CONST_API_PROVIDER_ID,
            ),
            (
                "cline",
                cline_provider_paths().unwrap().1,
                "providers",
                "const-api",
            ),
        ] {
            for ids in [
                ["z-order", "m-order", "a-order"],
                ["m-order", "a-order", "z-order"],
            ] {
                apply_order_test_catalog(tool, &admin_order_models(&ids));
                let raw = fs::read_to_string(&path).unwrap();
                let offsets = ids.map(|id| raw.find(&format!("\"{id}\":")).unwrap());
                assert!(
                    offsets.windows(2).all(|p| p[0] < p[1]),
                    "{tool} dictionary order"
                );
                let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
                assert_eq!(
                    value[collection][provider]["models"]
                        .as_object()
                        .unwrap()
                        .len(),
                    3
                );
                if matches!(tool, "mimocode" | "openscience" | "zcode") {
                    assert_eq!(value["model"], format!("const-api/{}", ids[0]));
                }
                apply_order_test_catalog(tool, &admin_order_models(&ids));
                assert_eq!(
                    fs::read_to_string(&path).unwrap(),
                    raw,
                    "{tool} idempotence"
                );
            }
        }
    });
}

#[test]
fn array_and_toml_catalogs_write_admin_order_including_reconfiguration() {
    with_temp_home(|_| {
        for ids in [
            ["z-order", "m-order", "a-order"],
            ["m-order", "a-order", "z-order"],
        ] {
            let models = admin_order_models(&ids);
            for tool in [
                "raven",
                "pi",
                "qwencode",
                "goose",
                "reasonix",
                "deepseek-harness",
                "kimicode",
                "grok-build",
                "mistral-vibe",
                "minimax-code",
                "workbuddy",
            ] {
                apply_order_test_catalog(tool, &models);
            }
            for (tool, path, pointer, field) in [
                ("raven", raven_config_path(), "/providers/custom/models", ""),
                ("pi", pi_models_path(), "/providers/const-api/models", "id"),
                (
                    "qwencode",
                    qwencode_config_path(),
                    "/modelProviders/const-api",
                    "id",
                ),
                ("goose", goose_config_paths().0, "/models", "name"),
                ("workbuddy", workbuddy_models_path(), "", "id"),
            ] {
                let root: serde_json::Value =
                    serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
                let actual = root
                    .pointer(pointer)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| {
                        if field.is_empty() {
                            entry
                        } else {
                            &entry[field]
                        }
                    })
                    .map(|v| v.as_str().unwrap())
                    .map(|id| {
                        if tool == "raven" {
                            id.strip_prefix("custom/").unwrap()
                        } else {
                            id
                        }
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual, ids, "{tool}");
            }
            for (tool, path, table) in [
                ("kimicode", kimicode_config_path(), "models"),
                ("grok-build", grok_build_config_path(), "model"),
            ] {
                let doc = fs::read_to_string(path)
                    .unwrap()
                    .parse::<DocumentMut>()
                    .unwrap();
                assert_eq!(
                    doc[table]
                        .as_table()
                        .unwrap()
                        .iter()
                        .map(|(key, _)| key.strip_prefix("const-api/").unwrap())
                        .collect::<Vec<_>>(),
                    ids,
                    "{tool}"
                );
            }
            let reasonix = fs::read_to_string(reasonix_config_paths().0)
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            assert_eq!(reasonix_models(&reasonix), ids);
            let vibe = fs::read_to_string(mistral_vibe_config_paths().0)
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            assert_eq!(
                vibe["models"]
                    .as_array_of_tables()
                    .unwrap()
                    .iter()
                    .map(|m| m["name"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ids
            );
            let dsh =
                deepseek_harness_effective_settings(&deepseek_harness_settings_groups().unwrap()[0])
                    .unwrap();
            assert_eq!(
                deepseek_harness_model_ids(
                    &parse_deepseek_harness_yaml_mapping(&dsh, "test").unwrap()
                ),
                ids
            );
            let mini: serde_json::Value =
                serde_yaml::from_slice(&fs::read(minimax_code_config_path()).unwrap()).unwrap();
            assert_eq!(
                mini["custom_provider"]["const-api"]["model_order"],
                serde_json::json!(ids)
            );
            let codex = build_codex_model_catalog(&models, &serde_json::json!({})).unwrap();
            assert_eq!(
                codex["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| m["slug"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ids
            );
            assert_eq!(
                openclaw_model_entries(
                    &models,
                    TOOL_CONFIG_ROOT_URL,
                    ToolProtocol::OpenAiResponses
                )
                .iter()
                .map(|m| m["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
                ids
            );
            let vscode = vscode_const_api_provider(
                TOOL_CONFIG_ROOT_URL,
                "mock",
                &models,
                ToolProtocol::OpenAiResponses,
            )
            .unwrap();
            assert_eq!(
                vscode["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| m["id"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ids
            );
        }
    });
}

#[test]
fn grok_reordering_preserves_advanced_tables_without_their_old_positions() {
    with_temp_home(|_| {
        apply_order_test_catalog("grok-build", &admin_order_models(&["z-order", "a-order"]));
        let path = grok_build_config_path();
        let mut doc = fs::read_to_string(&path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        doc["model"]["const-api/z-order"]["advanced"]["user_setting"] = toml_value(true);
        fs::write(&path, doc.to_string()).unwrap();
        apply_order_test_catalog("grok-build", &admin_order_models(&["a-order", "z-order"]));
        let raw = fs::read_to_string(&path).unwrap();
        let doc = raw.parse::<DocumentMut>().unwrap();
        assert_eq!(
            doc["model"]
                .as_table()
                .unwrap()
                .iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>(),
            ["const-api/a-order", "const-api/z-order"]
        );
        assert_eq!(
            doc["model"]["const-api/z-order"]["advanced"]["user_setting"].as_bool(),
            Some(true)
        );
        apply_order_test_catalog("grok-build", &admin_order_models(&["a-order", "z-order"]));
        assert_eq!(fs::read_to_string(&path).unwrap(), raw);
    });
}

#[test]
fn cancel_removes_created_model_namespaces_after_runtime_edits_preserving_user_config() {
    with_temp_home(|_| {
        for tool in [
            "mimocode",
            "openscience",
            "zcode",
            "cline",
            "pi",
            "qwencode",
        ] {
            apply_order_test_catalog(tool, &admin_order_models(&["z-order", "a-order"]));
            let (path, collection) = match tool {
                "mimocode" => (mimocode_config_path().unwrap(), "provider"),
                "openscience" => (openscience_config_path().unwrap(), "provider"),
                "zcode" => (zcode_config_path(), "provider"),
                "cline" => (cline_provider_paths().unwrap().1, "providers"),
                "pi" => (pi_models_path(), "providers"),
                _ => (qwencode_config_path(), "modelProviders"),
            };
            let mut current: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            current["user_setting"] = serde_json::json!("keep");
            current[collection]["personal"] =
                serde_json::json!({"models":{"z-order":{"name":"user model"}}});
            if tool == "qwencode" {
                current[collection]["const-api"][0]["runtime_metadata"] = serde_json::json!(true);
            } else {
                current[collection]["const-api"]["runtime_metadata"] = serde_json::json!(true);
            }
            fs::write(&path, serde_json::to_vec(&current).unwrap()).unwrap();
            apply_order_test_catalog(tool, &admin_order_models(&["a-order", "z-order"]));
            remove_additional_tool_config(tool).unwrap();
            let after: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert!(
                after[collection].get("const-api").is_none(),
                "{tool} leaked its models"
            );
            assert_eq!(
                after[collection]["personal"], current[collection]["personal"],
                "{tool}"
            );
            assert_eq!(after["user_setting"], "keep");
        }
    });
}

#[test]
fn cancel_removes_only_added_models_inside_a_preexisting_provider() {
    with_temp_home(|_| {
        let path = mimocode_config_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = serde_json::json!({"provider":{"const-api":{"keep":true,
            "models":{"original-model":{"name":"Mine"}}}}});
        fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        apply_order_test_catalog("mimocode", &admin_order_models(&["z-order"]));
        let mut current: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        current["provider"]["const-api"]["models"]["z-order"]["runtime"] = serde_json::json!(true);
        current["provider"]["const-api"]["models"]["user-added"] =
            serde_json::json!({"name":"User added"});
        fs::write(&path, serde_json::to_vec(&current).unwrap()).unwrap();
        remove_additional_tool_config("mimocode").unwrap();
        let after: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let provider = &after["provider"]["const-api"];
        assert_eq!(provider["keep"], true);
        assert_eq!(
            provider["models"]["original-model"],
            original["provider"]["const-api"]["models"]["original-model"]
        );
        assert_eq!(
            provider["models"]["user-added"],
            current["provider"]["const-api"]["models"]["user-added"]
        );
        assert!(provider["models"].get("z-order").is_none());
    });
}

#[test]
fn cancel_toml_named_models_and_namespaced_tables_after_runtime_edits() {
    with_temp_home(|_| {
        for tool in ["kimicode", "grok-build", "reasonix", "mistral-vibe"] {
            apply_order_test_catalog(tool, &admin_order_models(&["z-order"]));
            let path = match tool {
                "kimicode" => kimicode_config_path(),
                "grok-build" => grok_build_config_path(),
                "reasonix" => reasonix_config_paths().0,
                _ => mistral_vibe_config_paths().0,
            };
            let mut doc = fs::read_to_string(&path)
                .unwrap()
                .parse::<DocumentMut>()
                .unwrap();
            doc["user_setting"] = toml_value("keep");
            match tool {
                "kimicode" => {
                    doc["models"]["const-api/z-order"]["runtime_metadata"] = toml_value(true)
                }
                "grok-build" => {
                    doc["model"]["const-api/z-order"]["runtime_metadata"] = toml_value(true)
                }
                "reasonix" => {
                    doc["providers"]
                        .as_array_of_tables_mut()
                        .unwrap()
                        .get_mut(0)
                        .unwrap()["runtime_metadata"] = toml_value(true)
                }
                _ => {
                    doc["models"]
                        .as_array_of_tables_mut()
                        .unwrap()
                        .get_mut(0)
                        .unwrap()["runtime_metadata"] = toml_value(true)
                }
            }
            fs::write(&path, doc.to_string()).unwrap();
            remove_additional_tool_config(tool).unwrap();
            let after = fs::read_to_string(&path).unwrap();
            assert!(!after.contains("z-order"), "{tool} left a model behind");
            assert!(after.contains("user_setting = \"keep\""), "{tool}");
        }
    });
}

#[test]
fn toml_named_restore_preserves_preexisting_entries_and_ignores_quote_rewrites() {
    with_temp_home(|home| {
        let path = home.join("owned.toml");
        let original =
            "[[providers]]\nname = 'const-api'\nurl = 'https://personal.test'\nuser_field = 1\n";
        let applied = "[[providers]]\nname = \"const-api\"\nurl = \"http://127.0.0.1:38788\"\nuser_field = 1\n[[models]]\nname = \"z-order\"\nprovider = \"const-api\"\n";
        let fields = tool_config_owned_fields(
            ToolConfigFormat::Toml,
            Some(original.as_bytes()),
            applied.as_bytes(),
        )
        .unwrap();
        assert!(
            fields.iter().all(|field| field
                .path
                .iter()
                .any(|segment| matches!(segment, ToolConfigFieldPath::Named(_)))),
            "{fields:?}"
        );
        fs::write(&path, original).unwrap();
        write_text_with_backup(&path, applied, "reasonix", &mut ToolApplyBuilder::default())
            .unwrap();
        let current = applied.replace('"', "'")
            + "runtime = true\n[[models]]\nname = 'user-model'\nprovider = 'personal'\n";
        let current = current.replace("user_field = 1", "user_field = 2\nuser_added = true");
        fs::write(&path, current).unwrap();
        restore_tool_config_from_manifest("reasonix").unwrap();
        let after = fs::read_to_string(&path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        let provider = after["providers"]
            .as_array_of_tables()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(provider["name"].as_str(), Some("const-api"));
        assert_eq!(provider["url"].as_str(), Some("https://personal.test"));
        assert_eq!(provider["user_field"].as_integer(), Some(2));
        assert_eq!(provider["user_added"].as_bool(), Some(true));
        let models = after["models"].as_array_of_tables().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models.get(0).unwrap()["name"].as_str(), Some("user-model"));
    });
}

#[test]
fn toml_restore_recreates_removed_original_model_tables_and_named_entries() {
    with_temp_home(|home| {
        let path = home.join("owned.toml");
        let original = "[models.'const-api/original']\nname = 'original'\ncontext = 8192\n[[providers]]\nname = 'original'\nurl = 'https://personal.test'\n";
        fs::write(&path, original).unwrap();
        write_text_with_backup(&path, "", "reasonix", &mut ToolApplyBuilder::default()).unwrap();
        fs::write(&path, "user_added = true\n").unwrap();
        restore_tool_config_from_manifest("reasonix").unwrap();
        let after = fs::read_to_string(&path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(after["user_added"].as_bool(), Some(true));
        assert_eq!(
            after["models"]["const-api/original"]["context"].as_integer(),
            Some(8192)
        );
        assert_eq!(
            after["providers"]
                .as_array_of_tables()
                .unwrap()
                .get(0)
                .unwrap()["url"]
                .as_str(),
            Some("https://personal.test")
        );
    });
}

#[test]
fn legacy_leaf_ownership_promotes_only_the_complete_new_entry() {
    with_temp_home(|home| {
        for preexisting in [false, true] {
            let path = home.join(if preexisting {
                "existing.json"
            } else {
                "created.json"
            });
            fs::write(
                &path,
                if preexisting {
                    r#"{"providers":{"const-api":{"user_field":1}}}"#
                } else {
                    "{}"
                },
            )
            .unwrap();
            let applied = if preexisting {
                r#"{"providers":{"const-api":{"name":"CONST","user_field":1}}}"#
            } else {
                r#"{"providers":{"const-api":{"name":"CONST"}}}"#
            };
            write_text_with_backup(&path, applied, "pi", &mut ToolApplyBuilder::default()).unwrap();
            let mut manifest = load_tool_config_manifest().unwrap();
            manifest
                .files
                .get_mut(&manifest_file_key(&path))
                .unwrap()
                .fields = vec![ToolConfigOwnedField {
                path: ["providers", "const-api", "name"]
                    .map(|key| ToolConfigFieldPath::Key(key.into()))
                    .to_vec(),
                original: ToolConfigFieldValue::Missing,
                applied: ToolConfigFieldValue::Value(serde_json::json!("CONST")),
            }];
            save_tool_config_manifest(&manifest).unwrap();
            let current = serde_json::json!({"user_setting":"keep","providers":{"const-api":{"name":"CONST","runtime":true,"user_field":1},"personal":{"models":["z-order"]}}});
            fs::write(&path, serde_json::to_vec(&current).unwrap()).unwrap();
            restore_tool_config_from_manifest("pi").unwrap();
            let after: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(after["providers"].get("const-api").is_some(), preexisting);
            assert_eq!(
                after["providers"]["personal"],
                current["providers"]["personal"]
            );
            if preexisting {
                assert_eq!(after["providers"]["const-api"]["user_field"], 1);
            }
        }
    });
}

#[test]
fn workbuddy_legacy_cancel_prunes_removed_models_from_available_list() {
    with_temp_home(|_| {
        let path = workbuddy_models_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let endpoint =
            workbuddy_endpoint_url(TOOL_CONFIG_ROOT_URL, ToolProtocol::OpenAiChat).unwrap();
        let mut managed = serde_json::json!({});
        configure_workbuddy_model_entry(
            &mut managed,
            &catalog_test_model("z-order"),
            &endpoint,
            "mock",
        );
        let personal =
            serde_json::json!({"id":"user-model","name":"User","baseUrl":"https://example.test"});
        fs::write(&path, serde_json::to_vec(&serde_json::json!({"models":[managed,personal],"availableModels":["z-order","user-model"]})).unwrap()).unwrap();
        remove_workbuddy_config(TOOL_CONFIG_ROOT_URL, "mock").unwrap();
        let after: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(after["models"], serde_json::json!([personal]));
        assert_eq!(after["availableModels"], serde_json::json!(["user-model"]));
    });
}

#[test]
fn shared_desktop_editions_require_selection_but_saved_cli_and_single_desktop_do_not() {
    let mut candidates =
        ["Xiaomi MiMo.exe", "Xiaomi MiMo AI.exe", "mimo.exe"].map(|name| ToolProgramCandidate {
            path: format!("D:/apps/{name}"),
            kind: if name == "mimo.exe" {
                "command"
            } else {
                "windows_exe"
            }
            .into(),
            exists: true,
            ..Default::default()
        });
    assert!(tool_program_selection_required("mimocode", &candidates));
    candidates[2].selected = true;
    assert!(!tool_program_selection_required("mimocode", &candidates));
    assert_eq!(
        select_tool_launch_candidate("mimocode", &candidates)
            .unwrap()
            .path,
        candidates[2].path
    );
    candidates[2].exists = false;
    candidates[2].selected = true;
    assert!(tool_program_selection_required("mimocode", &candidates));
    assert!(
        preferred_tool_program_path_for_saved_selection(
            "mimocode",
            &candidates[2].path,
            &candidates,
        )
        .is_none(),
        "a stale CLI choice must not silently select one of two desktop editions"
    );
    candidates[2].selected = false;
    candidates[1].exists = false;
    assert!(!tool_program_selection_required("mimocode", &candidates));
    assert_eq!(
        select_tool_launch_candidate("mimocode", &candidates)
            .unwrap()
            .path,
        candidates[0].path
    );
}
