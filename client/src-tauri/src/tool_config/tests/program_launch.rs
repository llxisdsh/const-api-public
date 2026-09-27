    #[test]
    fn anythingllm_program_ownership_is_specific_to_its_desktop_app() {
        for path in ["/apps/AnythingLLM.exe", "/apps/AnythingLLM.app", "/home/test/AnythingLLMDesktop.AppImage"] {
            assert!(is_tool_owned_program_path("anythingllm", Path::new(path)));
        }
        assert!(!is_tool_owned_program_path("anythingllm", Path::new("/apps/Other.exe")));
    }

    #[test]
    fn external_environment_detection_lists_names_only_for_the_selected_tool() {
        let claude = defined_tool_environment_variables_with("claude", |name| {
            matches!(name, "ANTHROPIC_AUTH_TOKEN" | "CLAUDE_CODE_USE_VERTEX")
        });
        let gemini = defined_tool_environment_variables_with("gemini", |name| {
            matches!(name, "GEMINI_API_KEY" | "GEMINI_CLI_HOME")
        });

        assert_eq!(
            claude,
            vec!["ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_USE_VERTEX"]
        );
        assert_eq!(gemini, vec!["GEMINI_API_KEY", "GEMINI_CLI_HOME"]);
        assert!(defined_tool_environment_variables_with("codex", |_| true).is_empty());

        let warning = external_launch_environment_warning(&gemini);
        assert!(warning.contains("GEMINI_API_KEY, GEMINI_CLI_HOME"));
        assert!(warning.contains("Only variable names are shown"));
        assert!(warning.contains("external values"));
    }

    #[test]
    fn child_launch_environment_sets_gateway_values_and_removes_provider_conflicts() {
        assert_eq!(
            claude_code_child_environment("http://localhost/anthropic", "claude-key"),
            [
                ("ANTHROPIC_BASE_URL", "http://localhost/anthropic"),
                ("ANTHROPIC_AUTH_TOKEN", "claude-key"),
                ("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST", "1"),
            ]
        );
        assert!(CLAUDE_CODE_CONFLICTING_ENV_NAMES.contains(&"ANTHROPIC_API_KEY"));
        assert!(CLAUDE_CODE_CONFLICTING_ENV_NAMES.contains(&"ANTHROPIC_BASE_URL"));
        assert!(CLAUDE_CODE_CONFLICTING_ENV_NAMES.contains(&"ANTHROPIC_FOUNDRY_BASE_URL"));
        assert!(CLAUDE_CODE_CONFLICTING_ENV_NAMES.contains(&"CLAUDE_CODE_USE_BEDROCK"));
        assert!(CLAUDE_CODE_CONFLICTING_ENV_NAMES.contains(&"CLAUDE_CODE_USE_MANTLE"));
        assert!(CLAUDE_CODE_CONFLICTING_ENV_NAMES.contains(&"CLAUDE_CONFIG_DIR"));
        assert!(CLAUDE_CODE_CONFLICTING_ENV_NAMES
            .iter()
            .all(|name| CLAUDE_CODE_EXTERNAL_ENVIRONMENT_NAMES.contains(name)));
        assert_eq!(
            gemini_cli_child_environment("http://localhost/gemini", "gemini-key"),
            [
                ("GOOGLE_GEMINI_BASE_URL", "http://localhost/gemini"),
                ("GEMINI_API_KEY", "gemini-key"),
            ]
        );
        assert!(GEMINI_CLI_CONFLICTING_ENV_NAMES.contains(&"GOOGLE_APPLICATION_CREDENTIALS"));
        assert!(GEMINI_CLI_CONFLICTING_ENV_NAMES.contains(&"GOOGLE_GENAI_USE_VERTEXAI"));
        assert!(GEMINI_CLI_CONFLICTING_ENV_NAMES.contains(&"GOOGLE_VERTEX_BASE_URL"));
        assert!(GEMINI_CLI_CONFLICTING_ENV_NAMES.contains(&"GEMINI_CLI_HOME"));
        assert!(GEMINI_CLI_CONFLICTING_ENV_NAMES
            .iter()
            .all(|name| GEMINI_CLI_EXTERNAL_ENVIRONMENT_NAMES.contains(name)));
    }

    #[test]
    fn direct_claude_science_and_gemini_launchers_use_the_injected_runtime_path() {
        with_temp_home(|_| {
            set_test_tool_runtime("claude", false);
            set_test_tool_runtime("claude-science", false);
            set_test_tool_runtime("gemini", false);

            let (_, claude_path) =
                launch_claude_code_program("http://localhost", "claude-key")
                    .expect("launch Claude");
            let (_, gemini_path) =
                launch_gemini_cli_program("http://localhost", "gemini-key")
                    .expect("launch Gemini");
            let (_, science_path) = launch_claude_science_program(
                "http://localhost/anthropic",
                "science-key",
            )
            .expect("launch Claude Science");

            assert_eq!(claude_path, PathBuf::from("test-runtime://claude"));
            assert_eq!(gemini_path, PathBuf::from("test-runtime://gemini"));
            assert_eq!(science_path, PathBuf::from("test-runtime://claude-science"));
            assert_eq!(
                test_tool_runtime_snapshot("claude")
                    .expect("Claude runtime")
                    .launch_calls,
                1
            );
            assert_eq!(
                test_tool_runtime_snapshot("gemini")
                    .expect("Gemini runtime")
                    .launch_calls,
                1
            );
            assert_eq!(
                test_tool_runtime_snapshot("claude-science")
                    .expect("Claude Science runtime")
                    .launch_calls,
                1
            );
        });
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_injected_environment_wins_when_a_name_is_also_removed() {
        let mut command = Command::new("unused-test-command");
        apply_windows_command_environment(
            &mut command,
            &[("ANTHROPIC_BASE_URL", "http://localhost/anthropic")],
            &["ANTHROPIC_BASE_URL"],
        );

        let value = command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new("ANTHROPIC_BASE_URL"))
            .and_then(|(_, value)| value)
            .and_then(OsStr::to_str);
        assert_eq!(value, Some("http://localhost/anthropic"));
    }

    #[test]
    fn direct_copilot_and_goose_launchers_use_the_configured_runtime_path() {
        with_temp_home(|_| {
            let models = tool_models_from_ids(&["gpt-5.6-sol".to_string()]);
            apply_additional_tool_config_with_model_info_for_protocol(
                "copilot",
                TOOL_CONFIG_OPENAI_BASE_URL,
                "copilot-key",
                &models,
                ToolProtocol::OpenAiChat,
            )
            .expect("configure Copilot CLI");
            apply_additional_tool_config_with_model_info_for_protocol(
                "goose",
                TOOL_CONFIG_OPENAI_BASE_URL,
                "goose-key",
                &models,
                ToolProtocol::OpenAiChat,
            )
            .expect("configure Goose");
            set_test_tool_runtime("copilot", false);
            set_test_tool_runtime("goose", false);

            let (_, copilot_path) =
                launch_copilot_cli_program(TOOL_CONFIG_OPENAI_BASE_URL, "copilot-key")
                    .expect("launch Copilot CLI");
            let (_, goose_path) = launch_goose_program("goose-key").expect("launch Goose");

            assert_eq!(copilot_path, PathBuf::from("test-runtime://copilot"));
            assert_eq!(goose_path, PathBuf::from("test-runtime://goose"));
            assert_eq!(
                test_tool_runtime_snapshot("copilot")
                    .expect("Copilot runtime")
                    .launch_calls,
                1
            );
            assert_eq!(
                test_tool_runtime_snapshot("goose")
                    .expect("Goose runtime")
                    .launch_calls,
                1
            );
        });
    }

    #[test]
    fn command_search_dirs_include_node_manager_bins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let nvm_bin = home.join(".nvm/versions/node/v24.0.0/bin");
        let fnm_bin = home.join(".local/state/fnm_multishells/123/bin");
        fs::create_dir_all(&nvm_bin).expect("nvm bin");
        fs::create_dir_all(&fnm_bin).expect("fnm bin");

        let dirs = command_search_dirs(home);

        assert!(dirs.contains(&home.join(".local/bin")));
        assert!(dirs.contains(&home.join(".volta/bin")));
        assert!(dirs.contains(&home.join(".local/share/mise/shims")));
        assert!(dirs.contains(&home.join(".local/share/pnpm")));
        assert!(dirs.contains(&home.join(".opencode/bin")));
        assert!(dirs.contains(&home.join(".hermes/node/bin")));
        assert!(dirs.contains(&home.join(".kimi-code/bin")));
        assert!(dirs.contains(&home.join(".mimocode/bin")));
        assert!(dirs.contains(&nvm_bin));
        assert!(dirs.contains(&fnm_bin));
        #[cfg(target_os = "windows")]
        assert!(dirs.contains(&home.join("scoop/shims")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_command_launch_prefers_cmd_over_extensionless_shell_shim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let shim = dir.path().join("claude");
        let cmd = dir.path().join("claude.cmd");
        fs::write(&shim, "#!/bin/sh\n").expect("shim");
        fs::write(&cmd, "@echo off\r\n").expect("cmd");

        let candidates = windows_command_launch_candidates(&shim, "claude");
        assert_eq!(candidates.first(), Some(&cmd));
        assert!(candidates.iter().any(|candidate| candidate == &shim));
        assert_eq!(command_launch_path(&shim), cmd);
    }

    #[test]
    fn command_launch_handshake_fails_when_terminal_never_executes_the_script() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = wait_for_command_launch_handshake(
            &dir.path().join("started"),
            &dir.path().join("exited"),
            Instant::now(),
            Duration::from_millis(1),
        )
        .expect_err("missing handshake must fail");

        assert!(error.to_string().contains("TOOL_START_HANDSHAKE_TIMEOUT"));
    }

    #[test]
    fn command_launch_handshake_rejects_a_command_that_exits_during_stability_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let started = dir.path().join("started");
        let exited = dir.path().join("exited");
        fs::write(&started, b"started").expect("started marker");
        fs::write(&exited, b"exit=1").expect("exit marker");

        let error = wait_for_command_launch_handshake(
            &started,
            &exited,
            Instant::now() + Duration::from_millis(50),
            Duration::from_millis(10),
        )
        .expect_err("quick exit must fail");

        assert!(error.to_string().contains("TOOL_START_UNSTABLE"));
        assert!(error.to_string().contains("exit=1"));
    }

    #[test]
    fn command_launch_handshake_accepts_a_started_command_that_stays_alive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let started = dir.path().join("started");
        let exited = dir.path().join("exited");
        fs::write(&started, b"started").expect("started marker");

        wait_for_command_launch_handshake(
            &started,
            &exited,
            Instant::now() + Duration::from_millis(50),
            Duration::from_millis(10),
        )
        .expect("stable command handshake");
    }

    #[test]
    fn missing_launch_candidate_is_a_stable_error() {
        with_temp_home(|_| {
            let error = launch_tool_program("missing-launch-candidate")
                .expect_err("a missing candidate cannot count as a confirmed start");

            assert!(error.to_string().contains("TOOL_START_CANDIDATE_MISSING"));
        });
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_claude_code_launcher_opens_command_without_persisting_env_secrets() {
        let path = PathBuf::from(r"C:\Users\me\AppData\Roaming\npm\claude.cmd");
        let script = windows_terminal_launch_script(&path, "Claude Code");

        assert!(script.contains("CONST API - Claude Code"));
        assert!(script.contains("& 'C:\\Users\\me\\AppData\\Roaming\\npm\\claude.cmd'"));
        assert!(!script.contains("ANTHROPIC_AUTH_TOKEN"));
        assert!(!script.contains("ANTHROPIC_BASE_URL"));
        assert_eq!(ps_single_quote("sk-'quoted'"), "'sk-''quoted'''");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_terminal_tools_launch_through_visible_powershell_script() {
        let path = PathBuf::from(r"C:\Users\me\AppData\Roaming\npm\gemini.cmd");
        let script = windows_terminal_launch_script(&path, &tool_display_name_from_path(&path));

        assert!(script.contains("CONST API - gemini"));
        assert!(script.contains("& 'C:\\Users\\me\\AppData\\Roaming\\npm\\gemini.cmd'"));
        assert!(script.contains("gemini exited with code"));
        assert!(!script.contains("ANTHROPIC_AUTH_TOKEN"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_claude_science_launcher_passes_the_serve_command() {
        let path = PathBuf::from(
            r"C:\Users\me\AppData\Local\Programs\ClaudeScience\claude-science.exe",
        );
        let script = windows_terminal_launch_script_with_handshake(
            &path,
            "Claude Science",
            &["serve"],
            None,
        );

        assert!(script.contains(
            "& 'C:\\Users\\me\\AppData\\Local\\Programs\\ClaudeScience\\claude-science.exe' 'serve'"
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_command_launcher_emits_one_command_with_handshake_markers() {
        let path = PathBuf::from(r"C:\Users\me\AppData\Roaming\npm\opencode.cmd");
        let handshake = CommandLaunchHandshake {
            id: "opencode-test".to_string(),
            started: PathBuf::from(r"C:\Temp\opencode.started"),
            exited: PathBuf::from(r"C:\Temp\opencode.exited"),
        };
        let script = windows_terminal_launch_script_with_handshake(
            &path,
            "OpenCode",
            &[],
            Some(&handshake),
        );

        assert_eq!(
            script
                .matches("& 'C:\\Users\\me\\AppData\\Roaming\\npm\\opencode.cmd'")
                .count(),
            1
        );
        assert!(script.contains("C:\\Temp\\opencode.started"));
        assert!(script.contains("C:\\Temp\\opencode.exited"));
        assert!(script.contains("$constCommandSucceeded"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_appx_candidates_include_claude_store_executable() {
        let candidates = windows_appx_executable_candidates_from_locations(
            vec![PathBuf::from(
                r"C:\Program Files\WindowsApps\Claude_1.18286.0.0_x64__pzs8sxrjxfjjc",
            )],
            &["app\\claude.exe", "claude.exe"],
        );

        assert_eq!(
            candidates.first(),
            Some(&PathBuf::from(
                r"C:\Program Files\WindowsApps\Claude_1.18286.0.0_x64__pzs8sxrjxfjjc\app\claude.exe"
            ))
        );
        assert!(candidates.iter().any(|path| path.ends_with("claude.exe")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_appx_executable_uses_appx_candidate_kind() {
        assert_eq!(
            path_candidate_kind(
                r"C:\Program Files\WindowsApps\Claude_1.18286.0.0_x64__pzs8sxrjxfjjc\app\claude.exe"
            ),
            "windows_appx"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn vscode_gui_executable_is_not_classified_as_terminal_command() {
        assert_eq!(
            selected_path_kind(
                "vscode",
                r"C:\CONST API test\Microsoft VS Code\Code.exe"
            ),
            "windows_exe"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn appx_manifest_parser_selects_application_by_executable() {
        let manifest = r#"
        <Package>
          <Applications>
            <Application Id="Other" Executable="app\other.exe" />
            <Application Id="Claude" Executable="app\Claude.exe" EntryPoint="Windows.FullTrustApplication" />
          </Applications>
        </Package>
        "#;

        assert_eq!(
            appx_manifest_application_id_for_executable(manifest, "app\\claude.exe").as_deref(),
            Some("Claude")
        );
    }

    #[test]
    fn additional_tools_use_their_official_cli_command_names() {
        assert_eq!(tool_program_commands("claude-science"), &["claude-science"]);
        assert_eq!(tool_program_commands("kimicode"), &["kimi"]);
        assert_eq!(tool_program_commands("mimocode"), &["mimo"]);
        assert_eq!(tool_program_commands("qwencode"), &["qwen"]);
        assert!(tool_program_commands("openscience").is_empty());
        assert_eq!(tool_program_commands("vibe-trading"), &["vibe-trading"]);
        assert_eq!(tool_program_commands("copilot"), &["copilot"]);
        assert_eq!(tool_program_commands("raven"), &["raven"]);
        assert_eq!(tool_program_commands("pi"), &["pi"]);
        assert_eq!(tool_program_commands("cline"), &["cline"]);
        assert_eq!(tool_program_commands("reasonix"), &["reasonix"]);
        assert_eq!(tool_program_commands("deepseek-harness"), &["dsh"]);
        assert_eq!(tool_program_launch_args("deepseek-harness"), &["web"]);
        assert_eq!(tool_program_commands("open-interpreter"), &["interpreter"]);
        assert_eq!(tool_program_commands("goose"), &["goose"]);
        assert_eq!(tool_program_commands("mistral-vibe"), &["vibe"]);
        assert_eq!(tool_program_commands("open-design"), &["od"]);

        #[cfg(target_os = "windows")]
        {
            assert_eq!(
                known_tool_process_names("claude-science"),
                &["claude-science.exe"]
            );
            assert_eq!(known_tool_process_names("kimicode"), &["kimi.exe"]);
            assert_eq!(known_tool_process_names("mimocode"), &["mimo.exe"]);
            assert_eq!(known_tool_process_names("qwencode"), &["qwen.exe"]);
            assert_eq!(known_tool_process_names("openscience"), &["ai4s-workbench.exe"]);
            assert_eq!(
                known_tool_process_names("vibe-trading"),
                &["vibe-trading.exe"]
            );
            assert_eq!(known_tool_process_names("copilot"), &["copilot.exe"]);
            assert_eq!(known_tool_process_names("raven"), &["raven.exe"]);
            assert_eq!(known_tool_process_names("pi"), &["pi.exe"]);
            assert_eq!(known_tool_process_names("cline"), &["cline.exe"]);
            assert_eq!(
                known_tool_process_names("reasonix"),
                &["Reasonix.exe", "reasonix.exe"]
            );
            assert_eq!(known_tool_process_names("deepseek-harness"), &["dsh.exe"]);
            assert_eq!(
                known_tool_process_names("open-interpreter"),
                &["interpreter.exe"]
            );
            assert_eq!(
                known_tool_process_names("goose"),
                &["Goose.exe", "goose.exe"]
            );
            assert_eq!(known_tool_process_names("mistral-vibe"), &["vibe.exe"]);
            assert_eq!(
                known_tool_process_names("open-design"),
                &[
                    "Open Design.exe",
                    "Open Design Beta.exe",
                    "Open Design Prerelease.exe",
                    "Open Design Preview.exe",
                    "od.exe",
                ]
            );
        }
    }

    #[test]
    fn every_configurable_tool_has_one_cross_platform_runtime_kill_manifest() {
        let tools = [
            "codex",
            "claude",
            "claude-desktop",
            "claude-science",
            "gemini",
            "copilot",
            "cline",
            "opencode",
            "openclaw",
            "goose",
            "raven",
            "reasonix",
            "deepseek-harness",
            "pi",
            "hermes",
            "vscode",
            "workbuddy",
            "kimicode",
            "mimocode",
            "qwencode",
            "openscience",
            "open-interpreter",
            "mistral-vibe",
            "open-design",
            "vibe-trading",
            "zcode",
        ];

        for tool in tools {
            let manifest = tool_runtime_kill_manifest(tool)
                .unwrap_or_else(|| panic!("missing runtime kill manifest for {tool}"));
            assert!(
                !manifest.commands.is_empty() || !manifest.windows_process_names.is_empty(),
                "{tool} has no Windows runtime owner"
            );
            assert!(
                !manifest.commands.is_empty() || !manifest.macos_process_names.is_empty(),
                "{tool} has no macOS runtime owner"
            );
            assert!(
                !manifest.commands.is_empty() || !manifest.linux_process_names.is_empty(),
                "{tool} has no Linux runtime owner"
            );
        }
        assert_eq!(tool_runtime_commands("codex"), &["codex"]);
        assert!(tool_program_commands("codex").is_empty());
        assert!(tool_runtime_kill_manifest("codex")
            .expect("Codex manifest")
            .extension_dir_prefixes
            .contains(&"openai.chatgpt-"));
    }

    #[test]
    fn hosted_runtime_command_line_requires_a_known_host_process_name() {
        let marker = if cfg!(target_os = "windows") {
            r"c:\users\me\.vscode\extensions\openai.chatgpt-1\bin\codex.js"
        } else {
            "/Users/me/.vscode/extensions/openai.chatgpt-1/bin/codex.js"
        };
        let known_host = normalized_process_name(if cfg!(target_os = "windows") {
            "node.exe"
        } else {
            "node"
        });
        let unrelated_host = normalized_process_name(if cfg!(target_os = "windows") {
            "notepad.exe"
        } else {
            "vim"
        });
        let scope = ToolProcessScope {
            executable_names: BTreeSet::from([known_host.clone()]),
            command_line_markers: BTreeSet::from([marker.to_string()]),
            ..Default::default()
        };
        let command_line = format!("node {marker} app-server");

        assert!(process_command_line_matches_scope_owner(
            &known_host,
            Some(&command_line),
            &scope,
        ));
        assert!(!process_command_line_matches_scope_owner(
            &unrelated_host,
            Some(&format!("vim {marker}")),
            &scope,
        ));
    }

    #[test]
    fn open_design_runtime_rejects_the_unix_system_od_utility() {
        assert!(!is_supported_tool_runtime_owner_path(
            "open-design",
            Path::new("/usr/bin/od"),
        ));
        assert!(!is_supported_tool_runtime_owner_path(
            "open-design",
            Path::new("/bin/od"),
        ));
        assert!(is_supported_tool_runtime_owner_path(
            "open-design",
            Path::new("/Users/me/.local/bin/od"),
        ));
        assert!(is_supported_tool_runtime_owner_path(
            "codex",
            Path::new("/usr/bin/od"),
        ));
    }

    #[test]
    fn npm_wrapper_parser_resolves_the_tool_specific_node_entry() {
        let temp = tempfile::tempdir().expect("temp dir");
        let bin = temp.path().join("bin");
        let entry = bin
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin")
            .join("codex.js");
        fs::create_dir_all(entry.parent().expect("entry parent")).expect("entry directory");
        fs::write(&entry, "// test entry").expect("entry file");
        let wrapper = bin.join("codex.cmd");
        let wrapper_body = if cfg!(target_os = "windows") {
            r#""%dp0%\node.exe" "%dp0%\node_modules\@openai\codex\bin\codex.js" %*"#
        } else {
            r#"exec node "$basedir/node_modules/@openai/codex/bin/codex.js" "$@""#
        };
        fs::write(&wrapper, wrapper_body).expect("wrapper");

        let entries = command_wrapper_node_module_entry_paths(&wrapper);
        assert_eq!(entries, vec![entry]);
        assert!(!command_runtime_canonical_is_specific(
            Path::new("/snap/bin/codex"),
            Path::new("/usr/bin/snap"),
        ));
        assert!(command_runtime_canonical_is_specific(
            Path::new("/usr/local/bin/codex"),
            Path::new("/usr/local/lib/node_modules/@openai/codex/bin/codex.js"),
        ));
    }

    #[test]
    fn deepseek_harness_web_identity_probe_rejects_unrelated_http_services() {
        let ready = concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n",
            "<html><head><script>window.__DSH_BOOT__ = {}</script>",
            "<link rel=\"manifest\" href=\"/manifest.webmanifest\" />",
            "<title>DeepSeek Harness</title></head></html>"
        );
        let auth_required = concat!(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: text/plain; charset=utf-8\r\n\r\n",
            "dsh web authentication required; reopen the URL printed by dsh web.\n"
        );
        assert_eq!(deepseek_harness_http_response_probe(ready), DeepSeekHarnessWebProbe::Ready);
        assert_eq!(
            deepseek_harness_http_response_probe(auth_required),
            DeepSeekHarnessWebProbe::ReadyRequiresBrowserAuth
        );
        for response in [
            "HTTP/1.1 200 OK\r\n\r\n<title>Other service</title>",
            "HTTP/1.1 503 Service Unavailable\r\n\r\n<title>DeepSeek Harness</title> manifest.webmanifest __DSH_BOOT__",
            "HTTP/1.1 401 Unauthorized\r\n\r\nUnauthorized",
        ] {
            assert_eq!(
                deepseek_harness_http_response_probe(response),
                DeepSeekHarnessWebProbe::PortOccupied
            );
        }
    }

    #[test]
    fn deepseek_harness_prefers_installed_command_over_saved_npx_cache() {
        let cached = "/home/me/.npm/_npx/old/node_modules/.bin/dsh";
        let installed = "/home/me/.local/bin/dsh";
        let candidates = vec![
            ToolProgramCandidate {
                path: cached.to_string(),
                kind: "command".to_string(),
                exists: true,
                selected: true,
                version: Some("0.1.0-rc.7".to_string()),
                ..Default::default()
            },
            ToolProgramCandidate {
                path: installed.to_string(),
                kind: "command".to_string(),
                exists: true,
                version: Some("0.1.5-rc.2".to_string()),
                ..Default::default()
            },
        ];
        assert_eq!(
            preferred_tool_program_path_for_saved_selection(
                "deepseek-harness",
                cached,
                &candidates,
            )
            .as_deref(),
            Some(installed)
        );
        assert!(preferred_tool_program_path_for_saved_selection(
            "deepseek-harness",
            installed,
            &candidates,
        )
        .is_none());
        assert!(compare_tool_program_candidates("deepseek-harness", &candidates[1], &candidates[0]).is_gt());
    }

    #[test]
    fn npm_shim_versions_drive_shared_tool_selection_before_file_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let installed = dir.path().join("npm");
        let cached = dir.path().join("npm/_npx/old/node_modules/.bin");
        for (bin, package, version, entry) in [
            (
                installed.join("dsh.cmd"),
                installed.join("node_modules/@deepseek-ai/dsh"),
                "0.1.5-rc.2",
                "%dp0%\\node_modules\\@deepseek-ai\\dsh\\lib\\bin.js",
            ),
            (
                cached.join("dsh.cmd"),
                dir.path().join("npm/_npx/old/node_modules/@deepseek-ai/dsh"),
                "0.1.0-rc.7",
                "%dp0%\\..\\@deepseek-ai\\dsh\\lib\\bin.js",
            ),
        ] {
            fs::create_dir_all(&bin.parent().expect("bin parent")).expect("create bin");
            fs::create_dir_all(&package).expect("create package");
            fs::write(&bin, format!("node \"{entry}\" %*\n")).expect("write shim");
            fs::write(package.join("package.json"), format!(r#"{{"version":"{version}"}}"#))
                .expect("write package version");
        }
        let mut candidates = Vec::new();
        push_candidate(
            &mut candidates,
            "deepseek-harness",
            cached.join("dsh.cmd").display().to_string(),
            "cached",
            "command".to_string(),
            true,
        );
        push_candidate(
            &mut candidates,
            "deepseek-harness",
            installed.join("dsh.cmd").display().to_string(),
            "installed",
            "command".to_string(),
            false,
        );
        assert_eq!(candidates[0].version.as_deref(), Some("0.1.0-rc.7"));
        assert_eq!(candidates[1].version.as_deref(), Some("0.1.5-rc.2"));
        assert_eq!(
            preferred_tool_program_path_for_saved_selection(
                "deepseek-harness",
                &candidates[0].path,
                &candidates,
            )
            .as_deref(),
            Some(candidates[1].path.as_str())
        );
        assert_eq!(
            select_tool_launch_candidate("deepseek-harness", &candidates).map(|value| &value.path),
            Some(&candidates[1].path)
        );
        let mut other_tool = candidates.clone();
        for candidate in &mut other_tool {
            candidate.path = candidate.path.replace("dsh.cmd", "claude.cmd");
        }
        assert_eq!(
            select_tool_launch_candidate("claude", &other_tool).map(|value| &value.path),
            Some(&other_tool[1].path)
        );
    }

    #[test]
    fn version_order_beats_modified_time_for_automatic_candidates() {
        let candidates = [
            ToolProgramCandidate {
                path: "/usr/local/bin/claude".to_string(),
                kind: "command".to_string(),
                exists: true,
                version: Some("2.1.9".to_string()),
                modified_at_unix: 300,
                ..Default::default()
            },
            ToolProgramCandidate {
                path: "/opt/bin/claude".to_string(),
                kind: "command".to_string(),
                exists: true,
                version: Some("2.1.10".to_string()),
                modified_at_unix: 100,
                ..Default::default()
            },
        ];
        assert_eq!(
            select_tool_launch_candidate("claude", &candidates).map(|candidate| &candidate.path),
            Some(&candidates[1].path)
        );
        assert!(compare_tool_program_versions("2.1.10", "2.1.9").unwrap().is_gt());
        assert!(compare_tool_program_versions("2.1.10", "2.1.10-rc.1").unwrap().is_gt());
        assert!(compare_tool_program_versions("2.1.10.2", "2.1.10.1").unwrap().is_gt());
    }

    #[test]
    fn deepseek_harness_locator_finds_the_official_npx_cache_shim() {
        with_temp_home(|home| {
            let bin = home
                .join(".npm")
                .join("_npx")
                .join("cached-package")
                .join("node_modules")
                .join(".bin");
            fs::create_dir_all(&bin).expect("create npx cache bin");
            #[cfg(target_os = "windows")]
            let shim = bin.join("dsh.cmd");
            #[cfg(not(target_os = "windows"))]
            let shim = bin.join("dsh");
            fs::write(&shim, "official dsh shim").expect("write dsh shim");

            let candidates = find_tool_command_paths_without_shell("deepseek-harness", "dsh");
            assert!(candidates.iter().any(|candidate| candidate == &shim));
            assert!(tool_program_located_without_process_scan("deepseek-harness")
                .expect("locate cached DeepSeek Harness"));
        });
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn appx_manifest_protocol_parser_supports_codex_and_chatgpt_brand_executables() {
        let codex_manifest = r#"
        <Package>
          <Applications>
            <Application Id="Other" Executable="app\other.exe" />
            <Application Id="App" Executable="app\Codex.exe">
              <Extensions>
                <uap:Extension Category="windows.protocol">
                  <uap:Protocol Name="codex" />
                </uap:Extension>
              </Extensions>
            </Application>
          </Applications>
        </Package>
        "#;
        let chatgpt_manifest = codex_manifest.replace("app\\Codex.exe", "app/ChatGPT.exe");
        let unrelated_chatgpt_manifest = codex_manifest
            .replace("app\\Codex.exe", "app/ChatGPT.exe")
            .replace("Name=\"codex\"", "Name=\"chatgpt\"");

        assert_eq!(
            appx_manifest_protocol_executable(codex_manifest, "codex"),
            Some("app\\Codex.exe".to_string())
        );
        assert_eq!(
            appx_manifest_protocol_executable(&chatgpt_manifest, "codex"),
            Some("app/ChatGPT.exe".to_string())
        );
        assert_eq!(
            appx_manifest_protocol_executable(&unrelated_chatgpt_manifest, "codex"),
            None
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn installed_claude_appx_is_discovered_when_present() {
        let install_locations = windows_appx_install_locations(&["Claude"]);
        if install_locations.is_empty() {
            return;
        }

        let candidates = known_tool_program_paths("claude-desktop");
        for install_location in install_locations {
            assert!(
                candidates.contains(&install_location.join("app\\claude.exe")),
                "missing Claude AppX executable for {}",
                install_location.display()
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn installed_chatgpt_appx_is_discovered_under_the_legacy_codex_package_name() {
        let install_locations = windows_appx_install_locations(&["OpenAI.Codex"]);
        if install_locations.is_empty() {
            return;
        }

        let candidates = known_tool_program_paths("codex");
        for install_location in install_locations {
            assert!(
                candidates.contains(&install_location.join("app\\chatgpt.exe")),
                "missing ChatGPT AppX executable for {}",
                install_location.display()
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn installed_codex_package_discovers_the_protocol_declared_executable() {
        let install_locations = windows_appx_install_locations(&["OpenAI.Codex"]);
        if install_locations.is_empty() {
            return;
        }
        let candidates = windows_appx_protocol_executable_candidates(&["OpenAI.Codex"], "codex");

        for install_location in install_locations {
            let manifest = fs::read_to_string(install_location.join("AppxManifest.xml"))
                .expect("installed Codex AppX manifest");
            let relative_executable = appx_manifest_protocol_executable(&manifest, "codex")
                .expect("codex protocol executable");
            assert!(candidates.contains(&install_location.join(relative_executable)));
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn installed_chatgpt_is_preferred_as_the_codex_desktop_target() {
        let mut candidates = known_tool_program_paths("codex")
            .into_iter()
            .filter(|path| path.exists())
            .map(|path| ToolProgramCandidate {
                path: path.display().to_string(),
                label: "auto".to_string(),
                kind: path_candidate_kind(&path.display().to_string()),
                exists: true,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        if !candidates
            .iter()
            .any(|candidate| is_chatgpt_desktop_path(Path::new(&candidate.path)))
        {
            return;
        }
        for candidate in &mut candidates {
            annotate_tool_program_candidate("codex", candidate);
        }

        let selected = select_tool_launch_candidate("codex", &candidates)
            .expect("installed ChatGPT desktop target");
        assert!(is_chatgpt_desktop_path(Path::new(&selected.path)));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn legacy_codex_windows_desktop_path_is_discovered() {
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA");
        let expected = local_app_data
            .join("Programs")
            .join("Codex")
            .join("Codex.exe");

        assert!(known_tool_program_paths("codex").contains(&expected));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn workbuddy_windows_install_and_process_names_are_discovered() {
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA");
        let expected = local_app_data
            .join("Programs")
            .join("WorkBuddy")
            .join("WorkBuddy.exe");

        assert!(known_tool_program_paths("workbuddy").contains(&expected));
        assert_eq!(known_tool_process_names("workbuddy"), &["WorkBuddy.exe"]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn additional_desktop_coding_tools_use_their_official_windows_paths() {
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA");

        let expected = [
            (
                "zcode",
                "Programs/ZCode/ZCode.exe",
                &["ZCode.exe"][..],
            ),
            (
                "reasonix",
                "Programs/Reasonix/Reasonix.exe",
                &["Reasonix.exe", "reasonix.exe"][..],
            ),
            (
                "goose",
                "Programs/Goose/Goose.exe",
                &["Goose.exe", "goose.exe"][..],
            ),
            (
                "open-design",
                "Programs/Open Design/Open Design.exe",
                &[
                    "Open Design.exe",
                    "Open Design Beta.exe",
                    "Open Design Prerelease.exe",
                    "Open Design Preview.exe",
                    "od.exe",
                ][..],
            ),
        ];

        for (tool, relative_path, process_names) in expected {
            assert!(
                known_tool_program_paths(tool).contains(&local_app_data.join(relative_path)),
                "missing {tool} Windows program candidate"
            );
            assert_eq!(known_tool_process_names(tool), process_names);
            if tool == "zcode" {
                assert!(tool_program_commands(tool).is_empty());
            } else {
                assert!(!tool_program_commands(tool).is_empty());
            }
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn claude_science_and_other_listed_windows_locations_extend_tool_candidates() {
        let local_app_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .expect("LOCALAPPDATA");

        assert!(known_tool_program_paths("claude-desktop")
            .contains(&local_app_data.join("AnthropicClaude/Claude.exe")));
        assert!(known_tool_program_paths("claude-science").contains(
            &local_app_data.join("Programs/ClaudeScience/claude-science.exe")
        ));
        assert!(known_tool_program_paths("opencode")
            .contains(&local_app_data.join("Programs/OpenCode Beta/OpenCode Beta.exe")));
        assert!(tool_specific_command_paths("openclaw", "openclaw")
            .contains(&local_app_data.join("OpenClaw/bin/openclaw.exe")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn workbuddy_macos_install_and_process_names_are_discovered() {
        let candidates = known_tool_program_paths("workbuddy");

        assert!(candidates.contains(&PathBuf::from("/Applications/WorkBuddy.app")));
        assert!(candidates.contains(&home_dir().join("Applications").join("WorkBuddy.app")));
        assert_eq!(known_tool_process_names("workbuddy"), &["WorkBuddy"]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn installed_chatgpt_appx_resolves_its_shell_launch_target() {
        let Some(install_location) = windows_appx_install_locations(&["OpenAI.Codex"])
            .into_iter()
            .next()
        else {
            return;
        };
        let executable = install_location.join("app\\chatgpt.exe");
        if !executable.exists() {
            return;
        }

        let target = windows_appx_shell_target_for_executable(&executable)
            .expect("ChatGPT AppX shell target");
        assert!(target.starts_with("shell:AppsFolder\\OpenAI.Codex_"));
        assert!(target.ends_with("!App"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn installed_claude_appx_shell_target_is_resolved_when_present() {
        let Some(install_location) = windows_appx_install_locations(&["Claude"])
            .into_iter()
            .next()
        else {
            return;
        };
        let executable = install_location.join("app\\claude.exe");
        if !executable.exists() {
            return;
        }

        let target = windows_appx_shell_target_for_executable(&executable)
            .expect("Claude AppX shell target");
        assert!(target.starts_with("shell:AppsFolder\\Claude_"));
        assert!(target.ends_with("!Claude"));
    }

    #[test]
    fn selected_terminal_command_is_respected_when_an_unselected_executable_exists() {
        let candidates = vec![
            ToolProgramCandidate {
                path: r"C:\Users\me\AppData\Roaming\npm\claude.cmd".to_string(),
                label: "claude".to_string(),
                kind: "command".to_string(),
                exists: true,
                selected: true,
                ..Default::default()
            },
            ToolProgramCandidate {
                path: r"C:\Users\me\bin\claude.exe".to_string(),
                label: "auto".to_string(),
                kind: "windows_exe".to_string(),
                exists: true,
                selected: false,
                ..Default::default()
            },
        ];

        assert!(is_launchable_tool_candidate("claude", &candidates[0]));
        let selected = select_tool_launch_candidate("claude", &candidates).expect("candidate");
        assert!(selected.path.ends_with("claude.cmd"));
    }

    #[test]
    fn automatic_windows_executable_has_priority_over_other_entry_types() {
        let candidates = vec![
            ToolProgramCandidate {
                path: r"C:\Users\me\AppData\Roaming\npm\claude.cmd".to_string(),
                label: "claude".to_string(),
                kind: "command".to_string(),
                exists: true,
                ..Default::default()
            },
            ToolProgramCandidate {
                path: r"C:\Users\me\bin\claude.exe".to_string(),
                label: "claude".to_string(),
                kind: "windows_exe".to_string(),
                exists: true,
                ..Default::default()
            },
        ];

        let selected = select_tool_launch_candidate("claude", &candidates).expect("candidate");
        assert!(selected.path.ends_with("claude.exe"));
        assert!(
            tool_program_candidate_priority("claude", &candidates[1])
                > tool_program_candidate_priority("claude", &candidates[0])
        );
    }

    #[test]
    fn desktop_tools_prefer_a_desktop_program_over_a_saved_command() {
        for (tool, command, executable) in [
            ("hermes", "hermes", "Hermes.exe"),
            ("opencode", "opencode", "OpenCode.exe"),
            ("openclaw", "openclaw", "OpenClaw.exe"),
            ("vscode", "code", "Code.exe"),
            ("workbuddy", "workbuddy", "WorkBuddy.exe"),
            ("reasonix", "reasonix", "Reasonix.exe"),
            ("goose", "goose", "Goose.exe"),
            ("open-design", "od", "Open Design.exe"),
        ] {
            let command_path = format!(r"C:\Users\me\bin\{command}.cmd");
            let desktop_path = format!(r"C:\Users\me\Programs\{tool}\{executable}");
            let candidates = vec![
                ToolProgramCandidate {
                    path: command_path.clone(),
                    label: command.to_string(),
                    kind: "command".to_string(),
                    exists: true,
                    selected: true,
                    ..Default::default()
                },
                ToolProgramCandidate {
                    path: desktop_path.clone(),
                    label: "desktop".to_string(),
                    kind: "windows_exe".to_string(),
                    exists: true,
                    ..Default::default()
                },
            ];

            let command_fallback = select_tool_launch_candidate(tool, &candidates[..1])
                .expect("command fallback");
            assert_eq!(command_fallback.path, command_path, "{tool}");

            let selected =
                select_tool_launch_candidate(tool, &candidates).expect("desktop candidate");
            assert_eq!(selected.path, desktop_path, "{tool}");
            assert_eq!(
                preferred_desktop_program_path_for_saved_selection(
                    tool,
                    &command_path,
                    &candidates,
                )
                .as_deref(),
                Some(desktop_path.as_str()),
                "{tool}"
            );
        }
    }

    #[test]
    fn non_owned_executables_never_outrank_a_tool_executable() {
        for path in [
            r"C:\Users\me\AppData\Local\hermes\venv\Scripts\python.exe",
            r"C:\Program Files\nodejs\node.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Tools\UnrelatedAgent.exe",
            "/usr/bin/python3.13",
            "/usr/bin/node",
            "/bin/bash",
        ] {
            let candidate = ToolProgramCandidate {
                path: path.to_string(),
                label: "saved runtime host".to_string(),
                kind: "windows_exe".to_string(),
                exists: true,
                selected: true,
                ..Default::default()
            };
            assert!(!is_tool_owned_program_path("hermes", Path::new(path)), "{path}");
            assert!(!is_launchable_tool_candidate("hermes", &candidate), "{path}");
            assert!(!is_recommended_tool_program_candidate("hermes", &candidate), "{path}");
        }

        let python_path =
            r"C:\Users\me\AppData\Local\hermes\hermes-agent\venv\Scripts\python.exe";
        let hermes_path = r"C:\Users\me\AppData\Local\hermes\hermes-agent\apps\desktop\release\win-unpacked\Hermes.exe";
        let candidates = vec![
            ToolProgramCandidate {
                path: python_path.to_string(),
                label: "selected".to_string(),
                kind: "windows_exe".to_string(),
                exists: true,
                selected: true,
                ..Default::default()
            },
            ToolProgramCandidate {
                path: hermes_path.to_string(),
                label: "auto".to_string(),
                kind: "windows_exe".to_string(),
                exists: true,
                ..Default::default()
            },
        ];

        let selected =
            select_tool_launch_candidate("hermes", &candidates).expect("Hermes executable");
        assert_eq!(selected.path, hermes_path);
        assert_eq!(
            preferred_tool_program_path_for_saved_selection(
                "hermes",
                python_path,
                &candidates,
            )
            .as_deref(),
            Some(hermes_path)
        );
    }

    #[test]
    fn every_supported_tool_uses_its_manifest_as_the_program_allowlist() {
        for tool in [
            "codex",
            "claude",
            "claude-desktop",
            "claude-science",
            "gemini",
            "copilot",
            "cline",
            "opencode",
            "openclaw",
            "goose",
            "raven",
            "reasonix",
            "deepseek-harness",
            "pi",
            "hermes",
            "vscode",
            "workbuddy",
            "kimicode",
            "mimocode",
            "qwencode",
            "openscience",
            "open-interpreter",
            "mistral-vibe",
            "open-design",
            "vibe-trading",
            "zcode",
        ] {
            let manifest = tool_runtime_kill_manifest(tool).expect("supported tool manifest");
            let executable_name = manifest
                .windows_process_names
                .first()
                .copied()
                .or_else(|| manifest.commands.first().map(|command| *command))
                .expect("program identity");
            let allowed_path = format!(r"C:\Tools\{executable_name}");
            assert!(
                is_tool_owned_program_path(tool, Path::new(&allowed_path)),
                "{tool}: {allowed_path}"
            );
            for rejected in ["python.exe", "node.exe", "UnrelatedAgent.exe"] {
                let rejected_path = format!(r"C:\Tools\{rejected}");
                assert!(
                    !is_tool_owned_program_path(tool, Path::new(&rejected_path)),
                    "{tool}: {rejected_path}"
                );
            }
        }
        assert!(!is_tool_owned_program_path(
            "unknown-tool",
            Path::new(r"C:\Tools\unknown-tool.exe")
        ));
    }

    #[test]
    fn every_known_tool_program_path_matches_the_program_allowlist() {
        for tool in [
            "codex",
            "claude-desktop",
            "claude-science",
            "hermes",
            "opencode",
            "openclaw",
            "vscode",
            "workbuddy",
            "reasonix",
            "goose",
            "open-design",
            "zcode",
        ] {
            for path in known_tool_program_paths(tool) {
                assert!(
                    is_tool_owned_program_path(tool, &path),
                    "{tool}: {}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn invalid_saved_runtime_host_is_repaired_for_terminal_tools_too() {
        let python_path = r"C:\Tools\agent\venv\Scripts\python.exe";
        let claude_path = r"C:\Users\me\AppData\Roaming\npm\claude.cmd";
        let candidates = vec![
            ToolProgramCandidate {
                path: python_path.to_string(),
                label: "selected".to_string(),
                kind: "windows_exe".to_string(),
                exists: true,
                selected: true,
                ..Default::default()
            },
            ToolProgramCandidate {
                path: claude_path.to_string(),
                label: "claude".to_string(),
                kind: "command".to_string(),
                exists: true,
                ..Default::default()
            },
        ];

        assert_eq!(
            preferred_tool_program_path_for_saved_selection(
                "claude",
                python_path,
                &candidates,
            )
            .as_deref(),
            Some(claude_path)
        );
    }

    #[test]
    fn manually_selecting_a_non_owned_program_is_rejected_before_it_is_saved() {
        with_temp_home(|_| {
            let error = save_tool_program_path(
                "hermes",
                r"C:\Users\me\AppData\Local\hermes\venv\Scripts\python.exe".to_string(),
            )
            .expect_err("Python must not be persisted as Hermes");
            let message = error.to_string();
            assert!(
                message.contains("白名单") || message.contains("executable allowlist"),
                "unexpected localized error: {message}"
            );
        });
    }

    #[test]
    fn hermes_windows_desktop_executable_is_recommended() {
        let candidate = ToolProgramCandidate {
            path: r"C:\Users\me\AppData\Local\Programs\hermes\Hermes.exe".to_string(),
            label: "Hermes Desktop".to_string(),
            kind: "windows_exe".to_string(),
            exists: true,
            ..Default::default()
        };

        assert!(is_recommended_tool_program_candidate(
            "hermes",
            &candidate
        ));
    }

    #[test]
    fn desktop_tool_replaces_a_stale_saved_program_but_keeps_a_valid_desktop_choice() {
        let stale_path = r"C:\Old Apps\Microsoft VS Code\Code.exe";
        let current_path = r"C:\Users\me\AppData\Local\Programs\Microsoft VS Code\Code.exe";
        let stale_candidates = vec![
            ToolProgramCandidate {
                path: stale_path.to_string(),
                label: "old desktop".to_string(),
                kind: "windows_exe".to_string(),
                exists: false,
                selected: true,
                ..Default::default()
            },
            ToolProgramCandidate {
                path: current_path.to_string(),
                label: "current desktop".to_string(),
                kind: "windows_exe".to_string(),
                exists: true,
                ..Default::default()
            },
        ];

        assert_eq!(
            preferred_desktop_program_path_for_saved_selection(
                "vscode",
                stale_path,
                &stale_candidates,
            )
            .as_deref(),
            Some(current_path)
        );

        let valid_candidates = vec![ToolProgramCandidate {
            path: current_path.to_string(),
            label: "current desktop".to_string(),
            kind: "windows_exe".to_string(),
            exists: true,
            selected: true,
            ..Default::default()
        }];
        assert!(preferred_desktop_program_path_for_saved_selection(
            "vscode",
            current_path,
            &valid_candidates,
        )
        .is_none());
    }

    #[test]
    fn terminal_tools_keep_command_launch_semantics() {
        for (tool, command) in [
            ("claude", "claude"),
            ("claude-science", "claude-science"),
            ("gemini", "gemini"),
            ("copilot", "copilot"),
            ("raven", "raven"),
            ("pi", "pi"),
            ("cline", "cline"),
            ("deepseek-harness", "dsh"),
            ("open-interpreter", "interpreter"),
            ("mistral-vibe", "vibe"),
            ("kimicode", "kimi"),
            ("mimocode", "mimo"),
            ("qwencode", "qwen"),
            ("vibe-trading", "vibe-trading"),
        ] {
            let command_path = format!(r"C:\Users\me\bin\{command}.cmd");
            let candidates = vec![
                ToolProgramCandidate {
                    path: command_path.clone(),
                    label: command.to_string(),
                    kind: "command".to_string(),
                    exists: true,
                    selected: true,
                    ..Default::default()
                },
                ToolProgramCandidate {
                    path: format!(r"C:\Users\me\bin\{command}.exe"),
                    label: "native command".to_string(),
                    kind: "windows_exe".to_string(),
                    exists: true,
                    ..Default::default()
                },
            ];

            let selected =
                select_tool_launch_candidate(tool, &candidates).expect("command candidate");
            assert_eq!(selected.path, command_path, "{tool}");
            assert!(preferred_desktop_program_path_for_saved_selection(
                tool,
                &command_path,
                &candidates,
            )
            .is_none());
        }
    }

    #[test]
    fn desktop_program_preference_classifies_every_supported_tool() {
        for tool in [
            "openscience",
            "codex",
            "claude-desktop",
            "hermes",
            "opencode",
            "openclaw",
            "vscode",
            "workbuddy",
            "reasonix",
            "goose",
            "open-design",
            "zcode",
        ] {
            assert!(tool_prefers_desktop_program(tool), "{tool}");
        }
        for tool in [
            "claude",
            "claude-science",
            "gemini",
            "copilot",
            "raven",
            "pi",
            "cline",
            "deepseek-harness",
            "open-interpreter",
            "mistral-vibe",
            "kimicode",
            "mimocode",
            "qwencode",
            "vibe-trading",
        ] {
            assert!(!tool_prefers_desktop_program(tool), "{tool}");
        }
    }

    #[test]
    fn claude_desktop_prefers_appx_when_no_program_was_explicitly_selected() {
        let candidates = vec![
            ToolProgramCandidate {
                path: r"C:\Users\me\AppData\Local\Programs\Claude\Claude.exe".to_string(),
                label: "legacy executable".to_string(),
                kind: "windows_exe".to_string(),
                exists: true,
                ..Default::default()
            },
            ToolProgramCandidate {
                path: r"C:\Program Files\WindowsApps\Claude_2.0.0.0_x64__publisher\app\claude.exe"
                    .to_string(),
                label: "MSIX".to_string(),
                kind: "windows_appx".to_string(),
                exists: true,
                ..Default::default()
            },
        ];

        let selected =
            select_tool_launch_candidate("claude-desktop", &candidates).expect("candidate");
        assert_eq!(selected.kind, "windows_appx");
    }

    #[test]
    fn explicit_desktop_program_selection_still_overrides_automatic_appx_preference() {
        let candidates = vec![
            ToolProgramCandidate {
                path: r"C:\Users\me\AppData\Local\Programs\Claude\Claude.exe".to_string(),
                label: "selected executable".to_string(),
                kind: "windows_exe".to_string(),
                exists: true,
                selected: true,
                ..Default::default()
            },
            ToolProgramCandidate {
                path: r"C:\Program Files\WindowsApps\Claude_2.0.0.0_x64__publisher\app\claude.exe"
                    .to_string(),
                label: "MSIX".to_string(),
                kind: "windows_appx".to_string(),
                exists: true,
                ..Default::default()
            },
        ];

        let selected =
            select_tool_launch_candidate("claude-desktop", &candidates).expect("candidate");
        assert_eq!(selected.kind, "windows_exe");
    }

    #[test]
    fn tool_program_candidate_dedup_respects_platform_path_case_rules() {
        let mut candidates = Vec::new();
        push_candidate(
            &mut candidates,
            "opencode",
            r"C:\Users\me\AppData\Local\OpenCode\OpenCode.exe".to_string(),
            "auto",
            "windows_exe".to_string(),
            false,
        );
        push_candidate(
            &mut candidates,
            "opencode",
            r"c:/users/me/appdata/local/opencode/opencode.exe".to_string(),
            "auto",
            "windows_exe".to_string(),
            false,
        );

        assert_eq!(candidates.len(), if cfg!(windows) { 1 } else { 2 });
    }

    #[test]
    fn tool_program_candidate_includes_file_modified_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = dir.path().join("sample-tool");
        fs::write(&program, b"program").expect("program");
        let mut candidates = Vec::new();

        push_candidate(
            &mut candidates,
            "sample-tool",
            program.display().to_string(),
            "auto",
            "path".to_string(),
            false,
        );

        assert!(candidates[0].modified_at_unix > 0);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn locator_keeps_a_valid_candidate_when_running_process_enrichment_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = dir.path().join("ChatGPT.exe");
        fs::write(&program, b"fixture").expect("program fixture");
        let candidate = ToolProgramCandidate {
            path: program.display().to_string(),
            kind: "windows_exe".to_string(),
            exists: true,
            ..Default::default()
        };

        assert_eq!(
            locator_running_tool_paths(
                "codex",
                std::slice::from_ref(&candidate),
                Err(anyhow!("process enrichment failed")),
            )
            .expect("existing candidate keeps locator usable"),
            Vec::<PathBuf>::new()
        );
        assert!(locator_running_tool_paths(
            "codex",
            &[],
            Err(anyhow!("process discovery is required")),
        )
        .is_err());
    }

    #[test]
    fn terminal_tool_launch_allows_selected_command() {
        let candidates = vec![ToolProgramCandidate {
            path: r"C:\Users\me\AppData\Roaming\npm\gemini.cmd".to_string(),
            label: "gemini".to_string(),
            kind: "command".to_string(),
            exists: true,
            selected: true,
            ..Default::default()
        }];

        let selected = select_tool_launch_candidate("gemini", &candidates).expect("candidate");
        assert!(selected.path.ends_with("gemini.cmd"));
    }

    #[test]
    fn chatgpt_launch_rejects_codex_cli_command_shim() {
        let candidates = vec![ToolProgramCandidate {
            path: r"C:\Users\me\AppData\Roaming\npm\codex.cmd".to_string(),
            label: "codex".to_string(),
            kind: "command".to_string(),
            exists: true,
            selected: true,
            ..Default::default()
        }];

        assert!(select_tool_launch_candidate("codex", &candidates).is_none());
    }

    #[test]
    fn codex_launch_accepts_the_legacy_desktop_app_as_a_fallback() {
        let candidates = vec![ToolProgramCandidate {
            path: "/Applications/Codex.app".to_string(),
            label: "legacy Codex".to_string(),
            kind: "mac_app".to_string(),
            exists: true,
            ..Default::default()
        }];

        let selected = select_tool_launch_candidate("codex", &candidates).expect("legacy app");
        assert_eq!(selected.path, "/Applications/Codex.app");
    }

    #[test]
    fn codex_automatic_launch_prefers_chatgpt_over_legacy_codex() {
        let candidates = vec![
            ToolProgramCandidate {
                path: r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0\app\Codex.exe"
                    .to_string(),
                label: "legacy Codex".to_string(),
                kind: "windows_appx".to_string(),
                exists: true,
                modified_at_unix: 200,
                ..Default::default()
            },
            ToolProgramCandidate {
                path: r"C:\Users\me\AppData\Local\Programs\ChatGPT\ChatGPT.exe"
                    .to_string(),
                label: "current ChatGPT".to_string(),
                kind: "windows_exe".to_string(),
                exists: true,
                modified_at_unix: 100,
                ..Default::default()
            },
        ];

        let selected = select_tool_launch_candidate("codex", &candidates).expect("current app");
        assert!(selected.path.ends_with("ChatGPT.exe"));
    }

    #[test]
    fn codex_desktop_path_guard_rejects_cli_and_embedded_sidecars() {
        assert!(is_supported_codex_desktop_path(Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0\app\Codex.exe"
        )));
        assert!(!is_supported_codex_desktop_path(Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0.0\app\resources\codex.exe"
        )));
        assert!(!is_supported_codex_desktop_path(Path::new(
            r"C:\Users\me\AppData\Local\OpenAI\Codex\bin\hash\codex.exe"
        )));
    }

    #[test]
    fn explicit_legacy_codex_selection_still_overrides_automatic_chatgpt_preference() {
        let candidates = vec![
            ToolProgramCandidate {
                path: "/Applications/Codex.app".to_string(),
                label: "selected legacy Codex".to_string(),
                kind: "mac_app".to_string(),
                exists: true,
                selected: true,
                ..Default::default()
            },
            ToolProgramCandidate {
                path: "/Applications/ChatGPT.app".to_string(),
                label: "current ChatGPT".to_string(),
                kind: "mac_app".to_string(),
                exists: true,
                ..Default::default()
            },
        ];

        let selected = select_tool_launch_candidate("codex", &candidates).expect("selected app");
        assert_eq!(selected.path, "/Applications/Codex.app");
    }

    #[test]
    fn codex_process_matching_includes_session_writers() {
        with_temp_home(|_| {
            let names = tool_process_match_names("codex").expect("ChatGPT process names");
            let codex_name = if cfg!(target_os = "windows") { "codex.exe" } else { "codex" };
            assert!(names.contains(&normalized_process_name(codex_name)));
            let chatgpt_name = if cfg!(target_os = "windows") {
                "ChatGPT.exe"
            } else if cfg!(target_os = "macos") {
                "ChatGPT"
            } else {
                "chatgpt"
            };
            assert!(names.contains(&normalized_process_name(chatgpt_name)));
        });
    }

    #[test]
    fn codex_program_selection_rejects_codex_plus_plus() {
        with_temp_home(|_| {
            let err = save_tool_program_path("codex", "/Applications/Codex++.app".to_string())
                .expect_err("Codex++ must not be accepted as Codex");
            assert!(err.to_string().contains("Codex++"));
        });
    }

    #[test]
    fn codex_program_selection_accepts_the_legacy_desktop_app() {
        with_temp_home(|_| {
            let location = save_tool_program_path("codex", "/Applications/Codex.app".to_string())
                .expect("legacy Codex desktop app");
            assert_eq!(location.selected_path, "/Applications/Codex.app");
        });
    }

    #[test]
    fn hermes_setup_app_is_launchable_but_not_recommended() {
        let dir = tempfile::tempdir().expect("tempdir");
        let setup = dir.path().join("Hermes.app");
        fs::create_dir_all(setup.join("Contents/MacOS")).expect("setup app");
        fs::write(setup.join("Contents/MacOS/Hermes-Setup"), b"").expect("setup executable");

        assert!(is_hermes_setup_app_path(&setup));
        let mut candidate = ToolProgramCandidate {
            path: setup.display().to_string(),
            label: "selected".to_string(),
            kind: "mac_app".to_string(),
            exists: true,
            selected: true,
            ..Default::default()
        };
        annotate_tool_program_candidate("hermes", &mut candidate);

        assert!(candidate.launchable);
        assert!(!candidate.recommended);
        assert_eq!(candidate.label, "Hermes installer");
        assert!(candidate.reason.contains("installation"));
    }

    #[test]
    fn hermes_desktop_app_is_recommended() {
        let dir = tempfile::tempdir().expect("tempdir");
        let desktop = dir.path().join("release/mac-arm64/Hermes.app");
        fs::create_dir_all(desktop.join("Contents/MacOS")).expect("desktop app");
        fs::write(desktop.join("Contents/MacOS/Hermes"), b"").expect("desktop executable");

        let mut candidate = ToolProgramCandidate {
            path: desktop.display().to_string(),
            label: "auto".to_string(),
            kind: "mac_app".to_string(),
            exists: true,
            selected: false,
            ..Default::default()
        };
        annotate_tool_program_candidate("hermes", &mut candidate);

        assert!(candidate.launchable);
        assert!(candidate.recommended);
        assert_eq!(candidate.label, "Hermes Desktop");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_app_main_executable_uses_bundle_executable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = dir.path().join("Hermes Agent.app");
        fs::create_dir_all(app.join("Contents/MacOS")).expect("app contents");
        fs::write(
            app.join("Contents/Info.plist"),
            r#"
            <plist>
              <dict>
                <key>CFBundleExecutable</key>
                <string>Hermes</string>
              </dict>
            </plist>
            "#,
        )
        .expect("info plist");
        fs::write(app.join("Contents/MacOS/Hermes"), b"").expect("executable");

        assert_eq!(
            macos_app_main_executable_path(&app).as_deref(),
            Some(app.join("Contents/MacOS/Hermes").as_path())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_app_process_match_ignores_codex_helpers() {
        let executable = PathBuf::from("/Applications/ChatGPT.app/Contents/MacOS/ChatGPT");

        assert!(process_command_line_matches_executable(
            "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT --flag",
            &executable
        ));
        assert!(!process_command_line_matches_executable(
            "/Applications/ChatGPT.app/Contents/Resources/codex app-server --analytics-default-enabled",
            &executable
        ));
        assert!(!process_command_line_matches_executable(
            "/Applications/ChatGPT.app/Contents/Frameworks/ChatGPT Framework.framework/Versions/149.0.7827.197/Helpers/ChatGPT (Service).app/Contents/MacOS/ChatGPT (Service) --type=gpu-process",
            &executable
        ));
        assert!(!process_command_line_matches_executable(
            "/Applications/Codex.app/Contents/Frameworks/Codex Framework.framework/Versions/149.0.7827.197/Helpers/browser_crashpad_handler --database=/Users/me/Library/Application Support/Codex/Crashpad",
            &executable
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_codex_command_detection_ignores_app_server() {
        let app_server =
            "codex            /Applications/Codex.app/Contents/Resources/codex app-server";
        let cli = "codex            /opt/homebrew/bin/codex exec";
        let pid_app_server =
            "484 codex            /Applications/Codex.app/Contents/Resources/codex app-server";
        let pid_cli = "999 codex            /opt/homebrew/bin/codex exec";

        assert_eq!(
            codex_ucomm_process_command_line(app_server),
            Some("/Applications/Codex.app/Contents/Resources/codex app-server")
        );
        assert!(is_codex_app_server_command_line(
            "/Applications/Codex.app/Contents/Resources/codex app-server"
        ));
        assert!(!is_codex_app_server_command_line(
            "/opt/homebrew/bin/codex exec"
        ));
        assert_eq!(
            codex_ucomm_process_command_line(cli),
            Some("/opt/homebrew/bin/codex exec")
        );
        assert_eq!(
            codex_pid_and_command_line(pid_app_server),
            Some((
                484,
                "/Applications/Codex.app/Contents/Resources/codex app-server"
            ))
        );
        assert_eq!(
            codex_pid_and_command_line(pid_cli),
            Some((999, "/opt/homebrew/bin/codex exec"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_codex_app_server_match_is_scoped_to_selected_app_bundle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let official = dir.path().join("ChatGPT.app");
        let other = dir.path().join("Codex++.app");
        fs::create_dir_all(official.join("Contents/Resources")).expect("official dirs");
        fs::create_dir_all(other.join("Contents/Resources")).expect("other dirs");
        fs::write(official.join("Contents/Resources/codex"), b"").expect("official codex");
        fs::write(other.join("Contents/Resources/codex"), b"").expect("other codex");

        let official_command = format!(
            "{} app-server --analytics-default-enabled",
            official.join("Contents/Resources/codex").display()
        );
        let other_command = format!(
            "{} app-server --analytics-default-enabled",
            other.join("Contents/Resources/codex").display()
        );

        assert!(codex_app_server_command_line_matches_app(
            &official_command,
            &official
        ));
        assert!(!codex_app_server_command_line_matches_app(
            &other_command,
            &official
        ));
        assert!(!codex_app_server_command_line_matches_app(
            "/opt/homebrew/bin/codex exec",
            &official
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_process_id_match_uses_exact_ucomm() {
        assert_eq!(
            macos_process_id_for_name("99996 Codex", "Codex"),
            Some(99996)
        );
        assert_eq!(
            macos_process_id_for_name("105 Codex (Service)", "Codex"),
            None
        );
        assert_eq!(macos_process_id_for_name("484 codex", "Codex"), None);
    }

    #[test]
    fn codex_launch_is_blocked_when_codex_plus_plus_is_running() {
        let err = ensure_tool_launch_not_claimed_by_external_launcher("codex", true)
            .expect_err("Codex++ should block Codex launch");
        assert!(err.to_string().contains("Codex++"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_helper_processes_use_hidden_console_flag() {
        assert_eq!(windows_hidden_console_flag(), 0x08000000);
        assert_eq!(windows_visible_terminal_flag(), 0x00000010);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn detached_windows_launch_proxy_preserves_process_arguments() {
        assert_eq!(windows_quote_process_argument(OsStr::new("plain")), "plain");
        assert_eq!(
            windows_quote_process_argument(OsStr::new(r"C:\Program Files\Tool\tool.exe")),
            r#""C:\Program Files\Tool\tool.exe""#
        );
        assert_eq!(windows_quote_process_argument(OsStr::new("")), r#""""#);
        assert_eq!(
            windows_quote_process_argument(OsStr::new(r#"say "hello""#)),
            r#""say \"hello\"""#
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_process_identity_and_install_scope_checks_are_stable() {
        let first = ToolProcessIdentity {
            pid: 4242,
            executable_path: r"C:\Program Files\ChatGPT\ChatGPT.exe".to_string(),
            start_identity: "win-filetime:01da000000000001".to_string(),
        };
        let same_first = ToolProcessIdentity {
            pid: 4242,
            executable_path: r"c:\program files\chatgpt\chatgpt.exe".to_string(),
            start_identity: first.start_identity.clone(),
        };
        let replaced_first = ToolProcessIdentity {
            start_identity: "win-filetime:01da000000000002".to_string(),
            ..same_first.clone()
        };
        let scope = ToolProcessScope {
            executable_names: BTreeSet::from(["chatgpt.exe".to_string()]),
            exact_executable_paths: BTreeSet::from([normalized_windows_process_path(Path::new(
                &first.executable_path,
            ))]),
            install_roots: BTreeSet::from([normalized_windows_process_path(Path::new(
                r"C:\Program Files\ChatGPT",
            ))]),
            ..Default::default()
        };
        let target_probe = WindowsProcessProbe {
            identity: first.clone(),
            package_family_name: None,
            command_line: None,
        };
        let unrelated_probe = WindowsProcessProbe {
            identity: ToolProcessIdentity {
                pid: 4343,
                executable_path: r"C:\Users\me\.vscode\extensions\codex.exe".to_string(),
                start_identity: "win-filetime:01da000000000003".to_string(),
            },
            package_family_name: None,
            command_line: None,
        };

        assert!(windows_process_identity_matches(&first, &same_first));
        assert!(!windows_process_identity_matches(&first, &replaced_first));
        assert!(scope.matches_windows_probe(&target_probe));
        assert!(!scope.matches_windows_probe(&unrelated_probe));

        let extension_root = Path::new(
            r"C:\Users\me\.vscode\extensions\openai.chatgpt-26.818.31338-win32-x64",
        );
        let extension_probe = WindowsProcessProbe {
            identity: ToolProcessIdentity {
                pid: 4444,
                executable_path: extension_root
                    .join(r"bin\windows-x86_64\codex.exe")
                    .display()
                    .to_string(),
                start_identity: "win-filetime:01da000000000004".to_string(),
            },
            package_family_name: None,
            command_line: Some("codex app-server".to_string()),
        };
        let mut extension_scope = scope.clone();
        insert_windows_tool_process_scope_root(&mut extension_scope, extension_root);
        assert!(extension_scope.matches_windows_probe(&extension_probe));
        let hosted_extension_probe = WindowsProcessProbe {
            identity: ToolProcessIdentity {
                pid: 4494,
                executable_path: r"C:\Program Files\nodejs\node.exe".to_string(),
                start_identity: "win-filetime:01da000000000006".to_string(),
            },
            package_family_name: None,
            command_line: Some(
                extension_root
                    .join(r"out\extension.js")
                    .display()
                    .to_string(),
            ),
        };
        assert!(extension_scope.matches_windows_probe(&hosted_extension_probe));

        let cli_entry = normalized_windows_process_path(Path::new(
            r"C:\Users\me\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js",
        ));
        let hosted_cli_probe = WindowsProcessProbe {
            identity: ToolProcessIdentity {
                pid: 4545,
                executable_path: r"C:\Program Files\nodejs\node.exe".to_string(),
                start_identity: "win-filetime:01da000000000005".to_string(),
            },
            package_family_name: None,
            command_line: Some(format!(r#"node.exe "{cli_entry}""#)),
        };
        let hosted_cli_scope = ToolProcessScope {
            executable_names: BTreeSet::from(["node.exe".to_string()]),
            command_line_markers: BTreeSet::from([cli_entry]),
            ..Default::default()
        };
        assert!(hosted_cli_scope.matches_windows_probe(&hosted_cli_probe));
        assert_eq!(
            windows_tool_install_root(
                "codex",
                Path::new(r"C:\Program Files\OpenAI ChatGPT\ChatGPT.exe"),
            ),
            Some(PathBuf::from(r"C:\Program Files\OpenAI ChatGPT"))
        );
        assert_eq!(
            windows_tool_install_root("codex", Path::new(r"C:\Program Files\ChatGPT.exe")),
            None,
            "a shared Program Files directory must never become the tool scope"
        );
        assert_eq!(
            windows_descendant_process_ids(
                &BTreeSet::from([10]),
                &[
                    WindowsProcessEntry {
                        pid: 10,
                        parent_pid: 1,
                        name: "root.exe".to_string(),
                    },
                    WindowsProcessEntry {
                        pid: 11,
                        parent_pid: 10,
                        name: "helper.exe".to_string(),
                    },
                    WindowsProcessEntry {
                        pid: 12,
                        parent_pid: 11,
                        name: "worker.exe".to_string(),
                    },
                    WindowsProcessEntry {
                        pid: 20,
                        parent_pid: 1,
                        name: "unrelated.exe".to_string(),
                    },
                ],
            ),
            BTreeSet::from([10, 11, 12])
        );
    }

    #[test]
    fn codex_process_filter_excludes_external_launcher_in_every_snapshot() {
        let snapshot = ToolProcessSnapshot::new(vec![
            ToolProcessIdentity {
                pid: 1,
                executable_path: "/apps/Codex/ChatGPT.exe".into(),
                start_identity: "a".into(),
            },
            ToolProcessIdentity {
                pid: 2,
                executable_path: "/apps/Codex++/codex.exe".into(),
                start_identity: "b".into(),
            },
            ToolProcessIdentity {
                pid: 3,
                executable_path: "/apps/codexplusplus/helper.exe".into(),
                start_identity: "c".into(),
            },
        ]);
        let filtered = filter_tool_process_snapshot("codex", snapshot.clone());
        assert_eq!(
            filtered.processes.iter().map(|process| process.pid).collect::<Vec<_>>(),
            vec![1],
        );
        assert_eq!(filter_tool_process_snapshot("gemini", snapshot.clone()), snapshot);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn remaining_process_check_waits_for_late_exit_without_killing_new_processes() {
        with_temp_home(|home| {
            let program = copied_named_helper_program(home);
            let scope = ToolProcessScope {
                executable_names: BTreeSet::from([normalized_process_name(
                    program.file_name().unwrap().to_str().unwrap(),
                )]),
                exact_executable_paths: BTreeSet::from([normalized_windows_process_path(&program)]),
                ..Default::default()
            };
            let ticket = create_tool_config_operation("codex", ToolConfigAction::Apply, false)
                .unwrap();
            let operation = start_tool_config_operation(
                &ticket.operation_id, "codex", ToolConfigAction::Apply, false,
            ).unwrap();
            let mut child = spawn_named_helper(&program, "hang");
            let snapshot = windows_tool_process_snapshot_for_scope("codex", &scope).unwrap();
            assert!(snapshot.process_ids().contains(&child.pid()));

            // Simulate the app's late helper finishing its own shutdown. The production
            // wait must not issue termination to any process outside the confirmed set.
            let shutdown = thread::spawn(move || {
                thread::sleep(Duration::from_millis(500));
                assert!(child.is_running(), "passive wait must not kill the late helper");
                child.terminate();
            });
            assert!(wait_for_tool_processes_closed("codex", &snapshot, &operation).unwrap());
            shutdown.join().unwrap();

            let mut replacement = spawn_named_helper(&program, "hang");
            assert!(!wait_for_tool_processes_closed("codex", &snapshot, &operation).unwrap());
            assert!(replacement.is_running(), "a new persistent instance must not be killed");
            assert!(!home.join(".codex/config.toml").exists());
            finish_tool_config_operation(&operation, true);
        });
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_force_kill_revalidates_and_terminates_a_real_process() {
        let target_directory = tempfile::tempdir().expect("target process fixture directory");
        let unrelated_directory =
            tempfile::tempdir().expect("unrelated process fixture directory");
        let program = copied_named_helper_program(target_directory.path());
        let unrelated_program = unrelated_directory.path().join(
            program
                .file_name()
                .expect("target process fixture file name"),
        );
        fs::copy(&program, &unrelated_program).expect("copy unrelated same-name helper");
        let process_name = program
            .file_name()
            .and_then(|value| value.to_str())
            .map(normalized_process_name)
            .expect("helper process name");
        let mut child = spawn_named_helper(&program, "hang");
        let mut unrelated = spawn_named_helper(&unrelated_program, "hang");
        let scope = ToolProcessScope {
            executable_names: BTreeSet::from([process_name]),
            exact_executable_paths: BTreeSet::from([normalized_windows_process_path(&program)]),
            install_roots: BTreeSet::from([normalized_windows_process_path(
                target_directory.path(),
            )]),
            ..Default::default()
        };
        let snapshot = windows_process_snapshot(&scope).expect("capture scoped helper process");

        assert!(
            snapshot
                .processes
                .iter()
                .any(|identity| identity.pid == child.pid())
        );
        assert!(
            snapshot
                .processes
                .iter()
                .all(|identity| identity.pid != unrelated.pid()),
            "same-name helper outside the selected install root must not be authorized"
        );
        terminate_authorized_process_trees("native-process-fixture", &snapshot)
            .expect("revalidate and terminate native process identity");
        assert!(!child.is_running(), "authorized helper must be terminated");
        assert!(
            unrelated.is_running(),
            "unrelated same-name helper must remain running"
        );
    }

    #[test]
    fn external_launcher_guard_does_not_block_other_tools() {
        ensure_tool_launch_not_claimed_by_external_launcher("hermes", true)
            .expect("Hermes launch should not be blocked by Codex++");
        ensure_tool_launch_not_claimed_by_external_launcher("codex", false)
            .expect("Codex launch should proceed when Codex++ is not running");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn first_abs_path_line_skips_login_shell_noise() {
        assert_eq!(
            first_abs_path_line("welcome\n/Users/me/.nvm/versions/node/v24/bin/codex\n"),
            Some("/Users/me/.nvm/versions/node/v24/bin/codex")
        );
        assert_eq!(first_abs_path_line("welcome\nbye\n"), None);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn unix_shell_command_lookup_does_not_enable_interactive_job_control() {
        assert_eq!(SHELL_COMMAND_LOOKUP_FLAG, "-lc");
        assert!(!SHELL_COMMAND_LOOKUP_FLAG.contains('i'));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_codex_candidates_include_chatgpt_before_legacy_codex() {
        let app_candidates = known_tool_program_paths("codex");
        let chatgpt = app_candidates
            .iter()
            .position(|path| path.ends_with("ChatGPT.app"))
            .expect("ChatGPT app candidate");
        let legacy_codex = app_candidates
            .iter()
            .position(|path| path.ends_with("Codex.app"))
            .expect("legacy Codex app candidate");
        assert!(chatgpt < legacy_codex);
        assert!(tool_program_commands("codex").is_empty());
    }
