//! TraeCode / CN / TraeWork custom models are account-scoped vendor settings, not local
//! SQLite configuration. Never edit their caches or replace user-owned models.
use super::*;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
#[path = "trae_auth.rs"]
mod auth;
use auth::TraeSession;
#[path = "trae_capabilities.rs"]
mod capabilities;
use capabilities::{AdvancedState, Fields};

// Updates use bounded concurrency. Additions must be submitted in order;
// concurrent requests can scramble even the highest-priority model choices.
const MODEL_WRITE_CONCURRENCY: usize = 4;

#[derive(Clone, Default, Deserialize, Serialize)]
struct Manifest {
    #[serde(default)]
    accounts: BTreeMap<String, BTreeMap<String, OwnedModel>>,
    #[serde(default)]
    default_protocols: BTreeMap<String, String>,
    #[serde(default)]
    tools: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
}

#[derive(Clone, Deserialize, Serialize)]
struct OwnedModel {
    name: String,
    provider: String,
    registration: Registration,
    connection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    advanced: Option<AdvancedState>,
}

impl OwnedModel {
    fn has_pending_write(&self) -> bool {
        matches!(self.registration, Registration::Pending { .. })
            || self
                .advanced
                .as_ref()
                .is_some_and(|state| !state.pending.is_empty())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Registration {
    Ready {
        id: u64,
    },
    // Persist before add: a timeout must not cause an automatic duplicate POST.
    Pending {
        before_ids: Vec<u64>,
        confirmed: bool,
    },
}

#[derive(Clone, Debug, Default)]
struct RemoteModel {
    id: u64,
    name: String,
    provider: String,
    // Only a digest survives parsing; never retain/log the vendor credential.
    connection: Option<String>,
    // Only editable, non-secret fields; excludes prompts and credentials.
    advanced: Fields,
}

struct DesiredModel {
    key: String,
    name: String,
    provider: String,
    body: Value,
    connection: String,
    advanced: Fields,
    series_hint: Option<&'static str>,
    user_overrides: Fields,
}

pub(crate) struct PreparedTraeConfig {
    tool: String,
    manifest_path: PathBuf,
    session: TraeSession,
    manifest: Manifest,
    remote: Vec<RemoteModel>,
    desired: Vec<DesiredModel>,
    protocol: ToolProtocol,
    remove: bool,
    recovered_pending: bool,
    _lock: Option<fs::File>,
}

fn manifest_path() -> PathBuf {
    const_api_state_path("trae-managed-accounts.json")
}

fn protocol_key(account: &str, tool: &str) -> String {
    format!("{account}/{tool}")
}

fn read_manifest(path: &Path) -> Result<Option<Manifest>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .context("read Trae model ownership")
            .map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn merge_legacy_manifest(target: &mut Manifest, tool: &str, legacy: Manifest) -> Result<()> {
    for (account, models) in legacy.accounts {
        target
            .tools
            .entry(account.clone())
            .or_default()
            .entry(tool.into())
            .or_default()
            .extend(models.keys().cloned());
        let owned = target.accounts.entry(account.clone()).or_default();
        for (key, model) in models {
            if let Some(previous) = owned.get(&key) {
                match (&previous.registration, &model.registration) {
                    (Registration::Ready { id: left }, Registration::Ready { id: right })
                        if left != right =>
                    {
                        return Err(anyhow!(crate::native_i18n::text(
                            "同一 Trae 账号存在冲突的模型管理记录，未修改模型，请检查对应的 CONST API 条目。",
                            "Conflicting managed model IDs exist for this Trae account. No models were changed; check the corresponding CONST API entries.",
                        )));
                    }
                    // A sibling app may have tried to re-add an already owned
                    // cloud model. Its pending journal cannot replace a known ID.
                    (Registration::Ready { .. }, _) => continue,
                    (_, Registration::Ready { .. }) => {}
                    _ => continue,
                }
            }
            owned.insert(key, model);
        }
        if let Some(protocol) = legacy.default_protocols.get(&account) {
            target
                .default_protocols
                .insert(protocol_key(&account, tool), protocol.clone());
        }
    }
    Ok(())
}

fn load_manifest() -> Result<Manifest> {
    if let Some(manifest) = read_manifest(&manifest_path())? {
        return Ok(manifest);
    }
    let mut manifest = Manifest::default();
    for tool in ["trae", "trae-cn", "trae-work"] {
        if let Some(legacy) = read_manifest(&const_api_state_path(&format!(
            "{tool}-managed-models.json"
        )))? {
            merge_legacy_manifest(&mut manifest, tool, legacy)?;
        }
    }
    // Commit writes the new file even when empty. Legacy files stay untouched,
    // but cannot resurrect removed models on the next launch.
    Ok(manifest)
}

fn acquire_manifest_lock() -> Result<fs::File> {
    let path = const_api_state_path("trae-managed-accounts.lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|err| {
        if err.kind() == std::io::ErrorKind::WouldBlock {
            anyhow!(crate::native_i18n::text(
                "另一个 Trae 工具正在配置模型，请等待完成后重试。",
                "Another Trae tool is configuring models. Wait for it to finish and retry.",
            ))
        } else {
            err.into()
        }
    })?;
    Ok(file)
}

fn save_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    atomic_write(path, &serde_json::to_vec(manifest)?)
}

