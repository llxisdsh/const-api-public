pub(crate) fn subscription_adapter_inspection(
    channel: &ChannelConfig,
) -> SubscriptionAdapterInspection {
    let driver = crate::source_driver::source_driver(channel.source_driver()).ok();
    let retained_subscription = driver
        .map(|driver| driver.execution_kind() != crate::source_driver::ExecutionKind::HttpSurface)
        .unwrap_or(false);
    let platform = driver
        .and_then(|driver| driver.subscription_provider.as_deref())
        .unwrap_or(channel.subscription.platform.as_str());
    let mut checks = vec![
        format!("platform={platform}"),
        format!(
            "concurrency limit={} (0 means unlimited)",
            channel.max_concurrency()
        ),
    ];
    let mut warnings = Vec::new();
    if !retained_subscription {
        warnings.push("source driver is not a retained subscription executor".to_string());
    }
    if channel.v2.credential_ref.trim().is_empty() {
        warnings.push("credential reference is empty; concrete adapter cannot run yet".to_string());
    } else {
        checks.push("credential reference configured".to_string());
    }
    if channel.enabled {
        checks.push("sharing enabled; supplier accepts upstream account risk".to_string());
    }
    let safe_to_share = retained_subscription;
    let status = if warnings.is_empty() {
        "ready_for_concrete_adapter"
    } else {
        "skeleton_only"
    }
    .to_string();
    SubscriptionAdapterInspection {
        status,
        safe_to_share,
        checks,
        warnings,
    }
}

pub(crate) fn scan_subscription_credential_candidates() -> Vec<SubscriptionCredentialCandidate> {
    let mut files = Vec::new();
    for dir in subscription_credential_dirs() {
        collect_json_files(&dir, 0, &mut files);
    }
    let mut candidates: Vec<_> = files
        .into_iter()
        .filter_map(|path| subscription_credential_candidate_from_path(&path))
        .filter(|candidate| subscription_candidate_can_import(&candidate.status))
        .collect();
    candidates.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.email.cmp(&b.email))
            .then_with(|| a.path.cmp(&b.path))
    });
    candidates.dedup_by(|a, b| a.path == b.path);
    candidates
}

#[cfg(test)]
pub(crate) fn import_subscription_credential_channel(
    provider: &str,
    path: &str,
) -> Result<ChannelConfig> {
    import_subscription_credential_channel_in_dir(
        provider,
        path,
        &primary_subscription_credential_dir(),
    )
}

pub(crate) fn prepare_subscription_credential_channel(
    provider: &str,
    path: &str,
) -> Result<ChannelConfig> {
    let provider = normalize_subscription_provider(provider)
        .ok_or_else(|| anyhow!("unsupported subscription provider"))?;
    let source_path = PathBuf::from(path.trim());
    let candidate = subscription_credential_candidate_from_path(&source_path)
        .ok_or_else(|| anyhow!("unsupported credential file"))?;
    if candidate.provider != provider {
        return Err(anyhow!(
            "credential provider mismatch: expected {}, got {}",
            provider,
            candidate.provider
        ));
    }
    if !subscription_candidate_can_import(&candidate.status) {
        return Err(anyhow!("credential file is missing refresh token"));
    }
    let token = read_subscription_credential(&source_path)?;
    prepare_subscription_token_and_import(&provider, token)
}

#[cfg(test)]
pub(crate) fn import_subscription_credential_channel_in_dir(
    provider: &str,
    path: &str,
    managed_dir: &Path,
) -> Result<ChannelConfig> {
    let requested_provider = normalize_subscription_provider(provider)
        .ok_or_else(|| anyhow!("unsupported subscription provider"))?;
    let source_path = PathBuf::from(path);
    let candidate = subscription_credential_candidate_from_path(&source_path)
        .ok_or_else(|| anyhow!("unsupported subscription credential file"))?;
    let provider = normalize_subscription_provider(&requested_provider)
        .or_else(|| normalize_subscription_provider(&candidate.provider))
        .ok_or_else(|| anyhow!("unsupported subscription provider"))?;
    if provider != candidate.provider {
        return Err(anyhow!(
            "credential provider mismatch: expected {}, got {}",
            provider,
            candidate.provider
        ));
    }
    if !subscription_candidate_can_import(&candidate.status) {
        return Err(anyhow!("credential file is missing refresh token"));
    }

    let managed_path = if subscription_credential_is_managed_in(&source_path, managed_dir) {
        source_path
    } else {
        let token = read_subscription_credential(&source_path)?;
        save_subscription_token_to_managed_dir(
            &provider,
            token,
            managed_dir,
            Some(managed_import_storage_key(&source_path)),
        )?
    };
    subscription_channel_from_managed_credential(&provider, &managed_path)
}

