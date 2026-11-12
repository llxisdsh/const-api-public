//! OpenDesign's account login and Cloud API key are distinct credentials.
//! See docs/coding-gateways.md for the released Vela contract and billing limits.
use crate::{
    model::{ChannelConfig, QuotaWindow},
    model_catalog::{ModelCatalogKind, ModelObservation},
    source_driver::SourceDriverId,
    upstream_transport::AdaptiveRequestBuilderExt,
};
use anyhow::{Context, Result, anyhow, ensure};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{sync::OnceLock, time::Duration};

const API_ORIGIN: &str = "https://amr-api.open-design.ai";
const LINK_ORIGIN: &str = "https://amr-link.open-design.ai";
static DIRECT: OnceLock<crate::http_client::HttpClient> = OnceLock::new();
static PROXY: OnceLock<crate::http_client::HttpClient> = OnceLock::new();

fn client(use_system_proxy: bool) -> Result<Client> {
    Ok(crate::http_client::cached_client(
        if use_system_proxy { &PROXY } else { &DIRECT },
        use_system_proxy,
        move || {
            crate::http_client::proxy_policy(
                Client::builder()
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(Duration::from_secs(30))
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never()),
                use_system_proxy,
            )
        },
    )?)
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("OpenDesign credential is missing {key}"))
}

fn workspace_id(value: &str) -> Result<&str> {
    ensure!(
        !value.is_empty()
            && value.len() <= 256
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
        "OpenDesign workspace ID is invalid"
    );
    Ok(value)
}

fn check_service(value: &Value, field: &str, expected: &str) -> Result<()> {
    if let Some(url) = value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        ensure!(
            url.trim_end_matches('/') == expected,
            "OpenDesign {field} must use the official production endpoint"
        );
    }
    Ok(())
}

/// This is a scoped runtime/control-key pair, not a refreshable OAuth token.
pub(crate) fn validate_credential(value: &Value) -> Result<()> {
    for key in ["access_token", "control_key"] {
        HeaderValue::from_str(required(value, key)?)
            .context("OpenDesign credential contains invalid header characters")?;
    }
    workspace_id(required(value, "workspace_id")?)?;
    check_service(value, "apiUrl", API_ORIGIN)?;
    check_service(value, "linkUrl", LINK_ORIGIN)?;
    Ok(())
}

fn credential(channel: &ChannelConfig) -> Result<Value> {
    let path = crate::detection::subscription_credential_path(channel)?;
    let value = crate::detection::read_subscription_credential(&path)?;
    ensure!(
        value["provider"] == "opendesign",
        "OpenDesign credential provider mismatch"
    );
    validate_credential(&value)?;
    Ok(value)
}

fn local_profile(value: &Value) -> Result<&Value> {
    let profile = value
        .pointer("/profiles/prod")
        .filter(|v| v.is_object())
        .ok_or_else(|| {
            anyhow!(
                "OpenDesign production login was not found; sign in to OpenDesign or Vela first"
            )
        })?;
    required(profile, "runtimeKey")?;
    required(profile, "controlKey")?;
    check_service(profile, "apiUrl", API_ORIGIN)?;
    check_service(profile, "linkUrl", LINK_ORIGIN)?;
    Ok(profile)
}

fn default_workspace(value: &Value) -> Result<&Value> {
    let items = value["items"]
        .as_array()
        .ok_or_else(|| anyhow!("OpenDesign workspace list is invalid"))?;
    let active: Vec<_> = items
        .iter()
        .filter(|item| item["memberStatus"] == "active" && item["lifecycleState"] == "active")
        .collect();
    let defaults: Vec<_> = active
        .iter()
        .copied()
        .filter(|item| item["isDefault"] == true)
        .collect();
    let selected = match defaults.as_slice() {
        [one] => *one,
        [] if active.len() == 1 => active[0],
        _ => {
            return Err(anyhow!(
                "OpenDesign has no unique default workspace; choose a default in OpenDesign before importing"
            ));
        }
    };
    workspace_id(required(selected, "workspaceId")?)?;
    Ok(selected)
}

