#[test]
fn image_skill_configuration_and_both_removal_paths_preserve_the_chat_configuration_contract() {
    with_temp_home(|home| {
        let key = "test-only-local-access";
        for (tool, config_file) in [
            ("codex", ".codex/config.toml"),
            ("claude", ".claude/settings.json"),
            ("workbuddy", ".workbuddy/models.json"),
            ("workbuddy-ai", ".workbuddy-ai/models.json"),
        ] {
            let chat_path = home.join(config_file);
            let mut chat = match tool {
                "workbuddy" | "workbuddy-ai" => {
                    apply_workbuddy_config_with_model_info_for_protocol(
                        tool,
                        TOOL_CONFIG_OPENAI_BASE_URL,
                        key,
                        &[catalog_test_model("fixture-chat")],
                        ToolProtocol::OpenAiChat,
                    )
                }
                _ => apply_tool_config_by_name(
                    tool,
                    TOOL_CONFIG_OPENAI_BASE_URL,
                    TOOL_CONFIG_ROOT_URL,
                    key,
                ),
            }
            .unwrap();
            let original_chat = fs::read(&chat_path).unwrap();
            let path = chat_path
                .parent()
                .unwrap()
                .join("skills/const-api-image/SKILL.md");
            let other_skill = chat_path.parent().unwrap().join("skills/studio/SKILL.md");
            fs::create_dir_all(other_skill.parent().unwrap()).unwrap();
            fs::write(&other_skill, b"studio image workflow").unwrap();
            let mut preview = chat.clone();
            preview.already_configured = true;
            preview_tool_config_apply(|| sync_tool_image_skill(&mut preview, true)).unwrap();
            assert!(!preview.already_configured && !path.exists());

            sync_tool_image_skill(&mut chat, true).unwrap();
            let content = fs::read_to_string(&path).unwrap();
            assert!(content.starts_with("---\nname: const-api-image\n"));
            assert!(!content.contains(key));
            let cli: serde_json::Value = serde_json::from_str(
                content
                    .split("```json\n")
                    .nth(1)
                    .unwrap()
                    .split("\n```")
                    .next()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(cli["models"], serde_json::json!(["image", "models"]));
            assert_eq!(cli["media_models"], serde_json::json!(["media", "models"]));
            assert_eq!(
                cli["video_create"],
                serde_json::json!(["media", "video-create"])
            );
            assert_eq!(cli["speech"][0], "media");
            assert!(cli.get("available_models").is_none());
            preview.already_configured = true;
            preview_tool_config_apply(|| sync_tool_image_skill(&mut preview, true)).unwrap();
            assert!(preview.already_configured);
            preview_tool_config_apply(|| sync_tool_image_skill(&mut preview, false)).unwrap();
            assert!(!preview.already_configured && path.is_file());

            sync_tool_image_skill(&mut chat, false).unwrap();
            assert!(!path.exists());
            assert_eq!(fs::read(&chat_path).unwrap(), original_chat);
            assert_eq!(fs::read(&other_skill).unwrap(), b"studio image workflow");
            sync_tool_image_skill(&mut chat, true).unwrap();
            remove_tool_config_by_name_with_mode(
                tool,
                TOOL_CONFIG_OPENAI_BASE_URL,
                TOOL_CONFIG_ROOT_URL,
                key,
                ToolConfigRemoveMode::RestorePreConst,
            )
            .unwrap();
            assert!(!path.exists());
            assert!(!tool_has_owned_config_files(tool).unwrap());
            assert_eq!(fs::read(other_skill).unwrap(), b"studio image workflow");
        }
    });
}

#[test]
fn media_skill_tool_selection_only_controls_personal_skill_files() {
    with_temp_home(|_| {
        let mut cfg = crate::config::default_config();
        assert!(media_skill_enabled(&cfg, "codex") && media_skill_enabled(&cfg, "claude"));
        assert!(!media_skill_enabled(&cfg, "workbuddy") && !media_skill_enabled(&cfg, "hermes"));
        cfg.media_skill_tools = vec!["hermes".into()];
        assert!(!media_skill_enabled(&cfg, "codex"));
        let paths = tool_image_skill_paths("hermes").unwrap();
        assert!(!paths.is_empty());
        let mut result = ToolApplyBuilder::default().finish("hermes");
        sync_tool_image_skill(&mut result, media_skill_enabled(&cfg, "hermes")).unwrap();
        for path in &paths {
            assert!(path.is_file());
        }
        for config in hermes_config_paths() {
            assert!(!config.exists());
        }
        cfg.configure_image_skill = false;
        sync_tool_image_skill(&mut result, media_skill_enabled(&cfg, "hermes")).unwrap();
        for path in paths {
            assert!(!path.exists());
        }
        assert_eq!(cfg.media_skill_tools, ["hermes"]);
        let mut old = serde_json::to_value(&cfg).unwrap();
        old.as_object_mut().unwrap().remove("media_skill_tools");
        let restored: ClientConfig = serde_json::from_value(old).unwrap();
        assert!(!restored.configure_image_skill);
        assert_eq!(restored.media_skill_tools, default_media_skill_tools());
    });
}

#[test]
fn image_skill_leaves_user_authored_and_modified_skills_in_place() {
    with_temp_home(|_| {
        let mut result = ToolApplyBuilder::default().finish("claude");
        let path = tool_image_skill_paths("claude").unwrap().remove(0);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"user-owned skill").unwrap();
        sync_tool_image_skill(&mut result, true).unwrap();
        assert_eq!(result.details["image_skill"], "user_managed");
        assert!(!tool_has_owned_config_files("claude").unwrap());
        sync_tool_image_skill(&mut result, false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"user-owned skill");
        fs::remove_file(&path).unwrap();
        sync_tool_image_skill(&mut result, true).unwrap();
        fs::write(&path, b"locally edited skill").unwrap();
        sync_tool_image_skill(&mut result, true).unwrap();
        sync_tool_image_skill(&mut result, false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"locally edited skill");
        assert!(!tool_has_owned_config_files("claude").unwrap());

        fs::remove_file(&path).unwrap();
        sync_tool_image_skill(&mut result, true).unwrap();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        remove_claude_config(TOOL_CONFIG_ROOT_URL, "fixture").unwrap();
        assert!(path.is_dir());
        assert!(!tool_has_owned_config_files("claude").unwrap());
    });
}

