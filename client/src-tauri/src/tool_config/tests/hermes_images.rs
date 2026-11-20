fn seed_hermes_runtime(config: &Path, version: &str) {
    let install = config.with_file_name("hermes-agent");
    let plugin = install.join("plugins/image_gen/openai/__init__.py");
    fs::create_dir_all(plugin.parent().unwrap()).unwrap();
    fs::write(plugin, "").unwrap();
    fs::write(
        install.join("install-stamp.json"),
        format!("\u{feff}{}", serde_json::json!({"baseVersion": version})),
    )
    .unwrap();
}

#[tokio::test]
async fn hermes_images_discover_both_operations_by_exact_id_in_an_authenticated_get() {
    use crate::image_models::{EDIT, GENERATE};
    use warp::Filter;
    let payload = serde_json::json!({
        "data": [
            {"id": "a/Image", "const_api": {"image_operations": [EDIT, GENERATE]}},
            {"id": "b/Image", "const_api": {"image_operations": [EDIT, GENERATE]}},
            {"id": "a/Image", "const_api": {"image_operations": [GENERATE, EDIT]}},
            {"id": " padded ", "const_api": {"image_operations": [EDIT, GENERATE]}},
            {"id": "generate", "const_api": {"image_operations": [GENERATE]}},
            {"id": "edit", "const_api": {"image_operations": [EDIT]}},
            {"id": "split", "const_api": {"image_operations": [EDIT]}},
            {"id": "split", "const_api": {"image_operations": [GENERATE]}},
            {"id": "vision", "const_api": {"vision": true, "output_modalities": ["image"]}},
            {"id": "gpt-image-2"},
            {"id": "paths", "const_api": {"image_operations": ["/v1/images/generations", "/images/edits"]}},
            {"id": "malformed", "const_api": {"image_operations": GENERATE}},
            {"id": "malformed-list", "const_api": {"image_operations": [EDIT, null, GENERATE]}},
            {"id": "vendor/Hidden", "const_api": {"image_operations": [EDIT, GENERATE]}}
        ],
        "const_api_model_presentation": {"schema_version": 1, "rules": [{"pattern": "hidden", "hidden": true}]}
    });
    let route = warp::get()
        .and(warp::path!("v1" / "models"))
        .and(warp::header::exact("authorization", "Bearer fixture-local"))
        .map(move || warp::reply::json(&payload));
    let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
    let server = tokio::spawn(server);
    let mut cfg = crate::default_config();
    cfg.listen = addr.to_string();
    cfg.api_key = "fixture-local".into();
    cfg.tool_model_injection.all_models = false;
    let ticket = create_tool_config_operation("hermes", ToolConfigAction::Apply, false).unwrap();
    let operation = start_tool_config_operation(
        &ticket.operation_id,
        "hermes",
        ToolConfigAction::Apply,
        false,
    )
    .unwrap();
    let fetched =
        crate::fetch_local_platform_model_payload_for_operation(&cfg, &operation, "hermes").await;
    finish_tool_config_operation(&operation, fetched.is_ok());
    server.abort();
    assert_eq!(
        hermes_image_model_ids(&fetched.unwrap().unwrap()),
        ["a/Image", "b/Image"]
    );
}

