use super::*;

#[derive(Default)]
struct MemorySecrets {
    values: HashMap<String, String>,
    writes: usize,
    fail_once_on: Option<String>,
}

impl Secrets for MemorySecrets {
    fn read(&mut self, id: &str) -> Result<Option<String>> {
        Ok(self.values.get(id).cloned())
    }
    fn write(&mut self, id: &str, value: Option<&str>) -> Result<()> {
        if self.fail_once_on.as_deref() == Some(id) {
            self.fail_once_on = None;
            return Err(anyhow!("mock keychain unavailable"));
        }
        self.writes += 1;
        if let Some(value) = value {
            self.values.insert(id.into(), value.into());
        } else {
            self.values.remove(id);
        }
        Ok(())
    }
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");
    let db = Connection::open(&path).unwrap();
    // Schema checked against the official Copilot App 1.1.23 database.
    db.execute_batch("PRAGMA foreign_keys=ON;
        CREATE TABLE model_providers(id TEXT PRIMARY KEY,name TEXT NOT NULL,type TEXT NOT NULL,
            settings_json TEXT NOT NULL,created_at TEXT DEFAULT '',updated_at TEXT DEFAULT '',account_id TEXT);
        CREATE TABLE provider_models(id TEXT PRIMARY KEY,
            provider_id TEXT REFERENCES model_providers(id) ON DELETE CASCADE,
            model_id TEXT NOT NULL,wire_model TEXT,display_name TEXT NOT NULL,
            max_prompt_tokens INTEGER,max_output_tokens INTEGER,wire_api_override TEXT,
            supported_reasoning_efforts TEXT,created_at TEXT DEFAULT '',updated_at TEXT DEFAULT '',
            UNIQUE(provider_id,model_id));
        CREATE TABLE sessions(id TEXT PRIMARY KEY,provider_id TEXT REFERENCES model_providers(id) ON DELETE SET NULL,
            history TEXT NOT NULL);
        INSERT INTO model_providers(id,name,type,settings_json) VALUES('user-provider','Personal','openai','{}');
        INSERT INTO sessions VALUES('personal-session','user-provider','private history');").unwrap();
    (dir, path)
}

fn models() -> Vec<DesktopModel> {
    let models = [
        ("gpt-native", "openai_responses"),
        ("chat-native", "openai_chat"),
        ("claude-native", "anthropic_messages"),
    ]
    .map(|(id, protocol)| ToolModelInfo {
        id: id.into(),
        display_name: format!("Display {id}"),
        native_protocols: vec![protocol.into()],
        context_tokens: Some(128_000),
        output_tokens: Some(4096),
        reasoning_efforts: vec!["low".into(), "high".into()],
        ..Default::default()
    });
    desired_models(&models, ToolProtocol::OpenAiResponses).unwrap()
}

#[test]
fn desktop_per_model_protocols_and_limits_match_native_schema() {
    let desired = models();
    let find = |id| desired.iter().find(|m| m.model == id).unwrap();
    assert_eq!(find("gpt-native").wire_api.as_deref(), Some("responses"));
    assert_eq!(find("chat-native").wire_api.as_deref(), Some("completions"));
    assert_eq!(find("claude-native").provider, PROVIDERS[1].0);
    assert_eq!(find("claude-native").wire_api, None);
    assert_eq!(find("gpt-native").context, Some(128_000));
    assert_eq!(find("gpt-native").output, Some(4096));
    assert_eq!(
        provider_settings(TOOL_CONFIG_OPENAI_BASE_URL, "anthropic")["baseUrl"],
        "http://127.0.0.1:38787/anthropic"
    );
}

#[test]
fn desktop_status_preserves_presence_for_an_incomplete_provider() {
    let (_dir, path) = fixture();
    let mut secrets = MemorySecrets::default();
    let check = |secrets: &mut MemorySecrets| {
        check_at(&path, TOOL_CONFIG_OPENAI_BASE_URL, "mock", secrets).unwrap()
    };
    assert!(
        !check(&mut secrets)
            .details
            .contains_key("has_managed_config")
    );
    apply_at(
        &path,
        TOOL_CONFIG_OPENAI_BASE_URL,
        Some("mock"),
        &models(),
        &mut secrets,
    )
    .unwrap();
    assert!(check(&mut secrets).already_configured);
    secrets.values.clear();
    let partial = check(&mut secrets);
    assert!(!partial.already_configured);
    assert_eq!(partial.details["has_managed_config"], "true");
    assert!(partial.files.is_empty());
    apply_at(&path, "", None, &[], &mut secrets).unwrap();
    assert!(
        !check(&mut secrets)
            .details
            .contains_key("has_managed_config")
    );
}

#[test]
fn desktop_inserts_models_in_admin_order_not_hashed_id_order() {
    let (_dir, path) = fixture();
    let mut secrets = MemorySecrets::default();
    let models = ["z-priority", "m-priority", "a-priority"]
        .into_iter()
        .enumerate()
        .map(|(index, id)| ToolModelInfo {
            id: id.into(),
            display_priority: 3 - index as i32,
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let desired = desired_models(&models, ToolProtocol::OpenAiResponses).unwrap();
    apply_at(
        &path,
        TOOL_CONFIG_OPENAI_BASE_URL,
        Some("mock"),
        &desired,
        &mut secrets,
    )
    .unwrap();
    let db = Connection::open(path).unwrap();
    let mut statement = db
        .prepare("SELECT model_id FROM provider_models ORDER BY rowid")
        .unwrap();
    let actual = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(actual, ["z-priority", "m-priority", "a-priority"]);
}

#[test]
fn desktop_preview_apply_repeat_remove_preserves_other_rows_and_sessions() {
    let (_dir, path) = fixture();
    let mut secrets = MemorySecrets::default();
    let desired = models();
    let preview = preview_tool_config_apply(|| {
        apply_at(
            &path,
            TOOL_CONFIG_OPENAI_BASE_URL,
            Some("mock-key"),
            &desired,
            &mut secrets,
        )
    })
    .unwrap();
    assert!(!preview.already_configured);
    assert!(preview.files.is_empty());
    assert_eq!(secrets.writes, 0);
    let db = open_database(&path, true).unwrap();
    assert!(current_provider(&db, PROVIDERS[0].0).unwrap().is_none());
    apply_at(
        &path,
        TOOL_CONFIG_OPENAI_BASE_URL,
        Some("mock-key"),
        &desired,
        &mut secrets,
    )
    .unwrap();
    assert!(
        check_at(&path, TOOL_CONFIG_OPENAI_BASE_URL, "mock-key", &mut secrets)
            .unwrap()
            .already_configured
    );
    assert!(
        !check_at(
            &path,
            TOOL_CONFIG_OPENAI_BASE_URL,
            "changed-key",
            &mut secrets
        )
        .unwrap()
        .already_configured
    );
    db.execute(
        "INSERT INTO sessions VALUES('managed-session',?1,'keep managed history')",
        [PROVIDERS[0].0],
    )
    .unwrap();
    db.execute("INSERT INTO provider_models(id,provider_id,model_id,display_name) VALUES('user-added',?1,'extra','Extra')", [PROVIDERS[0].0]).unwrap();
    let writes = secrets.writes;
    assert!(
        apply_at(
            &path,
            TOOL_CONFIG_OPENAI_BASE_URL,
            Some("mock-key"),
            &desired,
            &mut secrets
        )
        .unwrap()
        .already_configured
    );
    assert_eq!(secrets.writes, writes);
    apply_at(&path, "", None, &[], &mut secrets).unwrap();
    assert!(secrets.values.is_empty());
    assert!(current_provider(&db, "user-provider").unwrap().is_some());
    assert!(current_provider(&db, PROVIDERS[0].0).unwrap().is_none());
    assert_eq!(
        db.query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT provider_id FROM sessions WHERE id='managed-session'",
            [],
            |r| r.get::<_, Option<String>>(0)
        )
        .unwrap(),
        None
    );
    assert!(
        !apply_at(&path, "", None, &[], &mut secrets)
            .unwrap()
            .file_statuses[0]
            .changed
    );
}

#[test]
fn desktop_keychain_failure_rolls_back_sql_and_previous_key() {
    let (_dir, path) = fixture();
    let desired = models();
    let mut secrets = MemorySecrets::default();
    apply_at(
        &path,
        TOOL_CONFIG_OPENAI_BASE_URL,
        Some("old-key"),
        &desired,
        &mut secrets,
    )
    .unwrap();
    secrets.fail_once_on = Some(PROVIDERS[1].0.into());
    assert!(
        apply_at(
            &path,
            "http://127.0.0.1:9999/v1",
            Some("new-key"),
            &desired,
            &mut secrets
        )
        .is_err()
    );
    assert!(
        check_at(&path, TOOL_CONFIG_OPENAI_BASE_URL, "old-key", &mut secrets)
            .unwrap()
            .already_configured
    );
}

#[test]
fn desktop_missing_or_future_schema_does_not_create_tables_or_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.db");
    let mut secrets = MemorySecrets::default();
    assert!(
        !check_at(&path, "", "", &mut secrets)
            .unwrap()
            .already_configured
    );
    assert!(apply_at(&path, "", Some("key"), &models(), &mut secrets).is_err());
    assert!(!path.exists());
    Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE untouched(value TEXT)")
        .unwrap();
    assert!(apply_at(&path, "", Some("key"), &models(), &mut secrets).is_err());
    assert_eq!(secrets.writes, 0);
}

#[test]
fn desktop_fallback_protocol_metadata_is_readable_and_rolls_back_on_failure() {
    super::super::tests::with_temp_home(|_| {
        assert_eq!(
            configured_protocol().unwrap(),
            ToolProtocol::OpenAiResponses
        );
        let path = protocol_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"protocol":"anthropic_messages"}"#).unwrap();
        assert_eq!(
            configured_protocol().unwrap(),
            ToolProtocol::AnthropicMessages
        );
        let model = ToolModelInfo {
            id: "mock-model".into(),
            ..Default::default()
        };
        assert!(
            apply(
                TOOL_CONFIG_OPENAI_BASE_URL,
                "fake-key",
                &[model],
                ToolProtocol::OpenAiChat
            )
            .is_err()
        );
        assert_eq!(
            configured_protocol().unwrap(),
            ToolProtocol::AnthropicMessages
        );
    });
}

