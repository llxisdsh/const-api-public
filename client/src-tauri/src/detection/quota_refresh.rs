use super::*;
use std::sync::{Arc, Mutex, OnceLock};

const QUOTA_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
const QUOTA_RETRY_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default)]
struct QuotaCacheState {
    windows: Vec<QuotaWindow>,
    refresh_after: Option<Instant>,
    retry_not_before: Option<Instant>,
    last_attempt: Option<Instant>,
}

impl QuotaCacheState {
    fn refresh_due(&self, policy: SubscriptionCatalogPolicy, started: Instant) -> bool {
        !self
            .retry_not_before
            .is_some_and(|deadline| started < deadline)
            && !self.last_attempt.is_some_and(|attempt| {
                started.saturating_duration_since(attempt) < Duration::from_secs(10)
            })
            && subscription_refresh_due(policy, self.refresh_after, started)
    }

    fn complete(&mut self, result: &Result<Vec<QuotaWindow>>, now: Instant) {
        self.last_attempt = Some(now);
        self.retry_not_before = None;
        let delay = match result {
            Ok(windows) if !windows.is_empty() => {
                crate::supplier::merge_official_quota_windows(&mut self.windows, windows);
                QUOTA_REFRESH_INTERVAL
            }
            Ok(_) => QUOTA_RETRY_INTERVAL,
            Err(error) => {
                let delay = error
                    .downcast_ref::<QuotaFetchError>()
                    .and_then(|error| error.retry_after)
                    .unwrap_or(QUOTA_RETRY_INTERVAL)
                    .max(QUOTA_RETRY_INTERVAL);
                self.retry_not_before = now.checked_add(delay);
                delay
            }
        };
        self.refresh_after = now.checked_add(delay);
    }
}

#[derive(Default)]
struct QuotaCache {
    state: Mutex<QuotaCacheState>,
    // Do not hold the state lock during I/O: live response observations must
    // remain cheap and must not disappear while a usage query is in flight.
    refresh_lock: tokio::sync::Mutex<()>,
}

static QUOTA_CACHES: OnceLock<Mutex<std::collections::HashMap<String, Arc<QuotaCache>>>> =
    OnceLock::new();

fn quota_cache(channel: &ChannelConfig) -> Result<Arc<QuotaCache>> {
    let path = if crate::coding_plan::has_quota(channel.source_driver()) {
        PathBuf::from(hex::encode(sha2::Sha256::digest(
            channel.v2.credential_ref.as_bytes(),
        )))
    } else {
        subscription_credential_path(channel)?
    };
    let key = format!(
        "{}\0{}\0{}",
        channel.subscription.platform,
        channel.id.trim(),
        path.to_string_lossy()
    );
    let mut caches = QUOTA_CACHES
        .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(caches.entry(key).or_default().clone())
}

/// A metadata-only query, independent of model discovery and paid probes.
/// Empty results/errors retain the previous observation with its original age.
pub(super) async fn refresh_subscription_quota<F>(
    channel: &ChannelConfig,
    policy: SubscriptionCatalogPolicy,
    fetch: F,
) -> Result<Vec<QuotaWindow>>
where
    F: std::future::Future<Output = Result<Vec<QuotaWindow>>>,
{
    let cache = quota_cache(channel)?;
    let started = Instant::now();
    let _refresh = cache.refresh_lock.lock().await;
    {
        let state = cache.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state.refresh_due(policy, started) {
            return Ok(state.windows.clone());
        }
    }
    let result = fetch.await;
    let mut state = cache.state.lock().unwrap_or_else(|p| p.into_inner());
    state.complete(&result, Instant::now());
    if let Err(error) = result {
        // One record per actual attempt, not per 30-second status snapshot.
        log::warn!(
            "[const-api][subscription-quota] channel={} provider={} preserving previous quota: {error:#}",
            channel.id,
            channel.subscription.platform
        );
    }
    Ok(state.windows.clone())
}

pub(super) fn observe_subscription_quota(channel: &ChannelConfig, windows: &[QuotaWindow]) {
    if windows.is_empty() {
        return;
    }
    if let Ok(cache) = quota_cache(channel) {
        let mut state = cache.state.lock().unwrap_or_else(|p| p.into_inner());
        crate::supplier::merge_official_quota_windows(&mut state.windows, windows);
        // A response usually reports only a subset. Never postpone the complete
        // usage query, including the initial query, because one bucket changed.
    }
}

