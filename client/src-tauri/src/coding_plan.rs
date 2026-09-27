//! Read-only subscription quota APIs. Generation stays in the shared HTTP executor.
use crate::{
    model::{ChannelConfig, QuotaWindow},
    source_driver::SourceDriverId,
};
use anyhow::{Result, ensure};
use serde_json::Value;

pub(crate) fn has_quota(driver: SourceDriverId) -> bool {
    matches!(
        driver,
        SourceDriverId::KimiCode | SourceDriverId::GlmCodingPlan | SourceDriverId::MinimaxTokenPlan
    )
}

pub(crate) fn validate_key(channel: &ChannelConfig) -> Result<()> {
    let key = channel.v2.credential_ref.trim();
    if channel.source_driver() == SourceDriverId::MinimaxTokenPlan && !key.is_empty() {
        ensure!(
            key.starts_with("sk-cp-"),
            "MiniMax Token Plan requires its dedicated sk-cp- key; a pay-as-you-go key is not accepted"
        );
    }
    Ok(())
}

pub(crate) fn quota_url(driver: SourceDriverId) -> Option<&'static str> {
    match driver {
        SourceDriverId::KimiCode => Some("https://api.kimi.com/coding/v1/usages"),
        SourceDriverId::GlmCodingPlan => Some("https://api.z.ai/api/monitor/usage/quota/limit"),
        SourceDriverId::MinimaxTokenPlan => Some("https://www.minimax.io/v1/token_plan/remains"),
        _ => None,
    }
}

pub(crate) fn quota_rejection(
    channel: &ChannelConfig,
    operation: crate::surface::ApiOperation,
) -> Result<Option<reqwest::Response>> {
    use crate::surface::ApiOperation;
    if !has_quota(channel.source_driver())
        || !matches!(
            operation,
            ApiOperation::ChatCompletions
                | ApiOperation::Responses
                | ApiOperation::Messages
                | ApiOperation::ResponsesCompact
        )
    {
        return Ok(None);
    }
    let supplier = crate::supplier_from_channel(channel);
    let state = crate::supplier::subscription_safety_state_for_channel(&supplier)?;
    let snapshot = crate::supplier::quota_snapshot_for_state(&supplier, Some(&state));
    let decision = crate::supplier::quota_reserve_decision(
        &snapshot.quota_windows,
        channel.quota_reserve_percent(),
        crate::now_unix(),
    );
    let blocked = decision.filter(|decision| decision.blocked);
    if blocked.is_none() && snapshot.status != "exhausted" {
        return Ok(None);
    }
    let reset_at = blocked.map_or(snapshot.quota_reset_at_unix, |decision| {
        decision.reset_at_unix
    });
    let retry_after = if reset_at > crate::now_unix() {
        reset_at - crate::now_unix()
    } else {
        60
    };
    let (code, message) = if snapshot.status == "exhausted" {
        (
            "quota_exhausted",
            "Subscription quota is exhausted; wait for the quota window to reset",
        )
    } else {
        (
            "quota_reserved",
            "Subscription quota has reached your reserve; wait for the quota window to reset or lower the reserve",
        )
    };
    let response = http::Response::builder()
        .status(429)
        .header("content-type", "application/json")
        .header("retry-after", retry_after.to_string())
        .body(
            serde_json::json!({"error":{"type":"rate_limit_error","code":code,
            "message":message}, "error_kind":code, "failure_scope":"channel",
            "safe_to_retry_other_channel":true, "safe_to_retry_same_channel":false,
            "retry_after_seconds":retry_after})
            .to_string(),
        )?;
    Ok(Some(reqwest::Response::from(response)))
}

pub(crate) async fn fetch_quota(
    client: &reqwest::Client,
    channel: &ChannelConfig,
) -> Result<Vec<QuotaWindow>> {
    validate_key(channel)?;
    let url = quota_url(channel.source_driver()).expect("quota driver has a fixed endpoint");
    fetch_quota_at(client, channel, url).await
}

