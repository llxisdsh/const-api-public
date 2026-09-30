use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn model() -> ToolModelInfo {
    ToolModelInfo {
        id: "provider-model-id".into(),
        display_name: "Friendly model name".into(),
        context_tokens: Some(128_000),
        output_tokens: Some(4096),
        input_modalities: vec!["text".into(), "image".into()],
        native_protocols: vec!["anthropic_messages".into()],
        ..Default::default()
    }
}

fn desired(models: &[ToolModelInfo], key: &str) -> Vec<DesiredModel> {
    desired_models(
        "trae-cn",
        TOOL_CONFIG_ROOT_URL,
        key,
        models,
        ToolProtocol::OpenAiChat,
    )
    .unwrap()
}

#[test]
fn new_models_encrypt_credentials_and_keep_ids_and_advanced_capabilities() {
    for tool in ["trae", "trae-cn", "trae-work"] {
        let entries = desired_models(
            tool,
            "http://127.0.0.1:38788/v1",
            "mock-key",
            &[model()],
            ToolProtocol::OpenAiChat,
        )
        .unwrap();
        let body = &entries[0].body;
        assert_eq!(body["model_name"], "provider-model-id");
        assert_eq!(body["display_name"], "CONST API · Friendly model name");
        assert_eq!(body["provider"], "custom_anthropic_compatible");
        assert_eq!(
            body["base_url"],
            "http://127.0.0.1:38788/anthropic/v1/messages"
        );
        assert_ne!(body["ak"], "mock-key");
        assert_eq!(
            auth::decode_model_key(body["ak"].as_str().unwrap()).as_deref(),
            Some("mock-key")
        );
        assert_eq!(
            body["config_detail"]["model_hyper_params"],
            json!({"prompt_max_tokens":128000,"max_tokens":4096,"thinking_enable":0})
        );
        assert_eq!(body["config_detail"]["max_turn"], 500);
        assert_eq!(body["config_detail"]["multimodal"], true);
        assert!(body.get("custom_model_type").is_none());
    }
}

#[tokio::test]
async fn all_editions_write_and_read_mixed_protocols_with_default_thinking() {
    let models = crate::tool_model_metadata::tool_models_from_response(&json!({"data":[
        {"id":"gpt-6-luna","const_api":{"native_protocols":["openai_chat","openai_responses"],"preferred_protocol":"openai_chat"}},
        {"id":"claude-opus-4-6","const_api":{"native_protocols":["openai_chat","openai_responses","anthropic_messages"],"preferred_protocol":"openai_responses"}},
        {"id":"chat-only","const_api":{"native_protocols":["openai_chat"]}}
    ]}));
    for tool in ["trae", "trae-cn", "trae-work"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owned.json");
        let vendor = Vendor::start().await;
        let mut config = prepared(
            &path,
            &vendor,
            Manifest::default(),
            desired_models(
                tool,
                TOOL_CONFIG_ROOT_URL,
                "mock",
                &models,
                ToolProtocol::OpenAiChat,
            )
            .unwrap(),
        );
        config.tool = tool.into();
        config.commit(|_, _| {}).await.unwrap();
        let remote = vendor.state.lock().unwrap().models.clone();
        for (id, provider, url) in [
            ("gpt-6-luna", "custom_responses_compatible", "/v1/responses"),
            (
                "claude-opus-4-6",
                "custom_anthropic_compatible",
                "/anthropic/v1/messages",
            ),
            (
                "chat-only",
                "custom_openai_compatible",
                "/v1/chat/completions",
            ),
        ] {
            let entry = remote.iter().find(|entry| entry["name"] == id).unwrap();
            assert_eq!(entry["provider"], provider);
            assert_eq!(entry["base_url"], format!("{TOOL_CONFIG_ROOT_URL}{url}"));
            assert_eq!(
                entry["config_detail"]["model_hyper_params"]["thinking_enable"],
                0
            );
        }
        let mut config = prepared(
            &path,
            &vendor,
            load(&path),
            desired_models(
                tool,
                TOOL_CONFIG_ROOT_URL,
                "mock",
                &models,
                ToolProtocol::OpenAiChat,
            )
            .unwrap(),
        );
        config.tool = tool.into();
        assert!(config.preview().already_configured);
        let writes = vendor.state.lock().unwrap().writes;
        config.commit(|_, _| {}).await.unwrap();
        assert_eq!(vendor.state.lock().unwrap().writes, writes);
    }
}

#[tokio::test]
async fn protocol_migration_keeps_user_options_and_verifies_before_retiring_old_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let mut info = reasoning_model("gpt-6-luna");
    let mut old = desired(&[info.clone()], "mock").remove(0);
    old.body["config_detail"]["model_hyper_params"]["thinking_enable"] = json!(1);
    old.advanced.insert("thinking_enable".into(), json!(1));
    vendor.seed(42, &old);
    let manifest = Manifest {
        accounts: BTreeMap::from([(
            "mock-account".into(),
            BTreeMap::from([(old.key.clone(), owned(42, &old))]),
        )]),
        tools: BTreeMap::from([(
            "mock-account".into(),
            BTreeMap::from([("trae-cn".into(), BTreeSet::from([old.key.clone()]))]),
        )]),
        ..Default::default()
    };
    {
        let mut state = vendor.state.lock().unwrap();
        let detail = &mut state.models[0]["config_detail"];
        detail["model_hyper_params"]["thinking_enable"] = json!(2);
        detail["model_hyper_params"]["temperature"] = json!(0.3);
        detail["model_hyper_params"]["prompt_max_tokens"] = json!(64000);
        detail["max_turn"] = json!(64);
    }
    info.native_protocols = vec!["openai_responses".into()];
    prepared(&path, &vendor, manifest, desired(&[info.clone()], "mock"))
        .commit(|_, _| {})
        .await
        .unwrap();
    let state = vendor.state.lock().unwrap();
    assert_eq!(state.models.len(), 1);
    assert_ne!(state.models[0]["custom_model_id"], 42);
    assert_eq!(state.models[0]["provider"], "custom_responses_compatible");
    let detail = &state.models[0]["config_detail"];
    assert_eq!(detail["max_turn"], 64);
    assert_eq!(detail["model_hyper_params"]["thinking_enable"], 2);
    assert_eq!(detail["model_hyper_params"]["temperature"], 0.3);
    assert_eq!(detail["model_hyper_params"]["prompt_max_tokens"], 64000);
    let deletion = state
        .requests
        .iter()
        .position(|(_, body)| body["action"] == "delete")
        .unwrap();
    assert!(
        state.requests[..deletion]
            .iter()
            .any(|(path, _)| path.contains("get_detail_param"))
    );
    let writes = state.writes;
    drop(state);
    // A later refresh must not mistake copied user choices for managed defaults.
    prepared(&path, &vendor, load(&path), desired(&[info], "mock"))
        .commit(|_, _| {})
        .await
        .unwrap();
    assert_eq!(vendor.state.lock().unwrap().writes, writes);
}

#[test]
fn thinking_default_migrates_only_the_managed_value() {
    let old = AdvancedState {
        applied: Fields::from([("thinking_enable".into(), json!(1))]),
        ..Default::default()
    };
    let target = Fields::from([("thinking_enable".into(), json!(0))]);
    assert_eq!(
        capabilities::plan(Some(&old), &old.applied, &target).pending,
        target
    );
    let user_off = Fields::from([("thinking_enable".into(), json!(2))]);
    assert!(
        capabilities::plan(Some(&old), &user_off, &target)
            .pending
            .is_empty()
    );
    assert!(
        capabilities::plan(None, &old.applied, &target)
            .pending
            .is_empty(),
        "untracked on is not ours to overwrite"
    );
}

fn owned(id: u64, desired: &DesiredModel) -> OwnedModel {
    OwnedModel {
        name: desired.name.clone(),
        provider: desired.provider.clone(),
        registration: Registration::Ready { id },
        connection: desired.connection.clone(),
        advanced: Some(AdvancedState {
            applied: desired.advanced.clone(),
            pending: Fields::new(),
            user_overrides: Fields::new(),
            image_input_limited: false,
        }),
    }
}

