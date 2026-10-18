fn create_octop_test_database(home: &Path) {
    let root = home.join(".octop");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("octop.db");
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE providers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                base_url TEXT,
                api_key TEXT,
                extra_json TEXT,
                models_json TEXT,
                note TEXT,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO providers
                (name, kind, base_url, api_key, models_json, enabled, created_at, updated_at)
            VALUES
                ('test', 'openai', 'http://127.0.0.1:38787/v1', 'test-key',
                 '[{\"id\":\"existing-model\",\"name\":\"existing-model\",\"enabled\":true}]',
                 1, 1, 1);
            INSERT INTO settings (key, value) VALUES ('active_model', 'test/existing-model');",
        )
        .unwrap();
}

#[test]
fn octop_upsert_manages_only_const_provider_and_selects_first_model() {
    with_temp_home(|home| {
        create_octop_test_database(home);
        let models = tool_models_from_ids(&["gpt-6-luna".to_string(), "gpt-6-sol".to_string()]);
        let applied = apply_octop_config(TOOL_CONFIG_ROOT_URL, "secret-key", &models).unwrap();
        assert!(!applied.already_configured);
        assert_eq!(applied.details.get("model_count"), Some(&"2".to_string()));
        assert!(!applied.backups.is_empty());

        let connection = rusqlite::Connection::open(home.join(".octop/octop.db")).unwrap();
        let providers = connection
            .prepare("SELECT name, base_url, api_key, note, models_json FROM providers ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].0, "test");
        assert_eq!(providers[1].0, OCTOP_PROVIDER_NAME);
        assert_eq!(providers[1].1, "http://127.0.0.1:38787/v1");
        assert_eq!(providers[1].2, "secret-key");
        assert_eq!(providers[1].3.as_deref(), Some(OCTOP_PROVIDER_NOTE));
        let models_json: serde_json::Value = serde_json::from_str(&providers[1].4).unwrap();
        assert_eq!(models_json.as_array().unwrap().len(), 2);
        assert_eq!(models_json[0]["enabled"], true);
        assert!(models_json[0]["input"]
            .as_array()
            .is_some_and(|input| input.iter().any(|value| value == "text")));
        assert_eq!(
            connection
                .query_row(
                    "SELECT value FROM settings WHERE key = 'active_model'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "CONST API/gpt-6-luna"
        );

        let second = apply_octop_config(TOOL_CONFIG_ROOT_URL, "secret-key", &models).unwrap();
        assert!(second.already_configured);
        assert!(
            second.backups.is_empty(),
            "idempotent Octop sync should not rewrite the DB"
        );
    });
}

#[test]
fn octop_remove_restores_previous_active_model_and_preserves_other_provider() {
    with_temp_home(|home| {
        create_octop_test_database(home);
        let models = tool_models_from_ids(&["gpt-6-luna".to_string()]);
        apply_octop_config(TOOL_CONFIG_ROOT_URL, "secret-key", &models).unwrap();
        remove_octop_config().unwrap();

        let connection = rusqlite::Connection::open(home.join(".octop/octop.db")).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM providers WHERE name = ?1",
                    [OCTOP_PROVIDER_NAME],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM providers WHERE name = 'test'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT value FROM settings WHERE key = 'active_model'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "test/existing-model"
        );
    });
}

#[test]
fn octop_refuses_to_overwrite_unmanaged_const_api_provider() {
    with_temp_home(|home| {
        create_octop_test_database(home);
        let connection = rusqlite::Connection::open(home.join(".octop/octop.db")).unwrap();
        connection
            .execute(
                "INSERT INTO providers
                 (name, kind, base_url, api_key, models_json, enabled, created_at, updated_at)
                 VALUES (?1, 'openai', 'http://user.example/v1', 'user-key', '[]', 1, 1, 1)",
                [OCTOP_PROVIDER_NAME],
            )
            .unwrap();
        let error = apply_octop_config(
            TOOL_CONFIG_ROOT_URL,
            "secret-key",
            &tool_models_from_ids(&["gpt-6-luna".to_string()]),
        )
        .expect_err("unmanaged provider must not be overwritten");
        assert!(error.to_string().contains("OCTOP_PROVIDER_NAME_CONFLICT"));
    });
}