fn subscription_channel_from_managed_credential(
    provider: &str,
    path: &Path,
) -> Result<ChannelConfig> {
    let candidate = subscription_credential_candidate_from_path(path)
        .ok_or_else(|| anyhow!("unsupported managed subscription credential"))?;
    if candidate.provider != provider {
        return Err(anyhow!(
            "credential provider mismatch: expected {}, got {}",
            provider,
            candidate.provider
        ));
    }
    if !subscription_candidate_can_import(&candidate.status) {
        return Err(anyhow!("credential file is missing refresh token"));
    }

    let path_text = path.to_string_lossy().to_string();
    let mut subscription = default_subscription_adapter_config();
    subscription.platform = provider.to_string();
    subscription.account_label = candidate.email.clone();
    subscription.credential_ref = path_text.clone();
    let name = subscription_channel_name(provider).to_string();
    // A channel is a local installation instance, not the subscription account itself.
    // Give every newly imported instance its own identity so a reset followed by a
    // different platform login cannot recreate another user's supplier unit. Existing
    // channels keep the ID persisted in their configuration.
    let id = format!("channel-{}", hex::encode(random_bytes(16)?));
    let source_driver = match provider {
        "openai" => crate::source_driver::SourceDriverId::OpenAiSubscription,
        "antigravity" => crate::source_driver::SourceDriverId::GeminiSubscription,
        "grok" => crate::source_driver::SourceDriverId::GrokSubscription,
        _ => crate::source_driver::SourceDriverId::ClaudeSubscription,
    };
    let mut v2 = channel_v2_contract_for_source(source_driver);
    v2.credential_ref = path_text;
    if provider == "grok" {
        // Match the official CLI adapter's conservative default. Users may raise
        // this channel-wide limit after validating their account and network path.
        v2.max_concurrency = 1;
    }

    Ok(normalize_channel(
        ChannelConfig {
            v2,
            id,
            enabled: false,
            share_enabled: false,
            kind: "subscription_adapter".to_string(),
            api_format: "subscription_skeleton".to_string(),
            node_id: String::new(),
            name,
            server_ws_url: "ws://127.0.0.1:8080/supplier/ws".to_string(),
            server_quic_url: String::new(),
            upstream_base_url: String::new(),
            upstream_api_key: String::new(),
            public_model: String::new(),
            upstream_model: String::new(),
            models: Vec::new(),
            supported_protocols: subscription_supported_protocols(provider),
            capability_profiles: Vec::new(),
            detection_checks: Vec::new(),
            surface_bindings: source_driver_surface_bindings(source_driver, ""),
            price_ratio: 1.0,
            subscription,
        },
        0,
        None,
        None,
    ))
}

fn primary_subscription_credential_dir() -> PathBuf {
    crate::client_data_root().join("auths")
}

fn pending_subscription_credential_dir_in(managed_dir: &Path) -> PathBuf {
    managed_dir.join(".pending")
}

fn subscription_managed_credential_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![primary_subscription_credential_dir()];
    if let Some(app_support) = dirs::data_dir() {
        dirs.push(app_support.join("const-api").join("auths"));
    }
    dirs
}

fn subscription_credential_is_managed_in(path: &Path, extra_dir: &Path) -> bool {
    path_is_within_directory(path, extra_dir)
        || subscription_managed_credential_dirs()
            .iter()
            .any(|directory| path_is_within_directory(path, directory))
}

fn path_is_within_directory(path: &Path, directory: &Path) -> bool {
    let path = absolute_or_canonical_path(path);
    let directory = absolute_or_canonical_path(directory);
    #[cfg(target_os = "windows")]
    {
        let path = path
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase();
        let directory = directory
            .to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase();
        return path == directory || path.starts_with(&format!("{directory}\\"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        path.starts_with(directory)
    }
}

fn absolute_or_canonical_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    })
}