fn connection_digest(base_url: &str, api_key: &str, provider: &str) -> String {
    let mut digest = Sha256::new();
    // Invalidate journals made by the former plaintext-key writer.
    digest.update(b"trae-native-ak-v1");
    for value in [base_url, api_key.trim(), provider] {
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value.as_bytes());
    }
    hex::encode(digest.finalize())
}

fn model_connection(base_url: &str, protocol: ToolProtocol) -> Result<(&'static str, String)> {
    match protocol {
        ToolProtocol::OpenAiResponses => Ok((
            "custom_responses_compatible",
            format!("{}/responses", tool_surface_url(base_url, "v1")),
        )),
        ToolProtocol::OpenAiChat => Ok((
            "custom_openai_compatible",
            format!("{}/chat/completions", tool_surface_url(base_url, "v1")),
        )),
        ToolProtocol::AnthropicMessages => Ok((
            "custom_anthropic_compatible",
            format!("{}/v1/messages", tool_surface_url(base_url, "anthropic")),
        )),
        _ => Err(anyhow!(
            "Trae does not support protocol {}",
            protocol.as_str()
        )),
    }
}

fn desired_models(
    tool: &str,
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    fallback: ToolProtocol,
) -> Result<Vec<DesiredModel>> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let encrypted_key = auth::encode_model_key(api_key)?;
    let mut models = models.iter().collect::<Vec<_>>();
    models.sort_by(|a, b| crate::tool_model_metadata::tool_model_display_order(a, b));
    for model in models {
        if model.id.trim().is_empty() || !seen.insert(&model.id) {
            continue;
        }
        let protocol = tool_model_protocol(tool, model, fallback);
        let (provider, endpoint) = model_connection(base_url, protocol)?;
        let display = if model.display_name.trim().is_empty() {
            &model.id
        } else {
            &model.display_name
        };
        let display: String = format!("CONST API · {display}").chars().take(64).collect();
        // A capability is not an instruction to force a request mode. Let the
        // model/tool choose its default, preserving user overrides on refresh.
        let mut hyper = json!({"thinking_enable": 0});
        if let Some(tokens) = model.context_tokens {
            hyper["prompt_max_tokens"] = json!(tokens);
        }
        if let Some(tokens) = model.output_tokens {
            hyper["max_tokens"] = json!(tokens);
        }
        let connection = connection_digest(&endpoint, api_key, provider);
        let body = json!({
            "model_name": model.id, "provider": provider, "is_custom": true,
            "ak": encrypted_key, "auth_type": 0, "base_url": endpoint, "display_name": display,
            "config_detail": {
                "multimodal": model.input_modalities.iter().any(|value| value == "image"),
                "model_hyper_params": hyper,
                "max_turn": 500
            }
        });
        entries.push(DesiredModel {
            key: format!("{provider}//{}", model.id),
            name: model.id.clone(),
            provider: provider.to_string(),
            connection,
            advanced: capabilities::desired_fields(&body),
            user_overrides: Fields::new(),
            series_hint: matches!(
                protocol,
                ToolProtocol::OpenAiChat | ToolProtocol::OpenAiResponses
            )
            .then(|| capabilities::series_hint(model))
            .flatten(),
            body,
        });
    }
    Ok(entries)
}

