// Paginated history owns absolute byte offsets and ordinals. Provider changes
// must follow Codex's SQLite-only metadata path, not edit the rollout header.
fn indexed_codex_fixture(home: &Path, header_fields: &str) -> (PathBuf, PathBuf, Vec<u8>) {
    let root = home.join(".codex");
    let path = root.join("sessions/2026/09/indexed.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let raw = format!(
        "{{{header_fields}\"type\":\"session_meta\",\"payload\":{{\"id\":\"indexed\",\"history_mode\":\"paginated\",\"model_provider\":\"openai\",\"cwd\":\"/old-checkout\"}}}}\r\n\
         {{\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"review\"}}}}\n\
         {{\"ordinal\":2,\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"审查完成\"}}]}}}}\n"
    ).into_bytes();
    fs::write(&path, &raw).unwrap();
    let state = Connection::open(root.join("state_5.sqlite")).unwrap();
    state.execute_batch(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT, cwd TEXT, has_user_event INTEGER, history_mode TEXT);
         INSERT INTO threads VALUES ('indexed', 'openai', '/current-worktree', 1, 'paginated');"
    ).unwrap();
    fs::create_dir_all(root.join("sqlite")).unwrap();
    let catalog = Connection::open(root.join("sqlite/codex-dev.db")).unwrap();
    catalog
        .execute_batch(
            "CREATE TABLE local_thread_catalog (thread_id TEXT PRIMARY KEY, model_provider TEXT);
         INSERT INTO local_thread_catalog VALUES ('indexed', 'openai');",
        )
        .unwrap();
    let history_path = root.join("thread_history_1.sqlite");
    let history = Connection::open(&history_path).unwrap();
    // Relevant columns from Codex's history schema; no inference or models needed.
    history.execute_batch(
        "CREATE TABLE thread_history_projection_state (thread_id TEXT PRIMARY KEY, next_rollout_byte_offset INTEGER, next_rollout_ordinal INTEGER);
         CREATE TABLE thread_turns (thread_id TEXT, turn_id TEXT, rollout_byte_offset INTEGER, rollout_end_byte_offset INTEGER);
         CREATE TABLE thread_items (thread_id TEXT, item_id TEXT, item_json TEXT);
         INSERT INTO thread_items VALUES ('indexed', 'review-result', '{\"text\":\"审查完成\"}');"
    ).unwrap();
    history
        .execute(
            "INSERT INTO thread_history_projection_state VALUES ('indexed', ?1, 3)",
            [raw.len() as i64],
        )
        .unwrap();
    history
        .execute(
            "INSERT INTO thread_turns VALUES ('indexed', 'review', ?1, ?2)",
            (
                raw.iter().position(|b| *b == b'\n').unwrap() as i64 + 1,
                raw.len() as i64,
            ),
        )
        .unwrap();
    drop(history);
    (path, history_path, raw)
}

#[test]
fn codex_indexed_apply_remove_preserves_every_history_byte_and_offset() {
    with_temp_home(|home| {
        let (path, history_path, mut raw) = indexed_codex_fixture(home, "\"ordinal\":0,");
        let history_before = fs::read(&history_path).unwrap();
        let before_modified = fs::metadata(&path).unwrap().modified().unwrap();
        let applied = apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert_eq!(fs::read(&path).unwrap(), raw);
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            before_modified
        );
        assert_eq!(applied.details["session_migrated_files"], "0");
        assert_eq!(applied.details["session_migrated_sqlite_rows"], "2");
        assert!(!applied
            .details
            .contains_key("session_migrated_sqlite_error"));
        assert_eq!(
            scan_codex_sessions().unwrap().provider_counts[CODEX_CONST_API_PROVIDER_ID],
            1
        );
        let state = Connection::open(home.join(".codex/state_5.sqlite")).unwrap();
        let cwd: String = state
            .query_row("SELECT cwd FROM threads WHERE id='indexed'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            cwd, "/current-worktree",
            "historical header must not overwrite mutable metadata"
        );
        drop(state);

        // Appending a turn while using CONST must not be lost when switching back.
        let appended =
            b"{\"ordinal\":3,\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n";
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(appended)
            .unwrap();
        raw.extend_from_slice(appended);
        let result = remove_codex_config_with_progress_and_mode(
            TOOL_CONFIG_OPENAI_BASE_URL,
            "sk-test",
            ToolConfigRemoveMode::NativeRoute,
            &mut |_, _, _, _| {},
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), raw);
        assert_eq!(
            fs::read(&history_path).unwrap(),
            history_before,
            "migration must not modify or reset history projections"
        );
        assert_eq!(result.details["session_migrated_files"], "0");
        assert_eq!(scan_codex_sessions().unwrap().provider_counts["openai"], 1);
        let repeated = remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert_eq!(repeated.details["session_migrated_sqlite_rows"], "0");
        assert_eq!(fs::read(&path).unwrap(), raw);
    });
}