fn managed_import_storage_key(source_path: &Path) -> String {
    // This key must remain stable after the source is removed. Canonicalizing an existing
    // Windows path adds a verbatim prefix, while the same missing path is returned without
    // one, which would otherwise point a retry at a different managed copy.
    let absolute = if source_path.is_absolute() {
        source_path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(source_path)
    };
    let source = absolute.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    let source = source
        .strip_prefix(r"\\?\")
        .unwrap_or(&source)
        .replace('/', "\\")
        .to_ascii_lowercase();
    generated_id_from_seed(&format!("subscription-import:{source}"), "import")
}

fn managed_subscription_storage_key(token: &serde_json::Value) -> Result<String> {
    if let Some(identity) = subscription_account_identity_from_value(token) {
        return Ok(identity);
    }
    Ok(format!("credential-{}", hex::encode(random_bytes(12)?)))
}

pub(crate) fn subscription_account_identity(channel: &ChannelConfig) -> Option<String> {
    let credential_path = channel.v2.credential_ref.trim();
    if credential_path.is_empty() {
        return None;
    }
    let token = read_subscription_credential(Path::new(credential_path)).ok()?;
    subscription_account_identity_from_value(&token)
}

fn subscription_account_identity_from_value(value: &serde_json::Value) -> Option<String> {
    let provider = infer_subscription_provider(value).unwrap_or_else(|| "subscription".to_string());
    let stable_identity = [
        json_string(value, &["account_id"]),
        json_string(value, &["accountID"]),
        json_string(value, &["chatgpt_account_id"]),
        json_nested_string(value, &["account", "id"]),
        json_nested_string(value, &["account", "uuid"]),
        json_nested_string(value, &["user", "id"]),
        json_nested_string(value, &["user", "uuid"]),
        jwt_nested_claim_from_token_value(
            value,
            &["https://api.openai.com/auth", "chatgpt_account_id"],
        ),
        jwt_claim_from_token_value(value, "sub"),
        json_string(value, &["email"]),
        json_nested_string(value, &["account", "email"]),
        json_nested_string(value, &["account", "email_address"]),
        json_nested_string(value, &["user", "email"]),
        email_from_id_token(value),
        json_string(value, &["refresh_token"]),
    ]
    .into_iter()
    .flatten()
    .map(|identity| identity.trim().to_string())
    .find(|identity| !identity.is_empty())?;
    Some(generated_id_from_seed(
        &format!("subscription-account:{provider}:{stable_identity}"),
        "credential",
    ))
}

fn jwt_claims_from_token_value(value: &serde_json::Value) -> Vec<serde_json::Value> {
    ["id_token", "access_token"]
        .into_iter()
        .filter_map(|key| json_string(value, &[key]))
        .filter_map(|token| {
            let payload = token.split('.').nth(1)?;
            let decoded = decode_jwt_payload(payload).ok()?;
            serde_json::from_slice(&decoded).ok()
        })
        .collect()
}

fn jwt_claim_from_token_value(value: &serde_json::Value, claim: &str) -> Option<String> {
    jwt_claims_from_token_value(value)
        .into_iter()
        .find_map(|claims| json_string(&claims, &[claim]))
}

fn jwt_nested_claim_from_token_value(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    jwt_claims_from_token_value(value)
        .into_iter()
        .find_map(|claims| json_nested_string(&claims, path))
}

pub(crate) fn normalize_subscription_token(
    provider: &str,
    mut token: serde_json::Value,
) -> Result<(String, serde_json::Value)> {
    let provider = normalize_subscription_provider(provider)
        .ok_or_else(|| anyhow!("unsupported subscription provider"))?;
    if let Some(declared_provider) = infer_subscription_provider(&token) {
        if declared_provider != provider {
            return Err(anyhow!(
                "credential provider mismatch: expected {}, got {}",
                provider,
                declared_provider
            ));
        }
    }
    if provider == "antigravity" {
        token = normalize_antigravity_credential_value(&token)
            .ok_or_else(|| anyhow!("token JSON is not a supported Antigravity credential"))?;
    }
    ensure_subscription_token_provider(&mut token, &provider);
    if provider == "antigravity" {
        ensure_antigravity_token_defaults(&mut token);
    }
    normalize_token_expiry(&mut token);
    let candidate = subscription_credential_candidate_from_value(&token, None)
        .ok_or_else(|| anyhow!("token JSON is missing refresh token"))?;
    if candidate.provider != provider {
        return Err(anyhow!(
            "credential provider mismatch: expected {}, got {}",
            provider,
            candidate.provider
        ));
    }
    if !subscription_candidate_can_import(&candidate.status) {
        return Err(anyhow!("token JSON is missing refresh token"));
    }
    Ok((provider, token))
}

fn save_subscription_token_to_managed_dir(
    provider: &str,
    token: serde_json::Value,
    managed_dir: &Path,
    storage_key: Option<String>,
) -> Result<PathBuf> {
    let (provider, token) = normalize_subscription_token(provider, token)?;
    let storage_key = match storage_key {
        Some(value) => sanitize_filename_part(&value),
        None => managed_subscription_storage_key(&token)?,
    };
    let path = managed_dir.join(format!("{provider}-{storage_key}.json"));
    write_subscription_credential_atomically(&path, &token)?;
    Ok(path)
}

#[cfg(test)]
pub(crate) fn save_subscription_token_and_import_in_dir(
    provider: &str,
    token: serde_json::Value,
    managed_dir: &Path,
) -> Result<ChannelConfig> {
    save_subscription_token_and_import_in_dir_with_key(provider, token, managed_dir, None)
}

#[cfg(test)]
fn save_subscription_token_and_import_in_dir_with_key(
    provider: &str,
    token: serde_json::Value,
    managed_dir: &Path,
    storage_key: Option<String>,
) -> Result<ChannelConfig> {
    let provider = normalize_subscription_provider(provider)
        .ok_or_else(|| anyhow!("unsupported subscription provider"))?;
    let path = save_subscription_token_to_managed_dir(&provider, token, managed_dir, storage_key)?;
    subscription_channel_from_managed_credential(&provider, &path)
}

pub(crate) async fn exchange_subscription_oauth_code_to_channel(
    provider: &str,
    code_or_callback: &str,
    state: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<ChannelConfig> {
    let provider = normalize_subscription_provider(provider)
        .ok_or_else(|| anyhow!("unsupported subscription provider"))?;
    let parsed = parse_oauth_code_or_callback(code_or_callback)?;
    if !state.trim().is_empty() && !parsed.state.trim().is_empty() && parsed.state != state.trim() {
        return Err(anyhow!("OAuth state mismatch"));
    }
    let token = match provider.as_str() {
        "openai" => exchange_openai_oauth_code(&parsed.code, code_verifier, redirect_uri).await?,
        "claude" => {
            exchange_claude_oauth_code(&parsed.code, code_verifier, redirect_uri, &parsed.state)
                .await?
        }
        "antigravity" => {
            exchange_antigravity_oauth_code(&parsed.code, code_verifier, redirect_uri).await?
        }
        "grok" => exchange_grok_oauth_code(&parsed.code, code_verifier, redirect_uri).await?,
        _ => return Err(anyhow!("unsupported subscription provider")),
    };
    prepare_subscription_token_and_import(&provider, token)
}

pub(crate) fn import_subscription_token_json_channel(
    provider: &str,
    token_json: &str,
) -> Result<ChannelConfig> {
    let provider = normalize_subscription_provider(provider)
        .ok_or_else(|| anyhow!("unsupported subscription provider"))?;
    let token: serde_json::Value = serde_json::from_str(token_json.trim())
        .map_err(|err| anyhow!("invalid token JSON: {err}"))?;
    prepare_subscription_token_and_import(&provider, token)
}

pub(crate) fn prepare_subscription_token_and_import(
    provider: &str,
    token: serde_json::Value,
) -> Result<ChannelConfig> {
    prepare_subscription_token_and_import_in_dir(
        provider,
        token,
        &primary_subscription_credential_dir(),
    )
}

pub(crate) fn prepare_subscription_token_and_import_in_dir(
    provider: &str,
    token: serde_json::Value,
    managed_dir: &Path,
) -> Result<ChannelConfig> {
    let (provider, token) = normalize_subscription_token(provider, token)?;
    let pending_dir = pending_subscription_credential_dir_in(managed_dir);
    let pending_key = format!("pending-{}", hex::encode(random_bytes(16)?));
    let path = pending_dir.join(format!("{provider}-{pending_key}.json"));
    write_subscription_credential_atomically(&path, &token)?;
    subscription_channel_from_managed_credential(&provider, &path)
}

pub(crate) fn finalize_subscription_import_channel(
    channel: ChannelConfig,
    create_duplicate: bool,
) -> Result<ChannelConfig> {
    finalize_subscription_import_channel_in_dir(
        channel,
        create_duplicate,
        &primary_subscription_credential_dir(),
    )
}

pub(crate) fn finalize_subscription_import_channel_in_dir(
    mut channel: ChannelConfig,
    create_duplicate: bool,
    managed_dir: &Path,
) -> Result<ChannelConfig> {
    let source_path = PathBuf::from(channel.v2.credential_ref.trim());
    if !path_is_within_directory(
        &source_path,
        &pending_subscription_credential_dir_in(managed_dir),
    ) {
        return Ok(normalize_channel(channel, 0, None, None));
    }

    let provider = normalize_subscription_provider(&channel.subscription.platform)
        .ok_or_else(|| anyhow!("unsupported subscription provider"))?;
    let token = read_subscription_credential(&source_path)?;
    let (token_provider, token) = normalize_subscription_token(&provider, token)?;
    if token_provider != provider {
        return Err(anyhow!(
            "credential provider mismatch: expected {}, got {}",
            provider,
            token_provider
        ));
    }

    let storage_key = managed_subscription_storage_key(&token)?;
    let mut final_path = managed_dir.join(format!("{provider}-{storage_key}.json"));
    if create_duplicate || final_path.exists() {
        final_path = managed_dir.join(format!(
            "{provider}-{storage_key}-{}.json",
            hex::encode(random_bytes(8)?)
        ));
    }
    write_subscription_credential_atomically(&final_path, &token)?;
    if source_path != final_path {
        match fs::remove_file(&source_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                let _ = fs::remove_file(&final_path);
                return Err(error).with_context(|| {
                    format!(
                        "remove pending credential {} after finalizing import",
                        source_path.display()
                    )
                });
            }
        }
    }

    let final_path_text = final_path.to_string_lossy().to_string();
    // Finalizing only moves the credential. The channel identity was assigned when the
    // import began and must remain stable across that move (and every later refresh).
    channel.v2.credential_ref = final_path_text.clone();
    channel.subscription.credential_ref = final_path_text;
    Ok(normalize_channel(channel, 0, None, None))
}

pub(crate) fn discard_subscription_import_channel(channel: &ChannelConfig) -> Result<bool> {
    discard_subscription_import_channel_in_dir(channel, &primary_subscription_credential_dir())
}

pub(crate) fn discard_subscription_import_channel_in_dir(
    channel: &ChannelConfig,
    managed_dir: &Path,
) -> Result<bool> {
    let path = PathBuf::from(channel.v2.credential_ref.trim());
    if !path_is_within_directory(&path, &pending_subscription_credential_dir_in(managed_dir)) {
        return Ok(false);
    }
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("remove pending subscription credential {}", path.display())),
    }
}