impl TraeSession {
    async fn resolve_model_series(&self, desired: &mut [DesiredModel]) -> Result<()> {
        // Options depend on the account, provider and endpoint, not the app's
        // display name. Fetch once per distinct connection, never per model.
        let connections = desired
            .iter()
            .filter(|entry| entry.series_hint.is_some())
            .map(|entry| {
                (
                    entry.provider.clone(),
                    entry.body["base_url"].as_str().unwrap().to_string(),
                )
            })
            .collect::<BTreeSet<_>>();
        for (provider, endpoint) in connections {
            let response = self
                .request(
                    "/api/ide/v1/get_custom_model_type_config",
                    Some(&json!({"provider_id": provider, "end_point": endpoint})),
                )
                .await?;
            let available = capabilities::parse_series(&response)?;
            for entry in desired
                .iter_mut()
                .filter(|entry| entry.provider == provider && entry.body["base_url"] == endpoint)
            {
                if let Some(series) = entry.series_hint.filter(|hint| available.contains(*hint)) {
                    entry.body["custom_model_type"] = json!(series);
                    entry
                        .advanced
                        .insert("custom_model_type".into(), json!(series));
                }
            }
        }
        Ok(())
    }

    async fn request(&self, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}{path}", self.host);
        let request = match body {
            Some(body) => self.client.post(url).json(body),
            None => self.client.get(url),
        };
        let mut response = request
            .send()
            .await
            .context("Trae model service connection failed")?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            return Err(anyhow!(crate::native_i18n::text(
                "Trae 登录已失效或无权管理模型，请在 Trae 中重新登录后重试。",
                "Trae sign-in expired or model management was denied. Sign in to Trae again and retry.",
            )));
        }
        if !status.is_success() {
            return Err(anyhow!(
                json!({
                    "code": "tool_config_trae_http_failed",
                    "params": { "status": status.as_u16() }
                })
                .to_string()
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
                return Err(anyhow!("Trae model response is too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).context("invalid Trae model response")?;
        if let Some(code) = value
            .get("code")
            .and_then(Value::as_i64)
            .filter(|code| *code != 0)
        {
            // Vendor error text can echo request fields, including credentials.
            return Err(anyhow!(
                json!({
                    "code": "tool_config_trae_rejected",
                    "params": { "vendor_code": code }
                })
                .to_string()
            ));
        }
        Ok(value)
    }

    async fn list(&self) -> Result<Vec<RemoteModel>> {
        // model_list is a legacy summary: current Trae editions return an empty
        // base_url even for correctly configured models. Read the same model
        // details as the native client, once for the whole catalog, without prompts.
        let response = self
            .request(
                "/api/ide/v1/get_detail_param",
                Some(&json!({"function": "chat", "show_custom_model": true, "need_prompt": false})),
            )
            .await?;
        parse_remote_models(&response)
    }

    async fn write_model(&self, path: &str, body: &Value) -> Result<()> {
        let value = self.request(path, Some(body)).await?;
        if value.get("code").and_then(Value::as_i64) != Some(0) {
            return Err(anyhow!("Trae did not acknowledge the model configuration"));
        }
        Ok(())
    }
}

fn parse_remote_models(response: &Value) -> Result<Vec<RemoteModel>> {
    let configs = response
        .get("config_info_list")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Trae model detail schema is not supported"))?;
    let mut result = Vec::new();
    for config in configs {
        let models = config
            .get("model_detail_list")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("Trae model detail list is missing"))?;
        for model in models {
            let custom = &model["custom_config"];
            let id = custom
                .get("custom_model_id")
                .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()));
            let Some(id) = id.filter(|id| *id != 0) else {
                continue;
            };
            let name = model
                .get("model_name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Trae custom model name is missing"))?;
            let provider = model
                .get("provider")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Trae custom model provider is missing"))?;
            result.push(RemoteModel {
                id,
                name: name.to_string(),
                provider: provider.to_string(),
                advanced: capabilities::remote_fields(config, model),
                connection: custom
                    .get("ak")
                    .and_then(Value::as_str)
                    .and_then(auth::decode_model_key)
                    .and_then(|key| {
                        custom
                            .get("base_url")
                            .and_then(Value::as_str)
                            .filter(|url| !url.trim().is_empty())
                            .map(|url| connection_digest(url, &key, provider))
                    }),
            });
        }
    }
    Ok(result)
}