#[test]
fn hermes_images_preserve_explicit_choices_and_skip_ambiguous_or_unavailable_models() {
    for (raw, ids) in [
        ("{}", &[][..]),
        ("{}", &["only"][..]),
        ("image_gen: {provider: openai}", &["only"][..]),
        ("{}", &["expensive-z", "expensive-a"][..]),
        (
            "image_gen: {provider: fal, model: existing, fal: {quality: high}}",
            &["a"],
        ),
        ("image_gen: {fal: {model: existing}}", &["a"]),
        ("image_gen: {provider: openai, openai: {model: a}}", &["a"]),
        (
            "providers: {other: {name: CONST_API, api: 'https://other.test/v1'}}",
            &["a"],
        ),
        (
            "image_gen: {provider: openai, openai: {provider: other}}",
            &["a"],
        ),
        (
            "image_gen: {openai: {base_url: 'https://example.test/v1'}}",
            &["a"],
        ),
        ("image_gen: {openai: {key_env: MY_IMAGE_KEY}}", &["a"]),
        ("image_gen: {model: preferred}", &["a"]),
        ("image_gen: {openai: {model: preferred}}", &["a"]),
        ("image_gen: {model: a, openai: {model: b}}", &["a", "b"]),
        (
            "image_gen: {provider: openai, openai: {provider: CONST_API, model: previous}}",
            &[],
        ),
        ("image_gen: {openai: []}", &["a"]),
        ("image_gen: false", &["a"]),
        // Hermes would send the base model with a quality parameter.
        ("{}", &["gpt-image-2-high"]),
    ] {
        let original = parse_hermes_config(raw).unwrap();
        let mut actual = original.clone();
        upsert_hermes_images(&mut actual, ids).unwrap();
        assert_eq!(actual, original, "{raw}");
    }
    for (preference, wire) in [
        ("custom-image", "custom-image"),
        ("gpt-image-2-low", "gpt-image-2"),
        ("gpt-image-2.5-flare-max", "gpt-image-2.5-flare"),
    ] {
        let mut root = parse_hermes_config(&format!("image_gen: {{provider: openai, model: {preference}, openai: {{provider: CONST_API}}}}")).unwrap();
        upsert_hermes_images(&mut root, &["other", wire]).unwrap();
        assert_eq!(root["image_gen"]["model"].as_str(), Some(preference));
        assert_eq!(
            root["image_gen"]["openai"]["model"].as_str(),
            Some(preference)
        );
        assert_eq!(
            root["image_gen"]["openai"]["provider"].as_str(),
            Some("CONST_API")
        );
    }
}