#[test]
fn codex_indexed_removal_uses_catalog_provider_not_historical_provider() {
    with_temp_home(|home| {
        let (path, history_path, raw) = indexed_codex_fixture(home, "\"ordinal\":0,");
        let archived = home.join(".codex/archived_sessions/indexed.jsonl");
        fs::create_dir_all(archived.parent().unwrap()).unwrap();
        fs::rename(&path, &archived).unwrap();
        let history_before = fs::read(&history_path).unwrap();
        apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        fs::write(
            home.join(".codex/config.toml"),
            "model_provider = \"long-user-owned-provider\"\n",
        )
        .unwrap();
        let restored = remove_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert_eq!(
            restored.details["session_target_provider"],
            "long-user-owned-provider"
        );
        assert_eq!(
            scan_codex_sessions().unwrap().provider_counts["long-user-owned-provider"],
            1
        );
        assert_eq!(fs::read(&archived).unwrap(), raw);
        assert_eq!(fs::read(&history_path).unwrap(), history_before);

        // Switching back also exposes local sessions from independent providers.
        let state = Connection::open(home.join(".codex/state_5.sqlite")).unwrap();
        state
            .execute("UPDATE threads SET model_provider='independent'", [])
            .unwrap();
        drop(state);
        remove_codex_config_with_progress_and_mode(
            TOOL_CONFIG_OPENAI_BASE_URL,
            "sk-test",
            ToolConfigRemoveMode::NativeRoute,
            &mut |_, _, _, _| {},
        )
        .unwrap();
        assert_eq!(
            scan_codex_sessions().unwrap().provider_counts["openai"],
            1
        );
        assert_eq!(fs::read(&archived).unwrap(), raw);
        assert_eq!(fs::read(&history_path).unwrap(), history_before);
    });
}

#[test]
fn codex_removal_migrates_new_independent_sessions_to_native_or_current_provider_once() {
    for (mode, target) in [(ToolConfigRemoveMode::NativeRoute, "openai"), (ToolConfigRemoveMode::RestorePreConst, "custom-target")] {
        with_temp_home(|home| {
            let (indexed, history, raw) = indexed_codex_fixture(home, "\"ordinal\":0,");
            let history_before = fs::read(&history).unwrap();
            apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
            fs::write(home.join(".codex/config.toml"), "model_provider = \"custom-target\"\n").unwrap();
            // A newly imported independent legacy session did not participate in
            // the original apply. Reverse migration must include it too.
            let legacy = home.join(".codex/archived_sessions/independent.jsonl");
            fs::create_dir_all(legacy.parent().unwrap()).unwrap();
            let body = "{\"type\":\"response_item\",\"payload\":{\"text\":\"review result\"}}\n";
            fs::write(&legacy, format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"independent\",\"model_provider\":\"external\"}}}}\n{body}")).unwrap();
            let state = Connection::open(home.join(".codex/state_5.sqlite")).unwrap();
            state.execute("INSERT INTO threads (id, model_provider) VALUES ('independent', 'external')", []).unwrap();
            drop(state);
            let result = remove_codex_config_with_progress_and_mode(
                TOOL_CONFIG_OPENAI_BASE_URL, "sk-test", mode, &mut |_, _, _, _| {},
            ).unwrap();
            assert_eq!(result.details["session_target_provider"], target);
            assert_eq!(scan_codex_sessions().unwrap().provider_counts[target], 2);
            assert_eq!(fs::read_to_string(&legacy).unwrap().split_once('\n').unwrap().1, body);
            assert_eq!(fs::read(&indexed).unwrap(), raw);
            assert_eq!(fs::read(&history).unwrap(), history_before);
            let repeated = remove_codex_config_with_progress_and_mode(
                TOOL_CONFIG_OPENAI_BASE_URL, "sk-test", mode, &mut |_, _, _, _| {},
            ).unwrap();
            assert_eq!(repeated.details["session_migrated_files"], "0");
            assert_eq!(repeated.details["session_migrated_sqlite_rows"], "0");
        });
    }
}

#[test]
fn codex_indexed_unknown_or_numbered_formats_never_use_legacy_rewrite() {
    for raw in [
        "{\"type\":\"session_meta\",\"payload\":{\"history_mode\":\"paginated\",\"model_provider\":\"openai\"}}\n",
        "{\"type\":\"session_meta\",\"payload\":{\"history_mode\":\"future-mode\",\"model_provider\":\"openai\"}}\n",
        "{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{\"model_provider\":\"openai\"}}\n",
        "{\"type\":\"session_meta\",\"payload\":{\"history_mode\":{\"version\":2},\"model_provider\":\"openai\"}}\n",
    ] {
        with_temp_home(|home| {
            let path = home.join("indexed.jsonl");
            fs::write(&path, raw).unwrap();
            let err = rewrite_codex_session_provider(&path, CODEX_CONST_API_PROVIDER_ID, &mut |_| {}).unwrap_err();
            assert!(err.to_string().contains("CODEX_SESSION_INDEXED_HISTORY"));
            assert_eq!(fs::read_to_string(path).unwrap(), raw);
        });
    }
}