fn remote_matches(owned: &OwnedModel, remote: &RemoteModel) -> bool {
    matches!(owned.registration, Registration::Ready { id } if id == remote.id)
        && owned.name == remote.name
        && owned.provider == remote.provider
}

fn reconcile_advanced(
    owned: &mut BTreeMap<String, OwnedModel>,
    remote: &[RemoteModel],
) -> Result<()> {
    for model in owned.values_mut() {
        if let Some(entry) = remote.iter().find(|entry| remote_matches(model, entry))
            && let Some(state) = &mut model.advanced
        {
            state.recover(&model.name, &entry.advanced)?;
        }
    }
    Ok(())
}

fn reconcile_pending(
    owned: &mut BTreeMap<String, OwnedModel>,
    remote: &[RemoteModel],
    just_added: bool,
) -> Result<()> {
    let mut absent = Vec::new();
    for (key, model) in owned.iter_mut() {
        let Registration::Pending {
            before_ids,
            confirmed,
        } = &model.registration
        else {
            continue;
        };
        let new = remote
            .iter()
            .filter(|entry| {
                entry.name == model.name
                    && entry.provider == model.provider
                    && !before_ids.contains(&entry.id)
            })
            .collect::<Vec<_>>();
        match (new.as_slice(), *confirmed) {
            ([entry], true) => model.registration = Registration::Ready { id: entry.id },
            // On a later explicit retry, the user may already have removed
            // the uncertain entry. An immediate post-add list must contain it.
            ([], _) if !just_added => absent.push(key.clone()),
            _ => {
                return Err(anyhow!(
                    "{}: {}",
                    crate::native_i18n::text(
                        "无法确认上次添加的 Trae 模型，未重复添加或删除。请在 Trae 中检查并移除对应的 CONST API 条目后重试",
                        "The previous Trae addition could not be confirmed. Nothing was duplicated or deleted. Check and remove the corresponding CONST API entry in Trae, then retry",
                    ),
                    model.name
                ));
            }
        }
    }
    for key in absent {
        owned.remove(&key);
    }
    // A write acknowledgement is not proof that the connection was stored.
    // Only a matching, readable remote credential may mark it configured.
    for model in owned.values_mut() {
        if matches!(model.registration, Registration::Ready { .. }) {
            model.connection = remote
                .iter()
                .find(|entry| remote_matches(model, entry))
                .and_then(|entry| entry.connection.clone())
                .unwrap_or_default();
        }
    }
    Ok(())
}