pub(crate) fn create_subscription_oauth_session_data(
    provider: &str,
) -> Result<SubscriptionOAuthSession> {
    let provider = normalize_subscription_provider(provider)
        .ok_or_else(|| anyhow!("unsupported subscription provider"))?;
    let state = if provider == "openai" {
        hex::encode(random_bytes(32)?)
    } else {
        base64_url_no_pad(&random_bytes(32)?)
    };
    let code_verifier = if provider == "openai" {
        hex::encode(random_bytes(64)?)
    } else {
        base64_url_no_pad(&random_bytes(32)?)
    };
    let code_challenge = pkce_challenge(&code_verifier);
    let (auth_url, redirect_uri, callback_hint) = match provider.as_str() {
        "openai" => {
            let redirect_uri = "http://localhost:1455/auth/callback".to_string();
            let mut url = reqwest::Url::parse("https://auth.openai.com/oauth/authorize")?;
            url.query_pairs_mut()
                .append_pair("response_type", "code")
                .append_pair("client_id", OPENAI_OAUTH_CLIENT_ID)
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair(
                    "scope",
                    "openid profile email offline_access api.connectors.read api.connectors.invoke",
                )
                .append_pair("state", &state)
                .append_pair("code_challenge", &code_challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("id_token_add_organizations", "true")
                .append_pair("codex_cli_simplified_flow", "true")
                .append_pair("originator", CODEX_ORIGINATOR);
            (
                url.to_string(),
                redirect_uri,
                "After signing in, copy the localhost callback URL from the address bar, even if the browser cannot open it."
                    .to_string(),
            )
        }
        "claude" => {
            let redirect_uri = CLAUDE_OAUTH_REDIRECT_URI.to_string();
            let mut url = reqwest::Url::parse(CLAUDE_OAUTH_AUTHORIZE_URL)?;
            url.query_pairs_mut()
                .append_pair("code", "true")
                .append_pair("client_id", CLAUDE_OAUTH_CLIENT_ID)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("scope", CLAUDE_OAUTH_AUTHORIZE_SCOPE)
                .append_pair("code_challenge", &code_challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &state);
            (
                url.to_string(),
                redirect_uri,
                "After authorization, copy the callback URL containing code. Claude also accepts code#state.".to_string(),
            )
        }
        "antigravity" => {
            crate::local_policy::require_antigravity_oauth_app()?;
            let redirect_uri = ANTIGRAVITY_REDIRECT_URI.to_string();
            let mut url = reqwest::Url::parse(ANTIGRAVITY_OAUTH_AUTHORIZE_URL)?;
            url.query_pairs_mut()
                .append_pair("response_type", "code")
                .append_pair("client_id", ANTIGRAVITY_OAUTH_CLIENT_ID)
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("scope", ANTIGRAVITY_SCOPES)
                .append_pair("state", &state)
                .append_pair("code_challenge", &code_challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("access_type", "offline")
                .append_pair("prompt", "consent")
                .append_pair("include_granted_scopes", "true");
            (
                url.to_string(),
                redirect_uri,
                "If Google redirects to an unreachable localhost page, copy the full callback URL from the address bar."
                    .to_string(),
            )
        }
        "grok" => {
            let redirect_uri = GROK_OAUTH_REDIRECT_URI.to_string();
            let nonce = base64_url_no_pad(&random_bytes(32)?);
            let mut url = reqwest::Url::parse(GROK_OAUTH_AUTHORIZE_URL)?;
            url.query_pairs_mut()
                .append_pair("response_type", "code")
                .append_pair("client_id", GROK_OAUTH_CLIENT_ID)
                .append_pair("redirect_uri", &redirect_uri)
                .append_pair("scope", GROK_OAUTH_SCOPE)
                .append_pair("state", &state)
                .append_pair("nonce", &nonce)
                .append_pair("code_challenge", &code_challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("plan", "generic")
                .append_pair("referrer", "const-api");
            (
                url.to_string(),
                redirect_uri,
                "If Grok redirects to an unreachable local page, copy the full callback URL from the address bar."
                    .to_string(),
            )
        }
        _ => return Err(anyhow!("unsupported subscription provider")),
    };
    Ok(SubscriptionOAuthSession {
        provider,
        auth_url,
        state,
        code_verifier,
        redirect_uri,
        callback_hint,
        auto_callback: false,
    })
}

pub(crate) struct ParsedOAuthCode {
    pub(crate) code: String,
    pub(crate) state: String,
}

pub(crate) fn parse_oauth_code_or_callback(input: &str) -> Result<ParsedOAuthCode> {
    let input = input.trim();
    if input.is_empty() {
        return Err(anyhow!("OAuth code is empty"));
    }
    if let Ok(url) = reqwest::Url::parse(input) {
        let code = url
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.to_string())
            .unwrap_or_default();
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.to_string())
            .unwrap_or_default();
        if !code.is_empty() {
            return Ok(ParsedOAuthCode { code, state });
        }
    }
    if let Some((code, state)) = input.split_once('#') {
        let code = code.trim().to_string();
        if code.is_empty() {
            return Err(anyhow!("OAuth code is empty"));
        }
        return Ok(ParsedOAuthCode {
            code,
            state: state.trim().to_string(),
        });
    }
    Ok(ParsedOAuthCode {
        code: input.to_string(),
        state: String::new(),
    })
}

pub(crate) async fn exchange_openai_oauth_code(
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<serde_json::Value> {
    let redirect_uri = if redirect_uri.trim().is_empty() {
        "http://localhost:1455/auth/callback"
    } else {
        redirect_uri.trim()
    };
    let mut form = std::collections::HashMap::new();
    form.insert("grant_type", "authorization_code");
    form.insert("client_id", OPENAI_OAUTH_CLIENT_ID);
    form.insert("code", code.trim());
    form.insert("redirect_uri", redirect_uri);
    form.insert("code_verifier", code_verifier.trim());
    let client = long_http_client();
    // Authorization-code exchange uses Codex's raw auth client: no model
    // provider `version` header and no synthesized default identity headers.
    let mut token = client
        .post("https://auth.openai.com/oauth/token")
        .timeout(SUBSCRIPTION_OAUTH_HTTP_TIMEOUT)
        .form(&form)
        .send_adaptive()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    ensure_subscription_token_provider(&mut token, "openai");
    normalize_token_expiry(&mut token);
    Ok(token)
}

pub(crate) async fn exchange_claude_oauth_code(
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
    state: &str,
) -> Result<serde_json::Value> {
    let redirect_uri = if redirect_uri.trim().is_empty() {
        CLAUDE_OAUTH_REDIRECT_URI
    } else {
        redirect_uri.trim()
    };
    let mut body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": CLAUDE_OAUTH_CLIENT_ID,
        "code": code.trim(),
        "redirect_uri": redirect_uri,
        "code_verifier": code_verifier.trim(),
    });
    if !state.trim().is_empty() {
        body["state"] = serde_json::Value::String(state.trim().to_string());
    }
    let mut token = long_http_client()
        .post(CLAUDE_OAUTH_TOKEN_URL)
        .timeout(SUBSCRIPTION_OAUTH_HTTP_TIMEOUT)
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header("User-Agent", CLAUDE_OAUTH_AXIOS_USER_AGENT)
        .json(&body)
        .send_adaptive()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    ensure_subscription_token_provider(&mut token, "claude");
    normalize_token_expiry(&mut token);
    Ok(token)
}