#[test]
fn local_status_keeps_presence_separate_from_account_readiness() {
    let entry = desired(&[model()], "mock-key").remove(0);
    for tool in ["trae", "trae-cn", "trae-work"] {
        let mut manifest = Manifest {
            accounts: BTreeMap::from([(
                "account".into(),
                BTreeMap::from([(entry.key.clone(), owned(42, &entry))]),
            )]),
            tools: BTreeMap::from([(
                "account".into(),
                BTreeMap::from([(tool.into(), BTreeSet::from([entry.key.clone()]))]),
            )]),
            ..Default::default()
        };
        let check = |manifest: &Manifest, account: &str, key: &str| {
            check_account_config(tool, account, manifest, TOOL_CONFIG_ROOT_URL, key)
        };
        assert!(check(&manifest, "account", "mock-key").already_configured);
        let changed_key = check(&manifest, "account", "new-key");
        assert!(!changed_key.already_configured);
        assert_eq!(changed_key.details["has_managed_config"], "true");
        manifest
            .accounts
            .get_mut("account")
            .unwrap()
            .get_mut(&entry.key)
            .unwrap()
            .advanced
            .as_mut()
            .unwrap()
            .pending
            .insert("multimodal".into(), json!(true));
        let partial = check(&manifest, "account", "mock-key");
        assert!(!partial.already_configured);
        assert_eq!(partial.details["has_managed_config"], "true");
        let pending = manifest
            .accounts
            .get_mut("account")
            .unwrap()
            .get_mut(&entry.key)
            .unwrap();
        pending.advanced.as_mut().unwrap().pending.clear();
        pending.registration = Registration::Pending {
            before_ids: vec![],
            confirmed: false,
        };
        let pending_registration = check(&manifest, "account", "mock-key");
        assert!(!pending_registration.already_configured);
        assert_eq!(pending_registration.details["has_managed_config"], "true");
        let other_account = check(&manifest, "another-account", "mock-key");
        assert!(!other_account.already_configured);
        assert!(!other_account.details.contains_key("has_managed_config"));
        // An interrupted cancellation may remove the tool reference before all
        // account models have been deleted. Those remaining records still count.
        manifest.tools.clear();
        let partial_removal = check(&manifest, "account", "mock-key");
        assert!(!partial_removal.already_configured);
        assert_eq!(partial_removal.details["has_managed_config"], "true");
        manifest.accounts.get_mut("account").unwrap().clear();
        assert_eq!(
            check(&manifest, "account", "mock-key").details["has_managed_config"],
            "false"
        );
    }
}

#[test]
fn editions_keep_separate_sign_ins_but_merge_proven_account_ownership() {
    assert!(
        auth::data_root("trae-work")
            .unwrap()
            .ends_with("TRAE SOLO CN")
    );
    assert_ne!(
        auth::data_root("trae-work").unwrap(),
        auth::data_root("trae-cn").unwrap()
    );
    let entry = desired(&[model()], "mock-key").remove(0);
    let ready = owned(42, &entry);
    let mut pending = ready.clone();
    pending.registration = Registration::Pending {
        before_ids: vec![42],
        confirmed: false,
    };
    let legacy = |account: &str, model: OwnedModel| Manifest {
        accounts: BTreeMap::from([(account.into(), BTreeMap::from([(entry.key.clone(), model)]))]),
        default_protocols: BTreeMap::from([(account.into(), "openai_chat".into())]),
        ..Default::default()
    };
    let mut merged = Manifest::default();
    merge_legacy_manifest(&mut merged, "trae-cn", legacy("cn-account", pending)).unwrap();
    merge_legacy_manifest(
        &mut merged,
        "trae-work",
        legacy("cn-account", ready.clone()),
    )
    .unwrap();
    merge_legacy_manifest(&mut merged, "trae", legacy("sg-account", ready.clone())).unwrap();
    assert_eq!(merged.accounts.len(), 2);
    assert_eq!(merged.accounts["cn-account"].len(), 1);
    assert!(matches!(
        merged.accounts["cn-account"][&entry.key].registration,
        Registration::Ready { id: 42 }
    ));
    assert_eq!(merged.tools["cn-account"].len(), 2);
    assert_eq!(
        merged.default_protocols["cn-account/trae-cn"],
        "openai_chat"
    );
    let mut conflict = ready;
    conflict.registration = Registration::Ready { id: 43 };
    assert!(
        merge_legacy_manifest(&mut merged, "trae-work", legacy("cn-account", conflict)).is_err()
    );
}

#[test]
fn uncertain_adds_do_not_adopt_an_unproven_user_model() {
    let mut owned = BTreeMap::from([(
        "key".into(),
        OwnedModel {
            name: "same-name".into(),
            provider: "custom_openai_compatible".into(),
            connection: "hash".into(),
            advanced: None,
            registration: Registration::Pending {
                before_ids: vec![7],
                confirmed: false,
            },
        },
    )]);
    let remote = vec![RemoteModel {
        id: 8,
        name: "same-name".into(),
        provider: "custom_openai_compatible".into(),
        ..Default::default()
    }];
    assert!(reconcile_pending(&mut owned, &remote, false).is_err());
    owned.get_mut("key").unwrap().registration = Registration::Pending {
        before_ids: vec![7],
        confirmed: true,
    };
    assert!(reconcile_pending(&mut owned, &[], true).is_err());
    reconcile_pending(&mut owned, &remote, false).unwrap();
    assert!(remote_matches(&owned["key"], &remote[0]));
    assert!(!remote_matches(
        &owned["key"],
        &RemoteModel {
            id: 9,
            ..remote[0].clone()
        }
    ));
    owned.get_mut("key").unwrap().registration = Registration::Pending {
        before_ids: vec![7],
        confirmed: true,
    };
    reconcile_pending(&mut owned, &[], false).unwrap();
    assert!(owned.is_empty());
}

#[derive(Default)]
struct VendorState {
    models: Vec<Value>,
    requests: Vec<(String, Value)>,
    writes: usize,
    reject_write: Option<usize>,
    ignore_updates: bool,
    series: BTreeMap<String, Vec<String>>,
}

// Same flattened detail response as the installed Trae model service, not the
// add/update payload. The editor reads the two different shapes separately.
fn vendor_details(models: &[Value]) -> Value {
    json!({"config_info_list": models.iter().map(|model| {
        let detail = &model["config_detail"];
        let hyper = &detail["model_hyper_params"];
        let mut custom = json!({
            "custom_model_id": model["custom_model_id"],
            "ak": model["ak"], "base_url": model["base_url"],
        });
        for key in ["thinking_enable", "temperature", "top_p", "top_k"] {
            if let Some(value) = hyper.get(key) { custom[key] = value.clone(); }
        }
        let mut item = json!({
            "model_name": model["name"], "provider": model["provider"],
            "custom_config": custom,
        });
        for key in ["prompt_max_tokens", "max_tokens"] {
            if let Some(value) = hyper.get(key) { item[key] = value.clone(); }
        }
        if let Some(value) = detail.get("max_turn") { item["max_turn"] = value.clone(); }
        let mut config = json!({"config_name": model["name"],
            "display_config": {"multimodal": detail["multimodal"].as_bool().unwrap_or(false)},
            "model_detail_list": [item]});
        if let Some(value) = model.get("custom_model_type") { config["custom_model_type"] = value.clone(); }
        config
    }).collect::<Vec<_>>()})
}

