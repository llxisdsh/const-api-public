    pub(super) fn with_temp_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = test_home_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = set_test_home_override(Some(dir.path().to_path_buf()));
        clear_test_tool_runtime_registry();
        let output = f(dir.path());
        clear_test_tool_runtime_registry();
        set_test_home_override(previous);
        output
    }

    #[test]
    fn test_home_runtime_detection_ignores_real_running_tools() {
        with_temp_home(|_| {
            assert!(
                !tool_program_running("codex").expect("running check"),
                "test config operations must not observe the real Codex.app process"
            );
        });
    }

    #[test]
    fn tool_process_wait_stops_at_its_deadline() {
        with_temp_home(|_| {
            set_test_tool_runtime("codex", true);

            let error = wait_for_tool_program_state_until("codex", false, Instant::now(), "关闭")
                .expect_err("running process must time out");

            assert!(error.to_string().contains("did not exit within 5 seconds"));
            assert!(error.to_string().contains("without writing configuration"));
        });
    }

    #[test]
    fn runtime_start_after_stale_ui_check_requires_confirmation_without_writing() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", false);
            assert!(!tool_program_running("gemini").expect("initial UI check"));
            set_test_tool_runtime("gemini", true);

            let outcome = gemini_confirmation_operation(&confirmation_request(
                "operation-stale-state",
                "apply",
                None,
                false,
                false,
            ))
            .expect("prepare config operation");

            assert!(matches!(
                outcome,
                ToolConfigWriteOutcome::ConfirmationRequired { running: true, .. }
            ));
            assert!(!home.join(".gemini/.env").exists());
        });
    }

    #[test]
    fn direct_unconfirmed_backend_operation_never_writes_while_tool_runs() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", true);

            let outcome = gemini_confirmation_operation(&confirmation_request(
                "operation-direct-unconfirmed",
                "apply",
                None,
                false,
                false,
            ))
            .expect("prepare config operation");

            assert!(matches!(
                outcome,
                ToolConfigWriteOutcome::ConfirmationRequired { running: true, .. }
            ));
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(runtime.running);
            assert_eq!(runtime.close_calls, 0);
            assert!(!home.join(".gemini/.env").exists());
        });
    }

    #[test]
    fn direct_forged_confirmation_boolean_cannot_close_or_write() {
        let command_source = include_str!("../../main.rs");
        assert!(!command_source.contains("confirmed: bool"));
        assert!(!command_source.contains("confirmed,"));
    }

    #[derive(Clone, Debug)]
    struct TestToolConfigOperationRequest {
        tool: String,
        action: String,
        protocol: Option<String>,
        restart: bool,
        context_digest: String,
        confirmation_token: Option<String>,
    }

    fn confirmation_request(
        operation_id: &str,
        action: &str,
        confirmation_token: Option<String>,
        confirmation_always_required: bool,
        restart: bool,
    ) -> TestToolConfigOperationRequest {
        let _ = (operation_id, confirmation_always_required);
        TestToolConfigOperationRequest {
            tool: "gemini".to_string(),
            action: action.to_string(),
            protocol: Some("gemini_native".to_string()),
            restart,
            context_digest: "test-context".to_string(),
            confirmation_token,
        }
    }

    fn test_atomic_operation<F, L>(
        tool: &str,
        action: ToolConfigAction,
        protocol: Option<&str>,
        context_digest: &str,
        confirmation_token: Option<&str>,
        restart: bool,
        write: F,
        launch: L,
    ) -> Result<ToolConfigWriteOutcome>
    where
        F: FnOnce() -> Result<ToolApplyResult>,
        L: FnOnce() -> Result<(String, PathBuf)>,
    {
        let ticket = create_tool_config_operation(tool, action, restart)?;
        let control = start_tool_config_operation(&ticket.operation_id, tool, action, restart)?;
        let outcome = execute_atomic_tool_config_operation(
            &control,
            protocol,
            context_digest,
            confirmation_token,
            write,
            |_| launch(),
        );
        finish_tool_config_operation(&control, outcome.is_ok());
        outcome
    }

    fn gemini_confirmation_operation(
        request: &TestToolConfigOperationRequest,
    ) -> Result<ToolConfigWriteOutcome> {
        let action = ToolConfigAction::parse(&request.action)?;
        test_atomic_operation(
            &request.tool,
            action,
            request.protocol.as_deref(),
            &request.context_digest,
            request.confirmation_token.as_deref(),
            request.restart,
            || match action {
                ToolConfigAction::Apply => apply_gemini_config("http://127.0.0.1:38787", "sk-test"),
                ToolConfigAction::Remove => {
                    remove_gemini_config("http://127.0.0.1:38787", "sk-test")
                }
            },
            || launch_tool_program("gemini"),
        )
    }

    fn required_confirmation_token(outcome: ToolConfigWriteOutcome) -> String {
        let ToolConfigWriteOutcome::ConfirmationRequired {
            confirmation_token, ..
        } = outcome
        else {
            panic!("operation should require confirmation");
        };
        confirmation_token
    }

    fn required_force_continue_token(outcome: ToolConfigWriteOutcome) -> (String, String) {
        let ToolConfigWriteOutcome::ManualCloseRequired {
            confirmation_token,
            reason,
            ..
        } = outcome
        else {
            panic!("operation should require a manual close decision");
        };
        (confirmation_token, reason)
    }

    #[test]
    fn running_tool_requires_an_unpredictable_server_confirmation_token() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", true);

            let token = required_confirmation_token(
                gemini_confirmation_operation(&confirmation_request(
                    "operation-token-issued",
                    "apply",
                    None,
                    false,
                    false,
                ))
                .expect("prepare operation"),
            );

            assert!(
                token.len() >= 40,
                "token must contain at least 256 random bits"
            );
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(runtime.running);
            assert_eq!(runtime.close_calls, 0);
            assert!(!home.join(".gemini/.env").exists());
        });
    }

    #[test]
    fn random_confirmation_token_is_rejected_without_close_or_write() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", true);
            let error = gemini_confirmation_operation(&confirmation_request(
                "operation-random-token",
                "apply",
                Some("not-a-server-token".to_string()),
                false,
                false,
            ))
            .expect_err("random token must be rejected");

            assert!(
                error
                    .to_string()
                    .contains("TOOL_CONFIG_CONFIRMATION_INVALID"),
                "{error:#}"
            );
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(runtime.running);
            assert_eq!(runtime.close_calls, 0);
            assert!(!home.join(".gemini/.env").exists());
        });
    }

    #[test]
    fn confirmation_token_is_bound_to_action_restart_and_context() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", true);
            let token = required_confirmation_token(
                gemini_confirmation_operation(&confirmation_request(
                    "operation-bound-token",
                    "apply",
                    None,
                    false,
                    false,
                ))
                .expect("prepare operation"),
            );
            let mut mismatched =
                confirmation_request("operation-bound-token", "remove", Some(token), true, true);
            mismatched.context_digest = "different-context".to_string();

            let error = gemini_confirmation_operation(&mismatched)
                .expect_err("mismatched token binding must be rejected");

            assert!(
                error
                    .to_string()
                    .contains("TOOL_CONFIG_CONFIRMATION_MISMATCH"),
                "{error:#}"
            );
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(runtime.running);
            assert_eq!(runtime.close_calls, 0);
            assert!(!home.join(".gemini/.env").exists());
        });
    }

    #[test]
    fn expired_confirmation_token_is_stably_rejected() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", true);
            let token = required_confirmation_token(
                gemini_confirmation_operation(&confirmation_request(
                    "operation-expired-token",
                    "apply",
                    None,
                    false,
                    false,
                ))
                .expect("prepare operation"),
            );
            expire_test_confirmation_token(&token);
            let request = confirmation_request(
                "operation-expired-token",
                "apply",
                Some(token),
                false,
                false,
            );

            for _ in 0..2 {
                let error = gemini_confirmation_operation(&request)
                    .expect_err("expired token must be rejected");
                assert!(
                    error
                        .to_string()
                        .contains("TOOL_CONFIG_CONFIRMATION_EXPIRED"),
                    "{error:#}"
                );
            }
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(runtime.running);
            assert_eq!(runtime.close_calls, 0);
            assert!(!home.join(".gemini/.env").exists());
        });
    }

    #[test]
    fn consumed_confirmation_token_replay_is_stably_rejected() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", true);
            let token = required_confirmation_token(
                gemini_confirmation_operation(&confirmation_request(
                    "operation-replayed-token",
                    "apply",
                    None,
                    false,
                    false,
                ))
                .expect("prepare operation"),
            );
            let request = confirmation_request(
                "operation-replayed-token",
                "apply",
                Some(token),
                false,
                false,
            );
            assert!(matches!(
                gemini_confirmation_operation(&request).expect("consume token"),
                ToolConfigWriteOutcome::Completed(_)
            ));
            let configured = fs::read(home.join(".gemini/.env")).expect("configured bytes");
            set_test_tool_runtime("gemini", true);

            for _ in 0..2 {
                let error = gemini_confirmation_operation(&request)
                    .expect_err("replayed token must be rejected");
                assert!(
                    error
                        .to_string()
                        .contains("TOOL_CONFIG_CONFIRMATION_REPLAYED"),
                    "{error:#}"
                );
            }
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(runtime.running);
            assert_eq!(runtime.close_calls, 0);
            assert_eq!(
                fs::read(home.join(".gemini/.env")).expect("configured bytes after replay"),
                configured
            );
        });
    }

    #[test]
    fn confirmed_process_that_already_exited_proceeds_without_another_prompt() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", true);
            let token = required_confirmation_token(
                gemini_confirmation_operation(&confirmation_request(
                    "operation-state-change",
                    "apply",
                    None,
                    false,
                    false,
                ))
                .expect("prepare operation"),
            );
            set_test_tool_runtime("gemini", false);

            let outcome = gemini_confirmation_operation(&confirmation_request(
                "operation-state-change",
                "apply",
                Some(token),
                false,
                false,
            ))
            .expect("an already-exited process must not cause another prompt");

            assert!(matches!(outcome, ToolConfigWriteOutcome::Completed(_)));
            assert!(home.join(".gemini/.env").exists());
        });
    }

    #[test]
    fn confirmation_token_never_closes_a_replacement_process() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", true);
            let token = required_confirmation_token(
                gemini_confirmation_operation(&confirmation_request(
                    "operation-process-replacement",
                    "apply",
                    None,
                    false,
                    true,
                ))
                .expect("prepare operation for process A"),
            );
            let process_a = test_tool_process_snapshot("gemini").expect("process A snapshot");

            replace_test_tool_process("gemini");
            let process_b = test_tool_process_snapshot("gemini").expect("process B snapshot");
            assert_ne!(
                process_a, process_b,
                "replacement must retain a new identity"
            );

            let manual = gemini_confirmation_operation(&confirmation_request(
                "operation-process-replacement",
                "apply",
                Some(token),
                false,
                true,
            ))
            .expect("a replacement process must require an explicit manual choice");
            let (continue_token, reason) = required_force_continue_token(manual);

            assert!(!continue_token.is_empty());
            assert!(reason.contains("TOOL_CONFIG_CONFIRMATION_PROCESS_REPLACED"), "{reason}");
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(runtime.running, "replacement process B must remain running");
            assert_eq!(runtime.close_calls, 0, "process B must not be closed");
            assert_eq!(
                runtime.launch_calls, 0,
                "no replacement process may be started"
            );
            assert!(
                !home.join(".gemini/.env").exists(),
                "configuration must not be written"
            );
        });
    }

    struct NamedHelperChild(std::process::Child);

    impl NamedHelperChild {
        fn pid(&self) -> u32 {
            self.0.id()
        }

        fn terminate(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }

        fn is_running(&mut self) -> bool {
            self.0.try_wait().expect("inspect helper child").is_none()
        }
    }

    impl Drop for NamedHelperChild {
        fn drop(&mut self) {
            self.terminate();
        }
    }

    fn copied_named_helper_program(home: &Path) -> PathBuf {
        let generated_name =
            random_tool_config_secret("const-api-review-helper").expect("test helper name");
        // Linux exposes `comm` through ps, which is limited to TASK_COMM_LEN
        // (15 visible bytes). Keep this real-process fixture within that limit
        // so the snapshot exercises identity replacement instead of failing on
        // a kernel-truncated test-only executable name.
        #[cfg(target_os = "linux")]
        let name = generated_name.chars().take(15).collect::<String>();
        #[cfg(not(target_os = "linux"))]
        let name = generated_name;
        let file_name = if cfg!(target_os = "windows") {
            format!("{name}.exe")
        } else {
            name
        };
        let path = home.join(file_name);
        fs::copy(
            std::env::current_exe().expect("current test executable"),
            &path,
        )
        .expect("copy named helper executable");
        path
    }

    fn copied_tool_named_helper_program(home: &Path, tool_name: &str) -> PathBuf {
        let file_name = if cfg!(target_os = "windows") {
            format!("{tool_name}.exe")
        } else {
            tool_name.to_string()
        };
        let path = home.join(file_name);
        fs::copy(
            std::env::current_exe().expect("current test executable"),
            &path,
        )
        .expect("copy tool-named helper executable");
        path
    }

    fn spawn_named_helper(program: &Path, mode: &str) -> NamedHelperChild {
        let mut command = Command::new(program);
        command
            .arg("tool_config::tests::helper_command_fixture_process")
            .arg("--exact")
            .arg("--nocapture")
            .env("CONST_API_TEST_HELPER_MODE", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        NamedHelperChild(command.spawn().expect("spawn named helper"))
    }

    fn wait_for_real_process_identity(
        match_names: &HashSet<String>,
        pid: u32,
    ) -> ToolProcessIdentity {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = platform_process_snapshot(match_names).expect("capture helper process");
            if let Some(identity) = snapshot.into_iter().find(|process| process.pid == pid) {
                return identity;
            }
            assert!(
                Instant::now() < deadline,
                "named helper PID {pid} never appeared in the process snapshot"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn authorized_process_identity_that_already_exited_is_safe_to_skip() {
        let authorized = ToolProcessIdentity {
            pid: 41001,
            executable_path: "/tmp/tool".to_string(),
            start_identity: "start-a".to_string(),
        };

        let is_current = authorized_process_identity_matches_snapshot(
            "opencode",
            &ToolProcessSnapshot::default(),
            &authorized,
        )
        .expect("an already exited authorized process is not a replacement");

        assert!(!is_current);
    }

    #[test]
    fn authorized_process_identity_rejects_reused_pid() {
        let authorized = ToolProcessIdentity {
            pid: 41002,
            executable_path: "/tmp/tool".to_string(),
            start_identity: "start-a".to_string(),
        };
        let current = ToolProcessSnapshot::new(vec![ToolProcessIdentity {
            pid: authorized.pid,
            executable_path: authorized.executable_path.clone(),
            start_identity: "start-b".to_string(),
        }]);

        let error = authorized_process_identity_matches_snapshot("opencode", &current, &authorized)
            .expect_err("a reused PID must invalidate the confirmation");

        assert!(
            error
                .to_string()
                .contains("TOOL_CONFIG_CONFIRMATION_PROCESS_REPLACED"),
            "{error:#}"
        );
    }

    #[test]
    fn confirmed_real_process_replacement_is_not_killed_written_or_started() {
        with_temp_home(|home| {
            use std::sync::atomic::AtomicUsize;

            const TOOL: &str = "gemini";
            let program = copied_tool_named_helper_program(home, TOOL);
            save_tool_program_path(TOOL, program.display().to_string())
                .expect("select named helper program");
            let process_name = program
                .file_name()
                .and_then(|value| value.to_str())
                .map(normalized_process_name)
                .expect("named helper process name");
            let match_names = HashSet::from([process_name]);
            set_test_process_match_names(TOOL, match_names.clone());

            let process_a = Arc::new(Mutex::new(Some(spawn_named_helper(&program, "hang"))));
            let process_a_pid = process_a
                .lock()
                .expect("process A lock")
                .as_ref()
                .expect("process A")
                .pid();
            let process_a_identity = wait_for_real_process_identity(&match_names, process_a_pid);
            assert_eq!(process_a_identity.pid, process_a_pid);
            assert!(!process_a_identity.executable_path.trim().is_empty());
            assert!(!process_a_identity.start_identity.trim().is_empty());

            let prepare_ticket = create_tool_config_operation(TOOL, ToolConfigAction::Apply, true)
                .expect("create confirmation operation");
            let prepare_operation = start_tool_config_operation(
                &prepare_ticket.operation_id,
                TOOL,
                ToolConfigAction::Apply,
                true,
            )
            .expect("start confirmation operation");
            let prepared = execute_atomic_tool_config_operation(
                &prepare_operation,
                None,
                "real-process-replacement-context",
                None,
                || panic!("confirmation preparation must not write"),
                |_| panic!("confirmation preparation must not launch"),
            )
            .expect("prepare confirmation capability");
            finish_tool_config_operation(&prepare_operation, true);
            let token = required_confirmation_token(prepared);

            let process_b = Arc::new(Mutex::new(None::<NamedHelperChild>));
            let process_a_for_hook = Arc::clone(&process_a);
            let process_b_for_hook = Arc::clone(&process_b);
            let program_for_hook = program.clone();
            let match_names_for_hook = match_names.clone();
            set_test_before_authorized_close_hook(move || {
                let mut process_a = process_a_for_hook.lock().expect("process A hook lock");
                process_a.as_mut().expect("process A hook").terminate();
                process_a.take();

                let replacement = spawn_named_helper(&program_for_hook, "hang");
                let replacement_pid = replacement.pid();
                wait_for_real_process_identity(&match_names_for_hook, replacement_pid);
                *process_b_for_hook.lock().expect("process B hook lock") = Some(replacement);
            });

            let ticket =
                create_confirmed_tool_config_operation(TOOL, ToolConfigAction::Apply, true)
                    .expect("create confirmed operation");
            let operation = start_tool_config_operation(
                &ticket.operation_id,
                TOOL,
                ToolConfigAction::Apply,
                true,
            )
            .expect("start confirmed operation");
            let writes = AtomicUsize::new(0);
            let launches = AtomicUsize::new(0);
            let outcome = execute_atomic_tool_config_operation(
                &operation,
                None,
                "real-process-replacement-context",
                Some(&token),
                || {
                    writes.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolApplyBuilder::default().finish(TOOL))
                },
                |_| {
                    launches.fetch_add(1, Ordering::SeqCst);
                    Ok(("started".to_string(), program.clone()))
                },
            );
            finish_tool_config_operation(&operation, outcome.is_ok());

            let (_, reason) = required_force_continue_token(
                outcome.expect("replacement must require an explicit manual decision"),
            );
            assert_eq!(reason, "TOOL_CONFIG_PROCESS_STILL_RUNNING");
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            assert_eq!(launches.load(Ordering::SeqCst), 0);
            let terminal = wait_for_tool_config_operation_terminal(
                &ticket.operation_id,
                Duration::from_millis(50),
            )
            .expect("replacement terminal state");
            let repeated = wait_for_tool_config_operation_terminal(
                &ticket.operation_id,
                Duration::from_millis(50),
            )
            .expect("repeated replacement terminal state");
            assert!(terminal.terminal);
            assert_eq!(terminal.state, "completed");
            assert_eq!(repeated.state, terminal.state);
            assert_eq!(repeated.terminal, terminal.terminal);
            let mut process_b = process_b.lock().expect("process B lock");
            assert!(
                process_b.as_mut().expect("process B").is_running(),
                "replacement process B must remain alive"
            );
        });
    }

    #[test]
    fn helper_command_hang_is_killed_and_reaped_at_the_timeout() {
        let mut command = helper_command_fixture("hang");
        let started = Instant::now();

        let error = run_helper_command_with_timeout(
            &mut command,
            Duration::from_millis(80),
            "test hanging helper",
        )
        .expect_err("hanging helper must time out");

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "helper timeout must interrupt the child promptly"
        );
        assert!(
            error.to_string().contains("TOOL_CONFIG_HELPER_TIMEOUT"),
            "{error:#}"
        );
    }

    fn wait_for_fixture_marker(path: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(value) = fs::read_to_string(path) {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "fixture marker missing: {path:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "windows")]
    fn test_process_pid_running(pid: u32) -> bool {
        use windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE;

        let Some(process) = windows_open_process(pid, PROCESS_SYNCHRONIZE)
            .expect("open test process for liveness check")
        else {
            return false;
        };
        !windows_process_handle_exited(process.raw()).expect("read test process liveness")
    }

    #[cfg(not(target_os = "windows"))]
    fn test_process_pid_running(pid: u32) -> bool {
        let executable = std::env::current_exe().expect("current test executable");
        let process_name = executable
            .file_name()
            .and_then(|value| value.to_str())
            .map(normalized_process_name)
            .expect("test process name");
        platform_process_snapshot(&HashSet::from([process_name]))
            .expect("inspect test processes")
            .iter()
            .any(|identity| identity.pid == pid)
    }

    fn force_cleanup_test_process_tree(pid: u32) {
        #[cfg(target_os = "windows")]
        {
            let mut command = helper_command("taskkill");
            command.args(["/PID", &pid.to_string(), "/T", "/F"]);
            let _ = run_helper_command_with_timeout(
                &mut command,
                Duration::from_secs(2),
                "cleanup stubborn helper tree",
            );
        }
        #[cfg(not(target_os = "windows"))]
        {
            let mut command = helper_command("kill");
            command.args(["-KILL", &pid.to_string()]);
            let _ = run_helper_command_with_timeout(
                &mut command,
                Duration::from_secs(2),
                "cleanup stubborn helper tree",
            );
        }
    }

    #[test]
    fn helper_timeout_kills_stubborn_grandchild_and_bounds_pipe_reader_join() {
        let marker_dir = tempfile::tempdir().expect("helper marker directory");
        let grandchild_marker = marker_dir.path().join("grandchild.pid");
        let mut command = helper_command_fixture("stubborn-tree");
        command.env("CONST_API_TEST_HELPER_MARKER_DIR", marker_dir.path());
        let (result_tx, result_rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            let result = run_helper_command_with_timeout(
                &mut command,
                Duration::from_secs(1),
                "test stubborn helper tree",
            );
            let _ = result_tx.send(result);
        });

        let grandchild_pid = wait_for_fixture_marker(&grandchild_marker)
            .trim()
            .parse::<u32>()
            .expect("grandchild PID");
        let first_result = result_rx.recv_timeout(Duration::from_secs(5));
        let completed_before_cleanup = first_result.is_ok();
        let survived_timeout = test_process_pid_running(grandchild_pid);
        if survived_timeout {
            force_cleanup_test_process_tree(grandchild_pid);
        }
        let result = match first_result {
            Ok(result) => result,
            Err(_) => result_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("helper runner must unblock after test cleanup"),
        };

        assert!(
            completed_before_cleanup,
            "helper runner blocked on inherited pipe; survived_timeout={survived_timeout}; result={result:?}"
        );
        assert!(
            !survived_timeout,
            "helper timeout left grandchild PID {grandchild_pid} running"
        );
        let error = result.expect_err("stubborn helper tree must time out");
        assert!(
            error.to_string().contains("TOOL_CONFIG_HELPER_TIMEOUT"),
            "{error:#}"
        );
    }

    #[test]
    fn process_probe_nonzero_exit_is_an_error_not_not_running() {
        let mut command = helper_command_fixture("nonzero");

        let error = run_process_probe_command_with_timeout(
            &mut command,
            Duration::from_secs(2),
            "test failing probe",
        )
        .expect_err("nonzero probe must fail closed");

        assert!(
            error
                .to_string()
                .contains("TOOL_CONFIG_PROCESS_PROBE_FAILED"),
            "{error:#}"
        );
    }

    #[test]
    fn process_probe_start_failure_is_an_error_not_not_running() {
        let missing_program = std::env::temp_dir().join(
            random_tool_config_secret("const-api-missing-process-probe")
                .expect("test missing process name"),
        );
        let mut command = Command::new(&missing_program);

        let error = run_process_probe_command_with_timeout(
            &mut command,
            Duration::from_secs(2),
            "test missing probe",
        )
        .expect_err("probe startup failure must fail closed");

        assert!(
            error
                .to_string()
                .contains("TOOL_CONFIG_HELPER_START_FAILED"),
            "{error:#}"
        );
    }

    #[test]
    fn close_helper_distinguishes_an_allowed_not_found_exit_from_real_errors() {
        let mut command = helper_command_fixture("nonzero");

        let matched = run_close_helper_command_with_timeout(
            &mut command,
            Duration::from_secs(2),
            "test no-match close",
            &[23],
        )
        .expect("explicit no-match exit must be ignored");

        assert!(!matched);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_native_process_probe_captures_path_and_kernel_start_identity() {
        let probe = windows_process_probe_by_pid(std::process::id())
            .expect("native Windows process probe")
            .expect("current process identity");
        let identity = probe.identity;

        assert!(!identity.executable_path.trim().is_empty());
        assert!(identity.start_identity.starts_with("win-filetime:"));
        assert_eq!(identity.start_identity.len(), "win-filetime:".len() + 16);
        assert!(
            probe
                .command_line
                .as_deref()
                .is_some_and(|line| !line.trim().is_empty()),
            "native Windows probe should expose the hosted-runtime command line"
        );
    }

    #[test]
    fn unix_process_snapshot_row_preserves_an_executable_path_with_spaces() {
        let identity = parse_unix_process_snapshot_row(
            "424 Thu Jul 16 07:30:01 2026 /Applications/Hermes Agent.app/Contents/MacOS/Hermes Agent",
        )
        .expect("parse process identity");

        assert_eq!(identity.pid, 424);
        assert_eq!(
            identity.executable_path,
            "/Applications/Hermes Agent.app/Contents/MacOS/Hermes Agent"
        );
        assert_eq!(identity.start_identity, "Thu Jul 16 07:30:01 2026");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_snapshot_skips_unmatched_process_before_proc_identity_probe() {
        let names = HashSet::from([normalized_process_name("const-api-target")]);
        let snapshot = parse_unix_process_snapshot(
            b"1 Thu Jul 16 07:30:01 2026 /sbin/init\n",
            &names,
        )
        .expect("an unrelated process must not require readable /proc identity");

        assert!(snapshot.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_snapshot_keeps_matching_identity_probe_fail_closed() {
        let names = HashSet::from([normalized_process_name("const-api-target")]);
        let error = parse_unix_process_snapshot(
            b"4294967295 Thu Jul 16 07:30:01 2026 /tmp/const-api-target\n",
            &names,
        )
        .expect_err("a matching process without readable identity must fail closed");

        assert!(
            error
                .to_string()
                .contains("TOOL_CONFIG_PROCESS_IDENTITY_UNAVAILABLE"),
            "{error:#}"
        );
    }

    #[test]
    fn app_bundle_owner_scope_accepts_helpers_and_rejects_same_named_other_apps() {
        let names = HashSet::from([normalized_process_name("ChatGPT")]);
        let paths = HashSet::from([String::from(
            "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT",
        )]);

        assert!(process_path_matches_owner(
            Path::new("/Applications/ChatGPT.app/Contents/Resources/codex"),
            &normalized_process_name("codex"),
            &names,
            &paths,
            true,
        ));
        assert!(!process_path_matches_owner(
            Path::new("/Users/me/Other/ChatGPT"),
            &normalized_process_name("ChatGPT"),
            &names,
            &paths,
            true,
        ));

        let cli_names = HashSet::from([normalized_process_name("opencode")]);
        let cli_paths = HashSet::from([String::from("/usr/local/bin/opencode")]);
        assert!(process_path_matches_owner(
            Path::new("/usr/local/bin/opencode"),
            &normalized_process_name("opencode"),
            &cli_names,
            &cli_paths,
            false,
        ));
        assert!(!process_path_matches_owner(
            Path::new("/opt/other/opencode"),
            &normalized_process_name("opencode"),
            &cli_names,
            &cli_paths,
            false,
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_process_snapshot_exact_path_distinguishes_electron_apps() {
        let names = HashSet::from([normalized_process_name("WorkBuddy")]);
        let paths = HashSet::from([String::from(
            "/Applications/WorkBuddy.app/Contents/MacOS/Electron",
        )]);
        let snapshot = parse_unix_process_snapshot_with_exact_paths(
            b"424 Thu Jul 16 07:30:01 2026 /Applications/WorkBuddy.app/Contents/MacOS/Electron\n\
              425 Thu Jul 16 07:31:01 2026 /Applications/Antigravity.app/Contents/MacOS/Electron\n",
            &names,
            &paths,
        )
        .expect("parse Electron process snapshot");

        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].pid, 424);
        assert_eq!(
            snapshot[0].executable_path,
            "/Applications/WorkBuddy.app/Contents/MacOS/Electron"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_process_snapshot_keeps_bundle_helpers_but_rejects_same_named_other_apps() {
        let names = HashSet::from([normalized_process_name("ChatGPT")]);
        let paths = HashSet::from([String::from(
            "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT",
        )]);
        let snapshot = parse_unix_process_snapshot_with_exact_paths(
            b"424 Thu Jul 16 07:30:01 2026 /Applications/ChatGPT.app/Contents/MacOS/ChatGPT\n\
              425 Thu Jul 16 07:31:01 2026 /Applications/ChatGPT.app/Contents/Resources/codex\n\
              426 Thu Jul 16 07:32:01 2026 /Users/me/Other/ChatGPT\n",
            &names,
            &paths,
        )
        .expect("parse selected app process snapshot");

        assert_eq!(
            snapshot.iter().map(|process| process.pid).collect::<Vec<_>>(),
            vec![424, 425]
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn malformed_unix_process_snapshot_is_an_error_not_not_running() {
        let names = HashSet::from([normalized_process_name("gemini")]);
        let error = parse_unix_process_snapshot(b"malformed", &names)
            .expect_err("malformed process snapshot must fail closed");
        assert!(
            error
                .to_string()
                .contains("TOOL_CONFIG_PROCESS_SNAPSHOT_PARSE_FAILED"),
            "{error:#}"
        );
    }

    #[test]
    fn process_probe_failure_requires_a_manual_choice_without_writing() {
        with_temp_home(|_| {
            use std::sync::atomic::AtomicUsize;

            set_test_tool_runtime("gemini", true);
            set_test_tool_runtime_probe_failure("gemini");
            let writes = AtomicUsize::new(0);
            let launches = AtomicUsize::new(0);

            let outcome = test_atomic_operation(
                "gemini",
                ToolConfigAction::Apply,
                Some("gemini_native"),
                "probe-failure-context",
                None,
                true,
                || {
                    writes.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolApplyBuilder::default().finish("gemini"))
                },
                || {
                    launches.fetch_add(1, Ordering::SeqCst);
                    Ok(("started".to_string(), PathBuf::from("test://started")))
                },
            )
            .expect("an unavailable process scan must request a manual choice");

            let (continue_token, reason) = required_force_continue_token(outcome);
            assert!(!continue_token.is_empty());
            assert_eq!(reason, "TOOL_CONFIG_PROCESS_PROBE_FAILED");
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(runtime.running);
            assert_eq!(runtime.close_calls, 0);
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            assert_eq!(launches.load(Ordering::SeqCst), 0);

            let continued = test_atomic_operation(
                "gemini",
                ToolConfigAction::Apply,
                Some("gemini_native"),
                "probe-failure-context",
                Some(&continue_token),
                true,
                || {
                    writes.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolApplyBuilder::default().finish("gemini"))
                },
                || {
                    launches.fetch_add(1, Ordering::SeqCst);
                    Ok(("started".to_string(), PathBuf::from("test://started")))
                },
            )
            .expect("explicit manual continuation must skip the failed probe");
            assert!(matches!(continued, ToolConfigWriteOutcome::Completed(_)));
            assert_eq!(writes.load(Ordering::SeqCst), 1);
            assert_eq!(launches.load(Ordering::SeqCst), 1);
        });
    }

    fn helper_command_fixture(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command
            .arg("tool_config::tests::helper_command_fixture_process")
            .arg("--exact")
            .arg("--nocapture")
            .env("CONST_API_TEST_HELPER_MODE", mode);
        command
    }

    #[test]
    #[allow(clippy::zombie_processes)] // The parent-kill test intentionally leaves this helper running.
    fn helper_command_fixture_process() {
        match std::env::var("CONST_API_TEST_HELPER_MODE").as_deref() {
            Ok("hang") => thread::sleep(Duration::from_secs(30)),
            Ok("delay") => thread::sleep(Duration::from_millis(250)),
            Ok("stubborn-tree") => {
                let marker_dir = std::env::var_os("CONST_API_TEST_HELPER_MARKER_DIR")
                    .map(PathBuf::from)
                    .expect("helper marker directory");
                let mut grandchild = helper_command_fixture("ignore-term-hold-pipe");
                grandchild
                    .env("CONST_API_TEST_HELPER_MARKER_DIR", &marker_dir)
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit());
                let grandchild = grandchild.spawn().expect("spawn stubborn grandchild");
                fs::write(
                    marker_dir.join("grandchild.pid"),
                    grandchild.id().to_string(),
                )
                .expect("write grandchild PID");
                thread::sleep(Duration::from_secs(30));
            }
            Ok("ignore-term-hold-pipe") => {
                ignore_test_process_termination();
                thread::sleep(Duration::from_secs(30));
            }
            Ok("marker-hang") => {
                let marker = std::env::var_os("CONST_API_TEST_HELPER_MARKER")
                    .map(PathBuf::from)
                    .expect("helper marker path");
                fs::write(marker, std::process::id().to_string()).expect("write helper marker");
                thread::sleep(Duration::from_secs(30));
            }
            Ok("nonzero") => std::process::exit(23),
            _ => {}
        }
    }

    #[cfg(target_os = "windows")]
    fn ignore_test_process_termination() {}

    #[cfg(not(target_os = "windows"))]
    fn ignore_test_process_termination() {
        unsafe extern "C" {
            fn signal(signal: i32, handler: usize) -> usize;
        }
        const SIGTERM: i32 = 15;
        const SIG_IGN: usize = 1;
        unsafe {
            signal(SIGTERM, SIG_IGN);
        }
    }