fn managed_credential(profile: &Value, workspace: &Value) -> Result<Value> {
    let value = json!({
        "provider": "opendesign", "type": "opendesign",
        "access_token": required(profile, "runtimeKey")?,
        "control_key": required(profile, "controlKey")?,
        "workspace_id": required(workspace, "workspaceId")?,
        "workspace_name": workspace["workspaceName"],
        "user": profile["user"],
    });
    validate_credential(&value)?;
    Ok(value)
}

async fn control_json(client: &Client, url: &str, key: &str) -> Result<Value> {
    let response = client
        .get(url)
        .bearer_auth(key)
        .header("accept", "application/json")
        .header("user-agent", "Go-http-client/1.1")
        .send_adaptive()
        .await?;
    crate::http_client::observe_response(&response);
    // Never include a control-plane response body or credential in diagnostics.
    ensure!(
        response.status().is_success(),
        "OpenDesign account query returned HTTP {}",
        response.status().as_u16()
    );
    response
        .json()
        .await
        .context("OpenDesign account query returned invalid JSON")
}

#[tauri::command]
pub(crate) async fn import_opendesign_local_credential(
    use_system_proxy: bool,
    config_json: Option<String>,
) -> Result<String, String> {
    async {
        // Read only the official production profile. No ambient key, test profile,
        // browser cookie, CLI process, or OpenDesign configuration is modified.
        // Newer packaged OpenDesign builds can use an isolated AMR_HOME.
        // Let the user select that login file instead of guessing another
        // environment/account or scanning unrelated application data.
        let bytes = match config_json {
            Some(config) => config.into_bytes(),
            None => tokio::fs::read(crate::home_dir().join(".amr").join("config.json"))
                .await
                .context("OpenDesign login file is unavailable; sign in or select the Vela config.json file")?,
        };
        let file: Value =
            serde_json::from_slice(&bytes).context("OpenDesign login file is invalid")?;
        let profile = local_profile(&file)?;
        let workspaces = control_json(
            &client(use_system_proxy)?,
            &format!("{API_ORIGIN}/api/v1/workspaces"),
            required(profile, "controlKey")?,
        )
        .await?;
        let workspace = default_workspace(&workspaces)?;
        Ok(managed_credential(profile, workspace)?.to_string())
    }
    .await
    .map_err(|error: anyhow::Error| error.to_string())
}

/// Replace caller account scope with the channel's pinned scope. Control keys
/// are never used on the inference path. The API-key preset does not call this.
pub(crate) fn prepare_auth(
    channel: &ChannelConfig,
    headers: &mut HeaderMap,
) -> Result<ChannelConfig> {
    let value = credential(channel)?;
    headers.insert(
        "x-vela-workspace-id",
        HeaderValue::from_str(required(&value, "workspace_id")?)?,
    );
    // CONST invokes the standalone CLI contract; it is not an OpenDesign app
    // session and cannot promise app-only promotional/free-model entitlements.
    headers.insert(
        "x-amr-client-source",
        HeaderValue::from_static("standalone"),
    );
    headers.insert("x-amr-request-role", HeaderValue::from_static("primary"));
    let mut authenticated = channel.clone();
    authenticated.upstream_api_key = required(&value, "access_token")?.to_owned();
    Ok(authenticated)
}

pub(crate) async fn catalog(
    client: &Client,
    channel: &ChannelConfig,
) -> Result<Vec<ModelObservation>> {
    let mut headers = HeaderMap::new();
    let authenticated = prepare_auth(channel, &mut headers)?;
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", authenticated.upstream_api_key))?,
    );
    crate::model_catalog::fetch_model_catalog(
        client,
        crate::coding_gateway::catalog_url(
            SourceDriverId::OpenDesignSubscription,
            &channel.upstream_base_url,
        )?,
        headers,
        ModelCatalogKind::OpenDesign,
    )
    .await
}

pub(crate) async fn quota(channel: &ChannelConfig) -> Result<Vec<QuotaWindow>> {
    let value = credential(channel)?;
    let id = required(&value, "workspace_id")?;
    let body = control_json(
        &client(channel.v2.use_system_proxy)?,
        &format!("{API_ORIGIN}/api/v1/workspaces/{id}/billing/preflight"),
        required(&value, "control_key")?,
    )
    .await?;
    quota_windows(&body, id, crate::now_unix())
}