struct Vendor {
    session: TraeSession,
    state: Arc<Mutex<VendorState>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Vendor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Vendor {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(VendorState::default()));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let (path, body) = read_request(&mut socket).await;
                let response = {
                    let mut state = shared.lock().unwrap();
                    state.requests.push((path.clone(), body.clone()));
                    if path.contains("/get_custom_model_type_config") {
                        let types = state
                            .series
                            .get(body["provider_id"].as_str().unwrap())
                            .cloned()
                            .unwrap_or_default();
                        json!({"custom_model_type_list": types.into_iter().map(|name|
                            json!({"type_name":name})).collect::<Vec<_>>()})
                    } else if path.contains("/model_list") {
                        // The legacy summary omits the address, even for valid
                        // user-added models. Do not use it for read-back validation.
                        let mut summary = state.models.clone();
                        for model in &mut summary {
                            model["base_url"] = json!("");
                        }
                        json!({"model_configs":summary})
                    } else if path.contains("/get_detail_param") {
                        assert_eq!(
                            body,
                            json!({
                                "function": "chat", "show_custom_model": true, "need_prompt": false
                            })
                        );
                        vendor_details(&state.models)
                    } else {
                        state.writes += 1;
                        if state.reject_write == Some(state.writes) {
                            json!({"code":42,"message":"rejected key=mock-secret"})
                        } else {
                            if path.contains("/add_custom_model") {
                                let id = 1000 + state.writes as u64;
                                let mut added = body.clone();
                                added["custom_model_id"] = json!(id);
                                added["name"] = body["model_name"].clone();
                                state.models.insert(0, added);
                            } else if body["action"] == "delete" {
                                state
                                    .models
                                    .retain(|model| model["custom_model_id"] != body["id"]);
                            } else if !state.ignore_updates {
                                let model = state
                                    .models
                                    .iter_mut()
                                    .find(|m| m["custom_model_id"] == body["id"])
                                    .unwrap();
                                for field in [
                                    "ak",
                                    "base_url",
                                    "auth_type",
                                    "custom_model_type",
                                    "config_detail",
                                ] {
                                    if let Some(value) = body.get(field) {
                                        model[field] = value.clone();
                                    }
                                }
                            }
                            json!({"code":0})
                        }
                    }
                };
                let bytes = serde_json::to_vec(&response).unwrap();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).as_bytes()).await.unwrap();
                socket.write_all(&bytes).await.unwrap();
            }
        });
        Self {
            session: TraeSession {
                host,
                account: "mock-account".into(),
                client: reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
            },
            state,
            task,
        }
    }
    fn session(&self) -> TraeSession {
        TraeSession {
            host: self.session.host.clone(),
            account: self.session.account.clone(),
            client: self.session.client.clone(),
        }
    }
    fn remote(&self) -> Vec<RemoteModel> {
        parse_remote_models(&vendor_details(&self.state.lock().unwrap().models)).unwrap()
    }
    fn seed(&self, id: u64, entry: &DesiredModel) {
        let mut body = entry.body.clone();
        body["name"] = json!(entry.name);
        body["custom_model_id"] = json!(id);
        self.state.lock().unwrap().models.push(body);
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 4096];
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let len = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + len {
                return (
                    headers.lines().next().unwrap().to_string(),
                    if len == 0 {
                        Value::Null
                    } else {
                        serde_json::from_slice(&bytes[end + 4..end + 4 + len]).unwrap()
                    },
                );
            }
        }
    }
}

fn prepared(
    path: &Path,
    vendor: &Vendor,
    mut manifest: Manifest,
    desired: Vec<DesiredModel>,
) -> PreparedTraeConfig {
    manifest
        .accounts
        .entry(vendor.session.account.clone())
        .or_default();
    PreparedTraeConfig {
        tool: "trae-cn".into(),
        manifest_path: path.into(),
        session: vendor.session(),
        manifest,
        remote: vendor.remote(),
        desired,
        protocol: ToolProtocol::OpenAiChat,
        remove: false,
        recovered_pending: false,
        _lock: None,
    }
}
fn load(path: &Path) -> Manifest {
    read_manifest(path).unwrap().unwrap()
}

#[test]
fn authoritative_model_details_keep_exact_ids_and_reject_unverifiable_connections() {
    let entry = desired(&[model()], "mock-key").remove(0);
    // IDs must never pass through an f64 or be matched by display name.
    let id = 9_007_199_254_740_993_u64;
    let mut response = json!({"config_info_list": [
        {"model_detail_list": [{"model_name": "built-in", "provider": "builtin"}]},
        {"config_name": "Friendly name is not the wire ID", "model_detail_list": [{
            "model_name": entry.name, "provider": entry.provider,
            "custom_config": {
                "custom_model_id": id,
                "ak": entry.body["ak"], "base_url": entry.body["base_url"]
            }
        }]}
    ]});
    let parsed = parse_remote_models(&response).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id, id);
    assert_eq!(parsed[0].name, entry.name);
    assert_eq!(parsed[0].connection.as_ref(), Some(&entry.connection));

    let custom = &mut response["config_info_list"][1]["model_detail_list"][0]["custom_config"];
    custom["custom_model_id"] = json!(id.to_string());
    custom["base_url"] = json!("");
    let parsed = parse_remote_models(&response).unwrap();
    assert_eq!(parsed[0].id, id);
    assert!(parsed[0].connection.is_none());
    assert!(parse_remote_models(&json!({"model_configs": []})).is_err());
    assert!(parse_remote_models(&json!({"config_info_list": [{}]})).is_err());
}

#[tokio::test]
async fn summary_based_journal_recovers_without_rewriting_shared_models() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let entry = desired(&[model()], "mock-key").remove(0);
    vendor.seed(42, &entry);
    let original = vendor.state.lock().unwrap().models.clone();
    let mut prior = owned(42, &entry);
    prior.connection = connection_digest("", "mock-key", &entry.provider);
    let mut manifest = Manifest {
        accounts: BTreeMap::from([(
            "mock-account".into(),
            BTreeMap::from([(entry.key.clone(), prior)]),
        )]),
        tools: BTreeMap::from([(
            "mock-account".into(),
            BTreeMap::from([
                ("trae-cn".into(), BTreeSet::from([entry.key.clone()])),
                ("trae-work".into(), BTreeSet::from([entry.key.clone()])),
            ]),
        )]),
        default_protocols: BTreeMap::from([(
            "mock-account/trae-work".into(),
            "openai_chat".into(),
        )]),
    };
    let remote = vendor.session.list().await.unwrap();
    reconcile_pending(
        manifest.accounts.get_mut("mock-account").unwrap(),
        &remote,
        false,
    )
    .unwrap();
    let config = prepared(&path, &vendor, manifest, vec![entry]);
    assert!(
        !config.preview().already_configured,
        "CN still needs its local protocol record"
    );
    config.commit(|_, _| {}).await.unwrap();
    let saved = load(&path);
    assert_eq!(
        saved.default_protocols["mock-account/trae-cn"],
        "openai_chat"
    );
    let repeat = prepared(&path, &vendor, saved, desired(&[model()], "mock-key"));
    assert!(repeat.preview().already_configured);
    repeat.commit(|_, _| {}).await.unwrap();
    let state = vendor.state.lock().unwrap();
    assert_eq!(state.writes, 0, "no add, update, or delete is needed");
    assert_eq!(state.models, original, "preserve IDs and advanced settings");
    assert_eq!(
        state.requests.len(),
        1,
        "one authoritative read for the whole catalog"
    );
}