#[test]
fn optional_skill_respects_occupied_names_and_linked_skills_root() {
    with_temp_home(|home| {
        let mut result = ToolApplyBuilder::default().finish("claude");
        let path = tool_image_skill_paths("claude").unwrap().remove(0);
        let skill = path.parent().unwrap();
        let root = skill.parent().unwrap();
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        for occupied in [root, skill] {
            fs::create_dir_all(occupied.parent().unwrap()).unwrap();
            fs::write(occupied, "user file").unwrap();
            sync_tool_image_skill(&mut result, true).unwrap();
            assert_eq!(result.details["image_skill"], "user_managed");
            assert_eq!(fs::read(occupied).unwrap(), b"user file");
            fs::remove_file(occupied).unwrap();
        }
        // Even an exact copy of the installed file in a shared collection
        // belongs to the user after they link their tool's skills root to it.
        sync_tool_image_skill(&mut result, true).unwrap();
        let shared = home.join("shared-skills");
        fs::rename(root, &shared).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&shared, root).unwrap();
        #[cfg(windows)]
        {
            let junction = std::process::Command::new("cmd")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(root)
                .arg(&shared)
                .output()
                .unwrap();
            assert!(
                junction.status.success(),
                "could not create the test directory junction"
            );
        }
        sync_tool_image_skill(&mut result, false).unwrap();
        assert!(path.is_file());
        assert!(!tool_has_owned_config_files("claude").unwrap());
        fs::remove_dir(root)
            .or_else(|_| fs::remove_file(root))
            .unwrap();
    });
}