async fn fetch_quota_at(
    client: &reqwest::Client,
    channel: &ChannelConfig,
    url: &str,
) -> Result<Vec<QuotaWindow>> {
    use crate::upstream_transport::AdaptiveRequestBuilderExt;
    let auth = if channel.source_driver() == SourceDriverId::GlmCodingPlan {
        channel.v2.credential_ref.trim().to_string()
    } else {
        format!("Bearer {}", channel.v2.credential_ref.trim())
    };
    let request = client
        .get(url)
        .header("authorization", auth)
        .header("accept", "application/json");
    let response = crate::channel_user_agent::apply_to_request(channel, request)
        .send_adaptive()
        .await?;
    let value = crate::detection::quota_response_json(response, "coding plan quota").await?;
    parse_quota(channel.source_driver(), &value, crate::now_unix())
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|value| value.is_finite())
}

fn reset(value: &Value) -> i64 {
    if let Some(value) = number(value).filter(|value| *value > 0.0) {
        return if value >= 1_000_000_000_000.0 {
            (value / 1000.0) as i64
        } else {
            value as i64
        };
    }
    value
        .as_str()
        .and_then(|text| {
            time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()
        })
        .map(|time| time.unix_timestamp())
        .unwrap_or(0)
}

fn kimi_period(window: &Value) -> String {
    let duration = number(&window["duration"]).filter(|n| *n > 0.0 && n.fract() == 0.0);
    let unit = match window["timeUnit"].as_str() {
        Some("TIME_UNIT_MINUTE") => "m",
        Some("TIME_UNIT_HOUR") => "h",
        Some("TIME_UNIT_DAY") => "d",
        Some("TIME_UNIT_WEEK") => "w",
        _ => return "primary".into(),
    };
    duration.map_or_else(|| "primary".into(), |duration| format!("{duration}{unit}"))
}