#[tokio::test]
async fn lifecycle_repairs_old_plaintext_and_preserves_ids_and_advanced_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let entry = desired(&[model()], "first-key").remove(0);
    vendor.seed(42, &entry);
    vendor.seed(7, &entry);
    {
        let mut state = vendor.state.lock().unwrap();
        state.models[0]["ak"] = json!("first-key");
        state.models[0]["display_name"] = json!("my title");
        state.models[0]["config_detail"] = json!({"max_turn":64,"custom_future_option":true});
    }
    let mut manifest = Manifest::default();
    let mut old = owned(42, &entry);
    old.connection = "legacy-plaintext-digest".into();
    manifest.accounts.insert(
        "mock-account".into(),
        BTreeMap::from([(entry.key.clone(), old)]),
    );
    manifest
        .accounts
        .insert("another-account".into(), BTreeMap::new());
    let first = prepared(&path, &vendor, manifest, vec![entry]);
    assert!(!first.preview().already_configured);
    first.commit(|_, _| {}).await.unwrap();
    let repeat = prepared(
        &path,
        &vendor,
        load(&path),
        desired(&[model()], "first-key"),
    );
    assert!(repeat.preview().already_configured);
    repeat.commit(|_, _| {}).await.unwrap();
    assert_eq!(
        vendor.state.lock().unwrap().requests.len(),
        2,
        "one repair + one verification; reconfigure writes nothing"
    );
    let changed = prepared(&path, &vendor, load(&path), desired(&[model()], "new-key"));
    assert!(!changed.preview().already_configured);
    changed.commit(|_, _| {}).await.unwrap();
    {
        let state = vendor.state.lock().unwrap();
        assert_eq!(state.models[0]["custom_model_id"], 42);
        assert_eq!(state.models[0]["display_name"], "my title");
        assert_eq!(
            state.models[0]["config_detail"],
            json!({"max_turn":64,"custom_future_option":true})
        );
        assert_eq!(
            auth::decode_model_key(state.models[0]["ak"].as_str().unwrap()).as_deref(),
            Some("new-key")
        );
        assert_eq!(
            auth::decode_model_key(state.models[1]["ak"].as_str().unwrap()).as_deref(),
            Some("first-key")
        );
        for (_, body) in state
            .requests
            .iter()
            .filter(|(path, _)| path.contains("/update_custom_model"))
        {
            assert!(body.get("display_name").is_none());
            assert!(body.get("config_detail").is_none());
        }
    }
    let mut removal = prepared(&path, &vendor, load(&path), vec![]);
    removal.remove = true;
    removal.commit(|_, _| {}).await.unwrap();
    assert_eq!(
        vendor
            .remote()
            .iter()
            .map(|model| model.id)
            .collect::<Vec<_>>(),
        [7]
    );
    let saved = load(&path);
    assert!(saved.accounts["mock-account"].is_empty());
    assert!(saved.accounts.contains_key("another-account"));
    let disk = fs::read_to_string(&path).unwrap();
    assert!(!disk.contains("first-key") && !disk.contains("new-key"));
}

#[tokio::test]
async fn same_account_editions_share_models_and_remove_only_their_reference() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    prepared(
        &path,
        &vendor,
        Manifest::default(),
        desired(&[model()], "mock-key"),
    )
    .commit(|_, _| {})
    .await
    .unwrap();
    let id = vendor.remote()[0].id;
    let mut work = prepared(&path, &vendor, load(&path), desired(&[model()], "mock-key"));
    work.tool = "trae-work".into();
    work.commit(|_, _| {}).await.unwrap();
    assert_eq!(
        vendor.state.lock().unwrap().writes,
        1,
        "no duplicate account-wide add"
    );
    let mut remove_cn = prepared(&path, &vendor, load(&path), vec![]);
    remove_cn.remove = true;
    remove_cn.commit(|_, _| {}).await.unwrap();
    assert_eq!(vendor.remote()[0].id, id, "Work still uses this entry");
    let mut remove_work = prepared(&path, &vendor, load(&path), vec![]);
    remove_work.tool = "trae-work".into();
    remove_work.remove = true;
    remove_work.commit(|_, _| {}).await.unwrap();
    assert!(vendor.remote().is_empty());
    assert_eq!(vendor.state.lock().unwrap().writes, 2);
}