pub(crate) async fn exchange_antigravity_oauth_code(
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<serde_json::Value> {
    let redirect_uri = if redirect_uri.trim().is_empty() {
        ANTIGRAVITY_REDIRECT_URI
    } else {
        redirect_uri.trim()
    };
    let mut form = std::collections::HashMap::new();
    form.insert("grant_type", "authorization_code");
    crate::local_policy::require_antigravity_oauth_app()?;
    form.insert("client_id", ANTIGRAVITY_OAUTH_CLIENT_ID);
    form.insert("client_secret", ANTIGRAVITY_OAUTH_CLIENT_SECRET);
    form.insert("code", code.trim());
    form.insert("redirect_uri", redirect_uri);
    form.insert("code_verifier", code_verifier.trim());
    let response = long_http_client()
        .post(ANTIGRAVITY_OAUTH_TOKEN_URL)
        .timeout(SUBSCRIPTION_OAUTH_HTTP_TIMEOUT)
        .form(&form)
        .send_adaptive()
        .await?;
    let status = response.status();
    let mut token = response.json::<serde_json::Value>().await?;
    if !status.is_success() {
        let error = json_string(&token, &["error"]).unwrap_or_else(|| "unknown_error".to_string());
        let description = json_string(&token, &["error_description"]).unwrap_or_default();
        if error == "invalid_grant" {
            return Err(anyhow!(
                "Antigravity OAuth code expired, was already used, or belongs to another authorization session; generate a new authorization link."
            ));
        }
        return Err(anyhow!(
            "Antigravity OAuth token exchange returned {status}: {error}{}",
            if description.trim().is_empty() {
                String::new()
            } else {
                format!(": {description}")
            }
        ));
    }
    ensure_subscription_token_provider(&mut token, "antigravity");
    ensure_antigravity_token_defaults(&mut token);
    normalize_token_expiry(&mut token);
    if let Some(access_token) =
        json_string(&token, &["access_token"]).filter(|value| !value.trim().is_empty())
    {
        if let Ok(response) = long_http_client()
            .get(ANTIGRAVITY_USERINFO_URL)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("User-Agent", antigravity_user_agent())
            .send_adaptive()
            .await
        {
            if response.status().is_success() {
                if let Ok(user_info) = response.json::<serde_json::Value>().await {
                    if let Some(email) =
                        json_string(&user_info, &["email"]).filter(|value| !value.trim().is_empty())
                    {
                        token["email"] = serde_json::Value::String(email);
                    }
                }
            }
        }
    }
    Ok(token)
}

pub(crate) async fn exchange_grok_oauth_code(
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> Result<serde_json::Value> {
    let redirect_uri = if redirect_uri.trim().is_empty() {
        GROK_OAUTH_REDIRECT_URI
    } else {
        redirect_uri.trim()
    };
    let mut form = std::collections::HashMap::new();
    form.insert("grant_type", "authorization_code");
    form.insert("client_id", GROK_OAUTH_CLIENT_ID);
    form.insert("code", code.trim());
    form.insert("redirect_uri", redirect_uri);
    form.insert("code_verifier", code_verifier.trim());
    let response = long_http_client()
        .post(GROK_OAUTH_TOKEN_URL)
        .timeout(SUBSCRIPTION_OAUTH_HTTP_TIMEOUT)
        .header("User-Agent", "const-api-grok-oauth/1.0")
        .form(&form)
        .send_adaptive()
        .await?;
    let status = response.status();
    let mut token = response.json::<serde_json::Value>().await?;
    if !status.is_success() {
        return Err(anyhow!(
            "Grok OAuth token exchange returned {status}: {token}"
        ));
    }
    ensure_subscription_token_provider(&mut token, "grok");
    if let Some(object) = token.as_object_mut() {
        object.insert(
            "client_id".to_string(),
            serde_json::Value::String(GROK_OAUTH_CLIENT_ID.to_string()),
        );
    }
    normalize_token_expiry(&mut token);
    Ok(token)
}

pub(crate) fn ensure_subscription_token_provider(token: &mut serde_json::Value, provider: &str) {
    let inferred_email = email_from_id_token(token)
        .or_else(|| json_nested_string(token, &["account", "email_address"]));
    // Codex sends the selected account ID with both HTTP and WebSocket requests.
    // OAuth token responses can carry it only inside the JWT, unlike imported files.
    let inferred_account_id = (provider == "openai")
        .then(|| {
            json_string(token, &["account_id"])
                .or_else(|| json_string(token, &["accountID"]))
                .or_else(|| json_string(token, &["chatgpt_account_id"]))
                .filter(|id| !id.trim().is_empty())
                .or_else(|| {
                    jwt_nested_claim_from_token_value(
                        token,
                        &["https://api.openai.com/auth", "chatgpt_account_id"],
                    )
                })
        })
        .flatten();
    if let Some(object) = token.as_object_mut() {
        if let Some(account_id) = inferred_account_id {
            object.insert(
                "account_id".to_string(),
                serde_json::Value::String(account_id),
            );
        }
        object.insert(
            "type".to_string(),
            serde_json::Value::String(provider.to_string()),
        );
        if !object.contains_key("email") {
            if let Some(email) = inferred_email {
                object.insert("email".to_string(), serde_json::Value::String(email));
            }
        }
    }
}

pub(crate) fn ensure_antigravity_token_defaults(token: &mut serde_json::Value) {
    let Some(object) = token.as_object_mut() else {
        return;
    };
    object
        .entry("oauth_type")
        .or_insert_with(|| serde_json::Value::String("antigravity".to_string()));
    object
        .entry("token_type")
        .or_insert_with(|| serde_json::Value::String("Bearer".to_string()));
}

pub(crate) fn normalize_token_expiry(token: &mut serde_json::Value) {
    let existing_expiry = token_expiry_unix(token);
    let Some(object) = token.as_object_mut() else {
        return;
    };
    if let Some(expiry) = existing_expiry {
        object.insert(
            "expired".to_string(),
            serde_json::Value::Number(expiry.into()),
        );
        return;
    }
    let expires_in = object
        .get("expires_in")
        .and_then(|value| match value {
            serde_json::Value::Number(value) => value.as_i64(),
            serde_json::Value::String(value) => value.trim().parse::<i64>().ok(),
            _ => None,
        })
        .unwrap_or(0);
    if expires_in > 0 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        let expiry = (now + expires_in).max(now + 30);
        object.insert(
            "expired".to_string(),
            serde_json::Value::Number(expiry.into()),
        );
    }
}

pub(crate) fn random_bytes(len: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0_u8; len];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .context("generate secure random bytes")?;
    Ok(bytes)
}