#[test]
fn codex_indexed_created_under_const_stays_visible_after_native_removal() {
    with_temp_home(|home| {
        let (path, history_path, raw) = indexed_codex_fixture(home, "\"ordinal\":0,");
        // This models a new file created by Codex itself while CONST was active,
        // not a pre-existing rollout whose header CONST is allowed to edit.
        let raw = String::from_utf8(raw)
            .unwrap()
            .replace("openai", "CONST_API")
            .into_bytes();
        fs::write(&path, &raw).unwrap();
        let state = Connection::open(home.join(".codex/state_5.sqlite")).unwrap();
        state
            .execute("UPDATE threads SET model_provider='CONST_API'", [])
            .unwrap();
        let catalog = Connection::open(home.join(".codex/sqlite/codex-dev.db")).unwrap();
        catalog
            .execute(
                "UPDATE local_thread_catalog SET model_provider='CONST_API'",
                [],
            )
            .unwrap();
        let history = Connection::open(&history_path).unwrap();
        history
            .execute(
                "UPDATE thread_history_projection_state SET next_rollout_byte_offset=?1",
                [raw.len() as i64],
            )
            .unwrap();
        drop(history);
        drop(catalog);
        drop(state);
        let history_before = fs::read(&history_path).unwrap();
        let removed = remove_codex_config_with_progress_and_mode(
            TOOL_CONFIG_OPENAI_BASE_URL,
            "sk-test",
            ToolConfigRemoveMode::NativeRoute,
            &mut |_, _, _, _| {},
        )
        .unwrap();
        assert_eq!(removed.details["session_target_provider"], "openai");
        assert_eq!(removed.details["session_migrated_sqlite_rows"], "2");
        assert_eq!(scan_codex_sessions().unwrap().provider_counts["openai"], 1);
        assert_eq!(fs::read(path).unwrap(), raw);
        assert_eq!(fs::read(history_path).unwrap(), history_before);
    });
}

#[test]
fn codex_indexed_missing_or_corrupt_catalog_reports_error_without_rewriting() {
    for corrupt in [false, true] {
        with_temp_home(|home| {
            let path = home.join(".codex/sessions/indexed.jsonl");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let raw = "{\"ordinal\":0,\"type\":\"session_meta\",\"payload\":{\"id\":\"indexed\",\"history_mode\":\"paginated\",\"model_provider\":\"openai\"}}\n";
            fs::write(&path, raw).unwrap();
            if corrupt {
                fs::write(home.join(".codex/state_5.sqlite"), b"not a database").unwrap();
            }
            let result = apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
            assert!(result.details.contains_key("session_migrated_sqlite_error"));
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
            assert!(!home.join(".codex/thread_history_1.sqlite").exists());
        });
    }
}

#[test]
fn codex_indexed_custom_sqlite_home_is_used_without_touching_stale_indexes() {
    with_temp_home(|home| {
        let (path, history_path, raw) = indexed_codex_fixture(home, "\"ordinal\":0,");
        let external = home.join("custom-sqlite");
        fs::create_dir_all(&external).unwrap();
        fs::rename(
            home.join(".codex/state_5.sqlite"),
            external.join("state_5.sqlite"),
        )
        .unwrap();
        let stale = Connection::open(home.join(".codex/state_5.sqlite")).unwrap();
        stale.execute_batch("CREATE TABLE threads (id TEXT, model_provider TEXT); INSERT INTO threads VALUES ('indexed', 'stale');").unwrap();
        fs::write(
            home.join(".codex/config.toml"),
            format!("sqlite_home = '{}'\n", external.display()),
        )
        .unwrap();
        let history_before = fs::read(&history_path).unwrap();
        apply_codex_config(TOOL_CONFIG_OPENAI_BASE_URL, "sk-test").unwrap();
        assert_eq!(
            scan_codex_sessions().unwrap().provider_counts[CODEX_CONST_API_PROVIDER_ID],
            1
        );
        let old: String = stale
            .query_row("SELECT model_provider FROM threads", [], |r| r.get(0))
            .unwrap();
        assert_eq!(old, "stale");
        assert_eq!(fs::read(&path).unwrap(), raw);
        assert_eq!(fs::read(&history_path).unwrap(), history_before);
    });
}
