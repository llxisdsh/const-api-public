//! The local edition has no CONST account, reward or credential implementation.
use crate::model::{AppState, ClientConfig, Endpoint};
use anyhow::{Result, anyhow};
use serde::Serialize;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct AccountStatus {
    pub(crate) state: String,
    pub(crate) endpoint_state: String,
    pub(crate) platform_id: String,
    pub(crate) user_id: String,
    pub(crate) email: String,
    pub(crate) username: String,
    pub(crate) role: String,
    pub(crate) email_verified: bool,
    pub(crate) must_change_password: bool,
    pub(crate) balance: Option<f64>,
    pub(crate) display_cny_per_usd: f64,
    pub(crate) access_expires_at: String,
    pub(crate) credential_persistence: String,
    pub(crate) legal_state: String,
    pub(crate) legal_required_agreement_version: u32,
    pub(crate) legal_required_privacy_version: u32,
    pub(crate) last_error: String,
}

pub(crate) struct AccountRuntime;
impl AccountRuntime {
    pub(crate) fn new() -> Self {
        Self
    }
    pub(crate) fn with_persisted_session(_: &Path, _: &ClientConfig) -> Self {
        Self
    }
    pub(crate) fn status(&self) -> AccountStatus {
        AccountStatus {
            state: "signed_out".into(),
            endpoint_state: "offline".into(),
            legal_state: "not_applicable".into(),
            credential_persistence: "none".into(),
            display_cny_per_usd: 7.0,
            ..Default::default()
        }
    }
    pub(crate) fn access_token(&self) -> Option<String> {
        None
    }
    pub(crate) fn access_token_handle(&self) -> Arc<Mutex<String>> {
        Arc::new(Mutex::new(String::new()))
    }
}

pub(crate) fn select_account_endpoint<'a>(
    _: &'a ClientConfig,
    _: Option<&str>,
) -> Result<(String, &'a Endpoint)> {
    Err(anyhow!(
        "CONST platform accounts are unavailable in the local edition"
    ))
}
pub(crate) async fn run_account_refresh_loop(_: AppState) {
    std::future::pending::<()>().await;
}
pub(crate) async fn account_logout_inner(_: &AppState) -> Result<AccountStatus, String> {
    Ok(AccountRuntime.status())
}
pub(crate) async fn account_market_prices(
    _: &AppState,
    _: Option<String>,
    _: Option<String>,
    _: Option<String>,
    _: Option<String>,
    _: Option<String>,
    _: Option<usize>,
    _: Option<usize>,
    _: Option<String>,
) -> Result<serde_json::Value, String> {
    Err("The hosted model market is unavailable in the local edition".into())
}

#[tauri::command]
pub(crate) async fn account_status() -> Result<AccountStatus, String> {
    Ok(AccountRuntime.status())
}
#[tauri::command]
pub(crate) async fn account_refresh() -> Result<AccountStatus, String> {
    Ok(AccountRuntime.status())
}
#[tauri::command]
pub(crate) async fn account_logout() -> Result<AccountStatus, String> {
    Ok(AccountRuntime.status())
}

macro_rules! unavailable_commands {
    ($($name:ident),* $(,)?) => { $(
        #[tauri::command]
        pub(crate) async fn $name() -> Result<serde_json::Value, String> {
            Err("CONST platform services are unavailable in the local edition".into())
        }
    )* };
}
unavailable_commands!(
    account_registration_policy,
    account_request_code,
    account_register,
    account_password_login,
    account_verify_code,
    account_reset_password,
    account_change_password,
    account_request_email_code,
    account_verify_email,
    account_billing_summary,
    account_referral_overview,
    account_referral_activate,
    account_referral_reset_development,
    account_billing_records,
    account_billing_stats,
    account_billing_detail,
    account_transfer_funds,
    account_finance_availability,
    account_payment_orders,
    account_refunds,
    account_create_payment,
    account_refresh_payment,
    account_cancel_payment,
    account_complete_fake_payment,
    account_payout_accounts,
    account_create_payout_account,
    account_reauthenticate,
    account_usage_gifts,
    account_preview_gift_recipient,
    account_create_usage_gift,
    account_withdrawals,
    account_create_withdrawal,
    account_cancel_withdrawal,
);