pub(crate) fn pkce_challenge(verifier: &str) -> String {
    let digest = sha2::Sha256::digest(verifier.as_bytes());
    base64_url_no_pad(&digest)
}

pub(crate) fn base64_url_no_pad(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn sanitize_filename_part(value: &str) -> String {
    let value = value.trim();
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "account".to_string()
    } else {
        sanitized
    }
}

pub(crate) fn subscription_credential_candidate_from_path(
    path: &Path,
) -> Option<SubscriptionCredentialCandidate> {
    let raw = fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    subscription_credential_candidate_from_value(&value, Some(path))
}

pub(crate) fn normalize_antigravity_credential_value(
    value: &serde_json::Value,
) -> Option<serde_json::Value> {
    let declared_provider =
        json_string(value, &["type"]).or_else(|| json_string(value, &["provider"]));
    if declared_provider.as_deref().is_some_and(|provider| {
        normalize_subscription_provider(provider).as_deref() != Some("antigravity")
    }) {
        return None;
    }
    if json_string(value, &["oauth_type"])
        .as_deref()
        .is_some_and(|oauth_type| !oauth_type.eq_ignore_ascii_case("antigravity"))
    {
        return None;
    }

    let token = value.get("token").unwrap_or(value);
    let access_token = json_string(token, &["accessToken"])
        .or_else(|| json_string(token, &["access_token"]))
        .or_else(|| json_string(value, &["access_token"]));
    let refresh_token = json_string(token, &["refreshToken"])
        .or_else(|| json_string(token, &["refresh_token"]))
        .or_else(|| json_string(value, &["refresh_token"]));
    if access_token
        .as_ref()
        .map(|token| token.trim().is_empty())
        .unwrap_or(true)
        && refresh_token
            .as_ref()
            .map(|token| token.trim().is_empty())
            .unwrap_or(true)
    {
        return None;
    }

    let mut normalized_object = value.as_object().cloned().unwrap_or_default();
    normalized_object.remove("token");
    for alias in ["accessToken", "refreshToken", "expiresAt", "tokenType"] {
        normalized_object.remove(alias);
    }
    if let Some(token_object) = token.as_object() {
        for (key, value) in token_object {
            if matches!(
                key.as_str(),
                "accessToken"
                    | "access_token"
                    | "refreshToken"
                    | "refresh_token"
                    | "expiresAt"
                    | "expires_at"
                    | "expiry_date"
                    | "expired"
                    | "expires_in"
                    | "tokenType"
                    | "token_type"
            ) {
                continue;
            }
            normalized_object
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }

    let mut normalized = serde_json::Value::Object(normalized_object);
    normalized["type"] = serde_json::Value::String("antigravity".to_string());
    normalized["provider"] = serde_json::Value::String("antigravity".to_string());
    normalized["oauth_type"] = serde_json::Value::String("antigravity".to_string());
    normalized["token_type"] = serde_json::Value::String(
        json_string(token, &["tokenType"])
            .or_else(|| json_string(token, &["token_type"]))
            .or_else(|| json_string(value, &["token_type"]))
            .unwrap_or_else(|| "Bearer".to_string()),
    );
    if let Some(access_token) = access_token.filter(|token| !token.trim().is_empty()) {
        normalized["access_token"] = serde_json::Value::String(access_token);
    }
    if let Some(refresh_token) = refresh_token.filter(|token| !token.trim().is_empty()) {
        normalized["refresh_token"] = serde_json::Value::String(refresh_token);
    }
    if let Some(scope) = json_string(token, &["scope"]).or_else(|| json_string(value, &["scope"])) {
        normalized["scope"] = serde_json::Value::String(scope);
    }
    if let Some(email) = json_string(value, &["email"])
        .or_else(|| json_nested_string(value, &["account", "email"]))
        .filter(|value| value.contains('@'))
    {
        normalized["email"] = serde_json::Value::String(email);
    }
    if let Some(project_id) = json_string(value, &["project_id"])
        .or_else(|| json_string(value, &["cloudaicompanionProject"]))
    {
        normalized["project_id"] = serde_json::Value::String(project_id);
    }
    if let Some(tier_id) =
        json_string(value, &["tier_id"]).or_else(|| json_string(value, &["tier"]))
    {
        normalized["tier_id"] = serde_json::Value::String(tier_id);
    }
    for expiry_key in [
        "expires_at_unix",
        "expired",
        "expires_at",
        "expiresAt",
        "expiry_date",
        "expires_in",
    ] {
        if normalized.get(expiry_key).is_none() {
            if let Some(expiry) = token.get(expiry_key) {
                normalized[expiry_key] = expiry.clone();
            }
        }
    }
    normalize_token_expiry(&mut normalized);
    Some(normalized)
}
pub(crate) fn subscription_credential_candidate_from_value(
    value: &serde_json::Value,
    path: Option<&Path>,
) -> Option<SubscriptionCredentialCandidate> {
    if json_string(value, &["disabled"])
        .map(|disabled| disabled.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return None;
    }
    let provider = infer_subscription_provider(value)?;
    let email = json_string(&value, &["email"])
        .or_else(|| json_nested_string(&value, &["account", "email"]))
        .or_else(|| json_nested_string(&value, &["account", "email_address"]))
        .or_else(|| json_nested_string(&value, &["user", "email"]))
        .or_else(|| email_from_id_token(value))
        .unwrap_or_else(|| "unknown account".to_string());
    let expires_at = json_string(&value, &["expires_at_unix"])
        .or_else(|| json_string(&value, &["expired"]))
        .or_else(|| json_string(&value, &["expires_at"]))
        .or_else(|| json_string(&value, &["expiresAt"]))
        .or_else(|| json_string(&value, &["expiry_date"]))
        .unwrap_or_default();
    let has_access_token = json_string(&value, &["access_token"])
        .map(|token| !token.trim().is_empty())
        .unwrap_or(false);
    let has_refresh_token = json_string(&value, &["refresh_token"])
        .map(|token| !token.trim().is_empty())
        .unwrap_or(false);
    let status = if !has_refresh_token {
        "missing_refresh"
    } else if !has_access_token {
        "refreshable"
    } else if credential_is_expired(&value) {
        "expired_refreshable"
    } else {
        "ready"
    }
    .to_string();
    Some(SubscriptionCredentialCandidate {
        provider: provider.clone(),
        label: subscription_provider_label(&provider).to_string(),
        email,
        path: path
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_default(),
        expires_at,
        has_access_token,
        has_refresh_token,
        status,
    })
}

pub(crate) fn subscription_credential_dirs() -> Vec<PathBuf> {
    let home = home_dir();
    let mut credential_dirs = vec![home.join(".cli-proxy-api")];
    for managed_dir in subscription_managed_credential_dirs() {
        if !credential_dirs.contains(&managed_dir) {
            credential_dirs.push(managed_dir);
        }
    }
    credential_dirs
}

pub(crate) fn collect_json_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 2 || !dir.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, depth + 1, out);
            continue;
        }
        if path
            .extension()
            .and_then(|value| value.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("json"))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