#[tokio::test]
async fn large_catalog_has_exact_priority_write_order_one_list_and_noop_reconfiguration() {
    let count = 468;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let models = (0..count)
        .map(|index| ToolModelInfo {
            id: format!("model-{index}"),
            display_priority: (count - index) as i32,
            ..model()
        })
        .collect::<Vec<_>>();
    let config = prepared(
        &path,
        &vendor,
        Manifest::default(),
        desired(&models, "mock-key"),
    );
    let mut progress = Vec::new();
    config
        .commit(|done, total| progress.push((done, total)))
        .await
        .unwrap();
    {
        let state = vendor.state.lock().unwrap();
        assert_eq!(state.writes, count);
        assert_eq!(
            state.requests.len(),
            count + 1,
            "one final list instead of 117"
        );
        assert_eq!(
            state
                .models
                .iter()
                .map(|value| value["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            state.requests[..count]
                .iter()
                .map(|(_, body)| body["model_name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            models
                .iter()
                .rev()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(progress.first(), Some(&(0, count)));
    assert_eq!(progress.last(), Some(&(count, count)));
    assert!(progress.windows(2).all(|pair| pair[0].0 <= pair[1].0));
    let saved = load(&path);
    assert_eq!(saved.accounts["mock-account"].len(), count);
    let repeat = prepared(&path, &vendor, saved, desired(&models, "mock-key"));
    assert!(repeat.preview().already_configured);
    repeat.commit(|_, _| {}).await.unwrap();
    assert_eq!(vendor.state.lock().unwrap().requests.len(), count + 1);
}

#[tokio::test]
async fn failed_add_preserves_uncertainty_and_never_retries_or_echoes_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    vendor.state.lock().unwrap().reject_write = Some(1);
    let error = prepared(
        &path,
        &vendor,
        Manifest::default(),
        desired(&[model()], "mock-secret"),
    )
    .commit(|_, _| {})
    .await
    .unwrap_err()
    .to_string();
    let descriptor: Value = serde_json::from_str(&error).unwrap();
    assert_eq!(descriptor["code"], "tool_config_trae_rejected");
    assert_eq!(descriptor["params"]["vendor_code"], 42);
    assert!(!error.contains("mock-secret"));
    assert!(matches!(
        load(&path).accounts["mock-account"]
            .values()
            .next()
            .unwrap()
            .registration,
        Registration::Pending {
            confirmed: false,
            ..
        }
    ));
    assert_eq!(vendor.state.lock().unwrap().requests.len(), 1);
}

#[tokio::test]
async fn failed_update_batch_drains_inflight_but_starts_no_next_batch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let models = (0..8)
        .map(|index| ToolModelInfo {
            id: format!("model-{index}"),
            ..model()
        })
        .collect::<Vec<_>>();
    let old_entries = desired(&models, "old-key");
    let mut manifest = Manifest::default();
    for (index, entry) in old_entries.iter().enumerate() {
        let id = index as u64 + 1;
        vendor.seed(id, entry);
        manifest
            .accounts
            .entry("mock-account".into())
            .or_default()
            .insert(entry.key.clone(), owned(id, entry));
    }
    vendor.state.lock().unwrap().reject_write = Some(1);
    let result = prepared(&path, &vendor, manifest, desired(&models, "new-key"))
        .commit(|_, _| {})
        .await;
    let descriptor: Value = serde_json::from_str(&result.unwrap_err().to_string()).unwrap();
    assert_eq!(descriptor["params"]["vendor_code"], 42);
    assert_eq!(vendor.state.lock().unwrap().writes, MODEL_WRITE_CONCURRENCY);
    assert_eq!(
        vendor
            .remote()
            .iter()
            .filter(|entry| entry.connection
                == Some(connection_digest(
                    "http://127.0.0.1:38787/anthropic/v1/messages",
                    "new-key",
                    "custom_anthropic_compatible"
                )))
            .count(),
        MODEL_WRITE_CONCURRENCY - 1
    );
}

#[tokio::test]
async fn acknowledged_but_ineffective_update_is_not_reported_as_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let entry = desired(&[model()], "old-key").remove(0);
    vendor.seed(42, &entry);
    let mut manifest = Manifest::default();
    manifest.accounts.insert(
        "mock-account".into(),
        BTreeMap::from([(entry.key.clone(), owned(42, &entry))]),
    );
    vendor.state.lock().unwrap().ignore_updates = true;
    assert!(
        prepared(&path, &vendor, manifest, desired(&[model()], "new-key"))
            .commit(|_, _| {})
            .await
            .is_err()
    );
    assert_eq!(vendor.remote()[0].id, 42);
    assert_eq!(vendor.state.lock().unwrap().requests.len(), 2);
    assert!(
        load(&path).accounts["mock-account"]
            .values()
            .all(|entry| entry.connection != desired(&[model()], "new-key")[0].connection)
    );
}

#[test]
fn migration_is_one_time_and_shared_manifest_writes_are_exclusive() {
    crate::tool_config::tests::with_temp_home(|root| {
        assert!(
            manifest_path().starts_with(root),
            "test must not touch real configuration"
        );
        let entry = desired(&[model()], "mock-key").remove(0);
        let mut legacy = Manifest::default();
        legacy.accounts.insert(
            "mock-account".into(),
            BTreeMap::from([(entry.key.clone(), owned(42, &entry))]),
        );
        let old_path = const_api_state_path("trae-work-managed-models.json");
        save_manifest(&old_path, &legacy).unwrap();
        let migrated = load_manifest().unwrap();
        assert_eq!(migrated.accounts["mock-account"].len(), 1);
        assert!(migrated.tools["mock-account"]["trae-work"].contains(&entry.key));
        save_manifest(&manifest_path(), &Manifest::default()).unwrap();
        assert!(
            load_manifest().unwrap().accounts.is_empty(),
            "empty shared manifest must not re-import removed entries"
        );
        assert!(old_path.exists(), "keep the legacy recovery file");
        let lock = acquire_manifest_lock().unwrap();
        assert!(acquire_manifest_lock().is_err());
        drop(lock);
        assert!(acquire_manifest_lock().is_ok());
    });
}

#[tokio::test]
async fn failed_ordered_add_records_acknowledgements_without_starting_more() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    vendor.state.lock().unwrap().reject_write = Some(3);
    let models = (0..8)
        .map(|index| ToolModelInfo {
            id: format!("model-{index}"),
            ..model()
        })
        .collect::<Vec<_>>();
    assert!(
        prepared(
            &path,
            &vendor,
            Manifest::default(),
            desired(&models, "mock-key")
        )
        .commit(|_, _| {})
        .await
        .is_err()
    );
    assert_eq!(vendor.state.lock().unwrap().writes, 3);
    assert_eq!(
        load(&path).accounts["mock-account"]
            .values()
            .filter(|entry| matches!(
                entry.registration,
                Registration::Pending {
                    confirmed: true,
                    ..
                }
            ))
            .count(),
        2
    );
}

#[tokio::test]
async fn confirmation_digest_ignores_encryption_nonces_and_remote_list_ties() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let models = [
        model(),
        ToolModelInfo {
            id: "second-model".into(),
            ..model()
        },
    ];
    for (i, entry) in desired(&models, "mock-key").iter().enumerate() {
        vendor.seed(i as u64 + 1, entry);
    }
    let first = prepared(
        &path,
        &vendor,
        Manifest::default(),
        desired(&models, "mock-key"),
    );
    let mut second = prepared(
        &path,
        &vendor,
        Manifest::default(),
        desired(&models, "mock-key"),
    );
    second.remote.reverse();
    assert_eq!(
        first.context_digest().unwrap(),
        second.context_digest().unwrap()
    );
    second.remote[0]
        .advanced
        .insert("thinking_enable".into(), json!(2));
    assert_ne!(
        first.context_digest().unwrap(),
        second.context_digest().unwrap()
    );
}

fn reasoning_model(id: &str) -> ToolModelInfo {
    ToolModelInfo {
        id: id.into(),
        reasoning: true,
        reasoning_efforts: vec!["low".into(), "high".into(), "max".into()],
        native_protocols: vec!["openai_chat".into()],
        ..model()
    }
}

#[test]
fn thinking_mode_follows_model_default_and_does_not_invent_builtin_menus() {
    for tool in ["trae", "trae-cn", "trae-work"] {
        let entries = desired_models(
            tool,
            TOOL_CONFIG_ROOT_URL,
            "mock-key",
            &[reasoning_model("gpt-6-luna"), model()],
            ToolProtocol::OpenAiChat,
        )
        .unwrap();
        assert_eq!(
            entries[0].body["config_detail"]["model_hyper_params"]["thinking_enable"],
            0
        );
        assert_eq!(
            entries[1].body["config_detail"]["model_hyper_params"]["thinking_enable"],
            0
        );
        assert_eq!(entries[0].advanced["prompt_max_tokens"], 128000);
        assert!(
            !entries[0]
                .body
                .to_string()
                .contains("reasoning_effort_config")
        );
        assert!(!entries[0].body.to_string().contains("max_mode"));
    }
}

#[test]
fn series_mapping_respects_version_boundaries_and_real_protocols() {
    for (id, expected) in [
        ("gpt-6-luna", Some("gpt6")),
        ("gpt-5.6-luna", Some("gpt5")),
        ("google/gemini-3.1-pro-preview", Some("gemini3")),
        ("deepseek-v4-pro", Some("deepseek-v4")),
        ("gpt-50", None),
        ("gemini-30", None),
        ("gpt-4.1", None),
        ("qwen3.8-flash", None),
        ("claude-opus-5", None),
    ] {
        assert_eq!(
            capabilities::series_hint(&reasoning_model(id)),
            expected,
            "{id}"
        );
    }
    let alias = ToolModelInfo {
        id: "my-alias".into(),
        family: "gpt-5".into(),
        ..reasoning_model("")
    };
    assert_eq!(capabilities::series_hint(&alias), Some("gpt5"));
    let anthropic = ToolModelInfo {
        native_protocols: vec!["anthropic_messages".into()],
        ..reasoning_model("gpt-6-luna")
    };
    assert!(desired(&[anthropic], "mock-key")[0].series_hint.is_none());
    assert!(capabilities::parse_series(&json!({"custom_model_type_list":[{}]})).is_err());
    assert!(capabilities::parse_series(&json!({})).is_err());
}

#[tokio::test]
async fn series_options_are_account_scoped_and_fetched_once_per_connection() {
    let models = [
        reasoning_model("gpt-6-luna"),
        reasoning_model("gpt-5.6-luna"),
        reasoning_model("gemini-3.1-pro-preview"),
    ];
    for (tool, has_gpt6) in [("trae", false), ("trae-cn", true), ("trae-work", true)] {
        let vendor = Vendor::start().await;
        let mut types = vec!["gpt5".into(), "gemini3".into(), "other".into()];
        if has_gpt6 {
            types.push("gpt6".into());
        }
        vendor
            .state
            .lock()
            .unwrap()
            .series
            .insert("custom_openai_compatible".into(), types);
        let mut entries = desired_models(
            tool,
            TOOL_CONFIG_ROOT_URL,
            "mock-key",
            &models,
            ToolProtocol::OpenAiChat,
        )
        .unwrap();
        vendor
            .session
            .resolve_model_series(&mut entries)
            .await
            .unwrap();
        assert_eq!(
            entries
                .iter()
                .find(|model| model.name == "gpt-6-luna")
                .unwrap()
                .body
                .get("custom_model_type"),
            has_gpt6.then_some(&json!("gpt6"))
        );
        assert_eq!(
            entries
                .iter()
                .find(|model| model.name == "gpt-5.6-luna")
                .unwrap()
                .body["custom_model_type"],
            "gpt5"
        );
        assert_eq!(
            entries
                .iter()
                .find(|model| model.name == "gemini-3.1-pro-preview")
                .unwrap()
                .body["custom_model_type"],
            "gemini3"
        );
        assert_eq!(
            entries[2].body["config_detail"]["model_hyper_params"]["prompt_max_tokens"],
            128000
        );
        let state = vendor.state.lock().unwrap();
        assert_eq!(
            state.requests.len(),
            1,
            "one lookup for the entire model group"
        );
        assert_eq!(
            state.requests[0].1["end_point"],
            format!("{TOOL_CONFIG_ROOT_URL}/v1/chat/completions")
        );
        assert_eq!(state.writes, 0);
    }
}

#[test]
fn advanced_merge_preserves_sampling_turns_and_user_overrides() {
    let previous = AdvancedState {
        applied: Fields::from([
            ("thinking_enable".into(), json!(1)),
            ("custom_model_type".into(), json!("gpt5")),
            ("prompt_max_tokens".into(), json!(128000)),
        ]),
        pending: Fields::new(),
        user_overrides: Fields::new(),
        image_input_limited: false,
    };
    let current = Fields::from([
        ("thinking_enable".into(), json!(2)),
        ("custom_model_type".into(), json!("gemini3")),
        ("prompt_max_tokens".into(), json!(128000)),
        ("max_tokens".into(), json!(4096)),
        ("multimodal".into(), json!(false)),
        ("max_turn".into(), json!(64)),
        ("temperature".into(), json!(0.7)),
        ("top_p".into(), json!(0.8)),
        ("top_k".into(), json!(20)),
    ]);
    let target = Fields::from([
        ("thinking_enable".into(), json!(1)),
        ("custom_model_type".into(), json!("gpt6")),
        ("prompt_max_tokens".into(), json!(256000)),
        ("multimodal".into(), json!(true)),
    ]);
    let mut next = capabilities::plan(Some(&previous), &current, &target);
    assert_eq!(
        next.pending,
        Fields::from([("prompt_max_tokens".into(), json!(256000))])
    );
    let mut body = json!({"action":"update","id":42});
    capabilities::write_update(&mut body, &current, &next.pending);
    assert_eq!(
        body["custom_model_type"], "gemini3",
        "keep the user's series"
    );
    assert_eq!(
        body["config_detail"],
        json!({"max_turn":64,"multimodal":false,
        "model_hyper_params":{"prompt_max_tokens":256000,"max_tokens":4096,
        "thinking_enable":2,"temperature":0.7,"top_p":0.8,"top_k":20}})
    );
    assert!(
        next.confirm("test-model", &current).is_err(),
        "an ack is not evidence of the new limit"
    );
    let mut applied = current.clone();
    applied.insert("prompt_max_tokens".into(), json!(256000));
    next.confirm("test-model", &applied).unwrap();
    assert!(next.pending.is_empty());
    assert!(
        capabilities::plan(Some(&next), &applied, &target)
            .pending
            .is_empty()
    );
}

#[tokio::test]
async fn legacy_capabilities_upgrade_in_place_and_reconfigure_without_rewriting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let mut old_model = reasoning_model("gpt-6-luna");
    old_model.reasoning = false;
    let entry = desired(&[old_model], "mock-key").remove(0);
    vendor.seed(42, &entry);
    vendor.seed(7, &entry); // unrelated user entry with the same name and connection
    {
        let mut state = vendor.state.lock().unwrap();
        state.models[0]["display_name"] = json!("My custom title");
        state.models[0]["config_detail"]["max_turn"] = json!(64);
        state.models[0]["config_detail"]["model_hyper_params"]["prompt_max_tokens"] = json!(80000);
        state.models[0]["config_detail"]["model_hyper_params"]["temperature"] = json!(0.7);
    }
    let mut legacy = owned(42, &entry);
    legacy.advanced = None;
    let manifest = Manifest {
        accounts: BTreeMap::from([(
            "mock-account".into(),
            BTreeMap::from([(entry.key.clone(), legacy)]),
        )]),
        ..Default::default()
    };
    let desired_entry = || {
        let mut value = desired(&[reasoning_model("gpt-6-luna")], "mock-key").remove(0);
        value.body["custom_model_type"] = json!("gpt6");
        value
            .advanced
            .insert("custom_model_type".into(), json!("gpt6"));
        value
    };
    let config = prepared(&path, &vendor, manifest, vec![desired_entry()]);
    assert!(!config.preview().already_configured);
    config.commit(|_, _| {}).await.unwrap();
    {
        let state = vendor.state.lock().unwrap();
        let current = &state.models[0];
        assert_eq!(current["custom_model_id"], 42);
        assert_eq!(current["display_name"], "My custom title");
        assert_eq!(current["custom_model_type"], "gpt6");
        assert_eq!(current["config_detail"]["max_turn"], 64);
        assert_eq!(
            current["config_detail"]["model_hyper_params"]["thinking_enable"],
            0
        );
        assert_eq!(
            current["config_detail"]["model_hyper_params"]["prompt_max_tokens"],
            80000
        );
        assert_eq!(
            current["config_detail"]["model_hyper_params"]["temperature"],
            0.7
        );
        assert_eq!(
            state.models[1]["config_detail"]["model_hyper_params"]["thinking_enable"],
            0
        );
        assert_eq!(state.writes, 1);
    }
    let repeat = prepared(&path, &vendor, load(&path), vec![desired_entry()]);
    assert!(repeat.preview().already_configured);
    repeat.commit(|_, _| {}).await.unwrap();
    assert_eq!(vendor.state.lock().unwrap().writes, 1);
    let mut removal = prepared(&path, &vendor, load(&path), vec![]);
    removal.remove = true;
    removal.commit(|_, _| {}).await.unwrap();
    assert_eq!(
        vendor
            .remote()
            .iter()
            .map(|model| model.id)
            .collect::<Vec<_>>(),
        [7]
    );
}