#[test]
fn hermes_images_preview_check_and_restore_through_the_managed_profile() {
    with_temp_home(|_| {
        let path = hermes_config_paths().remove(0);
        seed_hermes_runtime(&path, "0.23.2");
        let original = "# Keep user formatting\nimage_gen:\n  max_parallel_requests: 2\n  provider: openai\n  openai: {provider: CONST_API}\n";
        fs::write(&path, original).unwrap();
        let env_file = path.with_file_name(".env");
        fs::write(&env_file, "USER_SETTING=preserve\n").unwrap();
        let root = "http://127.0.0.1:39871";
        let apply = || {
            apply_hermes_config_for_protocol(
                root,
                "fixture-local",
                ToolProtocol::OpenAiChat,
                &["image-a"],
            )
        };
        let check =
            || check_tool_config_by_name("hermes", &format!("{root}/v1"), root, "fixture-local");
        let remove =
            || remove_tool_config_by_name("hermes", &format!("{root}/v1"), root, "fixture-local");
        let preview = preview_tool_config_apply(apply).unwrap();
        assert!(!preview.already_configured);
        assert!(preview.files.is_empty() && preview.backups.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(!tool_config_manifest_path().exists());

        set_test_tool_runtime("hermes", false);
        let ticket =
            create_tool_config_operation("hermes", ToolConfigAction::Apply, false).unwrap();
        let operation = start_tool_config_operation(
            &ticket.operation_id,
            "hermes",
            ToolConfigAction::Apply,
            false,
        )
        .unwrap();
        let outcome = crate::execute_previewed_tool_config_operation(
            &operation,
            true,
            Some("openai_chat"),
            "hermes-image-fixture",
            None,
            apply,
            |_| panic!("configure-only must not launch Hermes"),
        );
        finish_tool_config_operation(&operation, outcome.is_ok());
        assert!(matches!(
            outcome.unwrap(),
            ToolConfigWriteOutcome::Completed(_)
        ));
        let configured = fs::read(&path).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_slice(&configured).unwrap();
        let image = &doc["image_gen"];
        assert_eq!(image["provider"], "openai");
        assert_eq!(image["openai"]["model"], "image-a");
        assert_eq!(image["openai"]["provider"], "CONST_API");
        assert_eq!(image["openai"].as_mapping().unwrap().len(), 2);
        assert_eq!(image["max_parallel_requests"].as_u64(), Some(2));
        let named = &doc["custom_providers"][0];
        assert_eq!(named["name"], image["openai"]["provider"]);
        assert_eq!(named["base_url"], "http://127.0.0.1:39871/v1");
        assert_eq!(named["api_key"], "fixture-local");
        assert_eq!(named["discover_models"].as_bool(), Some(true));
        assert!(named.get("models").is_none());
        assert!(check().unwrap().already_configured);
        assert!(preview_tool_config_apply(apply).unwrap().already_configured);
        assert!(preview_tool_config_apply(remove).unwrap().files.is_empty());
        assert_eq!(fs::read(&path).unwrap(), configured);
        remove().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(
            fs::read_to_string(&env_file).unwrap(),
            "USER_SETTING=preserve\n"
        );
    });
}

#[test]
fn hermes_images_reconfigure_respects_user_switches_and_later_edits() {
    with_temp_home(|_| {
        let path = hermes_config_paths().remove(0);
        seed_hermes_runtime(&path, "0.21.6");
        let read =
            || serde_yaml::from_slice::<serde_yaml::Value>(&fs::read(&path).unwrap()).unwrap();
        let save = |doc| fs::write(&path, serde_yaml::to_string(doc).unwrap()).unwrap();
        let apply = |ids: &[&str]| {
            apply_hermes_config_for_protocol(
                TOOL_CONFIG_ROOT_URL,
                "fixture-key",
                ToolProtocol::OpenAiChat,
                ids,
            )
            .unwrap()
        };
        fs::write(&path, "image_gen: {provider: openai, openai: {provider: CONST_API}}\n").unwrap();
        apply(&["image-a"]);
        let mut doc = read();
        doc["image_gen"].as_mapping_mut().unwrap().remove("openai");
        save(&doc);
        assert!(
            check_hermes_config(TOOL_CONFIG_ROOT_URL, "fixture-key")
                .unwrap()
                .already_configured
        );
        apply(&[]);
        assert_eq!(read()["image_gen"], doc["image_gen"]);
        apply(&["image-a"]);
        assert_eq!(read()["image_gen"], doc["image_gen"]);
        let mut doc = read();
        doc["image_gen"]["provider"] = yaml_string("xai");
        doc["image_gen"]["openai"]["model"] = yaml_string("user-choice");
        doc["image_gen"]["xai"]["model"] = yaml_string("user-xai");
        save(&doc);
        apply(&["image-b"]);
        assert_eq!(read()["image_gen"], doc["image_gen"]);
        remove_hermes_config(TOOL_CONFIG_ROOT_URL, "fixture-key").unwrap();
        let result = read();
        assert_eq!(result["image_gen"]["provider"], "xai");
        assert_eq!(result["image_gen"]["xai"]["model"], "user-xai");
        assert_eq!(result["image_gen"]["openai"]["model"], "user-choice");
        assert!(result["image_gen"]["openai"].get("provider").is_none());
        assert!(result.get("custom_providers").is_none());
    });
}

#[test]
fn hermes_images_old_and_unknown_installs_keep_chat_discovery() {
    with_temp_home(|_| {
        for (index, path) in hermes_config_paths().iter().enumerate() {
            if index == 0 {
                seed_hermes_runtime(path, "0.21.4");
            }
        }
        apply_hermes_config_for_protocol(
            TOOL_CONFIG_ROOT_URL,
            "fixture-key",
            ToolProtocol::OpenAiChat,
            &["image-both"],
        )
        .unwrap();
        for path in hermes_config_paths() {
            let doc: serde_yaml::Value = serde_yaml::from_slice(&fs::read(path).unwrap()).unwrap();
            assert!(doc.get("image_gen").is_none());
            assert_eq!(
                doc["custom_providers"][0]["discover_models"].as_bool(),
                Some(true)
            );
        }
    });
}

#[test]
fn hermes_images_leave_a_profile_env_model_override_in_control() {
    with_temp_home(|_| {
        let path = hermes_config_paths().remove(0);
        seed_hermes_runtime(&path, "0.23.2");
        let env_file = path.with_file_name(".env");
        for original in [
            "export\tOPENAI_IMAGE_MODEL='user-image'\n",
            "FAL_IMAGE_MODEL=existing-fal-model\n",
            "FAL_KEY=fixture-fal\n",
        ] {
            fs::write(&env_file, original).unwrap();
            apply_hermes_config_for_protocol(
                TOOL_CONFIG_ROOT_URL,
                "fixture-local",
                ToolProtocol::OpenAiChat,
                &["image-both"],
            )
            .unwrap();
            let doc: serde_yaml::Value = serde_yaml::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert!(doc.get("image_gen").is_none());
            assert_eq!(fs::read_to_string(&env_file).unwrap(), original);
        }
    });
}

#[test]
fn hermes_images_restore_an_existing_named_endpoint_used_by_the_user() {
    with_temp_home(|_| {
        let path = hermes_config_paths().remove(0);
        seed_hermes_runtime(&path, "0.23.2");
        let original = concat!(
            "image_gen: {provider: openai, openai: {provider: CONST_API, model: original}}\n",
            "custom_providers:\n",
            "- {name: CONST_API, base_url: 'http://127.0.0.1:40000/v1', api_key: fixture-original}\n",
        );
        fs::write(&path, original).unwrap();
        let remove = || remove_hermes_config(TOOL_CONFIG_ROOT_URL, "fixture-local").unwrap();
        remove();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        apply_hermes_config_for_protocol(
            TOOL_CONFIG_ROOT_URL,
            "fixture-local",
            ToolProtocol::OpenAiChat,
            &["original", "other"],
        )
        .unwrap();
        remove();
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    });
}

#[test]
fn hermes_images_detach_chat_while_retaining_an_independently_selected_image_endpoint() {
    with_temp_home(|_| {
        let path = hermes_config_paths().remove(0);
        let read = || serde_yaml::from_slice::<serde_yaml::Value>(&fs::read(&path).unwrap()).unwrap();
        for managed in [false, true] {
            if managed {
                apply_hermes_config_for_protocol(
                    TOOL_CONFIG_ROOT_URL, "fixture-local", ToolProtocol::OpenAiChat, &[],
                ).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, concat!(
                    "model: {provider: 'custom:CONST_API', base_url: 'http://127.0.0.1:38787/v1', api_key: fixture-local}\n",
                    "custom_providers: [{name: CONST_API, base_url: 'http://127.0.0.1:38787/v1', api_key: fixture-local}]\n",
                )).unwrap();
            }
            let mut current = read();
            current["image_gen"]["provider"] = yaml_string("openai");
            current["image_gen"]["openai"]["provider"] = yaml_string("CONST_API");
            current["image_gen"]["openai"]["model"] = yaml_string("canvas");
            fs::write(&path, serde_yaml::to_string(&current).unwrap()).unwrap();
            remove_hermes_config(TOOL_CONFIG_ROOT_URL, "fixture-local").unwrap();
            let removed = read();
            assert!(removed["model"].get("provider").is_none(), "chat still uses CONST");
            assert_eq!(removed["image_gen"], current["image_gen"]);
            assert_eq!(removed["custom_providers"], current["custom_providers"]);
            fs::remove_file(&path).unwrap();
        }
    });
}

#[test]
fn hermes_images_remove_keeps_the_newly_selected_openai_endpoint_active() {
    with_temp_home(|_| {
        let path = hermes_config_paths().remove(0);
        seed_hermes_runtime(&path, "0.23.2");
        // Simulate a legacy apply that created the native image binding.
        let mut applied = ToolApplyBuilder::default();
        write_text_with_backup(&path, "image_gen: {provider: openai, openai: {provider: CONST_API, model: image-both}}\n", "hermes", &mut applied).unwrap();
        apply_hermes_config(TOOL_CONFIG_ROOT_URL, "fixture-local").unwrap();
        let mut doc: serde_yaml::Value = serde_yaml::from_slice(&fs::read(&path).unwrap()).unwrap();
        doc["image_gen"]["openai"]["provider"] = yaml_string("my-images");
        doc["image_gen"]["openai"]["model"] = yaml_string("my-model");
        fs::write(&path, serde_yaml::to_string(&doc).unwrap()).unwrap();
        remove_hermes_config(TOOL_CONFIG_ROOT_URL, "fixture-local").unwrap();
        let restored: serde_yaml::Value = serde_yaml::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(restored["image_gen"], doc["image_gen"]);
        assert!(restored.get("custom_providers").is_none());
    });
}
