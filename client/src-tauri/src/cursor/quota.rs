//! Read-only account usage. Included pools and on-demand spend are not global
//! inference limits: exhausting one must not disable the other model pools.
use super::*;
use crate::model::QuotaWindow;
use serde::Deserialize;

pub(crate) async fn fetch(client: &Client, token: &str) -> Result<Vec<QuotaWindow>> {
    fetch_at(
        client,
        token,
        &format!("{API}/aiserver.v1.DashboardService/GetCurrentPeriodUsage"),
    )
    .await
}

async fn fetch_at(client: &Client, token: &str, url: &str) -> Result<Vec<QuotaWindow>> {
    let response = send(
        headers(client.post(url), token)
            .timeout(Duration::from_secs(15))
            .json(&json!({})),
    )
    .await?;
    if !response.status().is_success() {
        return Err(rejection(response, &[token]).await?.into());
    }
    let value = serde_json::from_slice(&bounded_body(response).await?)
        .context("invalid Cursor usage response")?;
    windows(value, crate::now_unix())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PeriodUsage {
    #[serde(default)]
    billing_cycle_end: Value,
    plan_usage: Option<PlanUsage>,
    spend_limit_usage: Option<SpendLimitUsage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanUsage {
    // Connect JSON omits zero-valued non-optional protobuf scalars.
    #[serde(default)]
    included_spend: f64,
    #[serde(default)]
    limit: f64,
    auto_percent_used: Option<f64>,
    api_percent_used: Option<f64>,
    total_percent_used: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpendLimitUsage {
    individual_limit: Option<f64>,
    #[serde(default)]
    individual_used: f64,
}

fn windows(value: Value, checked_at_unix: i64) -> Result<Vec<QuotaWindow>> {
    let usage: PeriodUsage =
        serde_json::from_value(value).context("invalid Cursor usage fields")?;
    // The RPC uses int64 milliseconds (JSON string); never guess a reset date.
    let reset_at_unix = match usage.billing_cycle_end {
        Value::Null => 0,
        Value::String(value) => {
            value
                .parse::<i64>()
                .context("invalid Cursor billing cycle end")?
                / 1000
        }
        value => {
            value
                .as_i64()
                .ok_or_else(|| anyhow!("invalid Cursor billing cycle end"))?
                / 1000
        }
    };
    let mut windows = Vec::new();
    let mut add = |bucket: &str, used_percent: f64| -> Result<()> {
        ensure!(
            used_percent.is_finite() && used_percent >= 0.0,
            "invalid Cursor usage percentage"
        );
        windows.push(QuotaWindow {
            source: "cursor_billing".into(),
            window: "monthly".into(),
            remaining_ratio: (1.0 - used_percent / 100.0).clamp(0.0, 1.0),
            used_percent,
            reset_at_unix: reset_at_unix.max(0),
            checked_at_unix,
            model: None,
            // Non-global buckets are advisory on existing clients AND servers.
            token_type: Some(bucket.into()),
        });
        Ok(())
    };
    if let Some(plan) = usage.plan_usage {
        // Current Cursor exposes separate first/third-party pools. Do not mix
        // those percentages with the legacy dollar-based plan total.
        if let Some(percent) = plan.auto_percent_used {
            add("cursor_models", percent)?;
        }
        if let Some(percent) = plan.api_percent_used {
            add("other_models", percent)?;
        }
        if plan.auto_percent_used.is_none() && plan.api_percent_used.is_none() {
            ensure!(
                plan.limit.is_finite()
                    && plan.limit >= 0.0
                    && plan.included_spend.is_finite()
                    && plan.included_spend >= 0.0,
                "invalid Cursor included usage"
            );
            // Match the native client's legacy calculation. Amounts are cents;
            // totalSpend also includes bonus usage and is not includedSpend.
            if plan.limit > 0.0 {
                add("included", plan.included_spend / plan.limit * 100.0)?;
            } else if let Some(percent) = plan.total_percent_used {
                add("included", percent)?;
            }
        }
    }
    if let Some(spend) = usage.spend_limit_usage
        && let Some(limit) = spend.individual_limit
        && limit > 0.0
    {
        ensure!(limit.is_finite(), "invalid Cursor on-demand limit");
        add("on_demand", spend.individual_used / limit * 100.0)?;
    }
    Ok(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_quota_prefers_actual_pools_without_turning_percent_into_fraction() {
        let windows = windows(
            json!({
                "billingCycleEnd":"1794316925000",
                "planUsage":{"includedSpend":20,"limit":2000,
                    "autoPercentUsed":0.0022222222222222222,"apiPercentUsed":0.8444444444444443,
                    "totalPercentUsed":0.04232804232804233},
                "spendLimitUsage":{"limitType":"user"}
            }),
            100,
        )
        .unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].token_type.as_deref(), Some("cursor_models"));
        assert_eq!(windows[1].token_type.as_deref(), Some("other_models"));
        assert!((windows[1].remaining_ratio - 0.9915555555555555).abs() < 1e-12);
        assert_eq!(windows[1].reset_at_unix, 1794316925);
        assert_eq!(windows[1].checked_at_unix, 100);
        assert!(crate::supplier::scoped_quota_summary(&windows).is_none());
    }

    #[test]
    fn cursor_quota_legacy_uses_included_spend_and_keeps_overage_separate() {
        let windows = windows(
            json!({"billingCycleEnd":1794316925000i64,
            "planUsage":{"includedSpend":2000,"totalSpend":2300,"limit":2000,"totalPercentUsed":12},
            "spendLimitUsage":{"individualUsed":100,"individualLimit":500}}),
            100,
        )
        .unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].remaining_ratio, 0.0);
        assert_eq!(windows[1].remaining_ratio, 0.8);
        assert_eq!(windows[1].token_type.as_deref(), Some("on_demand"));
        assert!(crate::supplier::channel_quota_summary(&windows).is_none());
        assert!(
            windows
                .iter()
                .all(|w| !crate::supplier::quota_window_limits_supply(w))
        );
    }

    #[test]
    fn cursor_quota_preserves_missing_zero_exhausted_and_unknown_distinctions() {
        for value in [
            json!({}),
            json!({"planUsage":{}}),
            json!({"spendLimitUsage":{"individualLimit":0}}),
        ] {
            assert!(windows(value, 100).unwrap().is_empty());
        }
        for value in [
            json!({"planUsage":{"limit":2000}}),
            json!({"planUsage":{"apiPercentUsed":0}}),
            json!({"planUsage":{"totalPercentUsed":0}}),
        ] {
            let windows = windows(value, 100).unwrap();
            assert_eq!(windows.len(), 1);
            assert_eq!(windows[0].remaining_ratio, 1.0);
            assert_eq!(windows[0].reset_at_unix, 0);
        }
        let windows = windows(json!({"planUsage":{"apiPercentUsed":120}}), 100).unwrap();
        assert_eq!(windows[0].used_percent, 120.0);
        assert_eq!(windows[0].remaining_ratio, 0.0);
    }

    #[test]
    fn cursor_quota_rejects_malformed_values_instead_of_reporting_zero() {
        for value in [
            json!({"planUsage":{"apiPercentUsed":-1}}),
            json!({"planUsage":{"apiPercentUsed":"NaN"}}),
            json!({"planUsage":{"includedSpend":-1,"limit":20}}),
            json!({"billingCycleEnd":"not-a-time"}),
            json!([]),
        ] {
            assert!(windows(value, 100).is_err());
        }
    }

    #[tokio::test]
    async fn cursor_quota_rpc_is_metadata_only_and_preserves_redacted_auth_errors() {
        use warp::Filter;
        for status in [200, 401, 503] {
            let route = warp::post()
                .and(warp::path("quota"))
                .and(warp::header::exact(
                    "authorization",
                    "Bearer synthetic-quota-token",
                ))
                .and(warp::header::exact("connect-protocol-version", "1"))
                .and(warp::body::json())
                .map(move |body: Value| {
                    assert_eq!(body, json!({}));
                    let value = if status == 200 {
                        json!({"planUsage":{"apiPercentUsed":20}})
                    } else {
                        json!({"code":"unauthenticated","message":"synthetic-quota-token rejected"})
                    };
                    warp::reply::with_status(
                        warp::reply::json(&value),
                        warp::http::StatusCode::from_u16(status).unwrap(),
                    )
                });
            let (addr, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
            let server = tokio::spawn(server);
            let result = fetch_at(
                &Client::builder().no_proxy().build().unwrap(),
                "synthetic-quota-token",
                &format!("http://{addr}/quota"),
            )
            .await;
            server.abort();
            if status == 200 {
                assert_eq!(result.unwrap()[0].remaining_ratio, 0.8);
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.downcast_ref::<ApiError>().unwrap().status, status);
                assert!(!error.to_string().contains("synthetic-quota-token"));
            }
        }
    }
}