pub(crate) fn infer_subscription_provider(value: &serde_json::Value) -> Option<String> {
    if let Some(provider) =
        json_string(value, &["type"]).and_then(|value| normalize_subscription_provider(&value))
    {
        return Some(provider);
    }
    if let Some(provider) =
        json_string(value, &["provider"]).and_then(|value| normalize_subscription_provider(&value))
    {
        return Some(provider);
    }
    None
}

pub(crate) fn normalize_subscription_provider(value: &str) -> Option<String> {
    let value = value.trim().to_lowercase();
    if value.contains("antigravity") {
        return Some("antigravity".to_string());
    }
    if value.contains("grok") || value == "xai" || value == "x.ai" {
        return Some("grok".to_string());
    }
    if value.contains("claude") || value.contains("anthropic") {
        return Some("claude".to_string());
    }
    if value.contains("codex") || value.contains("openai") || value.contains("chatgpt") {
        return Some("openai".to_string());
    }
    None
}

pub(crate) fn subscription_candidate_can_import(status: &str) -> bool {
    matches!(status, "ready" | "refreshable" | "expired_refreshable")
}

pub(crate) fn credential_is_expired(value: &serde_json::Value) -> bool {
    if value
        .get("expired")
        .and_then(|expired| expired.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    token_expiry_unix(value).is_some_and(|seconds| seconds > 0 && seconds < now)
}

pub(crate) fn email_from_id_token(value: &serde_json::Value) -> Option<String> {
    let token = json_string(value, &["id_token"])?;
    let payload = token.split('.').nth(1)?;
    let decoded = decode_jwt_payload(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    json_string(&claims, &["email"])
}

pub(crate) fn decode_jwt_payload(payload: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .or_else(|_| BASE64.decode(payload))
        .map_err(|err| anyhow!("decode jwt payload: {err}"))
}

pub(crate) fn subscription_provider_label(provider: &str) -> &'static str {
    match provider {
        "openai" => "OpenAI Account Subscription",
        "claude" => "Claude Account Subscription",
        "antigravity" => "Antigravity Account Subscription",
        "grok" => "Grok Account Subscription",
        _ => "Account Authorization",
    }
}

pub(crate) fn subscription_channel_name(provider: &str) -> &'static str {
    match provider {
        "openai" => "OpenAI Account Subscription",
        "claude" => "Claude Account Subscription",
        "antigravity" => "Antigravity Account Subscription",
        "grok" => "Grok Account Subscription",
        _ => "Account Subscription",
    }
}

pub(crate) fn json_string(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    match current {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn json_nested_string(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    json_string(value, path)
}