#[tokio::test]
async fn ignored_capability_update_remains_pending_and_recovers_on_explicit_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let mut old_model = reasoning_model("gpt-6-luna");
    old_model.reasoning = false;
    let mut entry = desired(&[old_model], "mock-key").remove(0);
    entry.body["config_detail"]["model_hyper_params"]["thinking_enable"] = json!(1);
    entry.advanced.insert("thinking_enable".into(), json!(1));
    vendor.seed(42, &entry);
    let manifest = Manifest {
        accounts: BTreeMap::from([(
            "mock-account".into(),
            BTreeMap::from([(entry.key.clone(), owned(42, &entry))]),
        )]),
        ..Default::default()
    };
    vendor.state.lock().unwrap().ignore_updates = true;
    let result = prepared(
        &path,
        &vendor,
        manifest,
        desired(&[reasoning_model("gpt-6-luna")], "mock-key"),
    )
    .commit(|_, _| {})
    .await;
    assert!(result.is_err());
    let mut saved = load(&path);
    assert!(saved.accounts["mock-account"][&entry.key].has_pending_write());
    assert_eq!(
        saved.accounts["mock-account"][&entry.key]
            .advanced
            .as_ref()
            .unwrap()
            .pending["thinking_enable"],
        0
    );
    let owned = saved.accounts.get_mut("mock-account").unwrap();
    let mut changed = vendor.remote();
    changed[0]
        .advanced
        .insert("thinking_enable".into(), json!(2));
    assert!(
        reconcile_advanced(owned, &changed).is_err(),
        "do not overwrite an unidentifiable third value"
    );
    reconcile_advanced(owned, &vendor.remote()).unwrap();
    vendor.state.lock().unwrap().ignore_updates = false;
    prepared(
        &path,
        &vendor,
        saved,
        desired(&[reasoning_model("gpt-6-luna")], "mock-key"),
    )
    .commit(|_, _| {})
    .await
    .unwrap();
    assert!(
        load(&path).accounts["mock-account"][&entry.key]
            .advanced
            .as_ref()
            .unwrap()
            .pending
            .is_empty()
    );
    assert_eq!(vendor.remote()[0].advanced["thinking_enable"], 0);
    assert!(!load(&path).accounts["mock-account"][&entry.key].has_pending_write());
}

#[test]
fn native_advanced_readback_keeps_sampling_and_normalizes_editor_defaults() {
    let fields = capabilities::remote_fields(
        &json!({"custom_model_type":"gpt6","display_config":{"multimodal":true}}),
        &json!({"prompt_max_tokens":128000,
        "max_tokens":4096,"max_turn":64,"custom_config":{
            "thinking_enable":null,
            "temperature":0,"top_p":0.9,"top_k":null,"ak":"never-copy-me"
        }}),
    );
    assert_eq!(
        fields,
        Fields::from([
            ("custom_model_type".into(), json!("gpt6")),
            ("multimodal".into(), json!(true)),
            ("thinking_enable".into(), json!(0)),
            ("prompt_max_tokens".into(), json!(128000)),
            ("max_tokens".into(), json!(4096)),
            ("max_turn".into(), json!(64)),
            ("temperature".into(), json!(0)),
            ("top_p".into(), json!(0.9)),
        ])
    );
    assert_eq!(
        capabilities::remote_fields(&json!({}), &json!({})),
        Fields::from([
            ("custom_model_type".into(), json!("other")),
            ("thinking_enable".into(), json!(0)),
        ])
    );
    for config in [json!({}), json!({"display_config":{"multimodal":"false"}})] {
        let mut state = AdvancedState {
            pending: Fields::from([("multimodal".into(), json!(true))]),
            ..Default::default()
        };
        assert!(
            state
                .confirm("model", &capabilities::remote_fields(&config, &json!({})))
                .is_err()
        );
        assert!(
            !state.image_input_limited,
            "missing or invalid vision is not a confirmed limit"
        );
    }
}

