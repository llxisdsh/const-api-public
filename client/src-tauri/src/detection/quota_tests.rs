use super::*;

#[test]
fn grok_billing_percentages_keep_units_and_ignore_invalid_numbers() {
    for percent in [0.0_f64, 0.25, 1.0, 99.0, 100.0, 120.0] {
        let mut windows = Vec::new();
        append_grok_billing_window(
            &mut windows,
            &serde_json::json!({
                "config": {"creditUsagePercent": percent}
            }),
            "weekly",
            1000,
            true,
        );
        append_grok_billing_window(
            &mut windows,
            &serde_json::json!({
                "config": {"used": {"val": percent}, "monthlyLimit": {"val": 100}}
            }),
            "monthly",
            1000,
            false,
        );
        assert_eq!(windows.len(), 2);
        assert!(
            windows
                .iter()
                .all(|window| (window.used_percent - percent).abs() < 0.000001)
        );
        assert!(crate::supplier::quota_window_limits_supply(&windows[0]));
        assert!(!crate::supplier::quota_window_limits_supply(&windows[1]));
    }
    let mut windows = Vec::new();
    append_grok_billing_window(
        &mut windows,
        &serde_json::json!({
            "config": {"creditUsagePercent": "NaN"}
        }),
        "weekly",
        1000,
        true,
    );
    append_grok_billing_window(
        &mut windows,
        &serde_json::json!({
            "config": {"used": 10, "monthlyLimit": "inf"}
        }),
        "monthly",
        1000,
        false,
    );
    assert!(windows.is_empty());
}

#[test]
fn grok_billing_reset_times_do_not_cross_weekly_and_monthly_windows() {
    let weekly_end = "2030-01-07T00:00:00Z";
    let monthly_end = "2030-02-01T00:00:00Z";
    for nested in [false, true] {
        for (weekly, monthly) in [
            (Some(weekly_end), Some(monthly_end)),
            (None, Some(monthly_end)),
            (Some(weekly_end), None),
            (None, None),
        ] {
            let config = serde_json::json!({
                "creditUsagePercent": 100,
                "used": 100, "monthlyLimit": 100,
                "currentPeriod": {"type": "weekly", "end": weekly},
                "billingPeriodEnd": monthly,
            });
            let value = if nested {
                serde_json::json!({"config": config})
            } else {
                config
            };
            let mut windows = Vec::new();
            append_grok_billing_window(&mut windows, &value, "weekly", 1000, true);
            append_grok_billing_window(&mut windows, &value, "monthly", 1000, false);
            assert_eq!(windows.len(), 2);
            assert_eq!(
                windows[0].reset_at_unix,
                weekly.and_then(parse_token_expiry_text).unwrap_or_default()
            );
            assert_eq!(
                windows[1].reset_at_unix,
                monthly
                    .and_then(parse_token_expiry_text)
                    .unwrap_or_default()
            );
            assert!(crate::supplier::quota_window_limits_supply(&windows[0]));
            assert!(!crate::supplier::quota_window_limits_supply(&windows[1]));
        }
    }
}

#[test]
fn oauth_usage_keeps_overage_for_diagnostics_with_zero_remaining() {
    let claude = claude_quota_windows_from_usage(&serde_json::json!({
        "five_hour": {"utilization":120}
    }));
    let openai = openai_quota_windows_from_usage(&serde_json::json!({
        "rate_limit": {"primary_window": {"used_percent":120,"limit_window_seconds":18000}}
    }));
    for windows in [claude, openai] {
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, 120.0);
        assert_eq!(windows[0].remaining_ratio, 0.0);
        assert!(crate::supplier::quota_window_limits_supply(&windows[0]));
    }
}

#[test]
fn openai_quota_reset_cannot_overflow_on_malformed_upstream_values() {
    let windows = openai_quota_windows_from_usage(&serde_json::json!({
        "rate_limit": {"primary_window": {
            "used_percent": 1,
            "reset_after_seconds": i64::MAX,
            "limit_window_seconds": "NaN"
        }}
    }));
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].used_percent, 1.0);
    assert_eq!(windows[0].window, "primary");
    assert_eq!(windows[0].reset_at_unix, i64::MAX);
}