#[derive(Debug)]
struct QuotaFetchError {
    message: String,
    retry_after: Option<Duration>,
}

impl std::fmt::Display for QuotaFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QuotaFetchError {}

pub(crate) async fn quota_response_json(
    response: reqwest::Response,
    operation: &str,
) -> Result<serde_json::Value> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .trim()
                .parse::<u64>()
                .ok()
                .map(Duration::from_secs)
                .or_else(|| {
                    time::OffsetDateTime::parse(
                        value,
                        &time::format_description::well_known::Rfc2822,
                    )
                    .ok()
                    .map(|date| {
                        Duration::from_secs(
                            date.unix_timestamp().saturating_sub(now_unix()).max(0) as u64
                        )
                    })
                })
        });
    if !status.is_success() {
        // Preserve Retry-After even for HTML/plain-text errors and malformed JSON.
        let body = response.text().await.unwrap_or_default();
        return Err(QuotaFetchError {
            message: format!("{operation} returned {status}: {body}"),
            retry_after: retry_after
                .or_else(|| (status.as_u16() == 429).then_some(QUOTA_REFRESH_INTERVAL)),
        }
        .into());
    }
    Ok(response.json().await?)
}

pub(super) fn combined_quota_error(first: anyhow::Error, second: anyhow::Error) -> anyhow::Error {
    let delay = |error: &anyhow::Error| {
        error
            .downcast_ref::<QuotaFetchError>()
            .and_then(|error| error.retry_after)
    };
    QuotaFetchError {
        retry_after: delay(&first).max(delay(&second)),
        message: format!("{first}; {second}"),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn coding_plan_refresh_shares_catalog_and_reuses_quota_without_inference() {
        use crate::source_driver::SourceDriverId;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use warp::Filter;
        let gets = Arc::new(AtomicUsize::new(0));
        let seen = gets.clone();
        let route = warp::any().and(warp::method()).and(warp::path::full())
            .and(warp::header::headers_cloned())
            .map(move |method: warp::http::Method, path: warp::path::FullPath, headers: warp::http::HeaderMap| {
                assert_eq!(method, warp::http::Method::GET, "refresh must never generate tokens");
                assert!(headers["authorization"].to_str().unwrap().starts_with("Bearer "));
                assert!(headers["user-agent"].to_str().unwrap().starts_with("const-api/"));
                seen.fetch_add(1, Ordering::Relaxed);
                if path.as_str() == "/api/tags" {
                    warp::reply::json(&serde_json::json!({"models":[{"name":"gpt-oss:120b"}]}))
                } else {
                    assert!(matches!(path.as_str(), "/v1/models" | "/api/coding/paas/v4/models"));
                    warp::reply::json(&serde_json::json!({"data":[{"id":"vendor/model","context_length":262144,"supported_parameters":["tools"]}]}))
                }
            });
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        for driver in [
            SourceDriverId::CommandCode,
            SourceDriverId::KimiCode,
            SourceDriverId::GlmCodingPlan,
            SourceDriverId::MinimaxTokenPlan,
            SourceDriverId::OllamaCloud,
        ] {
            let mut channel =
                crate::coding_gateway::tests::channel(driver, &format!("http://{addr}/v1"));
            channel.id = format!("refresh-plan-{driver:?}-{addr}");
            if crate::coding_plan::has_quota(driver) {
                let quota = vec![QuotaWindow {
                    source: "official_header".into(),
                    window: "5h".into(),
                    remaining_ratio: 0.8,
                    used_percent: 20.0,
                    checked_at_unix: now_unix(),
                    ..Default::default()
                }];
                refresh_subscription_quota(&channel, SubscriptionCatalogPolicy::Cached, async {
                    Ok(quota)
                })
                .await
                .unwrap();
            }
            let before = gets.load(Ordering::Relaxed);
            let result = crate::detection::refresh_channel(channel.clone())
                .await
                .unwrap();
            assert_eq!(
                gets.load(Ordering::Relaxed) - before,
                1,
                "{driver:?}: one catalog shared by all surfaces"
            );
            assert_eq!(result.models.len(), 1);
            assert!(
                result
                    .surface_results
                    .iter()
                    .all(|result| result.protocol_verification.state != "verified")
            );
            if driver == SourceDriverId::OllamaCloud {
                assert_eq!(result.models[0], "gpt-oss:120b");
            } else {
                assert!(
                    result
                        .model_capability_evidence
                        .iter()
                        .any(|e| e.context_tokens == Some(262144))
                );
            }
            if crate::coding_plan::has_quota(driver) {
                assert_eq!(result.quota_windows.len(), 1);
                assert_eq!(result.remaining_ratio, 0.8);
            }
            crate::supplier::merge_channel_detection_result(&mut channel, &result);
            assert!(crate::source_driver::channel_is_platform_shareable(
                &channel
            ));
            assert_eq!(crate::supplier_from_channel(&channel).source_driver, driver);
        }
        server.abort();
    }

    fn window(name: &str, checked_at: i64, used: f64) -> QuotaWindow {
        QuotaWindow {
            source: "openai_wham_usage".into(),
            window: name.into(),
            used_percent: used,
            remaining_ratio: 1.0 - used / 100.0,
            checked_at_unix: checked_at,
            ..Default::default()
        }
    }

    #[test]
    fn initial_quota_is_due_without_regard_to_a_saved_model_catalog() {
        assert!(
            QuotaCacheState::default()
                .refresh_due(SubscriptionCatalogPolicy::Cached, Instant::now())
        );
    }

    #[test]
    fn success_and_failure_have_independent_short_cadences() {
        let now = Instant::now();
        let mut state = QuotaCacheState::default();
        state.complete(&Ok(vec![window("weekly", 10, 20.0)]), now);
        assert!(!state.refresh_due(
            SubscriptionCatalogPolicy::Cached,
            now + Duration::from_secs(299)
        ));
        assert!(state.refresh_due(
            SubscriptionCatalogPolicy::Cached,
            now + Duration::from_secs(300)
        ));
        state.complete(&Err(anyhow!("network unavailable")), now);
        assert_eq!(state.windows[0].checked_at_unix, 10);
        assert!(!state.refresh_due(
            SubscriptionCatalogPolicy::Cached,
            now + Duration::from_secs(59)
        ));
        assert!(state.refresh_due(
            SubscriptionCatalogPolicy::Cached,
            now + Duration::from_secs(60)
        ));
        state.complete(&Ok(Vec::new()), now);
        assert_eq!(state.windows.len(), 1);
        assert_eq!(state.windows[0].checked_at_unix, 10);
    }

    #[test]
    fn manual_refresh_respects_upstream_retry_after() {
        let now = Instant::now();
        let mut state = QuotaCacheState::default();
        state.complete(
            &Err(QuotaFetchError {
                message: "limited".into(),
                retry_after: Some(Duration::from_secs(900)),
            }
            .into()),
            now,
        );
        assert!(!state.refresh_due(
            SubscriptionCatalogPolicy::Fresh,
            now + Duration::from_secs(899)
        ));
        assert!(state.refresh_due(
            SubscriptionCatalogPolicy::Fresh,
            now + Duration::from_secs(900)
        ));
    }

    #[tokio::test]
    async fn all_subscription_providers_share_cache_and_partial_observation_rules() {
        for platform in ["openai", "claude", "grok", "antigravity"] {
            let mut channel = crate::channel_from_supplier(
                format!("quota-{platform}-{}", rand::random::<u64>()),
                &crate::default_supplier_config(),
            );
            channel.subscription.platform = platform.into();
            channel.subscription.credential_ref = "quota-test-no-credential-read.json".into();
            let first =
                refresh_subscription_quota(&channel, SubscriptionCatalogPolicy::Cached, async {
                    Ok(vec![window("5h", 10, 20.0), window("weekly", 10, 30.0)])
                })
                .await
                .expect("first query");
            assert_eq!(first.len(), 2);
            observe_subscription_quota(&channel, &[window("5h", 20, 40.0)]);
            let next =
                refresh_subscription_quota(&channel, SubscriptionCatalogPolicy::Cached, async {
                    panic!("cache hit must not perform an upstream query")
                })
                .await
                .expect("cached quota");
            assert_eq!(next.len(), 2);
            assert_eq!(next[0].used_percent, 40.0);
            assert_eq!(next[1].checked_at_unix, 10);
        }
    }

    #[test]
    fn newer_partial_observations_keep_other_windows_and_reject_old_snapshots() {
        let mut windows = vec![window("5h", 20, 40.0), window("weekly", 10, 30.0)];
        crate::supplier::merge_official_quota_windows(&mut windows, &[window("5h", 15, 0.0)]);
        assert_eq!(windows[0].used_percent, 40.0);
        let mut from_header = window("5h", 30, 50.0);
        from_header.source = "codex_header".into();
        windows[0].token_type = Some("codex".into());
        crate::supplier::merge_official_quota_windows(&mut windows, &[from_header]);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].used_percent, 50.0);
        assert_eq!(windows[1].checked_at_unix, 10);
    }

    #[test]
    fn antigravity_newer_catalog_recovers_a_stale_usage_bucket_for_the_same_model() {
        let mut live = window("WTUS:model", 10, 100.0);
        live.model = Some("model".into());
        live.source = "antigravity_retrieve_user_quota".into();
        let mut catalog = window("model", 20, 30.0);
        catalog.model = Some("model".into());
        catalog.source = "antigravity_fetch_available_models".into();
        let mut windows = vec![live.clone()];
        crate::supplier::merge_official_quota_windows(&mut windows, &[catalog]);
        crate::supplier::merge_official_quota_windows(&mut windows, &[live]);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, 30.0);
    }

    #[tokio::test]
    async fn websocket_quota_events_work_before_catalog_cache_exists() {
        let mut channel = crate::channel_from_supplier(
            format!("ws-quota-{}", rand::random::<u64>()),
            &crate::default_supplier_config(),
        );
        channel.subscription.platform = "openai".into();
        channel.subscription.credential_ref = "ws-quota-no-file-read.json".into();
        super::super::observe_openai_subscription_response_event(
            &channel,
            r#"{
            "type":"codex.rate_limits", "metered_limit_name":"codex",
            "rate_limits":{"primary":{"used_percent":25,"window_minutes":300,"reset_at":2000000000}}
        }"#,
        );
        super::super::observe_openai_subscription_response_event(
            &channel,
            r#"{
            "type":"codex.response.metadata",
            "headers":{"x-codex-secondary-used-percent":"35","x-codex-secondary-window-minutes":"10080"}
        }"#,
        );
        let cache = quota_cache(&channel).expect("cache");
        let state = cache.state.lock().expect("state");
        assert!(state.refresh_due(SubscriptionCatalogPolicy::Cached, Instant::now()));
        assert_eq!(state.windows.len(), 2);
        assert_eq!(state.windows[0].window, "5h");
        assert_eq!(state.windows[0].used_percent, 25.0);
        assert_eq!(state.windows[1].window, "weekly");
        assert_eq!(state.windows[1].used_percent, 35.0);
    }

    #[tokio::test]
    async fn live_observation_is_not_lost_while_usage_query_is_in_flight() {
        let mut channel = crate::channel_from_supplier(
            format!("in-flight-quota-{}", rand::random::<u64>()),
            &crate::default_supplier_config(),
        );
        channel.subscription.credential_ref = "in-flight-quota-no-file-read.json".into();
        let observed = Arc::new(tokio::sync::Notify::new());
        let ready = Arc::new(tokio::sync::Notify::new());
        let querying_channel = channel.clone();
        let observed_query = observed.clone();
        let ready_query = ready.clone();
        let query = tokio::spawn(async move {
            refresh_subscription_quota(
                &querying_channel,
                SubscriptionCatalogPolicy::Cached,
                async {
                    ready_query.notify_one();
                    observed_query.notified().await;
                    Ok(vec![window("5h", 10, 10.0), window("weekly", 10, 20.0)])
                },
            )
            .await
        });
        ready.notified().await;
        observe_subscription_quota(&channel, &[window("5h", 20, 50.0)]);
        observed.notify_one();
        let windows = query.await.expect("join").expect("quota");
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].used_percent, 50.0);
    }

    #[tokio::test]
    async fn non_json_quota_error_preserves_retry_after() {
        use warp::Filter;
        let route = warp::any().map(|| {
            warp::reply::with_header(
                warp::reply::with_status(
                    "temporarily limited",
                    warp::http::StatusCode::TOO_MANY_REQUESTS,
                ),
                "Retry-After",
                "900",
            )
        });
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let response = Client::new()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("response");
        let error = quota_response_json(response, "mock usage")
            .await
            .expect_err("limited");
        assert_eq!(
            error
                .downcast_ref::<QuotaFetchError>()
                .and_then(|error| error.retry_after),
            Some(Duration::from_secs(900))
        );
        server.abort();
    }
}