#[tokio::test]
async fn pending_advanced_verification_does_not_prevent_owned_model_removal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let entry = desired(&[model()], "mock-key").remove(0);
    vendor.seed(42, &entry);
    vendor.seed(7, &entry);
    vendor.state.lock().unwrap().models[0]["config_detail"]["model_hyper_params"]["thinking_enable"] =
        json!(2);
    let mut pending = owned(42, &entry);
    pending
        .advanced
        .as_mut()
        .unwrap()
        .pending
        .insert("thinking_enable".into(), json!(1));
    let mut manifest = Manifest {
        accounts: BTreeMap::from([(
            "mock-account".into(),
            BTreeMap::from([(entry.key.clone(), pending)]),
        )]),
        ..Default::default()
    };
    assert!(
        reconcile_advanced(
            manifest.accounts.get_mut("mock-account").unwrap(),
            &vendor.remote()
        )
        .is_err()
    );
    let mut removal = prepared(&path, &vendor, manifest, vec![]);
    removal.remove = true;
    removal.commit(|_, _| {}).await.unwrap();
    assert_eq!(
        vendor
            .remote()
            .iter()
            .map(|model| model.id)
            .collect::<Vec<_>>(),
        [7]
    );
    assert!(load(&path).accounts["mock-account"].is_empty());
}

#[tokio::test]
async fn removal_cleans_the_full_owned_history_without_a_supply_catalog() {
    for tool in ["trae", "trae-cn", "trae-work"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owned.json");
        let vendor = Vendor::start().await;
        let models =
            ["still-available", "withdrawn-from-supply", "old-protocol"].map(|id| ToolModelInfo {
                id: id.into(),
                ..model()
            });
        let entries = desired(&models, "mock-key");
        let mut owned_models = BTreeMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let id = index as u64 + 10;
            vendor.seed(id, entry);
            owned_models.insert(entry.key.clone(), owned(id, entry));
        }
        // Same CONST display label, connection and model name, but never owned.
        vendor.seed(7, &entries[0]);
        let manifest = Manifest {
            tools: BTreeMap::from([(
                "mock-account".into(),
                BTreeMap::from([(
                    tool.into(),
                    entries.iter().map(|entry| entry.key.clone()).collect(),
                )]),
            )]),
            accounts: BTreeMap::from([("mock-account".into(), owned_models)]),
            ..Default::default()
        };
        let mut removal = prepared(&path, &vendor, manifest, vec![]);
        removal.tool = tool.into();
        removal.remove = true;
        removal.commit(|_, _| {}).await.unwrap();
        assert_eq!(
            vendor
                .remote()
                .iter()
                .map(|model| model.id)
                .collect::<Vec<_>>(),
            [7]
        );
        assert!(load(&path).accounts["mock-account"].is_empty());
        let state = vendor.state.lock().unwrap();
        assert_eq!(state.requests.len(), 3);
        assert!(state.requests.iter().all(|(path, body)| {
            path.contains("/api/ide/v1/update_custom_model") && body["action"] == "delete"
        }));
    }
}

#[tokio::test]
async fn incomplete_initial_image_fields_retry_owned_ids_then_report_vendor_limitation() {
    // Observed TraeWork state: new models exist and connections match, but two
    // image flags differ while every field is still in the initial journal.
    for ignore_updates in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owned.json");
        let vendor = Vendor::start().await;
        let models = ["glm-5.1", "glm-5.2", "gpt-6-luna"].map(|id| ToolModelInfo {
            id: id.into(),
            ..model()
        });
        let entries = desired(&models, "mock-secret");
        let mut owned_models = BTreeMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let id = index as u64 + 42;
            vendor.seed(id, entry);
            let mut saved = owned(id, entry);
            let advanced = saved.advanced.as_mut().unwrap();
            advanced.pending = std::mem::take(&mut advanced.applied);
            owned_models.insert(entry.key.clone(), saved);
        }
        {
            let mut state = vendor.state.lock().unwrap();
            state.ignore_updates = ignore_updates;
            for model in &mut state.models[..2] {
                model["config_detail"]["multimodal"] = json!(false);
                // Unrelated advanced choices must survive the repair.
                model["config_detail"]["model_hyper_params"]["temperature"] = json!(0.3);
            }
        }
        reconcile_pending(&mut owned_models, &vendor.remote(), false).unwrap();
        reconcile_advanced(&mut owned_models, &vendor.remote()).unwrap();
        let manifest = Manifest {
            accounts: BTreeMap::from([("mock-account".into(), owned_models)]),
            ..Default::default()
        };
        let mut operation = prepared(&path, &vendor, manifest, entries);
        operation.recovered_pending = true;
        let result = operation.commit(|_, _| {}).await.unwrap();
        let saved = load(&path);
        if ignore_updates {
            assert_eq!(
                result.details["trae_image_input_limited"],
                "glm-5.1, glm-5.2"
            );
        } else {
            assert!(!result.details.contains_key("trae_image_input_limited"));
            assert!(
                vendor
                    .remote()
                    .iter()
                    .all(|model| model.advanced["multimodal"] == true)
            );
        }
        assert!(
            saved.accounts["mock-account"]
                .values()
                .all(|model| !model.has_pending_write())
        );
        let repeat = prepared(&path, &vendor, saved, desired(&models, "mock-secret"));
        assert!(repeat.preview().already_configured);
        repeat.commit(|_, _| {}).await.unwrap();
        let state = vendor.state.lock().unwrap();
        assert_eq!(state.models.len(), 3);
        assert_eq!(
            state.writes, 2,
            "repair only the two pending fields, no re-add or delete"
        );
        assert_eq!(
            state.requests.len(),
            3,
            "one final list, not a read per model"
        );
        for (path, body) in &state.requests[..2] {
            assert!(path.contains("/api/ide/v1/update_custom_model"));
            assert_eq!(body["action"], "update");
            assert!([json!(42), json!(43)].contains(&body["id"]));
            assert_eq!(
                body["config_detail"]["model_hyper_params"]["temperature"],
                0.3
            );
        }
    }
}

#[tokio::test]
async fn default_series_can_upgrade_without_downgrading_existing_presets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let initial = desired(&[reasoning_model("gpt-6-luna")], "mock-key");
    let key = initial[0].key.clone();
    prepared(&path, &vendor, Manifest::default(), initial)
        .commit(|_, _| {})
        .await
        .unwrap();
    assert_eq!(
        load(&path).accounts["mock-account"][&key]
            .advanced
            .as_ref()
            .unwrap()
            .applied["custom_model_type"],
        "other"
    );

    let mut available = desired(&[reasoning_model("gpt-6-luna")], "mock-key");
    available[0].body["custom_model_type"] = json!("gpt6");
    available[0]
        .advanced
        .insert("custom_model_type".into(), json!("gpt6"));
    let upgrade = prepared(&path, &vendor, load(&path), available);
    assert!(!upgrade.preview().already_configured);
    upgrade.commit(|_, _| {}).await.unwrap();
    assert_eq!(vendor.remote()[0].advanced["custom_model_type"], "gpt6");
    assert_eq!(vendor.state.lock().unwrap().writes, 2);

    // Disappearance from a future options list is not an instruction to erase
    // an existing preset. Unsupported/unknown series are left unchanged.
    let unavailable = prepared(
        &path,
        &vendor,
        load(&path),
        desired(&[reasoning_model("gpt-6-luna")], "mock-key"),
    );
    assert!(unavailable.preview().already_configured);
    unavailable.commit(|_, _| {}).await.unwrap();
    assert_eq!(vendor.remote()[0].advanced["custom_model_type"], "gpt6");
    assert_eq!(vendor.state.lock().unwrap().writes, 2);
}

