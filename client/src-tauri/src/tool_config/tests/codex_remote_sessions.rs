#[test]
fn codex_session_sync_preserves_remote_catalog_rows_in_both_directions() {
    with_temp_home(|home| {
        let root = home.join(".codex");
        fs::create_dir_all(root.join("sessions")).unwrap();
        fs::create_dir_all(root.join("sqlite")).unwrap();
        for id in ["shared", "shared-null"] {
            let content = format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"openai\",\"cwd\":\"/local/worktree\"}}}}\n\
                 {{\"type\":\"user_message\",\"payload\":{{\"text\":\"keep history\"}}}}\n"
            );
            fs::write(root.join("sessions").join(format!("{id}.jsonl")), content).unwrap();
        }
        let catalog_path = root.join("sqlite/codex-dev.db");
        let conn = Connection::open(&catalog_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE local_thread_catalog (
                host_id TEXT NOT NULL, thread_id TEXT NOT NULL,
                model_provider TEXT, cwd TEXT, display_title TEXT,
                source_kind TEXT, source_updated_at REAL, observation_sequence INTEGER,
                PRIMARY KEY (host_id, thread_id)
             );
             CREATE TABLE local_thread_catalog_metadata (
                id INTEGER PRIMARY KEY, catalog_revision INTEGER NOT NULL
             );
             INSERT INTO local_thread_catalog_metadata VALUES (1, 3);
             INSERT INTO local_thread_catalog VALUES
                ('local', 'shared', 'openai', '', 'keep local title', 'vscode', 2.0, 1),
                ('local', 'shared-null', NULL, '', 'keep local title', 'cli', 2.0, 2);",
        )
        .unwrap();
        for (host, kind) in [
            ("ssh:fixture", "ssh"),
            ("wsl:fixture", "wsl"),
            ("remote-control:fixture", "remote-control"),
            ("chatgpt", "chatgpt"),
            ("future-host", "unknown"),
            ("", "unknown"),
        ] {
            for (id, provider) in [
                ("shared", Some("openai")),
                ("shared-null", None),
                ("remote-only", Some(CODEX_CONST_API_PROVIDER_ID)),
            ] {
                conn.execute(
                    "INSERT INTO local_thread_catalog VALUES (?1, ?2, ?3, '/remote/worktree', 'keep remote title', ?4, 9.0, 8)",
                    (host, id, provider, kind),
                ).unwrap();
            }
        }
        let remote_snapshot = || {
            let mut stmt = conn.prepare(
                "SELECT * FROM local_thread_catalog WHERE host_id <> 'local' ORDER BY host_id, thread_id"
            ).unwrap();
            stmt.query_map([], |row| {
                (0..row.as_ref().column_count())
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
        };
        let before = remote_snapshot();
        let before_check = fs::read(&catalog_path).unwrap();
        check_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert_eq!(fs::read(&catalog_path).unwrap(), before_check);

        let planned =
            plan_codex_state_db_provider_updates(CODEX_CONST_API_PROVIDER_ID, &HashMap::new())
                .unwrap();
        assert_eq!(
            planned.updates.len(),
            2,
            "only local rows belong in the plan"
        );
        let applied = apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert_eq!(applied.details["session_migrated_files"], "2");
        assert_eq!(applied.details["session_migrated_sqlite_rows"], "4");
        assert_eq!(remote_snapshot(), before);
        for id in ["shared", "shared-null"] {
            let local: (String, String, String) = conn.query_row(
                "SELECT model_provider, cwd, display_title FROM local_thread_catalog WHERE host_id = 'local' AND thread_id = ?1",
                [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).unwrap();
            assert_eq!(
                local,
                (
                    CODEX_CONST_API_PROVIDER_ID.into(),
                    "/local/worktree".into(),
                    "keep local title".into()
                )
            );
        }
        let repeated = apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert_eq!(repeated.details["session_migrated_sqlite_rows"], "0");
        assert_eq!(remote_snapshot(), before);

        let removed = remove_codex_config_with_progress_and_mode(
            TOOL_CONFIG_OPENAI_BASE_URL,
            "sk-test",
            ToolConfigRemoveMode::NativeRoute,
            &mut |_, _, _, _| {},
        )
        .unwrap();
        assert_eq!(removed.details["session_migrated_sqlite_rows"], "2");
        assert_eq!(remote_snapshot(), before);
        let providers: i64 = conn.query_row(
            "SELECT COUNT(*) FROM local_thread_catalog WHERE host_id = 'local' AND model_provider = 'openai' AND cwd = '/local/worktree'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(providers, 2);
        let revision: i64 = conn
            .query_row(
                "SELECT catalog_revision FROM local_thread_catalog_metadata WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 5, "only apply and remove change the catalog");
    });
}

#[test]
fn codex_session_sync_leaves_a_remote_only_catalog_unchanged() {
    with_temp_home(|home| {
        let dir = home.join(".codex/sqlite");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("codex-dev.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE local_thread_catalog (
                host_id TEXT NOT NULL, thread_id TEXT NOT NULL, model_provider TEXT,
                PRIMARY KEY (host_id, thread_id)
             );
             INSERT INTO local_thread_catalog VALUES
                ('ssh:fixture', 'remote', 'CONST_API'),
                ('chatgpt', 'cloud', NULL);
             CREATE TABLE local_thread_catalog_metadata (id INTEGER PRIMARY KEY, catalog_revision INTEGER);
             INSERT INTO local_thread_catalog_metadata VALUES (1, 7);"
        ).unwrap();
        drop(conn);
        let before = fs::read(&path).unwrap();
        for provider in [CODEX_CONST_API_PROVIDER_ID, "openai"] {
            let result =
                sync_codex_sessions_to_provider_inner(provider, &mut |_, _, _, _| {}).unwrap();
            assert!(result.sqlite_error.is_none());
            assert_eq!(result.migrated_sqlite_rows, 0);
            assert_eq!(fs::read(&path).unwrap(), before);
        }
        assert!(!const_api_state_path(CODEX_SESSION_SYNC_MANIFEST).exists());
    });
}

#[test]
fn codex_session_sync_rejects_catalog_without_host_identity() {
    with_temp_home(|home| {
        let dir = home.join(".codex/sqlite");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("codex-dev.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE local_thread_catalog (thread_id TEXT PRIMARY KEY, model_provider TEXT);
             INSERT INTO local_thread_catalog VALUES ('unknown-owner', 'openai');",
        )
        .unwrap();
        drop(conn);
        let before = fs::read(&path).unwrap();
        let result = sync_codex_sessions_to_provider_inner(
            CODEX_CONST_API_PROVIDER_ID,
            &mut |_, _, _, _| {},
        )
        .unwrap();
        assert!(
            result
                .sqlite_error
                .as_deref()
                .unwrap()
                .contains("has no host_id")
        );
        assert_eq!(result.migrated_sqlite_rows, 0);
        assert_eq!(fs::read(&path).unwrap(), before);
    });
}
