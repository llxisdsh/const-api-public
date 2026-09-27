    #[test]
    fn stopped_tool_writes_through_the_atomic_entry_without_a_token() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", false);

            let ToolConfigWriteOutcome::Completed(result) =
                gemini_confirmation_operation(&confirmation_request(
                    "operation-stopped-tool",
                    "apply",
                    None,
                    false,
                    false,
                ))
                .expect("stopped tool operation")
            else {
                panic!("stopped tool operation must complete");
            };
            assert!(home.join(".gemini/.env").exists());
            assert_eq!(
                result.details.get("configuration_changed").map(String::as_str),
                Some("true")
            );
        });
    }

    #[test]
    fn unchanged_planned_restart_skips_confirmation_close_and_write() {
        with_temp_home(|home| {
            use std::sync::atomic::AtomicUsize;

            set_test_tool_runtime("gemini", false);
            apply_gemini_config("http://127.0.0.1:38787", "sk-test")
                .expect("seed Gemini configuration");
            let path = home.join(".gemini/.env");
            let before = fs::read(&path).expect("read seeded Gemini configuration");
            set_test_tool_runtime("gemini", true);

            let writes = AtomicUsize::new(0);
            let ticket = create_tool_config_operation(
                "gemini",
                ToolConfigAction::Apply,
                true,
            )
            .expect("create planned restart");
            let operation = start_tool_config_operation(
                &ticket.operation_id,
                "gemini",
                ToolConfigAction::Apply,
                true,
            )
            .expect("start planned restart");
            let outcome = execute_atomic_tool_config_operation_with_preview(
                &operation,
                Some("gemini_native"),
                "unchanged-planned-restart",
                None,
                || {
                    preview_tool_config_apply(|| {
                        apply_gemini_config("http://127.0.0.1:38787", "sk-test")
                    })
                },
                || {
                    writes.fetch_add(1, Ordering::SeqCst);
                    apply_gemini_config("http://127.0.0.1:38787", "sk-test")
                },
                |_| launch_tool_program("gemini"),
            );
            finish_tool_config_operation(&operation, outcome.is_ok());

            let ToolConfigWriteOutcome::Completed(result) =
                outcome.expect("unchanged restart should launch directly")
            else {
                panic!("unchanged restart must not request confirmation");
            };
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            assert_eq!(fs::read(path).expect("configuration remains"), before);
            assert_eq!(
                result.details.get("configuration_unchanged").map(String::as_str),
                Some("true")
            );
            assert!(!result.details.contains_key("configuration_changed"));
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(runtime.running);
            assert_eq!(runtime.close_calls, 0);
            assert_eq!(runtime.launch_calls, 1);
        });
    }

    #[test]
    fn changed_planned_restart_uses_the_existing_confirmation_flow() {
        with_temp_home(|home| {
            use std::sync::atomic::AtomicUsize;

            set_test_tool_runtime("gemini", true);
            let writes = AtomicUsize::new(0);
            let ticket = create_tool_config_operation(
                "gemini",
                ToolConfigAction::Apply,
                true,
            )
            .expect("create changed restart");
            let operation = start_tool_config_operation(
                &ticket.operation_id,
                "gemini",
                ToolConfigAction::Apply,
                true,
            )
            .expect("start changed restart");
            let outcome = execute_atomic_tool_config_operation_with_preview(
                &operation,
                Some("gemini_native"),
                "changed-planned-restart",
                None,
                || {
                    preview_tool_config_apply(|| {
                        apply_gemini_config("http://127.0.0.1:38787", "sk-test")
                    })
                },
                || {
                    writes.fetch_add(1, Ordering::SeqCst);
                    apply_gemini_config("http://127.0.0.1:38787", "sk-test")
                },
                |_| launch_tool_program("gemini"),
            );
            finish_tool_config_operation(&operation, outcome.is_ok());

            assert!(matches!(
                outcome.expect("changed restart should be prepared"),
                ToolConfigWriteOutcome::ConfirmationRequired { running: true, .. }
            ));
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            assert!(!home.join(".gemini/.env").exists());
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert_eq!(runtime.close_calls, 0);
            assert_eq!(runtime.launch_calls, 0);
        });
    }

    #[test]
    fn cancelled_slow_detection_reaches_terminal_ack_before_a_second_operation_writes() {
        with_temp_home(|home| {
            use std::sync::atomic::AtomicUsize;

            set_test_tool_runtime("gemini", false);
            set_test_tool_runtime_state_check_delay("gemini", Duration::from_millis(160));
            let writes = Arc::new(AtomicUsize::new(0));
            let first_ticket = create_tool_config_operation(
                "gemini",
                ToolConfigAction::Apply,
                false,
            )
            .expect("create first operation");
            let first_operation = start_tool_config_operation(
                &first_ticket.operation_id,
                "gemini",
                ToolConfigAction::Apply,
                false,
            )
            .expect("start first operation");
            let first_writes = Arc::clone(&writes);
            let first = thread::spawn(move || {
                let outcome = execute_atomic_tool_config_operation(
                    &first_operation,
                    Some("gemini_native"),
                    "slow-detection-context",
                    None,
                    || {
                        first_writes.fetch_add(1, Ordering::SeqCst);
                        apply_gemini_config("http://127.0.0.1:38787", "sk-test")
                    },
                    |_| unreachable!("restart disabled"),
                );
                finish_tool_config_operation(&first_operation, outcome.is_ok());
                outcome
            });

            thread::sleep(Duration::from_millis(20));
            let cancel_started = Instant::now();
            let ack = cancel_tool_config_operation_and_wait(&first_ticket.operation_id)
                .expect("cancel acknowledgement");
            assert!(
                cancel_started.elapsed() < Duration::from_millis(250),
                "cancellation should stop after the in-flight 160ms check, not start another check"
            );
            let first_error = first
                .join()
                .expect("first operation join")
                .expect_err("cancelled detection must not write");
            assert!(ack.cancellation_accepted);
            assert_eq!(ack.state, "cancelled");
            assert!(
                first_error
                    .to_string()
                    .contains("TOOL_CONFIG_OPERATION_CANCELLED"),
                "{first_error:#}"
            );
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            assert!(!home.join(".gemini/.env").exists());

            set_test_tool_runtime_state_check_delay("gemini", Duration::ZERO);
            let second_ticket =
                create_tool_config_operation("gemini", ToolConfigAction::Apply, false)
                    .expect("create second operation");
            let second_operation = start_tool_config_operation(
                &second_ticket.operation_id,
                "gemini",
                ToolConfigAction::Apply,
                false,
            )
            .expect("start second operation");
            let second_outcome = execute_atomic_tool_config_operation(
                &second_operation,
                Some("gemini_native"),
                "second-operation-context",
                None,
                || {
                    writes.fetch_add(1, Ordering::SeqCst);
                    apply_gemini_config("http://127.0.0.1:38787", "sk-test")
                },
                |_| unreachable!("restart disabled"),
            );
            finish_tool_config_operation(&second_operation, second_outcome.is_ok());
            assert!(matches!(
                second_outcome.expect("second operation"),
                ToolConfigWriteOutcome::Completed(_)
            ));
            assert_eq!(writes.load(Ordering::SeqCst), 1);
            assert!(home.join(".gemini/.env").exists());
        });
    }

    #[test]
    fn cancelled_slow_close_never_writes_or_starts_before_a_second_operation() {
        with_temp_home(|home| {
            use std::sync::atomic::AtomicUsize;

            set_test_tool_runtime("gemini", true);
            let token = required_confirmation_token(
                gemini_confirmation_operation(&confirmation_request(
                    "operation-slow-close-prepare",
                    "apply",
                    None,
                    false,
                    true,
                ))
                .expect("prepare confirmed restart"),
            );
            set_test_tool_runtime_close_delay("gemini", Duration::from_millis(320));

            let writes = Arc::new(AtomicUsize::new(0));
            let launches = Arc::new(AtomicUsize::new(0));
            let ticket = create_tool_config_operation(
                "gemini",
                ToolConfigAction::Apply,
                true,
            )
            .expect("create confirmed operation");
            let operation = start_tool_config_operation(
                &ticket.operation_id,
                "gemini",
                ToolConfigAction::Apply,
                true,
            )
            .expect("start confirmed operation");
            let first_writes = Arc::clone(&writes);
            let first_launches = Arc::clone(&launches);
            let operation_id = ticket.operation_id.clone();
            let cancel = thread::spawn(move || {
                let close_entry_deadline = Instant::now() + Duration::from_secs(5);
                while test_tool_runtime_snapshot("gemini")
                    .is_some_and(|state| state.close_calls == 0)
                {
                    assert!(
                        Instant::now() < close_entry_deadline,
                        "operation did not enter the close stage"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                cancel_tool_config_operation_and_wait(&operation_id)
            });

            let outcome = execute_atomic_tool_config_operation(
                &operation,
                Some("gemini_native"),
                "test-context",
                Some(&token),
                || {
                    first_writes.fetch_add(1, Ordering::SeqCst);
                    apply_gemini_config("http://127.0.0.1:38787", "sk-test")
                },
                |_| {
                    first_launches.fetch_add(1, Ordering::SeqCst);
                    Ok(("started".to_string(), PathBuf::from("test://started")))
                },
            );
            finish_tool_config_operation(&operation, outcome.is_ok());
            let cancel_result = cancel.join();
            assert!(
                cancel_result.is_ok(),
                "cancel thread failed; outcome={outcome:?}, runtime={:?}",
                test_tool_runtime_snapshot("gemini")
            );
            let ack = cancel_result
                .expect("cancel thread checked")
                .expect("cancel acknowledgement");
            let first_error = outcome.expect_err("cancelled close must not write or start");
            assert!(ack.cancellation_accepted);
            assert_eq!(ack.state, "cancelled");
            assert!(
                first_error
                    .to_string()
                    .contains("TOOL_CONFIG_OPERATION_CANCELLED"),
                "{first_error:#}"
            );
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert_eq!(runtime.close_calls, 1, "operation must reach close stage");
            assert_eq!(writes.load(Ordering::SeqCst), 0);
            assert_eq!(launches.load(Ordering::SeqCst), 0);
            assert!(!home.join(".gemini/.env").exists());

            set_test_tool_runtime_close_delay("gemini", Duration::ZERO);
            let second = gemini_confirmation_operation(&confirmation_request(
                "operation-after-slow-close",
                "apply",
                None,
                false,
                false,
            ))
            .expect("second operation after cancellation acknowledgement");
            assert!(matches!(second, ToolConfigWriteOutcome::Completed(_)));
            assert!(home.join(".gemini/.env").exists());
        });
    }

    #[test]
    fn close_timeout_requires_manual_choice_and_force_continue_covers_all_actions() {
        for (action, restart) in [("apply", false), ("remove", false), ("apply", true)] {
            with_temp_home(|home| {
                if action == "remove" {
                    set_test_tool_runtime("gemini", false);
                    gemini_confirmation_operation(&confirmation_request(
                        "seed-remove-config",
                        "apply",
                        None,
                        false,
                        false,
                    ))
                    .expect("seed config for remove");
                }
                let path = home.join(".gemini/.env");
                let before = fs::read(&path).ok();
                set_test_tool_runtime("gemini", true);
                let token = required_confirmation_token(
                    gemini_confirmation_operation(&confirmation_request(
                        "operation-close-timeout",
                        action,
                        None,
                        false,
                        restart,
                    ))
                    .expect("prepare operation"),
                );
                set_test_tool_runtime_close_timeout("gemini");

                let manual = gemini_confirmation_operation(&confirmation_request(
                    "operation-close-timeout",
                    action,
                    Some(token),
                    false,
                    restart,
                ))
                .expect("close timeout must request a manual decision");
                let (force_token, reason) = required_force_continue_token(manual);
                assert_eq!(reason, "TOOL_CONFIG_PROCESS_STILL_RUNNING");
                assert_eq!(fs::read(&path).ok(), before, "cancel must leave files unchanged");

                let completed = gemini_confirmation_operation(&confirmation_request(
                    "operation-force-continue",
                    action,
                    Some(force_token),
                    false,
                    restart,
                ))
                .expect("explicit force continuation must complete");
                let ToolConfigWriteOutcome::Completed(result) = completed else {
                    panic!("force continuation must complete the operation");
                };
                assert_eq!(
                    result.details.get("continued_after_close_failure").map(String::as_str),
                    Some("true")
                );
                let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
                assert!(runtime.running);
                assert_eq!(runtime.close_calls, 1);
                assert_eq!(runtime.launch_calls, usize::from(restart));
                if action == "remove" {
                    assert_ne!(fs::read(&path).ok(), before);
                } else {
                    assert!(path.exists());
                }
            });
        }
    }

    #[test]
    fn confirmed_process_probe_failure_returns_manual_choice_without_writing() {
        with_temp_home(|home| {
            set_test_tool_runtime("gemini", true);
            let token = required_confirmation_token(
                gemini_confirmation_operation(&confirmation_request(
                    "confirmed-probe-failure",
                    "apply",
                    None,
                    false,
                    true,
                ))
                .expect("prepare confirmation"),
            );
            set_test_tool_runtime_probe_failure("gemini");

            let manual = gemini_confirmation_operation(&confirmation_request(
                "confirmed-probe-failure",
                "apply",
                Some(token),
                false,
                true,
            ))
            .expect("confirmed probe failure must return a manual choice");
            let (_force_token, reason) = required_force_continue_token(manual);

            assert!(reason.contains("TOOL_CONFIG_PROCESS_PROBE_FAILED"), "{reason}");
            assert!(!home.join(".gemini/.env").exists());
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert_eq!(runtime.close_calls, 0);
            assert_eq!(runtime.launch_calls, 0);
        });
    }

    #[test]
    fn cancel_uses_the_same_confirmation_close_and_write_guard() {
        with_temp_home(|home| {
            let path = home.join(".gemini/.env");
            gemini_confirmation_operation(&confirmation_request(
                "operation-initial-apply",
                "apply",
                None,
                false,
                false,
            ))
            .expect("initial config write");
            let configured = fs::read(&path).expect("configured Gemini bytes");
            set_test_tool_runtime("gemini", true);

            let prepare = gemini_confirmation_operation(&confirmation_request(
                "operation-remove",
                "remove",
                None,
                true,
                false,
            ))
            .expect("prepare cancel operation");
            let token = required_confirmation_token(prepare);
            assert_eq!(
                fs::read(&path).expect("config remains before confirm"),
                configured
            );

            let completed = gemini_confirmation_operation(&confirmation_request(
                "operation-remove",
                "remove",
                Some(token),
                true,
                false,
            ))
            .expect("confirmed cancel operation");
            assert!(matches!(completed, ToolConfigWriteOutcome::Completed(_)));
            assert!(
                !path.exists(),
                "cancel should restore the original missing file"
            );
            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(!runtime.running);
            assert_eq!(runtime.close_calls, 1);
            assert_eq!(runtime.launch_calls, 0);
        });
    }

    #[test]
    fn configure_only_closes_a_running_tool_before_writing() {
        with_temp_home(|_| {
            set_test_tool_runtime("gemini", true);

            let result = apply_tool_config_by_name_with_runtime_handling(
                "gemini",
                "http://127.0.0.1:38787/v1",
                "http://127.0.0.1:38787",
                "sk-test",
            )
            .expect("configure Gemini");

            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(!runtime.running);
            assert_eq!(runtime.close_calls, 1);
            assert_eq!(runtime.launch_calls, 0);
            assert_eq!(
                result.details.get("closed_before_config"),
                Some(&"true".to_string())
            );
        });
    }

    #[test]
    fn configure_and_restart_reuses_atomic_write_then_starts_once() {
        with_temp_home(|_| {
            set_test_tool_runtime("gemini", true);

            let prepared = test_atomic_operation(
                "gemini",
                ToolConfigAction::Apply,
                Some("gemini_native"),
                "restart-context",
                None,
                true,
                || apply_gemini_config("http://127.0.0.1:38787", "sk-test"),
                || launch_tool_program("gemini"),
            )
            .expect("prepare Gemini config");
            assert!(matches!(
                prepared,
                ToolConfigWriteOutcome::ConfirmationRequired { running: true, .. }
            ));
            let prepared_runtime =
                test_tool_runtime_snapshot("gemini").expect("prepared runtime state");
            assert!(prepared_runtime.running);
            assert_eq!(prepared_runtime.close_calls, 0);
            assert_eq!(prepared_runtime.launch_calls, 0);
            let token = required_confirmation_token(prepared);

            let completed = test_atomic_operation(
                "gemini",
                ToolConfigAction::Apply,
                Some("gemini_native"),
                "restart-context",
                Some(&token),
                true,
                || apply_gemini_config("http://127.0.0.1:38787", "sk-test"),
                || launch_tool_program("gemini"),
            )
            .expect("apply Gemini config");
            let ToolConfigWriteOutcome::Completed(config) = completed else {
                panic!("confirmed apply should complete");
            };

            let runtime = test_tool_runtime_snapshot("gemini").expect("runtime state");
            assert!(runtime.running);
            assert_eq!(runtime.close_calls, 1);
            assert_eq!(runtime.launch_calls, 1);
            assert_eq!(
                config.details.get("closed_before_config"),
                Some(&"true".to_string())
            );
            assert_eq!(
                config.details.get("restart_command"),
                Some(&"Configure and restart".to_string())
            );
            assert_eq!(
                config.details.get("restart_launcher_path"),
                Some(&"test-runtime://gemini".to_string())
            );
        });
    }

    #[test]
    fn atomic_restart_returns_a_stable_start_error() {
        with_temp_home(|_| {
            set_test_tool_runtime("stable-error-tool", false);
            let error = test_atomic_operation(
                "stable-error-tool",
                ToolConfigAction::Apply,
                None,
                "stable-error-context",
                None,
                true,
                || Ok(ToolApplyBuilder::default().finish("stable-error-tool")),
                || Err(anyhow!("simulated launcher failure")),
            )
            .expect_err("launcher failure must be returned");

            assert!(error.to_string().contains("TOOL_CONFIG_START_FAILED"));
            assert!(error.to_string().contains("simulated launcher failure"));
        });
    }

    #[test]
    fn production_operation_tickets_do_not_expose_a_renderer_deadline() {
        with_temp_home(|_| {
            let initial =
                create_tool_config_operation("budget-tool", ToolConfigAction::Apply, false)
                    .expect("initial operation ticket");
            let confirmed = create_confirmed_tool_config_operation(
                "budget-tool",
                ToolConfigAction::Apply,
                true,
            )
            .expect("confirmed operation ticket");

            assert_ne!(initial.operation_id, confirmed.operation_id);
            let wire = serde_json::to_value(initial).expect("serialize operation ticket");
            assert!(wire.get("deadline_ms").is_none());
        });
    }

    #[test]
    fn bounded_terminal_query_never_forges_state_before_worker_finishes() {
        with_temp_home(|_| {
            let ticket = create_tool_config_operation(
                "terminal-query-tool",
                ToolConfigAction::Apply,
                false,
            )
            .expect("create terminal-query operation");
            let operation = start_tool_config_operation(
                &ticket.operation_id,
                "terminal-query-tool",
                ToolConfigAction::Apply,
                false,
            )
            .expect("start terminal-query operation");

            let started = Instant::now();
            let running = wait_for_tool_config_operation_terminal(
                &ticket.operation_id,
                Duration::from_millis(40),
            )
            .expect("bounded nonterminal query");
            assert!(started.elapsed() < Duration::from_millis(500));
            assert!(!running.terminal);
            assert_eq!(running.state, "running");

            let cancel = request_cancel_tool_config_operation(&ticket.operation_id)
                .expect("request cancellation");
            assert!(cancel.cancellation_accepted);
            assert_eq!(cancel.state, "running");

            let requested = wait_for_tool_config_operation_terminal(
                &ticket.operation_id,
                Duration::from_millis(40),
            )
            .expect("bounded requested query");
            assert!(!requested.terminal);
            assert_eq!(requested.state, "running");
            assert!(requested.cancellation_requested);

            finish_tool_config_operation(&operation, false);
            let terminal = wait_for_tool_config_operation_terminal(
                &ticket.operation_id,
                Duration::from_millis(40),
            )
            .expect("terminal query");
            assert!(terminal.terminal);
            assert_eq!(terminal.state, "cancelled");
            assert!(terminal.cancellation_requested);
        });
    }

    #[test]
    fn progress_wait_returns_only_after_the_version_changes() {
        with_temp_home(|_| {
            let ticket = create_tool_config_operation(
                "progress-tool",
                ToolConfigAction::Apply,
                false,
            )
            .expect("create progress operation");
            let operation = start_tool_config_operation(
                &ticket.operation_id,
                "progress-tool",
                ToolConfigAction::Apply,
                false,
            )
            .expect("start progress operation");

            update_tool_config_operation_progress(
                &operation,
                "migrating_sessions",
                Some(12),
                Some(48),
            );
            let status = wait_for_tool_config_operation_update(
                &ticket.operation_id,
                0,
                Duration::from_millis(50),
            )
            .expect("progress update");

            assert_eq!(status.progress_version, 2);
            assert_eq!(status.progress_stage, "migrating_sessions");
            assert_eq!(status.progress_completed, Some(12));
            assert_eq!(status.progress_total, Some(48));
            assert!(!status.terminal);
            finish_tool_config_operation(&operation, true);
        });
    }

    #[test]
    fn cancellation_is_rejected_after_the_commit_boundary() {
        with_temp_home(|_| {
            let ticket = create_tool_config_operation(
                "non-cancellable-commit-tool",
                ToolConfigAction::Apply,
                true,
            )
            .expect("create commit operation");
            let operation = start_tool_config_operation(
                &ticket.operation_id,
                "non-cancellable-commit-tool",
                ToolConfigAction::Apply,
                true,
            )
            .expect("start commit operation");
            let operation_in_worker = Arc::clone(&operation);
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let worker = thread::spawn(move || {
                let outcome = execute_atomic_tool_config_operation(
                    &operation_in_worker,
                    None,
                    "non-cancellable-commit-context",
                    None,
                    || {
                        entered_tx.send(()).expect("signal commit entry");
                        release_rx.recv().expect("release commit");
                        Ok(ToolApplyBuilder::default().finish("non-cancellable-commit-tool"))
                    },
                    |_| Ok(("started".to_string(), PathBuf::from("test://started"))),
                );
                finish_tool_config_operation(&operation_in_worker, outcome.is_ok());
                outcome
            });

            entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("writer entered commit");
            let cancel = request_cancel_tool_config_operation(&ticket.operation_id)
                .expect("commit cancellation response");
            assert!(!cancel.cancellation_accepted);
            assert_eq!(cancel.state, "committing");
            release_tx.send(()).expect("release writer");
            let outcome = worker.join().expect("commit worker").expect("commit outcome");
            assert!(matches!(outcome, ToolConfigWriteOutcome::Completed(_)));

            let status = wait_for_tool_config_operation_terminal(
                &ticket.operation_id,
                Duration::from_millis(50),
            )
            .expect("completed status");
            assert_eq!(status.state, "completed");
            assert!(!status.cancellation_requested);
        });
    }

    #[test]
    fn cancellation_ack_waits_for_cleanup_and_same_tool_operation_cannot_overlap() {
        with_temp_home(|_| {
            set_test_tool_runtime("cancelled-model-tool", false);
            let first_ticket = create_tool_config_operation(
                "cancelled-model-tool",
                ToolConfigAction::Apply,
                false,
            )
            .expect("create first operation");
            let first_operation = start_tool_config_operation(
                &first_ticket.operation_id,
                "cancelled-model-tool",
                ToolConfigAction::Apply,
                false,
            )
            .expect("start first operation");
            let second_ticket = create_tool_config_operation(
                "cancelled-model-tool",
                ToolConfigAction::Apply,
                false,
            )
            .expect("create second operation");
            let second_operation = start_tool_config_operation(
                &second_ticket.operation_id,
                "cancelled-model-tool",
                ToolConfigAction::Apply,
                false,
            )
            .expect("start second operation");
            let wrote = Arc::new(AtomicBool::new(false));
            let launched = Arc::new(AtomicBool::new(false));
            let (first_entered_tx, first_entered_rx) = std::sync::mpsc::channel();
            let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();

            let first_wrote = Arc::clone(&wrote);
            let first_launched = Arc::clone(&launched);
            let first = thread::spawn(move || {
                let lease = acquire_tool_config_operation_lease(&first_operation)
                    .expect("first operation lease");
                first_entered_tx.send(()).expect("signal first lease");
                release_first_rx.recv().expect("release first operation");
                let outcome = execute_atomic_tool_config_operation(
                    &first_operation,
                    None,
                    "cancelled-model-context",
                    None,
                    move || {
                        first_wrote.store(true, Ordering::Release);
                        Ok(ToolApplyBuilder::default().finish("cancelled-model-tool"))
                    },
                    move |_| {
                        first_launched.store(true, Ordering::Release);
                        Ok(("started".to_string(), PathBuf::from("test://started")))
                    },
                );
                finish_tool_config_operation(&first_operation, outcome.is_ok());
                drop(lease);
                outcome
            });
            first_entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("first operation entered model stage");

            let cancel_operation_id = first_ticket.operation_id.clone();
            let (cancel_started_tx, cancel_started_rx) = std::sync::mpsc::channel();
            let cancel = thread::spawn(move || {
                cancel_started_tx.send(()).expect("signal cancel start");
                cancel_tool_config_operation_and_wait(&cancel_operation_id)
            });
            cancel_started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("cancel started");

            let (second_entered_tx, second_entered_rx) = std::sync::mpsc::channel();
            let second = thread::spawn(move || {
                let lease = acquire_tool_config_operation_lease(&second_operation)
                    .expect("second operation lease");
                second_entered_tx.send(()).expect("signal second lease");
                finish_tool_config_operation(&second_operation, true);
                drop(lease);
            });
            assert!(
                second_entered_rx
                    .recv_timeout(Duration::from_millis(100))
                    .is_err(),
                "second same-tool operation overlapped cancelled cleanup"
            );

            release_first_tx
                .send(())
                .expect("release cancelled operation");
            let first_error = first
                .join()
                .expect("first join")
                .expect_err("cancelled operation must fail");
            assert!(
                first_error
                    .to_string()
                    .contains("TOOL_CONFIG_OPERATION_CANCELLED"),
                "{first_error:#}"
            );
            let ack = cancel
                .join()
                .expect("cancel join")
                .expect("cancel acknowledgement");
            assert!(ack.cancellation_accepted);
            assert_eq!(ack.state, "cancelled");
            second_entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("second operation enters after cleanup");
            second.join().expect("second join");
            assert!(!wrote.load(Ordering::Acquire));
            assert!(!launched.load(Ordering::Acquire));
        });
    }

    #[test]
    fn tool_config_operations_for_different_tools_do_not_block_each_other() {
        with_temp_home(|_| {
            set_test_tool_runtime("lock-tool-a", false);
            set_test_tool_runtime("lock-tool-b", false);
            let (entered_a_tx, entered_a_rx) = std::sync::mpsc::channel();
            let (release_a_tx, release_a_rx) = std::sync::mpsc::channel();
            let first = thread::spawn(move || {
                test_atomic_operation(
                    "lock-tool-a",
                    ToolConfigAction::Apply,
                    None,
                    "lock-tool-a-context",
                    None,
                    false,
                    || {
                        entered_a_tx.send(()).expect("signal first entry");
                        release_a_rx.recv().expect("release first operation");
                        Ok(ToolApplyBuilder::default().finish("lock-tool-a"))
                    },
                    || unreachable!("restart disabled"),
                )
            });
            entered_a_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("first operation entered");

            let (entered_b_tx, entered_b_rx) = std::sync::mpsc::channel();
            let second = thread::spawn(move || {
                test_atomic_operation(
                    "lock-tool-b",
                    ToolConfigAction::Apply,
                    None,
                    "lock-tool-b-context",
                    None,
                    false,
                    || {
                        entered_b_tx.send(()).expect("signal second entry");
                        Ok(ToolApplyBuilder::default().finish("lock-tool-b"))
                    },
                    || unreachable!("restart disabled"),
                )
            });
            entered_b_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("different tool operation must enter concurrently");
            release_a_tx.send(()).expect("release first");
            first.join().expect("first join").expect("first result");
            second.join().expect("second join").expect("second result");
        });
    }

    #[test]
    fn tool_config_operations_for_the_same_tool_are_serialized() {
        with_temp_home(|_| {
            set_test_tool_runtime("lock-tool-same", false);
            let (entered_first_tx, entered_first_rx) = std::sync::mpsc::channel();
            let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();
            let first = thread::spawn(move || {
                test_atomic_operation(
                    "lock-tool-same",
                    ToolConfigAction::Apply,
                    None,
                    "lock-tool-same-context",
                    None,
                    false,
                    || {
                        entered_first_tx.send(()).expect("signal first entry");
                        release_first_rx.recv().expect("release first operation");
                        Ok(ToolApplyBuilder::default().finish("lock-tool-same"))
                    },
                    || unreachable!("restart disabled"),
                )
            });
            entered_first_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("first operation entered");

            let (entered_second_tx, entered_second_rx) = std::sync::mpsc::channel();
            let second = thread::spawn(move || {
                test_atomic_operation(
                    "lock-tool-same",
                    ToolConfigAction::Apply,
                    None,
                    "lock-tool-same-context",
                    None,
                    false,
                    || {
                        entered_second_tx.send(()).expect("signal second entry");
                        Ok(ToolApplyBuilder::default().finish("lock-tool-same"))
                    },
                    || unreachable!("restart disabled"),
                )
            });
            assert!(
                entered_second_rx
                    .recv_timeout(Duration::from_millis(150))
                    .is_err(),
                "same-tool operation entered before the first released its lock"
            );
            release_first_tx.send(()).expect("release first");
            entered_second_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("same-tool operation enters after release");
            first.join().expect("first join").expect("first result");
            second.join().expect("second join").expect("second result");
        });
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn chatgpt_runtime_includes_codex_session_writers() {
        let names = known_tool_process_names("codex");
        assert!(names.iter().any(|name| name == "ChatGPT.exe"));
        assert!(names.iter().any(|name| name.eq_ignore_ascii_case("codex.exe")));
    }

    #[test]
    fn removing_codex_config_closes_running_process_without_restarting_it() {
        with_temp_home(|_| {
            apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("apply codex");
            set_test_tool_runtime("codex", true);

            let result =
                remove_codex_config_with_runtime_handling("http://127.0.0.1:38787/v1", "sk-test")
                    .expect("remove codex");

            let runtime = test_tool_runtime_snapshot("codex").expect("runtime state");
            assert!(!runtime.running, "cancel config should leave Codex closed");
            assert_eq!(runtime.close_calls, 1);
            assert_eq!(runtime.launch_calls, 0);
            assert_eq!(
                result.details.get("closed_before_config"),
                Some(&"true".to_string())
            );
            assert!(
                !result.details.contains_key("restarted_after_config"),
                "cancel config must not restart Codex"
            );
            assert!(!result.details.contains_key("restart_command"));
            assert!(!result.details.contains_key("restart_launcher_path"));
            assert!(!result.details.contains_key("restart_error"));
        });
    }

    #[test]
    fn removing_codex_config_by_name_without_runtime_handling_does_not_touch_running_process() {
        with_temp_home(|_| {
            apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("apply codex");
            set_test_tool_runtime("codex", true);

            remove_tool_config_by_name(
                "codex",
                "http://127.0.0.1:38787/v1",
                "http://127.0.0.1:38787",
                "sk-test",
            )
            .expect("remove codex");

            let runtime = test_tool_runtime_snapshot("codex").expect("runtime state");
            assert!(runtime.running);
            assert_eq!(runtime.close_calls, 0);
            assert_eq!(runtime.launch_calls, 0);
        });
    }

    #[test]
    fn concurrent_codex_apply_and_remove_leave_config_and_sessions_consistent() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let session_path = codex
                .join("sessions")
                .join("2026")
                .join("07")
                .join("concurrent.jsonl");
            fs::create_dir_all(session_path.parent().expect("session parent"))
                .expect("session directory");
            fs::write(
                &session_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"concurrent-session\",\"model_provider\":\"openai\"}}\n",
                    "{\"type\":\"message\",\"payload\":{\"text\":\"hello\"}}\n"
                ),
            )
            .expect("seed session");
            apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("seed CONST config");
            set_test_tool_runtime("codex", false);

            let (apply_entered_tx, apply_entered_rx) = std::sync::mpsc::channel();
            let (release_apply_tx, release_apply_rx) = std::sync::mpsc::channel();
            let apply = thread::spawn(move || {
                test_atomic_operation(
                    "codex",
                    ToolConfigAction::Apply,
                    Some("openai_responses"),
                    "codex-apply-context",
                    None,
                    false,
                    || {
                        apply_entered_tx.send(()).expect("signal apply entry");
                        release_apply_rx.recv().expect("release apply");
                        apply_codex_config("http://127.0.0.1:38787/v1", "sk-test")
                    },
                    || unreachable!("apply does not restart"),
                )
            });
            apply_entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("apply entered atomic write");

            let (remove_done_tx, remove_done_rx) = std::sync::mpsc::channel();
            let remove = thread::spawn(move || {
                let outcome = test_atomic_operation(
                    "codex",
                    ToolConfigAction::Remove,
                    None,
                    "codex-concurrent-context",
                    None,
                    false,
                    || remove_codex_config("http://127.0.0.1:38787/v1", "sk-test"),
                    || unreachable!("remove does not restart"),
                );
                remove_done_tx.send(()).expect("signal remove completion");
                outcome
            });
            assert!(
                remove_done_rx
                    .recv_timeout(Duration::from_millis(100))
                    .is_err(),
                "remove overlapped the in-flight Codex apply"
            );
            release_apply_tx.send(()).expect("release apply");
            assert!(matches!(
                apply.join().expect("apply join").expect("apply outcome"),
                ToolConfigWriteOutcome::Completed(_)
            ));
            assert!(matches!(
                remove.join().expect("remove join").expect("remove outcome"),
                ToolConfigWriteOutcome::Completed(_)
            ));

            assert!(
                !codex.join("config.toml").exists(),
                "remove must restore the originally missing config"
            );
            assert_eq!(
                read_codex_session_provider(&session_path).expect("session provider"),
                Some("openai".to_string())
            );
            assert_eq!(
                load_codex_session_sync_manifest()
                    .expect("session manifest")
                    .last_session_provider
                    .as_deref(),
                Some("openai")
            );
        });
    }