#[test]
fn image_limitation_is_one_way_visible_and_clears_when_vendor_enables_it() {
    let target = Fields::from([("multimodal".into(), json!(true))]);
    let actual = Fields::from([("multimodal".into(), json!(false))]);
    let mut state = AdvancedState {
        pending: target.clone(),
        ..Default::default()
    };
    state.confirm("glm-5.1", &actual).unwrap();
    assert!(state.image_input_limited);
    assert_eq!(state.applied, actual);
    assert!(state.pending.is_empty());
    let repeat = capabilities::plan(Some(&state), &actual, &target);
    assert!(repeat.pending.is_empty());
    assert!(repeat.image_input_limited);
    assert!(!capabilities::plan(Some(&state), &target, &target).image_input_limited);
    assert!(!capabilities::plan(Some(&state), &actual, &actual).image_input_limited);

    let mut inverse = AdvancedState {
        pending: actual.clone(),
        ..Default::default()
    };
    assert!(inverse.confirm("text-only", &target).is_err());
    assert!(!inverse.image_input_limited);
    assert_eq!(inverse.pending, actual);

    let mut strict = AdvancedState {
        pending: Fields::from([
            ("max_tokens".into(), json!(4096)),
            ("thinking_enable".into(), json!(0)),
        ]),
        ..Default::default()
    };
    let error = strict
        .confirm(
            "model",
            &Fields::from([
                ("max_tokens".into(), json!(1024)),
                ("thinking_enable".into(), json!(0)),
            ]),
        )
        .unwrap_err();
    assert!(error.to_string().contains("max_tokens"));
    assert_eq!(strict.pending.len(), 1);
    assert_eq!(
        strict.applied["thinking_enable"], 0,
        "matched fields are retained even on failure"
    );
}

#[tokio::test]
async fn stale_image_limit_is_cleared_by_local_commit_without_remote_writes() {
    for remote_enabled in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owned.json");
        let vendor = Vendor::start().await;
        let mut info = model();
        if !remote_enabled {
            info.input_modalities = vec!["text".into()];
        }
        let entry = desired(&[info.clone()], "mock-key").remove(0);
        prepared(
            &path,
            &vendor,
            Manifest::default(),
            desired(&[info.clone()], "mock-key"),
        )
        .commit(|_, _| {})
        .await
        .unwrap();
        let mut manifest = load(&path);
        let state = manifest
            .accounts
            .get_mut("mock-account")
            .unwrap()
            .get_mut(&entry.key)
            .unwrap()
            .advanced
            .as_mut()
            .unwrap();
        state.applied.insert("multimodal".into(), json!(false));
        state.image_input_limited = true;
        let writes = vendor.state.lock().unwrap().writes;
        let config = prepared(
            &path,
            &vendor,
            manifest,
            desired(&[info.clone()], "mock-key"),
        );
        assert!(
            !config.preview().already_configured,
            "stale warning needs a local commit"
        );
        let result = config.commit(|_, _| {}).await.unwrap();
        assert!(!result.details.contains_key("trae_image_input_limited"));
        assert_eq!(vendor.state.lock().unwrap().writes, writes);
        assert!(
            prepared(&path, &vendor, load(&path), desired(&[info], "mock-key"))
                .preview()
                .already_configured
        );
    }
}

#[test]
fn trae_writer_uses_admin_order_even_for_unsorted_input() {
    let mut models = ["a-low", "z-high", "m-middle"].map(|id| ToolModelInfo {
        id: id.into(),
        ..model()
    });
    models[0].display_priority = 1;
    models[1].display_priority = 3;
    models[2].display_priority = 2;
    assert_eq!(
        desired(&models, "mock-key")
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>(),
        ["z-high", "m-middle", "a-low"]
    );
}

#[tokio::test]
async fn strict_readback_failure_persists_all_confirmed_ids_without_retiring_models() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let models = ["a-invalid", "z-valid"].map(|id| ToolModelInfo {
        id: id.into(),
        ..model()
    });
    let mut entries = desired(&models, "mock-key");
    // Simulate an acknowledged add whose output limit was not stored as requested.
    entries[0].body["config_detail"]["model_hyper_params"]["max_tokens"] = json!(1024);
    let error = prepared(&path, &vendor, Manifest::default(), entries)
        .commit(|_, _| {})
        .await
        .unwrap_err();
    assert!(error.to_string().contains("max_tokens"));
    let saved = load(&path);
    let owned = &saved.accounts["mock-account"];
    assert!(
        owned
            .values()
            .all(|m| matches!(m.registration, Registration::Ready { .. }))
    );
    let invalid = owned
        .values()
        .find(|m| m.name == "a-invalid")
        .unwrap()
        .advanced
        .as_ref()
        .unwrap();
    assert_eq!(
        invalid.pending,
        Fields::from([("max_tokens".into(), json!(4096))])
    );
    assert!(
        !owned
            .values()
            .find(|m| m.name == "z-valid")
            .unwrap()
            .has_pending_write()
    );
    assert_eq!(vendor.state.lock().unwrap().models.len(), 2);
}

#[tokio::test]
async fn config_scoped_series_recovers_saved_pending_without_remote_writes() {
    // Sanitized shape of the 2026-09-30 TraeWork response: series belongs to the
    // parent config; thinking belongs to model.custom_config. An acked update
    // was previously misread as "other", leaving these exact fields pending.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.json");
    let vendor = Vendor::start().await;
    let mut entries = Vec::new();
    let mut owned_models = BTreeMap::new();
    for (index, (name, series)) in [
        ("gpt-6-luna", "gpt6"),
        ("gpt-5.6-luna", "gpt5"),
        ("deepseek-v4-pro", "deepseek-v4"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut entry = desired(&[reasoning_model(name)], "mock-key").remove(0);
        // Historical write being recovered, not a newly generated default.
        entry.body["config_detail"]["model_hyper_params"]["thinking_enable"] = json!(1);
        entry.advanced.insert("thinking_enable".into(), json!(1));
        entry.body["custom_model_type"] = json!(series);
        entry
            .advanced
            .insert("custom_model_type".into(), json!(series));
        let id = 42 + index as u64;
        vendor.seed(id, &entry);
        let mut saved = owned(id, &entry);
        saved.advanced = Some(AdvancedState {
            image_input_limited: false,
            applied: Fields::from([
                ("custom_model_type".into(), json!("other")),
                ("thinking_enable".into(), json!(0)),
            ]),
            pending: Fields::from([
                ("custom_model_type".into(), json!(series)),
                ("thinking_enable".into(), json!(1)),
            ]),
            user_overrides: Fields::new(),
        });
        owned_models.insert(entry.key.clone(), saved);
        entries.push(entry);
    }
    let before = vendor.state.lock().unwrap().models.clone();
    let response = vendor_details(&before);
    assert_eq!(response["config_info_list"][0]["custom_model_type"], "gpt6");
    assert!(
        response["config_info_list"][0]["model_detail_list"][0]
            .get("custom_model_type")
            .is_none()
    );
    let remote = vendor.session.list().await.unwrap();
    reconcile_pending(&mut owned_models, &remote, false).unwrap();
    reconcile_advanced(&mut owned_models, &remote).unwrap();
    assert!(
        owned_models
            .values()
            .all(|model| !model.has_pending_write())
    );
    let manifest = Manifest {
        accounts: BTreeMap::from([("mock-account".into(), owned_models)]),
        ..Default::default()
    };
    let mut operation = prepared(&path, &vendor, manifest, entries);
    operation.recovered_pending = true;
    assert!(!operation.preview().already_configured);
    operation.commit(|_, _| {}).await.unwrap();
    let saved = load(&path);
    for (key, model) in &saved.accounts["mock-account"] {
        assert!(!model.has_pending_write(), "{key}");
        assert_ne!(
            model.advanced.as_ref().unwrap().applied["custom_model_type"],
            "other"
        );
    }
    let state = vendor.state.lock().unwrap();
    assert_eq!(
        state.writes, 0,
        "already saved settings must not be rewritten"
    );
    assert_eq!(
        state.models, before,
        "preserve existing model IDs and settings"
    );
    assert_eq!(
        state.requests.len(),
        1,
        "one read for the entire pending batch"
    );
}