pub(crate) fn parse_quota(
    driver: SourceDriverId,
    value: &Value,
    now: i64,
) -> Result<Vec<QuotaWindow>> {
    let mut windows = Vec::new();
    let source = match driver {
        SourceDriverId::KimiCode => "kimi_coding_usage",
        SourceDriverId::GlmCodingPlan => "glm_coding_usage",
        SourceDriverId::MinimaxTokenPlan => "minimax_token_usage",
        _ => return Ok(windows),
    };
    let mut push = |period: &str, used: Option<f64>, reset_value: &Value| {
        if let Some(used) = used.filter(|used| used.is_finite() && *used >= 0.0) {
            windows.push(QuotaWindow {
                source: source.into(),
                window: period.into(),
                remaining_ratio: (1.0 - used / 100.0).clamp(0.0, 1.0),
                used_percent: used,
                reset_at_unix: reset(reset_value),
                checked_at_unix: now,
                model: None,
                token_type: None,
            });
        }
    };
    match driver {
        SourceDriverId::KimiCode => {
            let utilization = |item: &Value| {
                let limit = number(&item["limit"]).filter(|limit| *limit > 0.0)?;
                let remaining = number(&item["remaining"])?;
                Some(((limit - remaining) / limit * 100.0).max(0.0))
            };
            if let Some(limits) = value["limits"].as_array() {
                for limit in limits {
                    if let Some(detail) = limit.get("detail") {
                        push(
                            &kimi_period(&limit["window"]),
                            utilization(detail),
                            &detail["resetTime"],
                        );
                    }
                }
            }
            // New memberships no longer have the legacy weekly allowance.
            // A reset timestamp alone does not identify the period.
            push(
                &kimi_period(&value["usage"]["window"]),
                utilization(&value["usage"]),
                &value["usage"]["resetTime"],
            );
        }
        SourceDriverId::GlmCodingPlan => {
            ensure!(
                value.get("success").and_then(Value::as_bool) != Some(false),
                "GLM quota API rejected the request"
            );
            if let Some(limits) = value["data"]["limits"].as_array() {
                let kind = if limits.iter().any(|limit| limit["type"] == "TOKENS_LIMIT") {
                    "TOKENS_LIMIT"
                } else {
                    "CREDIT_LIMIT"
                };
                for limit in limits.iter().filter(|limit| limit["type"] == kind) {
                    let period = match number(&limit["unit"]).map(|unit| unit as i64) {
                        Some(3) => "5h",
                        Some(6) => "weekly",
                        // Old plans can omit unit. Keep this interpretable as an
                        // account window, not a guessed 5-hour/weekly duration.
                        _ => "primary",
                    };
                    push(
                        period,
                        number(&limit["percentage"]),
                        &limit["nextResetTime"],
                    );
                }
            }
        }
        SourceDriverId::MinimaxTokenPlan => {
            ensure!(
                number(&value["base_resp"]["status_code"]).is_none_or(|code| code == 0.0),
                "MiniMax quota API rejected the request"
            );
            if let Some(general) = value["model_remains"]
                .as_array()
                .and_then(|models| models.iter().find(|model| model["model_name"] == "general"))
            {
                push(
                    "5h",
                    number(&general["current_interval_remaining_percent"])
                        .map(|remaining| (100.0 - remaining).max(0.0)),
                    &general["end_time"],
                );
                if number(&general["current_weekly_status"]) == Some(1.0) {
                    push(
                        "weekly",
                        number(&general["current_weekly_remaining_percent"])
                            .map(|remaining| (100.0 - remaining).max(0.0)),
                        &general["weekly_end_time"],
                    );
                }
            }
        }
        _ => unreachable!(),
    }
    ensure!(
        !windows.is_empty(),
        "{source} returned no recognized quota windows; remaining quota is unknown"
    );
    Ok(windows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn coding_plan_reserve_blocks_generation_but_not_free_operations() {
        use crate::surface::ApiOperation;
        for driver in [
            SourceDriverId::KimiCode,
            SourceDriverId::GlmCodingPlan,
            SourceDriverId::MinimaxTokenPlan,
        ] {
            let mut channel =
                crate::coding_gateway::tests::channel(driver, "https://unused.invalid");
            channel.id = format!("quota-gate-{driver:?}");
            channel.v2.quota_reserve_percent = 20;
            let supplier = crate::supplier_from_channel(&channel);
            let mut state =
                crate::supplier::subscription_safety_state_for_channel(&supplier).unwrap();
            let now = crate::now_unix();
            for (used, expected) in [
                (0.0, None),
                (90.0, Some("quota_reserved")),
                (100.0, Some("quota_exhausted")),
            ] {
                state.quota_windows = vec![QuotaWindow {
                    source: "official_header".into(),
                    window: "5h".into(),
                    used_percent: used,
                    remaining_ratio: 1.0 - used / 100.0,
                    checked_at_unix: now,
                    reset_at_unix: now + 300,
                    ..Default::default()
                }];
                crate::supplier::save_subscription_safety_state(state.clone()).unwrap();
                let response = quota_rejection(&channel, ApiOperation::Responses).unwrap();
                if let Some(code) = expected {
                    let response = response.expect("known quota must gate generation");
                    assert_eq!(response.status().as_u16(), 429);
                    assert!(response.headers().contains_key("retry-after"));
                    let body: Value = response.json().await.unwrap();
                    assert_eq!(body["error"]["code"], code);
                    assert_eq!(body["safe_to_retry_other_channel"], true);
                } else {
                    assert!(response.is_none());
                }
                for operation in [
                    ApiOperation::ListModels,
                    ApiOperation::CountTokens,
                    ApiOperation::ResponsesInputTokens,
                    ApiOperation::ResponsesCancel,
                ] {
                    assert!(quota_rejection(&channel, operation).unwrap().is_none());
                }
            }
            state.quota_windows[0].reset_at_unix = now - 1;
            crate::supplier::save_subscription_safety_state(state).unwrap();
            assert!(
                quota_rejection(&channel, ApiOperation::Responses)
                    .unwrap()
                    .is_none(),
                "expired exhaustion must not block recovery"
            );
        }
    }

    #[tokio::test]
    async fn quota_queries_use_read_only_provider_auth_and_retain_network_errors() {
        use warp::Filter;
        for (driver, auth, body) in [
            (
                SourceDriverId::KimiCode,
                "Bearer test-key",
                json!({"limits":[{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},"detail":{"limit":"100","remaining":"50"}}]}),
            ),
            (
                SourceDriverId::GlmCodingPlan,
                "test-key",
                json!({"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"percentage":50}]}}),
            ),
            (
                SourceDriverId::MinimaxTokenPlan,
                "Bearer sk-cp-test-key",
                json!({"model_remains":[{"model_name":"general","current_interval_remaining_percent":50}]}),
            ),
        ] {
            let route = warp::any()
                .and(warp::method())
                .and(warp::header::<String>("authorization"))
                .map(move |method: warp::http::Method, authorization: String| {
                    assert_eq!(method, warp::http::Method::GET);
                    assert_eq!(authorization, auth);
                    warp::reply::json(&body)
                });
            let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
            let server = tokio::spawn(server);
            let channel = crate::coding_gateway::tests::channel(driver, "https://unused.invalid");
            let windows = fetch_quota_at(
                &reqwest::Client::new(),
                &channel,
                &format!("http://{addr}/quota"),
            )
            .await
            .unwrap();
            assert_eq!(windows.len(), 1);
            assert_eq!(windows[0].remaining_ratio, 0.5);
            assert!(
                crate::supplier::quota_reserve_decision(&windows, 60, crate::now_unix())
                    .unwrap()
                    .blocked
            );
            server.abort();
        }
        let route = warp::any()
            .map(|| warp::reply::with_status("denied", warp::http::StatusCode::UNAUTHORIZED));
        let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let channel = crate::coding_gateway::tests::channel(
            SourceDriverId::KimiCode,
            "https://unused.invalid",
        );
        let error = fetch_quota_at(
            &reqwest::Client::new(),
            &channel,
            &format!("http://{addr}/quota"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("401"));
        server.abort();
    }

    #[test]
    fn plan_credentials_and_explicit_periods_do_not_guess_other_plans() {
        let mut channel = crate::coding_gateway::tests::channel(
            SourceDriverId::MinimaxTokenPlan,
            "https://unused.invalid",
        );
        assert!(validate_key(&channel).is_ok());
        channel.v2.credential_ref = "sk-api-payg".into();
        assert!(validate_key(&channel).is_err());
        let kimi = parse_quota(SourceDriverId::KimiCode, &json!({"usage":{"limit":100,"remaining":0},
            "limits":[{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},"detail":{"limit":100,"remaining":25}}]}), crate::now_unix()).unwrap();
        assert_eq!(kimi[0].window, "300m");
        assert_eq!(
            kimi[1].window, "primary",
            "unknown aggregate period must not be labelled weekly"
        );
        let glm = parse_quota(SourceDriverId::GlmCodingPlan, &json!({"data":{"limits":[
            {"type":"CREDIT_LIMIT","unit":3,"percentage":50}, {"type":"TIME_LIMIT","percentage":100}]}}), 1).unwrap();
        assert_eq!(glm.len(), 1);
        assert_eq!(glm[0].window, "5h");
    }

    #[test]
    fn plan_quota_units_missing_values_and_exhaustion() {
        let kimi = parse_quota(SourceDriverId::KimiCode, &json!({"limits":[{"detail":{"limit":"100","remaining":"0","resetTime":"2026-09-20T01:00:00Z"}}],"usage":{"limit":100,"remaining":99.5}}), 1).unwrap();
        assert_eq!(kimi[0].remaining_ratio, 0.0);
        assert_eq!(kimi[1].used_percent, 0.5);
        assert!(kimi[0].reset_at_unix > 1);
        let glm = parse_quota(SourceDriverId::GlmCodingPlan, &json!({"data":{"limits":[{"type":"TOKENS_LIMIT","unit":6,"percentage":120},{"type":"TOKENS_LIMIT","unit":3,"percentage":0.2},{"type":"TIME_LIMIT","percentage":100}]}}), 1).unwrap();
        assert_eq!(glm.len(), 2);
        assert_eq!(glm[0].window, "weekly");
        assert_eq!(glm[0].used_percent, 120.0);
        assert_eq!(glm[1].window, "5h");
        let minimax = parse_quota(SourceDriverId::MinimaxTokenPlan, &json!({"model_remains":[{"model_name":"video","current_interval_remaining_percent":0},{"model_name":"general","current_interval_remaining_percent":0,"end_time":1790000000000i64,"current_weekly_status":0,"current_weekly_remaining_percent":0}]}), 1).unwrap();
        assert_eq!(minimax.len(), 1);
        assert_eq!(minimax[0].reset_at_unix, 1790000000);
        assert_eq!(minimax[0].remaining_ratio, 0.0);
        for driver in [
            SourceDriverId::KimiCode,
            SourceDriverId::GlmCodingPlan,
            SourceDriverId::MinimaxTokenPlan,
        ] {
            assert!(parse_quota(driver, &json!({}), 1).is_err());
        }
        assert!(
            parse_quota(
                SourceDriverId::KimiCode,
                &json!({"usage":{"limit":0,"remaining":0}}),
                1
            )
            .is_err()
        );
    }
}