pub(crate) async fn prepare_trae_config(
    tool: &str,
    base_url: &str,
    api_key: &str,
    models: &[ToolModelInfo],
    protocol: ToolProtocol,
    remove: bool,
) -> Result<PreparedTraeConfig> {
    let lock = acquire_manifest_lock()?;
    let session = TraeSession::load(tool)?;
    let mut desired = if remove {
        Vec::new()
    } else {
        desired_models(tool, base_url, api_key, models, protocol)?
    };
    let (remote, ()) =
        tokio::try_join!(session.list(), session.resolve_model_series(&mut desired))?;
    let mut manifest = load_manifest()?;
    let owned = manifest
        .accounts
        .entry(session.account.clone())
        .or_default();
    let recovered_pending = owned.values().any(OwnedModel::has_pending_write);
    reconcile_pending(owned, &remote, false)?;
    if !remove {
        reconcile_advanced(owned, &remote)?;
    }
    if !remove && desired.is_empty() {
        return Err(anyhow!("No models are available to configure in Trae"));
    }
    Ok(PreparedTraeConfig {
        tool: tool.to_string(),
        manifest_path: manifest_path(),
        session,
        manifest,
        remote,
        desired,
        protocol,
        remove,
        recovered_pending,
        _lock: Some(lock),
    })
}

impl PreparedTraeConfig {
    fn preserve_protocol_migration_overrides(&mut self) {
        let owned = &self.manifest.accounts[&self.session.account];
        let Some(previous_keys) = self
            .manifest
            .tools
            .get(&self.session.account)
            .and_then(|tools| tools.get(&self.tool))
        else {
            return;
        };
        for desired in &mut self.desired {
            if owned.contains_key(&desired.key) {
                continue;
            }
            let Some((previous, remote)) = previous_keys
                .iter()
                .filter_map(|key| owned.get(key))
                .filter(|previous| previous.name == desired.name)
                .find_map(|previous| {
                    self.remote
                        .iter()
                        .find(|entry| remote_matches(previous, entry))
                        .map(|remote| (previous, remote))
                })
            else {
                continue;
            };
            let plan = capabilities::plan(
                previous.advanced.as_ref(),
                &remote.advanced,
                &desired.advanced,
            );
            let mut fields = remote.advanced.clone();
            fields.extend(plan.pending);
            // Series presets are provider-specific. Resolve them anew for the
            // new protocol instead of carrying an incompatible old preset.
            fields.remove("custom_model_type");
            if let Some(series) = desired.advanced.get("custom_model_type") {
                fields.insert("custom_model_type".into(), series.clone());
            }
            desired.user_overrides = fields
                .iter()
                .filter(|(key, value)| {
                    key.as_str() != "custom_model_type"
                        && desired.advanced.get(*key) != Some(*value)
                })
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            capabilities::write_update(&mut desired.body, &fields, &Fields::new());
            // Read-back must also verify the carried user settings.
            desired.advanced = fields;
        }
    }

    pub(crate) fn context_digest(&self) -> Result<String> {
        // Includes account/ownership identity; never puts credentials in IPC or files.
        Ok(sha256_hex(
            serde_json::to_vec(&(
                &self.session.account,
                self.remove,
                &self.manifest.accounts,
                &self.manifest.tools,
                &self.manifest.default_protocols,
                self.remote
                    .iter()
                    .map(|model| (model.id, (&model.connection, &model.advanced)))
                    .collect::<BTreeMap<_, _>>(),
                self.desired
                    .iter()
                    .map(|entry| (&entry.key, &entry.connection, &entry.advanced))
                    .collect::<Vec<_>>(),
            ))?
            .as_slice(),
        ))
    }