#[test]
fn desktop_catalog_updates_our_models_and_rotates_keys() {
    let (_dir, path) = fixture();
    let mut secrets = MemorySecrets::default();
    let mut desired = models();
    apply_at(
        &path,
        TOOL_CONFIG_OPENAI_BASE_URL,
        Some("old-key"),
        &desired,
        &mut secrets,
    )
    .unwrap();
    desired.retain(|m| m.model != "chat-native");
    apply_at(
        &path,
        TOOL_CONFIG_OPENAI_BASE_URL,
        Some("new-key"),
        &desired,
        &mut secrets,
    )
    .unwrap();
    assert_eq!(
        current_models(&open_database(&path, false).unwrap(), PROVIDERS[0].0)
            .unwrap()
            .len(),
        1
    );
    assert!(
        check_at(&path, TOOL_CONFIG_OPENAI_BASE_URL, "new-key", &mut secrets)
            .unwrap()
            .already_configured
    );
}

#[test]
#[ignore = "explicit isolated official-app validation; never touches the real app profile"]
fn copilot_desktop_official_app_contract() {
    // The OS store is shared even when COPILOT_HOME is isolated. Never replace
    // an existing real managed key while running this explicit smoke test.
    for (id, _) in PROVIDERS {
        let key = SystemSecrets.read(id).unwrap();
        assert!(key.is_none() || key.as_deref() == Some("const-api-isolated-fake-key"));
    }
    let path = std::env::temp_dir().join("const-tools-audit/copilot-isolated/data.db");
    assert!(path.exists(), "initialize the isolated official app first");
    let probe_models = [
        ("const-api-local-probe", "openai_chat"),
        ("const-api-responses-probe", "openai_responses"),
        ("const-api-messages-probe", "anthropic_messages"),
    ]
    .map(|(id, protocol)| ToolModelInfo {
        id: id.into(),
        display_name: "CONST API isolated probe".into(),
        native_protocols: vec![protocol.into()],
        context_tokens: Some(128_000),
        output_tokens: Some(4096),
        ..Default::default()
    });
    let desired = desired_models(&probe_models, ToolProtocol::OpenAiResponses).unwrap();
    if std::env::var("CONST_API_COPILOT_PROBE_REMOVE").as_deref() == Ok("1") {
        apply_at(&path, "", None, &[], &mut SystemSecrets).unwrap();
    } else {
        apply_at(
            &path,
            "http://127.0.0.1:43879/v1",
            Some("const-api-isolated-fake-key"),
            &desired,
            &mut SystemSecrets,
        )
        .unwrap();
        assert!(
            check_at(
                &path,
                "http://127.0.0.1:43879/v1",
                "const-api-isolated-fake-key",
                &mut SystemSecrets
            )
            .unwrap()
            .already_configured
        );
    }
}