fn quota_windows(value: &Value, workspace: &str, now: i64) -> Result<Vec<QuotaWindow>> {
    let plan = &value["codingPlan"];
    ensure!(
        value["schemaVersion"] == 1
            && value["workspaceId"].as_str() == Some(workspace)
            && plan["workspaceId"].as_str() == Some(workspace),
        "OpenDesign quota workspace or schema mismatch"
    );
    // The released account API's snapshot may omit designPlanWindows. The
    // read-only preflight returns the actual plan windows, even with modelId
    // absent (where its funding hint can be `wallet`). Do not use that hint
    // to infer plan exhaustion or turn this metadata read into generation.
    let windows = plan["windows"]
        .as_array()
        .ok_or_else(|| anyhow!("OpenDesign quota windows are invalid"))?;
    let mut result = Vec::new();
    for window in windows {
        let kind = match window["durationSeconds"].as_u64() {
            Some(18000) => "5h",
            Some(604800) => "7d",
            _ => continue,
        };
        let number = |key: &str| -> Result<f64> {
            window[key]
                .as_f64()
                .or_else(|| window[key].as_str()?.parse().ok())
                .filter(|n| n.is_finite())
                .ok_or_else(|| anyhow!("OpenDesign quota {key} is invalid"))
        };
        let remaining = number("remainingCredits")?;
        let limit = number("limitCredits")?;
        ensure!(
            limit > 0.0 && remaining >= 0.0 && remaining <= limit,
            "OpenDesign quota credits are out of range"
        );
        ensure!(
            !result.iter().any(|w: &QuotaWindow| w.window == kind),
            "OpenDesign quota repeats a window"
        );
        let reset = match window.get("resetsAt").filter(|v| !v.is_null()) {
            None => 0,
            Some(value) => time::OffsetDateTime::parse(
                value.as_str().unwrap_or_default(),
                &time::format_description::well_known::Rfc3339,
            )
            .context("OpenDesign quota reset time is invalid")?
            .unix_timestamp(),
        };
        result.push(QuotaWindow {
            source: "opendesign_billing".into(),
            window: kind.into(),
            remaining_ratio: remaining / limit,
            used_percent: (1.0 - remaining / limit) * 100.0,
            reset_at_unix: reset,
            checked_at_unix: now,
            model: None,
            token_type: None,
        });
    }
    // Wallet dollars and subscription credits are separate units. In particular,
    // a zero wallet is not an exhausted Design Plan, nor a fabricated percentage.
    Ok(result)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn profile() -> Value {
        json!({"runtimeKey":"runtime-test", "controlKey":"control-test",
            "apiUrl":API_ORIGIN, "linkUrl":LINK_ORIGIN, "user":{"id":"test-user"}})
    }
    fn workspace(id: &str, default: bool) -> Value {
        json!({"workspaceId":id,"workspaceName":"Test workspace", "isDefault":default,
            "memberStatus":"active","lifecycleState":"active"})
    }
    fn snapshot() -> Value {
        json!({"schemaVersion":1,"workspaceId":"workspace-a", "funding":"wallet",
            "balanceUsd":"0.00", "codingPlan":{"workspaceId":"workspace-a","eligible":true,"tier":"go","windows":[
                {"durationSeconds":18000,"remainingCredits":"80","limitCredits":"100","resetsAt":"2026-11-08T10:00:00Z"},
                {"durationSeconds":604800,"remainingCredits":"25","limitCredits":"100","resetsAt":null}]}})
    }

    pub(crate) fn attach_credential(channel: &mut ChannelConfig, directory: &std::path::Path) {
        let path = directory.join("opendesign.json");
        std::fs::write(
            &path,
            managed_credential(&profile(), &workspace("workspace-a", true))
                .unwrap()
                .to_string(),
        )
        .unwrap();
        channel.v2.credential_ref = path.to_string_lossy().into_owned();
        channel.subscription.credential_ref = channel.v2.credential_ref.clone();
        channel.subscription.platform = "opendesign".into();
        channel.upstream_api_key.clear();
    }

    #[test]
    fn opendesign_local_import_requires_production_login_and_a_unique_active_workspace() {
        let file = json!({"profiles":{"prod":profile(),"test":{"runtimeKey":"test-key"}}});
        assert_eq!(
            required(local_profile(&file).unwrap(), "runtimeKey").unwrap(),
            "runtime-test"
        );
        assert!(local_profile(&json!({"profiles":{"test":profile()}})).is_err());
        let mut custom = file.clone();
        custom["profiles"]["prod"]["linkUrl"] = json!("https://other.example");
        assert!(local_profile(&custom).is_err());
        let list = json!({"items":[workspace("workspace-a",false),workspace("workspace-b",true)]});
        assert_eq!(
            default_workspace(&list).unwrap()["workspaceId"],
            "workspace-b"
        );
        assert!(
            default_workspace(&json!({"items":[workspace("a",false),workspace("b",false)]}))
                .is_err()
        );
        assert!(
            default_workspace(&json!({"items":[workspace("a",true),workspace("b",true)]})).is_err()
        );
        let mut inactive = workspace("bad", true);
        inactive["memberStatus"] = json!("disabled");
        assert!(default_workspace(&json!({"items":[inactive]})).is_err());
        assert!(workspace_id("../other").is_err());
    }

    #[test]
    fn opendesign_import_does_not_confuse_api_keys_or_merge_workspaces() {
        let a = managed_credential(&profile(), &workspace("workspace-a", true)).unwrap();
        let b = managed_credential(&profile(), &workspace("workspace-b", true)).unwrap();
        let normalized = crate::detection::normalize_subscription_token("opendesign", a.clone())
            .unwrap()
            .1;
        assert_eq!(normalized["access_token"], "runtime-test");
        assert!(normalized.get("refresh_token").is_none());
        assert!(validate_credential(&json!({"access_token":"api-key"})).is_err());
        assert_ne!(
            crate::detection::subscription_account_identity_from_value(&a),
            crate::detection::subscription_account_identity_from_value(&b)
        );
        let dir = tempfile::tempdir().unwrap();
        let channel = crate::detection::prepare_subscription_token_and_import_in_dir(
            "opendesign",
            a,
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            channel.source_driver(),
            SourceDriverId::OpenDesignSubscription
        );
        assert_eq!(channel.v2.default_target.protocol.as_str(), "openai_chat");
        assert!(!channel.enabled);
        assert!(channel.upstream_api_key.is_empty());
        let encoded = serde_json::to_string(&crate::model::ChannelConfigV2::from(channel)).unwrap();
        assert!(!encoded.contains("runtime-test"));
        assert!(!encoded.contains("control-test"));
    }

    #[test]
    fn opendesign_quota_keeps_go_windows_separate_and_ignores_wallet_dollars() {
        let windows = quota_windows(&snapshot(), "workspace-a", 100).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].window, "5h");
        assert_eq!(windows[0].remaining_ratio, 0.8);
        assert_eq!(windows[1].window, "7d");
        assert_eq!(windows[1].remaining_ratio, 0.25);
        assert!(windows[0].reset_at_unix > 0);
        assert_eq!(windows[1].reset_at_unix, 0);
        assert!(
            windows
                .iter()
                .all(crate::supplier::quota_window_limits_supply)
        );
        let mut value = snapshot();
        value["balanceUsd"] = json!("100.00");
        value["codingPlan"]["windows"][0]["remainingCredits"] = json!("0");
        assert_eq!(
            quota_windows(&value, "workspace-a", 100).unwrap()[0].remaining_ratio,
            0.0
        );
        value["codingPlan"]["windows"] = json!([]);
        assert!(
            quota_windows(&value, "workspace-a", 100)
                .unwrap()
                .is_empty()
        );
        assert!(quota_windows(&snapshot(), "workspace-b", 100).is_err());
    }

    #[test]
    fn opendesign_quota_rejects_unreliable_values() {
        for invalid in [json!("NaN"), json!("-1"), json!("101"), Value::Null] {
            let mut value = snapshot();
            value["codingPlan"]["windows"][0]["remainingCredits"] = invalid;
            assert!(quota_windows(&value, "workspace-a", 100).is_err());
        }
        let mut value = snapshot();
        value["codingPlan"]["workspaceId"] = json!("another-workspace");
        assert!(quota_windows(&value, "workspace-a", 100).is_err());
        value = snapshot();
        value["codingPlan"]["windows"] = Value::Null;
        assert!(quota_windows(&value, "workspace-a", 100).is_err());
    }

    #[test]
    fn opendesign_catalog_preserves_entitlements_and_cli_context_semantics() {
        let models = crate::model_catalog::model_observations_from_value(
            ModelCatalogKind::OpenDesign,
            &json!({"data":[{"id":"allowed","enabled":true,
                "architecture":{"input_modalities":["text","image"],"output_modalities":["text"]},
                "metadata":{"context_limit":1000000,"context_budget":232000,"output_budget":32000}},
                {"id":"requires-upgrade","enabled":false}]}),
            "opendesign",
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "allowed");
        assert_eq!(models[0].context_tokens, Some(232000));
        assert_eq!(models[0].max_context_tokens, Some(1000000));
        assert_eq!(models[0].output_tokens, Some(32000));
        assert_eq!(models[0].input_tokens, None);
        assert!(models[0].input_modalities.contains(&"image".to_string()));
    }

    #[tokio::test]
    async fn opendesign_imported_channel_supports_manual_and_activation_probes() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use warp::Filter;

        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let route = warp::post()
            .and(warp::path!("v1" / "chat" / "completions"))
            .and(warp::header::headers_cloned())
            .and(warp::body::json())
            .map(move |headers: warp::http::HeaderMap, body: Value| {
                seen.fetch_add(1, Ordering::SeqCst);
                assert_eq!(headers["authorization"], "Bearer runtime-test");
                assert_eq!(headers["x-vela-workspace-id"], "workspace-a");
                assert_eq!(headers["x-amr-client-source"], "standalone");
                assert_eq!(headers["x-amr-request-role"], "primary");
                assert!(!format!("{headers:?}").contains("control-test"));
                assert_eq!(body["stream"], false);
                assert!(!body["messages"][0]["content"].as_str().unwrap().is_empty());
                warp::reply::json(&json!({
                    "id":"chat_probe", "object":"chat.completion", "model":body["model"],
                    "choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"OK"}}],
                    "usage":{"prompt_tokens":8,"completion_tokens":1}
                }))
            });
        let models = warp::get()
            .and(warp::path!("v1" / "models"))
            .and(warp::header::exact("authorization", "Bearer runtime-test"))
            .and(warp::header::exact("x-vela-workspace-id", "workspace-a"))
            .map(|| {
                warp::reply::json(&json!({"data":[
                    {"id":"claude-haiku-5.5","enabled":true},
                    {"id":"gpt-6-luna","enabled":true},
                    {"id":"minimax-m3.1-flash","enabled":true},
                    {"id":"disabled-model","enabled":false}
                ]}))
            });
        let (addr, server) = crate::bind_ephemeral!(route.or(models), ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let dir = tempfile::tempdir().unwrap();
        // Start with the actual imported channel shape, not a generic API fixture.
        let mut channel = crate::detection::prepare_subscription_token_and_import_in_dir(
            "opendesign",
            managed_credential(&profile(), &workspace("workspace-a", true)).unwrap(),
            dir.path(),
        )
        .unwrap();
        channel.v2.use_system_proxy = false;
        channel.surface_bindings = crate::source_driver_surface_bindings(
            channel.source_driver(),
            &format!("http://{addr}/v1"),
        );
        channel = crate::config::project_channel_legacy_fields(channel);
        channel.models = catalog(&Client::builder().no_proxy().build().unwrap(), &channel)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(channel.models.len(), 3);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "catalog discovery cannot generate"
        );
        let manual = crate::supplier::test_channel_upstream_model_inner(
            channel.clone(),
            "gpt-6-luna".into(),
            "Reply OK.".into(),
        )
        .await
        .unwrap();
        assert_eq!(manual.model, "gpt-6-luna");
        assert_eq!(
            (
                manual.http_status,
                manual.input_tokens,
                manual.output_tokens
            ),
            (200, 8, 1)
        );
        let automatic =
            crate::supplier::test_channel_upstream_inner(channel.clone(), "Reply OK.".into())
                .await
                .unwrap();
        assert_eq!(automatic.http_status, 200);

        // Activation checks every model family through the same probe entry point.
        let report = crate::supplier::availability::check(&channel).await;
        assert_eq!(report.groups.len(), 3);
        assert!(
            report
                .groups
                .iter()
                .all(|group| group.status == "available"),
            "{report:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        let (admitted, health) = crate::supplier::validate_supplier_channel(
            channel,
            crate::supplier::SupplierCheckMode::Cached,
        )
        .await
        .unwrap();
        assert!(admitted.enabled);
        assert_eq!(health.status, "available");
        assert_eq!(health.model_count, 3);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            5,
            "admission reuses the completed group checks"
        );
        server.abort();
    }

    /// Explicit opt-in only: this reads a real Vela login and consumes model quota.
    #[tokio::test]
    #[ignore = "requires authorized live account and explicit CONST_OPENDESIGN_LIVE_* settings"]
    async fn opendesign_live_import_and_probe() {
        let path = std::env::var("CONST_OPENDESIGN_LIVE_CONFIG")
            .expect("explicit Vela config path required");
        let model = std::env::var("CONST_OPENDESIGN_LIVE_MODEL").expect("explicit model required");
        let use_proxy: bool = std::env::var("CONST_OPENDESIGN_LIVE_USE_SYSTEM_PROXY")
            .expect("explicit proxy policy required")
            .parse()
            .unwrap();
        let local = std::fs::read_to_string(path).expect("read selected Vela config");
        let imported = import_opendesign_local_credential(use_proxy, Some(local))
            .await
            .expect("import workspace login");
        let dir = tempfile::tempdir().unwrap();
        let mut channel = crate::detection::prepare_subscription_token_and_import_in_dir(
            "opendesign",
            serde_json::from_str(&imported).unwrap(),
            dir.path(),
        )
        .unwrap();
        channel.v2.use_system_proxy = use_proxy;
        let models = catalog(&client(use_proxy).unwrap(), &channel)
            .await
            .expect("read account models");
        assert!(
            models.iter().any(|entry| entry.id == model),
            "requested model must be enabled for this account"
        );
        channel.models = models.into_iter().map(|entry| entry.id).collect();
        let before = quota(&channel).await.expect("read quota before probe");
        let result = crate::supplier::test_channel_upstream_model_inner(
            channel.clone(),
            model,
            "Reply with only OK. Do not use tools.".into(),
        )
        .await
        .expect("run one real supplier probe");
        assert_eq!(result.http_status, 200);
        let after = quota(&channel).await.expect("read quota after probe");
        // Log only status and counters, never credentials, user identity or raw content.
        println!(
            "OpenDesign live probe: status={}, latency_ms={}, input_tokens={}, output_tokens={}",
            result.http_status, result.latency_ms, result.input_tokens, result.output_tokens
        );
        for (label, windows) in [("before", before), ("after", after)] {
            for window in windows {
                println!(
                    "quota {label}: {} remaining={:.6}",
                    window.window, window.remaining_ratio
                );
            }
        }
    }

    #[tokio::test]
    async fn opendesign_control_errors_do_not_disclose_response_bodies() {
        use warp::Filter;
        let routes = warp::path("account").map(|| {
            warp::reply::with_status(
                "secret-in-upstream-error",
                warp::http::StatusCode::FORBIDDEN,
            )
        });
        let (addr, server) = crate::bind_ephemeral!(routes, ([127, 0, 0, 1], 0));
        let task = tokio::spawn(server);
        let error = control_json(
            &Client::builder().no_proxy().build().unwrap(),
            &format!("http://{addr}/account"),
            "synthetic-control",
        )
        .await
        .unwrap_err();
        task.abort();
        assert!(error.to_string().contains("403"));
        assert!(!format!("{error:#}").contains("secret-in-upstream-error"));
        assert!(!format!("{error:#}").contains("synthetic-control"));
    }
}