    pub(crate) fn preview(&self) -> ToolApplyResult {
        let owned = &self.manifest.accounts[&self.session.account];
        let tools = self.manifest.tools.get(&self.session.account);
        let desired_keys = self
            .desired
            .iter()
            .map(|model| model.key.clone())
            .collect::<BTreeSet<_>>();
        let unchanged = if self.remove {
            !tools.is_some_and(|tools| tools.contains_key(&self.tool)) && owned.is_empty()
        } else {
            self.desired.iter().all(|desired| {
                owned.get(&desired.key).is_some_and(|model| {
                    model.connection == desired.connection
                        && self
                            .remote
                            .iter()
                            .find(|entry| remote_matches(model, entry))
                            .is_some_and(|entry| {
                                let advanced = capabilities::plan(
                                    model.advanced.as_ref(),
                                    &entry.advanced,
                                    &desired.advanced,
                                );
                                entry.connection.as_ref() == Some(&desired.connection)
                                    && advanced.pending.is_empty()
                                    && advanced.image_input_limited
                                        == model
                                            .advanced
                                            .as_ref()
                                            .is_some_and(|state| state.image_input_limited)
                            })
                })
            }) && tools.and_then(|tools| tools.get(&self.tool)) == Some(&desired_keys)
                && self
                    .manifest
                    .default_protocols
                    .get(&protocol_key(&self.session.account, &self.tool))
                    .is_some_and(|value| value == self.protocol.as_str())
        };
        let mut result = ToolApplyBuilder::default().finish(&self.tool);
        result.already_configured = unchanged && !self.recovered_pending;
        attach_capability_limits(&mut result, owned.values());
        attach_tool_protocol(&mut result, self.protocol);
        result
    }

