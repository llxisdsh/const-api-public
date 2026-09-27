    #[test]
    fn codex_state_db_selection_uses_only_active_state_and_desktop_catalog() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let sqlite_dir = codex.join("sqlite");
            fs::create_dir_all(&sqlite_dir).expect("sqlite dir");
            let state_path = codex.join("state_5.sqlite");
            let catalog_path = sqlite_dir.join("codex-dev.db");
            Connection::open(&state_path).expect("create active state db");
            Connection::open(&catalog_path).expect("create desktop catalog");
            Connection::open(sqlite_dir.join("state_5.sqlite")).expect("create stale state db");
            Connection::open(sqlite_dir.join("other.db")).expect("create unrelated db");

            let paths = collect_codex_state_db_paths().expect("collect db paths");

            let mut expected = vec![state_path, catalog_path];
            expected.sort();
            assert_eq!(paths, expected);
        });
    }

    #[test]
    fn apply_codex_config_syncs_all_sessions_without_per_session_provider_ownership() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let session_path = codex
                .join("sessions")
                .join("2026")
                .join("07")
                .join("session.jsonl");
            fs::create_dir_all(session_path.parent().expect("session parent"))
                .expect("session dir");
            fs::write(
                &session_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
                    "{\"type\":\"message\",\"payload\":{\"text\":\"hello\"}}\n"
                ),
            )
            .expect("session write");

            let state_path = codex.join("state_5.sqlite");
            fs::create_dir_all(state_path.parent().expect("state parent")).expect("state dir");
            let conn = Connection::open(&state_path).expect("state open");
            conn.execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
                [],
            )
            .expect("create threads");
            conn.execute(
                "INSERT INTO threads (id, model_provider) VALUES ('t1', 'openai')",
                [],
            )
            .expect("insert thread");
            drop(conn);

            let mut progress = Vec::new();
            let result = apply_codex_config_with_progress(
                "http://127.0.0.1:38787/v1",
                "sk-test",
                &mut |stage, completed, total, ratio| {
                    progress.push((stage.to_string(), completed, total, ratio));
                },
            )
            .expect("apply codex");

            assert_eq!(
                read_codex_session_provider(&session_path).expect("read provider"),
                Some(CODEX_CONST_API_PROVIDER_ID.to_string())
            );
            let conn = Connection::open(&state_path).expect("state reopen");
            let provider: String = conn
                .query_row(
                    "SELECT model_provider FROM threads WHERE id = 't1'",
                    [],
                    |row| row.get(0),
                )
                .expect("provider row");
            assert_eq!(provider, CODEX_CONST_API_PROVIDER_ID);
            assert_eq!(
                result.details.get("session_migrated_files"),
                Some(&"1".to_string())
            );
            assert_eq!(
                result.details.get("session_migrated_sqlite_rows"),
                Some(&"1".to_string())
            );
            let manifest = load_codex_session_sync_manifest().expect("manifest");
            assert!(manifest.rollouts.is_empty());
            assert!(manifest.threads.is_empty());
            assert!(
                !result
                    .backups
                    .iter()
                    .any(|path| path.contains("codex-sessions")),
                "JSONL session sync should not create full-file backups: {:?}",
                result.backups
            );
            assert!(
                fs::read_to_string(&session_path)
                    .expect("session after sync")
                    .contains("{\"type\":\"message\",\"payload\":{\"text\":\"hello\"}}\n"),
                "session body must remain intact"
            );
            assert!(
                !result
                    .backups
                    .iter()
                    .any(|path| path.contains("codex-state")),
                "provider-only migration must not copy the whole state database: {:?}",
                result.backups
            );
            assert!(progress.contains(&(
                "scanning_sessions".to_string(),
                Some(1),
                Some(1),
                None,
            )));
            assert!(progress.iter().any(|(stage, completed, total, ratio)| {
                stage == "migrating_sessions"
                    && *completed == Some(1)
                    && *total == Some(1)
                    && *ratio == Some(1.0)
            }));
            assert!(
                progress
                    .iter()
                    .any(|(stage, _, _, ratio)| stage == "syncing_sessions"
                        && ratio.is_some())
            );
            let migration_ratios = progress
                .iter()
                .filter(|(stage, _, _, _)| {
                    stage == "migrating_sessions" || stage == "syncing_sessions"
                })
                .filter_map(|(_, _, _, ratio)| *ratio)
                .collect::<Vec<_>>();
            assert!(
                migration_ratios
                    .windows(2)
                    .all(|values| values[0] <= values[1])
            );
            assert!(
                progress
                    .iter()
                    .any(|(stage, _, _, _)| stage == "updating_session_index")
            );
        });
    }

    #[test]
    fn codex_apply_preserves_unchanged_auth_and_reformatted_config_without_backups() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            fs::create_dir_all(&codex).unwrap();
            let auth_path = codex.join("auth.json");
            let config_path = codex.join("config.toml");
            let original_auth = r#"{"tokens":{"refresh_token":"test-refresh"},"auth_mode":"chatgpt"}"#;
            fs::write(&auth_path, original_auth).unwrap();
            fs::write(
                &config_path,
                "model_provider = 'user-provider'\n# Keep user settings\ntheme = 'dark'\n",
            ).unwrap();

            let first = apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
            assert_eq!(first.backups.len(), 1, "only the modified config needs a backup");
            assert!(!first.files.contains(&auth_path.display().to_string()));
            let manifest = load_tool_config_manifest().unwrap();
            assert!(!manifest.files.contains_key(&manifest_file_key(&auth_path)));

            rewrite_tool_config_without_semantic_change(&config_path);
            let reformatted = fs::read_to_string(&config_path).unwrap()
                .replace("\"CONST_API\"", "'CONST_API'");
            fs::write(&config_path, reformatted).unwrap();
            let config_before = fs::read(&config_path).unwrap();
            let manifest_before = fs::read(tool_config_manifest_path()).unwrap();
            let second = apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
            assert!(second.already_configured);
            assert!(second.files.is_empty());
            assert!(second.backups.is_empty());
            assert_eq!(fs::read(&auth_path).unwrap(), original_auth.as_bytes());
            assert_eq!(fs::read(&config_path).unwrap(), config_before);
            assert_eq!(fs::read(tool_config_manifest_path()).unwrap(), manifest_before);
            assert!(second.file_statuses.iter().all(|status| {
                status.before_sha256 == status.after_sha256
            }));

            remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
            assert_eq!(fs::read(&auth_path).unwrap(), original_auth.as_bytes());
            let restored = fs::read_to_string(&config_path).unwrap()
                .parse::<DocumentMut>().unwrap();
            assert_eq!(restored["model_provider"].as_str(), Some("user-provider"));
            assert_eq!(restored["theme"].as_str(), Some("dark"));
        });
    }

    #[test]
    fn toml_restore_matches_formatting_but_preserves_real_user_changes() {
        let value = |raw: &str| ToolConfigFieldValue::Value(
            serde_json::Value::String(format!("{TOML_VALUE_PREFIX}{raw}")),
        );
        for (current, expected, matches) in [
            ("'CONST_API'", "\"CONST_API\"", true),
            ("1_000", "1000", true),
            ("[1, 2]", "[1,2]", true),
            ("'user-provider'", "\"CONST_API\"", false),
            ("[2,1]", "[1,2]", false),
            ("'1000'", "1000", false),
        ] {
            assert_eq!(toml_field_values_match(&value(current), &value(expected)).unwrap(), matches);
        }
        assert!(!toml_field_values_match(&ToolConfigFieldValue::Missing, &value("''")).unwrap());
    }

    #[test]
    fn codex_state_provider_transaction_rolls_back_without_whole_database_backup() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            fs::create_dir_all(&codex).unwrap();
            let conn = Connection::open(codex.join("state_5.sqlite")).unwrap();
            conn.execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL, title TEXT);
                 INSERT INTO threads VALUES ('a', 'openai', 'keep a'), ('b', 'openai', 'keep b');
                 CREATE TRIGGER reject_second_update BEFORE UPDATE ON threads
                 WHEN NEW.id = 'b' BEGIN SELECT RAISE(ABORT, 'injected update failure'); END;"
            ).unwrap();
            let mut plan = plan_codex_state_db_provider_updates(
                CODEX_CONST_API_PROVIDER_ID,
                &HashMap::new(),
            ).unwrap();
            plan.updates.sort_by(|a, b| a.thread_id.cmp(&b.thread_id));
            assert_eq!(plan.updates.len(), 2);
            let error = apply_codex_state_db_provider_updates(&plan.updates).unwrap_err();
            assert!(format!("{error:#}").contains("injected update failure"));
            let rows = conn
                .prepare("SELECT model_provider, title FROM threads ORDER BY id").unwrap()
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                }).unwrap()
                .collect::<rusqlite::Result<Vec<_>>>().unwrap();
            assert_eq!(rows, vec![
                ("openai".into(), "keep a".into()),
                ("openai".into(), "keep b".into()),
            ]);
            assert!(!backup_root().join("codex-state").exists());
        });
    }

    #[test]
    fn apply_codex_config_migrates_nullable_state_provider_rows() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            fs::create_dir_all(&codex).expect("Codex dir");
            let state_path = codex.join("state_5.sqlite");
            let conn = Connection::open(&state_path).expect("state open");
            conn.execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT)",
                [],
            )
            .expect("create nullable threads");
            conn.execute(
                "INSERT INTO threads (id, model_provider) VALUES ('null-provider', NULL)",
                [],
            )
            .expect("insert nullable thread");
            drop(conn);

            let result = apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("apply Codex with nullable provider");

            let conn = Connection::open(&state_path).expect("state reopen");
            let provider: Option<String> = conn
                .query_row(
                    "SELECT model_provider FROM threads WHERE id = 'null-provider'",
                    [],
                    |row| row.get(0),
                )
                .expect("nullable provider row");
            assert_eq!(provider.as_deref(), Some(CODEX_CONST_API_PROVIDER_ID));
            assert_eq!(
                result
                    .details
                    .get("session_migrated_sqlite_rows")
                    .map(String::as_str),
                Some("1")
            );
            assert!(!result.details.contains_key("session_sync_error"));
            assert!(!result
                .details
                .contains_key("session_migrated_sqlite_error"));
        });
    }

    #[test]
    fn native_codex_remove_repairs_null_state_providers_without_migration_errors() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let sqlite_dir = codex.join("sqlite");
            fs::create_dir_all(&sqlite_dir).expect("Codex sqlite dir");

            let state_path = codex.join("state_5.sqlite");
            let conn = Connection::open(&state_path).expect("state open");
            conn.execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT)",
                [],
            )
            .expect("create nullable threads");
            conn.execute(
                "INSERT INTO threads (id, model_provider) VALUES ('null-provider', NULL)",
                [],
            )
            .expect("insert nullable thread");
            drop(conn);

            let catalog_path = sqlite_dir.join("codex-dev.db");
            let conn = Connection::open(&catalog_path).expect("catalog open");
            conn.execute(
                "CREATE TABLE local_thread_catalog (thread_id TEXT, model_provider TEXT)",
                [],
            )
            .expect("create nullable catalog");
            conn.execute(
                "INSERT INTO local_thread_catalog (thread_id, model_provider) VALUES ('catalog-null-provider', NULL)",
                [],
            )
            .expect("insert nullable catalog rows");
            drop(conn);

            let mut ignore_progress =
                |_: &str, _: Option<u64>, _: Option<u64>, _: Option<f64>| {};
            let result = remove_codex_config_with_progress_and_mode(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                ToolConfigRemoveMode::NativeRoute,
                &mut ignore_progress,
            )
            .expect("native Codex removal with nullable providers");

            let conn = Connection::open(&state_path).expect("state reopen");
            let provider: Option<String> = conn
                .query_row(
                    "SELECT model_provider FROM threads WHERE id = 'null-provider'",
                    [],
                    |row| row.get(0),
                )
                .expect("nullable provider row");
            assert_eq!(provider.as_deref(), Some("openai"));
            let conn = Connection::open(&catalog_path).expect("catalog reopen");
            let catalog_provider: Option<String> = conn
                .query_row(
                    "SELECT model_provider FROM local_thread_catalog WHERE thread_id = 'catalog-null-provider'",
                    [],
                    |row| row.get(0),
                )
                .expect("nullable catalog provider row");
            assert_eq!(catalog_provider.as_deref(), Some("openai"));
            assert!(!result.details.contains_key("session_sync_error"));
            assert!(!result
                .details
                .contains_key("session_migrated_sqlite_error"));
            assert_eq!(
                result
                    .details
                    .get("session_migrated_sqlite_rows")
                    .map(String::as_str),
                Some("2")
            );
        });
    }

    #[test]
    fn native_codex_remove_skips_missing_thread_ids_with_an_index_diagnostic() {
        with_temp_home(|home| {
            let sqlite_dir = home.join(".codex/sqlite");
            fs::create_dir_all(&sqlite_dir).expect("Codex sqlite dir");
            let catalog_path = sqlite_dir.join("codex-dev.db");
            let conn = Connection::open(&catalog_path).expect("catalog open");
            conn.execute(
                "CREATE TABLE local_thread_catalog (thread_id TEXT, model_provider TEXT)",
                [],
            )
            .expect("create nullable catalog");
            conn.execute(
                "INSERT INTO local_thread_catalog (thread_id, model_provider) VALUES (NULL, 'CONST_API')",
                [],
            )
            .expect("insert missing thread id");
            drop(conn);

            let mut ignore_progress =
                |_: &str, _: Option<u64>, _: Option<u64>, _: Option<f64>| {};
            let result = remove_codex_config_with_progress_and_mode(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "sk-test",
                ToolConfigRemoveMode::NativeRoute,
                &mut ignore_progress,
            )
            .expect("native Codex removal with a missing thread id");

            assert!(!result.details.contains_key("session_sync_error"));
            assert!(!result
                .details
                .contains_key("session_migrated_sqlite_error"));
            assert_eq!(
                result
                    .details
                    .get("session_migrated_sqlite_invalid_rows")
                    .map(String::as_str),
                Some("1")
            );
            let conn = Connection::open(&catalog_path).expect("catalog reopen");
            let provider: String = conn
                .query_row(
                    "SELECT model_provider FROM local_thread_catalog WHERE thread_id IS NULL",
                    [],
                    |row| row.get(0),
                )
                .expect("missing id row remains");
            assert_eq!(provider, CODEX_CONST_API_PROVIDER_ID);
        });
    }

    #[test]
    fn codex_session_progress_is_weighted_by_bytes_not_file_count() {
        with_temp_home(|home| {
            let sessions = home.join(".codex").join("sessions");
            fs::create_dir_all(&sessions).expect("sessions dir");
            let small_path = sessions.join("a-small.jsonl");
            let large_path = sessions.join("z-large.jsonl");
            fs::write(
                &small_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"small\",\"model_provider\":\"openai\"}}\n",
                    "{\"type\":\"user_message\",\"payload\":{\"text\":\"hello\"}}\n"
                ),
            )
            .expect("small session");
            let mut large = concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"large\",\"model_provider\":\"openai\"}}\n",
                "{\"type\":\"user_message\",\"payload\":{\"text\":\"hello\"}}\n"
            )
            .as_bytes()
            .to_vec();
            large.extend(std::iter::repeat(b'x').take(2 * 1024 * 1024));
            large.push(b'\n');
            fs::write(&large_path, large).expect("large session");

            let mut progress = Vec::new();
            apply_codex_config_with_progress(
                "http://127.0.0.1:38787/v1",
                "sk-test",
                &mut |stage, completed, total, ratio| {
                    progress.push((stage.to_string(), completed, total, ratio));
                },
            )
            .expect("apply codex");

            let ratio_after_small_file = progress
                .iter()
                .find_map(|(stage, completed, total, ratio)| {
                    (stage == "migrating_sessions"
                        && *completed == Some(1)
                        && *total == Some(2))
                    .then_some(*ratio)
                    .flatten()
                })
                .expect("first file byte ratio");
            assert!(
                ratio_after_small_file < 0.01,
                "one of two files should remain below 1% when that file is tiny; ratio={ratio_after_small_file}"
            );
        });
    }

    #[test]
    fn apply_codex_config_moves_every_session_bucket_to_const_api() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let session_path = codex
                .join("sessions")
                .join("2026")
                .join("07")
                .join("azure.jsonl");
            fs::create_dir_all(session_path.parent().expect("session parent"))
                .expect("session dir");
            fs::write(
                &session_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s-azure\",\"model_provider\":\"azure\"}}\n",
                    "{\"type\":\"message\",\"payload\":{\"text\":\"hello\"}}\n"
                ),
            )
            .expect("session write");

            let state_path = codex.join("state_5.sqlite");
            let conn = Connection::open(&state_path).expect("state open");
            conn.execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
                [],
            )
            .expect("create threads");
            conn.execute(
                "INSERT INTO threads (id, model_provider) VALUES ('s-azure', 'ollama')",
                [],
            )
            .expect("insert thread");
            drop(conn);

            let result =
                apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("apply codex");

            assert_eq!(
                read_codex_session_provider(&session_path).expect("session provider"),
                Some(CODEX_CONST_API_PROVIDER_ID.to_string())
            );
            let conn = Connection::open(&state_path).expect("state reopen");
            let provider: String = conn
                .query_row(
                    "SELECT model_provider FROM threads WHERE id = 's-azure'",
                    [],
                    |row| row.get(0),
                )
                .expect("provider row");
            assert_eq!(provider, CODEX_CONST_API_PROVIDER_ID);
            assert_eq!(
                result.details.get("session_migrated_files"),
                Some(&"1".to_string())
            );
            assert_eq!(
                result.details.get("session_migrated_sqlite_rows"),
                Some(&"1".to_string())
            );
        });
    }

    #[test]
    fn codex_manifest_key_rotation_restores_owned_values_and_preserves_other_providers() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            fs::create_dir_all(&codex).expect("codex dir");
            fs::write(
                codex.join("auth.json"),
                r#"{
  "OPENAI_API_KEY": "sk-original",
  "user_auth_field": "keep-before"
}
"#,
            )
            .expect("seed auth");
            fs::write(
                codex.join("config.toml"),
                concat!(
                    "model_provider = \"azure\"\n",
                    "model = \"code-cheap\"\n",
                    "user_setting = \"keep-before\"\n\n",
                    "[model_providers.azure]\n",
                    "name = \"Azure\"\n",
                    "base_url = \"https://azure.example/v1\"\n",
                    "wire_api = \"responses\"\n\n",
                    "[model_providers.const_api]\n",
                    "name = \"Personal CONST slot\"\n",
                    "base_url = \"https://personal.example/v1\"\n",
                    "wire_api = \"chat\"\n",
                    "requires_openai_auth = false\n",
                    "experimental_bearer_token = \"sk-provider-original\"\n",
                    "custom_flag = true\n",
                ),
            )
            .expect("seed config");

            apply_codex_config("http://127.0.0.1:38787/v1", "sk-first").expect("first apply");

            let auth_path = codex.join("auth.json");
            let mut auth =
                read_json_or_default(&auth_path, serde_json::json!({})).expect("read applied auth");
            auth["user_auth_after_apply"] = serde_json::json!("preserve");
            fs::write(
                &auth_path,
                serde_json::to_string_pretty(&auth).expect("serialize auth") + "\n",
            )
            .expect("edit auth");

            let config_path = codex.join("config.toml");
            let mut config = fs::read_to_string(&config_path)
                .expect("read applied config")
                .parse::<DocumentMut>()
                .expect("parse applied config");
            let providers = config["model_providers"]
                .as_table_mut()
                .expect("provider table");
            let mut user_added = Table::new();
            user_added["name"] = toml_value("Added after apply");
            user_added["base_url"] = toml_value("https://after.example/v1");
            providers["user_added"] = Item::Table(user_added);
            fs::write(&config_path, config.to_string()).expect("edit config");

            apply_codex_config("http://127.0.0.1:38787/v1", "sk-rotated").expect("rotate key");
            let active_manifest = load_tool_config_manifest().expect("active tool manifest");
            let auth_ownership = active_manifest
                .files
                .get(&manifest_file_key(&auth_path))
                .expect("Codex auth ownership");
            let key_field = auth_ownership
                .fields
                .iter()
                .find(|field| {
                    field.path == vec![ToolConfigFieldPath::Key("OPENAI_API_KEY".to_string())]
                })
                .expect("owned Codex API key field");
            assert_eq!(
                key_field.original,
                ToolConfigFieldValue::Value(serde_json::json!("sk-original"))
            );
            assert_eq!(
                key_field.applied,
                ToolConfigFieldValue::Value(serde_json::json!("sk-rotated"))
            );
            remove_codex_config("http://127.0.0.1:38787/v1", "sk-rotated").expect("cancel codex");

            let restored_auth =
                read_json_or_default(&auth_path, serde_json::json!({})).expect("restored auth");
            assert_eq!(
                restored_auth
                    .get("OPENAI_API_KEY")
                    .and_then(serde_json::Value::as_str),
                Some("sk-original")
            );
            assert_eq!(
                restored_auth
                    .get("user_auth_field")
                    .and_then(serde_json::Value::as_str),
                Some("keep-before")
            );
            assert_eq!(
                restored_auth
                    .get("user_auth_after_apply")
                    .and_then(serde_json::Value::as_str),
                Some("preserve")
            );

            let restored_config = fs::read_to_string(&config_path)
                .expect("restored config")
                .parse::<DocumentMut>()
                .expect("parse restored config");
            assert_eq!(
                restored_config.get("model_provider").and_then(Item::as_str),
                Some("azure")
            );
            assert_eq!(
                restored_config.get("model").and_then(Item::as_str),
                Some("code-cheap")
            );
            assert_eq!(
                restored_config
                    .get("model_providers")
                    .and_then(|providers| providers.get("const_api"))
                    .and_then(|provider| provider.get("name"))
                    .and_then(Item::as_str),
                Some("Personal CONST slot")
            );
            assert_eq!(
                restored_config
                    .get("model_providers")
                    .and_then(|providers| providers.get("const_api"))
                    .and_then(|provider| provider.get("custom_flag"))
                    .and_then(Item::as_bool),
                Some(true)
            );
            assert_eq!(
                restored_config
                    .get("model_providers")
                    .and_then(|providers| providers.get("user_added"))
                    .and_then(|provider| provider.get("name"))
                    .and_then(Item::as_str),
                Some("Added after apply")
            );
            let manifest = load_tool_config_manifest().expect("tool manifest");
            assert!(manifest
                .files
                .values()
                .all(|ownership| ownership.tool != "codex"));
        });
    }

    #[test]
    fn remove_codex_config_restores_previous_provider_and_moves_sessions_to_it() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let session_path = codex
                .join("sessions")
                .join("2026")
                .join("07")
                .join("session.jsonl");
            let archived_path = codex
                .join("archived_sessions")
                .join("2026")
                .join("07")
                .join("archived.jsonl");
            for path in [&session_path, &archived_path] {
                fs::create_dir_all(path.parent().expect("session parent")).expect("session dir");
            }
            fs::write(
                &session_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
                    "{\"type\":\"message\",\"payload\":{\"text\":\"hello\"}}\n"
                ),
            )
            .expect("session write");
            fs::write(
                &archived_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s2\",\"model_provider\":\"custom\"}}\n",
                    "{\"type\":\"message\",\"payload\":{\"text\":\"archived\"}}\n"
                ),
            )
            .expect("archived write");

            let state_path = codex.join("state_5.sqlite");
            let conn = Connection::open(&state_path).expect("state open");
            conn.execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
                [],
            )
            .expect("create threads");
            conn.execute(
                "INSERT INTO threads (id, model_provider) VALUES ('s1', 'openai'), ('s2', 'custom')",
                [],
            )
            .expect("insert threads");
            drop(conn);

            apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("apply codex");
            assert_eq!(
                read_codex_session_provider(&session_path).expect("session provider"),
                Some(CODEX_CONST_API_PROVIDER_ID.to_string())
            );
            assert_eq!(
                read_codex_session_provider(&archived_path).expect("archived provider"),
                Some(CODEX_CONST_API_PROVIDER_ID.to_string())
            );

            let result =
                remove_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("remove codex");

            assert!(
                !codex.join("config.toml").exists(),
                "cancel should delete a config file created entirely by CONST"
            );
            assert_eq!(
                read_codex_session_provider(&session_path).expect("session provider"),
                Some("openai".to_string())
            );
            assert_eq!(
                read_codex_session_provider(&archived_path).expect("archived provider"),
                Some("openai".to_string())
            );
            let conn = Connection::open(&state_path).expect("state reopen");
            let provider_s1: String = conn
                .query_row(
                    "SELECT model_provider FROM threads WHERE id = 's1'",
                    [],
                    |row| row.get(0),
                )
                .expect("provider s1");
            let provider_s2: String = conn
                .query_row(
                    "SELECT model_provider FROM threads WHERE id = 's2'",
                    [],
                    |row| row.get(0),
                )
                .expect("provider s2");
            assert_eq!(provider_s1, "openai");
            assert_eq!(provider_s2, "openai");
            assert_eq!(
                result.details.get("session_migrated_files"),
                Some(&"2".to_string())
            );
            assert_eq!(
                result.details.get("session_migrated_sqlite_rows"),
                Some(&"2".to_string())
            );
            assert_eq!(
                result.details.get("session_target_provider"),
                Some(&"openai".to_string())
            );
        });
    }

    #[test]
    fn remove_codex_config_keeps_existing_and_new_session_content_complete() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            fs::create_dir_all(&codex).expect("Codex dir");
            fs::write(
                codex.join("config.toml"),
                concat!(
                    "model_provider = \"openai\"\n\n",
                    "[model_providers.openai]\n",
                    "name = \"OpenAI\"\n",
                    "base_url = \"https://api.openai.com/v1\"\n",
                    "wire_api = \"responses\"\n",
                ),
            )
            .expect("original config");
            let existing = codex.join("sessions/2026/07/existing.jsonl");
            fs::create_dir_all(existing.parent().expect("session parent")).expect("session dir");
            let existing_body =
                " {\"type\":\"message\",\"payload\":{\"text\":\"keep spacing and body\"}}\r\n";
            fs::write(
                &existing,
                format!(
                    "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"existing\",\"model_provider\":\"openai\"}}}}\r\n{existing_body}"
                ),
            )
            .expect("existing session");

            apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").expect("apply Codex");
            let after_apply = fs::read_to_string(&existing).expect("existing after apply");
            assert!(after_apply.contains(existing_body));
            assert_eq!(
                read_codex_session_provider(&existing).expect("existing provider"),
                Some(CODEX_CONST_API_PROVIDER_ID.to_string())
            );

            let created = codex.join("sessions/2026/07/created-after-apply.jsonl");
            let created_body =
                "{\"type\":\"message\",\"payload\":{\"text\":\"new session remains complete\"}}\n";
            fs::write(
                &created,
                format!(
                    "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"created\",\"model_provider\":\"CONST_API\"}}}}\n{created_body}"
                ),
            )
            .expect("new session");

            remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").expect("cancel Codex");

            assert_eq!(
                read_codex_session_provider(&existing).expect("restored existing provider"),
                Some("openai".to_string())
            );
            assert_eq!(
                read_codex_session_provider(&created).expect("restored new provider"),
                Some("openai".to_string())
            );
            assert!(fs::read_to_string(&existing)
                .expect("existing final")
                .contains(existing_body));
            assert!(fs::read_to_string(&created)
                .expect("created final")
                .contains(created_body));
        });
    }

    #[test]
    fn remove_codex_config_keeps_user_changed_provider_and_moves_sessions_to_current_provider() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let session_path = codex
                .join("sessions")
                .join("2026")
                .join("07")
                .join("session.jsonl");
            fs::create_dir_all(session_path.parent().expect("session parent"))
                .expect("session dir");
            fs::write(
                &session_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"model_provider\":\"openai\"}}\n",
                    "{\"type\":\"message\",\"payload\":{\"text\":\"hello\"}}\n"
                ),
            )
            .expect("session write");

            let state_path = codex.join("state_5.sqlite");
            let conn = Connection::open(&state_path).expect("state open");
            conn.execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
                [],
            )
            .expect("create threads");
            conn.execute(
                "INSERT INTO threads (id, model_provider) VALUES ('s1', 'openai')",
                [],
            )
            .expect("insert thread");
            drop(conn);

            apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("apply codex");
            fs::write(
                codex.join("config.toml"),
                concat!(
                    "model_provider = \"azure\"\n\n",
                    "[model_providers.azure]\n",
                    "name = \"Azure\"\n",
                    "base_url = \"https://azure.example/v1\"\n",
                    "wire_api = \"responses\"\n\n",
                    "[model_providers.const_api]\n",
                    "name = \"CONST API\"\n",
                    "base_url = \"http://127.0.0.1:38787/v1\"\n",
                    "wire_api = \"responses\"\n",
                    "requires_openai_auth = true\n",
                    "experimental_bearer_token = \"sk-test\"\n",
                ),
            )
            .expect("config write");

            let result =
                remove_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("remove codex");

            let config = fs::read_to_string(codex.join("config.toml")).expect("config read");
            let doc = config.parse::<DocumentMut>().expect("parse config");
            assert_eq!(
                doc.get("model_provider").and_then(|value| value.as_str()),
                Some("azure")
            );
            assert!(
                doc.get("model_providers")
                    .and_then(|providers| providers.get(CODEX_CONST_API_PROVIDER_ID))
                    .is_none(),
                "const_api provider should be removed: {config}"
            );
            assert_eq!(
                read_codex_session_provider(&session_path).expect("session provider"),
                Some("azure".to_string())
            );
            let conn = Connection::open(&state_path).expect("state reopen");
            let provider: String = conn
                .query_row(
                    "SELECT model_provider FROM threads WHERE id = 's1'",
                    [],
                    |row| row.get(0),
                )
                .expect("provider row");
            assert_eq!(provider, "azure");
            assert_eq!(
                result.details.get("session_target_provider"),
                Some(&"azure".to_string())
            );
        });
    }

    #[test]
    fn remove_codex_config_keeps_changed_provider_and_updates_catalog_bucket() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let session_path = codex
                .join("sessions")
                .join("2026")
                .join("07")
                .join("05")
                .join("session.jsonl");
            fs::create_dir_all(session_path.parent().expect("session parent"))
                .expect("session dir");
            fs::write(
                &session_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\",\"model_provider\":\"openai\",\"cwd\":\"/Users/ashely/code/chan.const\"}}\n",
                    "{\"type\":\"user_message\",\"payload\":{\"text\":\"hello\"}}\n",
                ),
            )
            .expect("session write");

            let sqlite_dir = codex.join("sqlite");
            fs::create_dir_all(&sqlite_dir).expect("sqlite dir");
            let catalog_path = sqlite_dir.join("codex-dev.db");
            let conn = Connection::open(&catalog_path).expect("open sqlite");
            conn.execute_batch(
                "CREATE TABLE local_thread_catalog (
                    host_id TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    display_title TEXT NOT NULL,
                    source_created_at REAL NOT NULL,
                    source_updated_at REAL NOT NULL,
                    cwd TEXT NOT NULL,
                    source_kind TEXT NOT NULL,
                    source_detail TEXT,
                    model_provider TEXT NOT NULL,
                    git_branch TEXT,
                    observation_sequence INTEGER NOT NULL,
                    missing_candidate INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (host_id, thread_id)
                );
                CREATE TABLE local_thread_catalog_metadata (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    catalog_revision INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO local_thread_catalog_metadata (id, catalog_revision) VALUES (1, 9);",
            )
            .expect("create catalog");
            conn.execute(
                "INSERT INTO local_thread_catalog (
                    host_id, thread_id, display_title, source_created_at, source_updated_at,
                    cwd, source_kind, source_detail, model_provider, git_branch,
                    observation_sequence, missing_candidate
                ) VALUES ('local', 't1', 'hello', 1.0, 2.0, '/Users/ashely/code/chan.const', 'vscode', NULL, 'openai', 'main', 1, 0)",
                [],
            )
            .expect("insert catalog");
            drop(conn);

            apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("apply codex");
            fs::write(codex.join("config.toml"), "model_provider = \"openai\"\n")
                .expect("reset config");

            let result =
                remove_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("remove codex");

            assert_eq!(
                read_codex_session_provider(&session_path).expect("session provider"),
                Some("openai".to_string())
            );
            let conn = Connection::open(&catalog_path).expect("open sqlite");
            let (provider, revision): (String, i64) = conn
                .query_row(
                    "SELECT catalog.model_provider, metadata.catalog_revision
                     FROM local_thread_catalog catalog
                     CROSS JOIN local_thread_catalog_metadata metadata
                     WHERE catalog.thread_id = 't1' AND metadata.id = 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("catalog row");
            assert_eq!(provider, "openai");
            assert!(revision > 9);
            assert_eq!(
                result.details.get("session_target_provider"),
                Some(&"openai".to_string())
            );
        });
    }

    #[test]
    fn check_codex_config_does_not_restore_sessions_as_a_side_effect() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let session_path = codex
                .join("sessions")
                .join("2026")
                .join("07")
                .join("hello.jsonl");
            fs::create_dir_all(session_path.parent().expect("session parent"))
                .expect("session dir");
            fs::write(
                &session_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"hello\",\"model_provider\":\"const_api\"}}\n",
                    "{\"type\":\"message\",\"payload\":{\"text\":\"hello\"}}\n"
                ),
            )
            .expect("session write");

            let config_path = codex.join("config.toml");
            fs::write(&config_path, "model_provider = \"openai\"\n").expect("config write");

            let state_path = codex.join("state_5.sqlite");
            let conn = Connection::open(&state_path).expect("state open");
            conn.execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL)",
                [],
            )
            .expect("create threads");
            conn.execute(
                "INSERT INTO threads (id, model_provider) VALUES ('hello', 'const_api')",
                [],
            )
            .expect("insert thread");
            drop(conn);

            let result =
                check_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("check codex");

            assert_eq!(
                read_codex_session_provider(&session_path).expect("session provider"),
                Some(LEGACY_CONST_API_PROVIDER_ID.to_string())
            );
            let conn = Connection::open(&state_path).expect("state reopen");
            let provider: String = conn
                .query_row(
                    "SELECT model_provider FROM threads WHERE id = 'hello'",
                    [],
                    |row| row.get(0),
                )
                .expect("provider row");
            assert_eq!(provider, LEGACY_CONST_API_PROVIDER_ID);
            assert!(!result.details.contains_key("session_restored_files"));
            assert!(!result.details.contains_key("session_restored_sqlite_rows"));
        });
    }

    #[test]
    fn apply_codex_config_repairs_new_sqlite_session_visibility_for_existing_const_api_sessions() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let session_path = codex
                .join("sessions")
                .join("2026")
                .join("07")
                .join("session.jsonl");
            fs::create_dir_all(session_path.parent().expect("session parent"))
                .expect("session dir");
            fs::write(
                &session_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\",\"model_provider\":\"const_api\",\"cwd\":\"/Users/ashely/code/chan.const\"}}\n",
                    "{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\"}}\n"
                ),
            )
            .expect("session write");

            let sqlite_dir = codex.join("sqlite");
            fs::create_dir_all(&sqlite_dir).expect("sqlite dir");
            let state_path = sqlite_dir.join("codex-dev.db");
            let conn = Connection::open(&state_path).expect("state open");
            conn.execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT NOT NULL, has_user_event INTEGER, cwd TEXT)",
                [],
            )
            .expect("create threads");
            conn.execute(
                "INSERT INTO threads (id, model_provider, has_user_event, cwd) VALUES ('t1', 'const_api', 0, '')",
                [],
            )
            .expect("insert thread");
            drop(conn);

            let result =
                apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("apply codex");

            let conn = Connection::open(&state_path).expect("state reopen");
            let row: (String, i64, String) = conn
                .query_row(
                    "SELECT model_provider, has_user_event, cwd FROM threads WHERE id = 't1'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("thread row");
            assert_eq!(
                row,
                (
                    CODEX_CONST_API_PROVIDER_ID.to_string(),
                    1,
                    "/Users/ashely/code/chan.const".to_string()
                )
            );
            assert_eq!(
                result.details.get("session_migrated_sqlite_rows"),
                Some(&"3".to_string())
            );
            drop(conn);
            let repeated =
                apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("repeat apply");
            assert_eq!(
                repeated.details.get("session_migrated_sqlite_rows"),
                Some(&"0".to_string())
            );
            assert!(
                !repeated
                    .backups
                    .iter()
                    .any(|path| path.contains("codex-state")),
                "an idempotent repeat must not back up an unchanged Codex database"
            );
        });
    }

    #[test]
    fn apply_codex_config_syncs_desktop_local_thread_catalog_provider() {
        with_temp_home(|home| {
            let codex = home.join(".codex");
            let session_path = codex
                .join("sessions")
                .join("2026")
                .join("07")
                .join("05")
                .join("session.jsonl");
            fs::create_dir_all(session_path.parent().expect("session parent"))
                .expect("session dir");
            fs::write(
                &session_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\",\"model_provider\":\"openai\",\"cwd\":\"/Users/ashely/code/chan.const\"}}\n",
                    "{\"type\":\"user_message\",\"payload\":{\"text\":\"hello\"}}\n",
                ),
            )
            .expect("session write");

            let sqlite_dir = codex.join("sqlite");
            fs::create_dir_all(&sqlite_dir).expect("sqlite dir");
            let catalog_path = sqlite_dir.join("codex-dev.db");
            let conn = Connection::open(&catalog_path).expect("open sqlite");
            conn.execute_batch(
                "CREATE TABLE local_thread_catalog (
                    host_id TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    display_title TEXT NOT NULL,
                    source_created_at REAL NOT NULL,
                    source_updated_at REAL NOT NULL,
                    cwd TEXT NOT NULL,
                    source_kind TEXT NOT NULL,
                    source_detail TEXT,
                    model_provider TEXT NOT NULL,
                    git_branch TEXT,
                    observation_sequence INTEGER NOT NULL,
                    missing_candidate INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (host_id, thread_id)
                );
                CREATE TABLE local_thread_catalog_metadata (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    catalog_revision INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO local_thread_catalog_metadata (id, catalog_revision) VALUES (1, 3);",
            )
            .expect("create catalog");
            conn.execute(
                "INSERT INTO local_thread_catalog (
                    host_id, thread_id, display_title, source_created_at, source_updated_at,
                    cwd, source_kind, source_detail, model_provider, git_branch,
                    observation_sequence, missing_candidate
                ) VALUES ('local', 't1', 'hello', 1.0, 2.0, '', 'vscode', NULL, 'openai', 'main', 1, 0)",
                [],
            )
            .expect("insert catalog");
            drop(conn);

            let result =
                apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("apply codex");
            assert_eq!(
                read_codex_session_provider(&session_path).expect("read provider"),
                Some(CODEX_CONST_API_PROVIDER_ID.to_string())
            );
            let conn = Connection::open(&catalog_path).expect("open sqlite");
            let (provider, cwd, revision): (String, String, i64) = conn
                .query_row(
                    "SELECT catalog.model_provider, catalog.cwd, metadata.catalog_revision
                     FROM local_thread_catalog catalog
                     CROSS JOIN local_thread_catalog_metadata metadata
                     WHERE catalog.thread_id = 't1' AND metadata.id = 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("catalog row");
            assert_eq!(provider, CODEX_CONST_API_PROVIDER_ID);
            assert_eq!(cwd, "/Users/ashely/code/chan.const");
            assert_eq!(revision, 4);
            assert_eq!(
                result.details.get("session_migrated_sqlite_rows"),
                Some(&"2".to_string())
            );
        });
    }

    #[test]
    fn remove_claude_config_without_manifest_does_not_claim_external_config() {
        with_temp_home(|home| {
            let path = home.join(".claude/settings.json");
            fs::create_dir_all(path.parent().expect("claude parent")).expect("mkdir claude");
            fs::write(
                &path,
                r#"{
  "env": {
    "ANTHROPIC_BASE_URL": "https://api.anthropic.com",
    "ANTHROPIC_AUTH_TOKEN": "sk-test",
    "ANTHROPIC_MODEL": "code-cheap"
  }
}
"#,
            )
            .expect("seed claude");
            let before = fs::read_to_string(&path).expect("before claude");

            let result = remove_claude_config("http://127.0.0.1:38787", "sk-test")
                .expect("unowned Claude config is a no-op");
            assert!(result.files.is_empty());
            assert_eq!(fs::read_to_string(&path).expect("after claude"), before);
        });
    }

    #[test]
    fn remove_claude_config_without_manifest_does_not_claim_localhost_config() {
        with_temp_home(|home| {
            let path = home.join(".claude/settings.json");
            fs::create_dir_all(path.parent().expect("claude parent")).expect("mkdir claude");
            let original = r#"{
  "env": {
    "ANTHROPIC_BASE_URL": "http://localhost:9999/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "sk-test"
  }
}
"#;
            fs::write(&path, original).expect("seed localhost claude");

            let result = remove_claude_config("http://127.0.0.1:38787", "sk-test")
                .expect("unowned localhost config is a no-op");

            assert!(result.files.is_empty());
            assert_eq!(fs::read_to_string(&path).expect("after claude"), original);
        });
    }

    #[test]
    fn remove_gemini_config_without_manifest_does_not_claim_external_config() {
        with_temp_home(|home| {
            let path = home.join(".gemini/.env");
            fs::create_dir_all(path.parent().expect("gemini parent")).expect("mkdir gemini");
            fs::write(
                &path,
                "GOOGLE_GEMINI_BASE_URL=https://generativelanguage.googleapis.com\nGEMINI_API_KEY=sk-test\nGEMINI_MODEL=code-cheap\n",
            )
            .expect("seed gemini");
            let before = fs::read_to_string(&path).expect("before gemini");

            let result = remove_gemini_config("http://127.0.0.1:38787", "sk-test")
                .expect("unowned Gemini config is a no-op");
            assert!(result.files.is_empty());
            assert_eq!(fs::read_to_string(&path).expect("after gemini"), before);
        });
    }

    #[test]
    fn remove_opencode_config_without_manifest_does_not_claim_const_api_slot() {
        with_temp_home(|home| {
            let path = home.join(".config/opencode/opencode.json");
            fs::create_dir_all(path.parent().expect("opencode parent")).expect("mkdir opencode");
            fs::write(
                &path,
                r#"{
  "provider": {
    "const_api": {
      "name": "Personal OpenAI Compatible",
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "https://provider.example/v1",
        "apiKey": "sk-user"
      }
    }
  }
}
"#,
            )
            .expect("seed opencode");
            let before = fs::read_to_string(&path).expect("before opencode");

            let result = remove_opencode_config("http://127.0.0.1:38787/v1", "sk-test")
                .expect("unowned OpenCode config is a no-op");
            assert!(result.files.is_empty());
            assert_eq!(fs::read_to_string(&path).expect("after opencode"), before);
        });
    }

    #[test]
    fn remove_codex_config_without_manifest_does_not_claim_external_const_api_slot() {
        with_temp_home(|home| {
            let path = home.join(".codex/config.toml");
            fs::create_dir_all(path.parent().expect("Codex parent")).expect("mkdir Codex");
            fs::write(
                &path,
                concat!(
                    "model_provider = \"const_api\"\n\n",
                    "[model_providers.const_api]\n",
                    "name = \"Personal OpenAI Compatible\"\n",
                    "base_url = \"https://provider.example/v1\"\n",
                    "wire_api = \"responses\"\n",
                    "experimental_bearer_token = \"sk-user\"\n",
                ),
            )
            .expect("seed Codex");
            let before = fs::read_to_string(&path).expect("before Codex");

            let result = remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test")
                .expect("unowned Codex config is a no-op");

            assert!(result.files.is_empty());
            assert_eq!(fs::read_to_string(&path).expect("after Codex"), before);
        });
    }

    #[test]
    fn remove_openclaw_config_without_manifest_does_not_claim_const_api_slot() {
        with_temp_home(|home| {
            let path = home.join(".openclaw/openclaw.json");
            fs::create_dir_all(path.parent().expect("openclaw parent")).expect("mkdir openclaw");
            fs::write(
                &path,
                r#"{
  "models": {
    "providers": {
      "const_api": {
        "baseUrl": "https://provider.example",
        "apiKey": "sk-user",
        "api": "anthropic-messages"
      }
    }
  }
}
"#,
            )
            .expect("seed openclaw");
            let before = fs::read_to_string(&path).expect("before openclaw");

            let result = remove_openclaw_config("http://127.0.0.1:38787", "sk-test")
                .expect("unowned OpenClaw config is a no-op");
            assert!(result.files.is_empty());
            assert_eq!(fs::read_to_string(&path).expect("after openclaw"), before);
        });
    }

    #[test]
    fn remove_hermes_config_without_manifest_does_not_claim_const_api_slot() {
        with_temp_home(|home| {
            let path = home.join(".hermes/config.yaml");
            fs::create_dir_all(path.parent().expect("hermes parent")).expect("mkdir hermes");
            fs::write(
                &path,
                "custom_providers:\n- name: const_api\n  base_url: https://provider.example/v1\n  api_key: sk-user\n  api_mode: chat_completions\n",
            )
            .expect("seed hermes");
            let before = fs::read_to_string(&path).expect("before hermes");

            let result = remove_hermes_config("http://127.0.0.1:38787", "sk-test")
                .expect("unowned Hermes config is a no-op");
            assert!(result.files.is_empty());
            assert_eq!(fs::read_to_string(&path).expect("after hermes"), before);
        });
    }

    #[test]
    fn apply_codex_config_reports_not_already_configured_when_session_sync_runs() {
        with_temp_home(|home| {
            apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("initial apply");

            let codex = home.join(".codex");
            let session_path = codex
                .join("sessions")
                .join("2026")
                .join("07")
                .join("05")
                .join("session.jsonl");
            fs::create_dir_all(session_path.parent().expect("session parent"))
                .expect("session dir");
            fs::write(
                &session_path,
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\",\"model_provider\":\"openai\",\"cwd\":\"/Users/ashely/code/chan.const\"}}\n",
                    "{\"type\":\"user_message\",\"payload\":{\"text\":\"hello\"}}\n",
                ),
            )
            .expect("session write");

            let sqlite_dir = codex.join("sqlite");
            fs::create_dir_all(&sqlite_dir).expect("sqlite dir");
            let catalog_path = sqlite_dir.join("codex-dev.db");
            let conn = Connection::open(&catalog_path).expect("open sqlite");
            conn.execute_batch(
                "CREATE TABLE local_thread_catalog (
                    host_id TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    display_title TEXT NOT NULL,
                    source_created_at REAL NOT NULL,
                    source_updated_at REAL NOT NULL,
                    cwd TEXT NOT NULL,
                    source_kind TEXT NOT NULL,
                    source_detail TEXT,
                    model_provider TEXT NOT NULL,
                    git_branch TEXT,
                    observation_sequence INTEGER NOT NULL,
                    missing_candidate INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (host_id, thread_id)
                );
                CREATE TABLE local_thread_catalog_metadata (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    catalog_revision INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO local_thread_catalog_metadata (id, catalog_revision) VALUES (1, 7);",
            )
            .expect("create catalog");
            conn.execute(
                "INSERT INTO local_thread_catalog (
                    host_id, thread_id, display_title, source_created_at, source_updated_at,
                    cwd, source_kind, source_detail, model_provider, git_branch,
                    observation_sequence, missing_candidate
                ) VALUES ('local', 't1', 'hello', 1.0, 2.0, '', 'vscode', NULL, 'openai', 'main', 1, 0)",
                [],
            )
            .expect("insert catalog");
            drop(conn);

            let result =
                apply_codex_config("http://127.0.0.1:38787/v1", "sk-test").expect("reapply codex");
            assert!(result.files.is_empty());
            assert!(!result.already_configured);
            assert_eq!(
                result.details.get("session_migrated_sqlite_rows"),
                Some(&"2".to_string())
            );

            let conn = Connection::open(&catalog_path).expect("open sqlite");
            let (provider, revision): (String, i64) = conn
                .query_row(
                    "SELECT catalog.model_provider, metadata.catalog_revision
                     FROM local_thread_catalog catalog
                     CROSS JOIN local_thread_catalog_metadata metadata
                     WHERE catalog.thread_id = 't1' AND metadata.id = 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("catalog row");
            assert_eq!(provider, CODEX_CONST_API_PROVIDER_ID);
            assert_eq!(revision, 8);
        });
    }