    pub(crate) async fn commit(
        mut self,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<ToolApplyResult> {
        self.preserve_protocol_migration_overrides();
        let tools = self
            .manifest
            .tools
            .entry(self.session.account.clone())
            .or_default();
        if self.remove {
            tools.remove(&self.tool);
            self.manifest
                .default_protocols
                .remove(&protocol_key(&self.session.account, &self.tool));
        } else {
            tools.insert(
                self.tool.clone(),
                self.desired.iter().map(|entry| entry.key.clone()).collect(),
            );
        }
        let retained: BTreeSet<_> = tools
            .values()
            .flat_map(|keys| keys.iter().cloned())
            .collect();
        let mut owned = self
            .manifest
            .accounts
            .remove(&self.session.account)
            .unwrap_or_default();
        let retired = owned
            .keys()
            .filter(|key| !retained.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        let mut updates = Vec::new();
        let mut additions = Vec::new();
        for desired in &self.desired {
            if let Some(current) = owned.get_mut(&desired.key).and_then(|model| {
                self.remote
                    .iter()
                    .find(|entry| remote_matches(model, entry))
                    .map(|entry| (model, entry))
            }) {
                let advanced = capabilities::plan(
                    current.0.advanced.as_ref(),
                    &current.1.advanced,
                    &desired.advanced,
                );
                let advanced_changed = !advanced.pending.is_empty();
                current.0.advanced = Some(advanced);
                if current.1.connection.as_ref() == Some(&desired.connection) && !advanced_changed {
                    // A confirmed remote connection is authoritative, even when
                    // upgrading an older journal or repairing a prior timeout.
                    current.0.connection = desired.connection.clone();
                } else {
                    let mut body = json!({
                        "action": "update", "id": current.1.id, "ak": desired.body["ak"],
                        "base_url": desired.body["base_url"], "auth_type": 0,
                    });
                    if advanced_changed {
                        capabilities::write_update(
                            &mut body,
                            &current.1.advanced,
                            &current.0.advanced.as_ref().unwrap().pending,
                        );
                    }
                    updates.push(body);
                }
            } else {
                additions.push(desired);
            }
        }
        let changed_remote = !updates.is_empty() || !additions.is_empty();
        let total = updates.len() + additions.len() + retired.len();
        let mut completed = 0;
        progress(completed, total);
        // One write-ahead journal for additions and advanced updates. Persist
        // add acknowledgements below, but do not rewrite twice per model.
        for desired in &additions {
            let mut initial_advanced = desired.advanced.clone();
            // An omitted series means the native default on creation. Record
            // that baseline so newly available account presets can upgrade it.
            initial_advanced
                .entry("custom_model_type".into())
                .or_insert_with(|| json!("other"));
            owned.insert(
                desired.key.clone(),
                OwnedModel {
                    name: desired.name.clone(),
                    provider: desired.provider.clone(),
                    connection: desired.connection.clone(),
                    advanced: Some(AdvancedState {
                        applied: Fields::new(),
                        pending: initial_advanced,
                        user_overrides: desired.user_overrides.clone(),
                        image_input_limited: false,
                    }),
                    registration: Registration::Pending {
                        before_ids: self
                            .remote
                            .iter()
                            .filter(|entry| {
                                entry.name == desired.name && entry.provider == desired.provider
                            })
                            .map(|entry| entry.id)
                            .collect(),
                        confirmed: false,
                    },
                },
            );
        }
        self.manifest
            .accounts
            .insert(self.session.account.clone(), owned.clone());
        save_manifest(&self.manifest_path, &self.manifest)?;
        for batch in updates.chunks(MODEL_WRITE_CONCURRENCY) {
            let session = &self.session;
            let mut pending = stream::iter(batch.iter().map(|body| async move {
                // IDs and display names stay unchanged. Advanced changes have
                // already been merged field-by-field with the read-back state.
                session
                    .write_model("/api/ide/v1/update_custom_model", body)
                    .await
            }))
            .buffer_unordered(MODEL_WRITE_CONCURRENCY);
            let mut failure = None;
            while let Some(result) = pending.next().await {
                if let Err(error) = result {
                    failure.get_or_insert(error);
                    continue;
                }
                completed += 1;
                progress(completed, total);
            }
            // Drain all in-flight updates before returning an error. Re-read on
            // the next explicit attempt; never blindly retry uncertain writes.
            if let Some(error) = failure {
                return Err(error);
            }
        }
        // Await each add acknowledgement before submitting the next priority.
        // No per-model read-back or artificial sleep; the vendor can still apply
        // its own tie-breaking to entries created within the same time bucket.
        // Never delete/recreate existing IDs merely to change their position.
        for desired in additions.into_iter().rev() {
            self.session
                .write_model("/api/ide/v1/add_custom_model", &desired.body)
                .await?;
            let model = owned
                .get_mut(&desired.key)
                .context("Trae ownership changed")?;
            if let Registration::Pending { confirmed, .. } = &mut model.registration {
                *confirmed = true;
            }
            self.manifest
                .accounts
                .insert(self.session.account.clone(), owned.clone());
            save_manifest(&self.manifest_path, &self.manifest)?;
            completed += 1;
            progress(completed, total);
        }
        if changed_remote {
            self.remote = self.session.list().await?;
            reconcile_pending(&mut owned, &self.remote, true)?;
            let mut verification_failure = None;
            for model in owned.values_mut() {
                if let Some(remote) = self
                    .remote
                    .iter()
                    .find(|entry| remote_matches(model, entry))
                    && let Some(state) = &mut model.advanced
                {
                    if let Err(error) = state.confirm(&model.name, &remote.advanced) {
                        verification_failure.get_or_insert(error);
                    }
                }
            }
            self.manifest
                .accounts
                .insert(self.session.account.clone(), owned.clone());
            save_manifest(&self.manifest_path, &self.manifest)?;
            // Save all confirmed IDs/fields even when one field remains pending.
            // Do not retire old models on a failed strict validation.
            if let Some(error) = verification_failure {
                return Err(error);
            }
            for desired in &self.desired {
                if !owned.get(&desired.key).is_some_and(|model| {
                    self.remote.iter().any(|entry| {
                        remote_matches(model, entry)
                            && entry.connection.as_ref() == Some(&desired.connection)
                    })
                }) {
                    return Err(anyhow!(
                        json!({
                            "code": "tool_config_trae_verification_failed"
                        })
                        .to_string()
                    ));
                }
            }
        }
        // Add replacements before removing retired owned entries. Never match
        // by name/URL alone when deleting; numeric IDs and account must agree.
        for key in retired {
            if let Some(remote) = self
                .remote
                .iter()
                .find(|entry| remote_matches(&owned[&key], entry))
            {
                self.session
                    .write_model(
                        "/api/ide/v1/update_custom_model",
                        &json!({"action": "delete", "id": remote.id}),
                    )
                    .await?;
            }
            owned.remove(&key);
            self.manifest
                .accounts
                .insert(self.session.account.clone(), owned.clone());
            save_manifest(&self.manifest_path, &self.manifest)?;
            completed += 1;
            progress(completed, total);
        }
        self.manifest
            .accounts
            .insert(self.session.account.clone(), owned);
        if !self.remove {
            self.manifest.default_protocols.insert(
                protocol_key(&self.session.account, &self.tool),
                self.protocol.as_str().to_string(),
            );
        }
        save_manifest(&self.manifest_path, &self.manifest)?;
        progress(total, total);
        let mut result = ToolApplyBuilder::default().finish(&self.tool);
        result.files.push(self.manifest_path.display().to_string());
        result.already_configured = !self.remove;
        attach_tool_protocol(&mut result, self.protocol);
        result.details.insert(
            "remote_models_managed".into(),
            self.desired.len().to_string(),
        );
        attach_capability_limits(
            &mut result,
            self.manifest.accounts[&self.session.account].values(),
        );
        Ok(result)
    }
}

fn attach_capability_limits<'a>(
    result: &mut ToolApplyResult,
    models: impl Iterator<Item = &'a OwnedModel>,
) {
    let names = models
        .filter(|model| {
            model
                .advanced
                .as_ref()
                .is_some_and(|state| state.image_input_limited)
        })
        .map(|model| model.name.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if !names.is_empty() {
        result
            .details
            .insert("trae_image_input_limited".into(), names.join(", "));
    }
}

#[cfg(test)]
#[path = "tests/trae.rs"]
mod tests;

pub(crate) fn check_trae_config(
    tool: &str,
    base_url: &str,
    api_key: &str,
) -> Result<ToolApplyResult> {
    let result = ToolApplyBuilder::default().finish(tool);
    let manifest = load_manifest()?;
    if manifest.accounts.is_empty() {
        return Ok(result);
    }
    let session = TraeSession::load(tool)?;
    Ok(check_account_config(
        tool,
        &session.account,
        &manifest,
        base_url,
        api_key,
    ))
}

fn check_account_config(
    tool: &str,
    account: &str,
    manifest: &Manifest,
    base_url: &str,
    api_key: &str,
) -> ToolApplyResult {
    let mut result = ToolApplyBuilder::default().finish(tool);
    let Some(owned) = manifest.accounts.get(account) else {
        return result;
    };
    // CN and Work can see the same account models. Pending writes still mean
    // CONST configuration exists, but must never bypass the readiness check.
    result
        .details
        .insert("has_managed_config".into(), (!owned.is_empty()).to_string());
    if let Some(protocol) = manifest
        .default_protocols
        .get(&protocol_key(account, tool))
        .and_then(|value| ToolProtocol::parse(value))
    {
        attach_tool_protocol(&mut result, protocol);
    }
    let keys = manifest
        .tools
        .get(account)
        .and_then(|tools| tools.get(tool));
    result.already_configured = keys.is_some_and(|keys| {
        !keys.is_empty()
            && keys.iter().all(|key| {
                let Some(model) = owned.get(key) else {
                    return false;
                };
                let protocol = match model.provider.as_str() {
                    "custom_openai_compatible" => ToolProtocol::OpenAiChat,
                    "custom_responses_compatible" => ToolProtocol::OpenAiResponses,
                    "custom_anthropic_compatible" => ToolProtocol::AnthropicMessages,
                    _ => return false,
                };
                model_connection(base_url, protocol).is_ok_and(|(provider, endpoint)| {
                    !model.has_pending_write()
                        && model.connection == connection_digest(&endpoint, api_key, provider)
                })
            })
    });
    attach_capability_limits(&mut result, owned.values());
    result
}
