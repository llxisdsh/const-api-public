use serde::Serialize;
use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter};
use warp::http::StatusCode;

const CLIENT_NOTICE_LIMIT: usize = 32;
const CONST_ERROR_CODE_HEADER: &str = "x-const-error-code";
const BALANCE_RECOVERY_COOLDOWN: Duration = Duration::from_secs(30);
const REQUEST_BLOCKED_NOTICE_CONTEXT: &str = "request_blocked";
const CLIENT_GUIDANCE_NOTICE_EVENT: &str = "client-guidance-notice";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientToastNotice {
    id: String,
    code: String,
    context: String,
    created_at: i64,
    dedupe_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    restriction_until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

fn notices() -> &'static Mutex<Vec<ClientToastNotice>> {
    static NOTICES: OnceLock<Mutex<Vec<ClientToastNotice>>> = OnceLock::new();
    NOTICES.get_or_init(|| Mutex::new(Vec::new()))
}

fn app_handle() -> &'static OnceLock<AppHandle> {
    static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
    &APP_HANDLE
}

fn last_balance_recovery() -> &'static Mutex<Option<Instant>> {
    static LAST_BALANCE_RECOVERY: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST_BALANCE_RECOVERY.get_or_init(|| Mutex::new(None))
}

fn last_platform_access_notice() -> &'static Mutex<Option<String>> {
    static LAST_NOTICE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    LAST_NOTICE.get_or_init(|| Mutex::new(None))
}

pub(crate) fn initialize_client_notices(app: AppHandle) {
    let _ = app_handle().set(app);
}

pub(crate) fn record_platform_toast(response: &warp::reply::Response) {
    record_platform_balance_notice(response, REQUEST_BLOCKED_NOTICE_CONTEXT);
    record_platform_access_notice(response);
}

pub(crate) fn record_platform_access_notice(response: &warp::reply::Response) {
    if response.status() != StatusCode::FORBIDDEN {
        return;
    }
    let error_code = response
        .headers()
        .get(CONST_ERROR_CODE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .unwrap_or_default();
    if error_code != "platform_access_restricted" {
        return;
    }
    let hold_until = response
        .headers()
        .get("x-const-platform-access-until")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let dedupe_key = format!(
        "platform_access_restricted:{}",
        hold_until.unwrap_or("unknown")
    );
    let Ok(mut last_notice) = last_platform_access_notice().lock() else {
        return;
    };
    if last_notice.as_deref() == Some(dedupe_key.as_str()) {
        return;
    }
    *last_notice = Some(dedupe_key.clone());
    drop(last_notice);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    enqueue_client_guidance_notice(
        ClientToastNotice {
            id: format!("platform-access-{now}"),
            code: error_code.to_string(),
            context: REQUEST_BLOCKED_NOTICE_CONTEXT.to_string(),
            created_at: now,
            dedupe_key,
            restriction_until: hold_until.map(str::to_string),
            version: None,
            title: None,
            body: None,
        },
        true,
    );
}

fn record_platform_balance_notice(response: &warp::reply::Response, context: &'static str) {
    if response.status() != StatusCode::PAYMENT_REQUIRED {
        return;
    }
    let error_code = response
        .headers()
        .get(CONST_ERROR_CODE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .unwrap_or_default();
    let suggested_action = response
        .headers()
        .get("x-const-suggested-action")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .unwrap_or_default();
    if error_code != "insufficient_balance" || suggested_action != "open_funds_topup" {
        return;
    }
    let topup_available = response
        .headers()
        .get("x-const-topup-available")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes"));
    if !topup_available {
        return;
    }

    let Ok(mut last_recovery) = last_balance_recovery().lock() else {
        return;
    };
    let now_instant = Instant::now();
    if last_recovery
        .as_ref()
        .is_some_and(|last| now_instant.duration_since(*last) < BALANCE_RECOVERY_COOLDOWN)
    {
        return;
    }
    *last_recovery = Some(now_instant);
    drop(last_recovery);

    let correlation_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    let recovery_window = now / BALANCE_RECOVERY_COOLDOWN.as_millis() as i64;
    let dedupe_key = format!("insufficient_balance:{recovery_window}");
    enqueue_client_guidance_notice(
        ClientToastNotice {
            id: format!("toast-{correlation_id}"),
            code: error_code.to_string(),
            context: context.to_string(),
            created_at: now,
            dedupe_key,
            restriction_until: None,
            version: None,
            title: None,
            body: None,
        },
        true,
    );
}

pub(crate) fn record_platform_announcement(version: String, title: String, body: String) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    let dedupe_key = "platform_announcement".to_string();
    enqueue_client_guidance_notice(
        ClientToastNotice {
            id: format!("announcement-{version}"),
            code: "platform_announcement".to_string(),
            context: "platform".to_string(),
            created_at: now,
            dedupe_key,
            restriction_until: None,
            version: Some(version),
            title: Some(title),
            body: Some(body),
        },
        false,
    );
}

pub(crate) fn clear_platform_announcement(version: String) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    enqueue_client_guidance_notice(
        ClientToastNotice {
            id: format!("announcement-clear-{version}"),
            code: "platform_announcement_clear".to_string(),
            context: "platform".to_string(),
            created_at: now,
            dedupe_key: "platform_announcement".to_string(),
            restriction_until: None,
            version: Some(version),
            title: None,
            body: None,
        },
        false,
    );
}

// Runtime notices carry allowlisted semantic identifiers and stay localized in
// the renderer. Admin announcements use the same bounded delivery path after
// their dynamic text has been length-checked on both sides of the network.
fn enqueue_client_guidance_notice(notice: ClientToastNotice, request_attention: bool) {
    let Ok(mut queue) = notices().lock() else {
        return;
    };
    if let Some(queued) = queue
        .iter_mut()
        .find(|queued| queued.dedupe_key == notice.dedupe_key)
    {
        *queued = notice.clone();
    } else {
        queue.push(notice.clone());
    }
    if queue.len() > CLIENT_NOTICE_LIMIT {
        let remove = queue.len() - CLIENT_NOTICE_LIMIT;
        queue.drain(0..remove);
    }
    drop(queue);

    if let Some(app) = app_handle().get() {
        let _ = app.emit(CLIENT_GUIDANCE_NOTICE_EVENT, notice);
        if request_attention {
            crate::request_main_window_attention(app);
        }
    }
}

pub(crate) fn drain_client_toast_notices_inner() -> Vec<ClientToastNotice> {
    notices()
        .lock()
        .map(|mut queue| queue.drain(..).collect())
        .unwrap_or_default()
}

#[tauri::command]
pub(crate) fn drain_client_toast_notices() -> Vec<ClientToastNotice> {
    drain_client_toast_notices_inner()
}

#[tauri::command]
pub(crate) fn request_client_attention(app: AppHandle) {
    crate::request_main_window_attention(&app);
}

#[cfg(test)]
pub(crate) fn clear_client_toast_notices() {
    if let Ok(mut queue) = notices().lock() {
        queue.clear();
    }
    if let Ok(mut last_recovery) = last_balance_recovery().lock() {
        *last_recovery = None;
    }
    if let Ok(mut last_notice) = last_platform_access_notice().lock() {
        *last_notice = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use warp::Reply;

    fn restricted_response(hold_until: &str) -> warp::reply::Response {
        warp::reply::with_header(
            warp::reply::with_header(
                warp::reply::with_status("", StatusCode::FORBIDDEN),
                CONST_ERROR_CODE_HEADER,
                "platform_access_restricted",
            ),
            "x-const-platform-access-until",
            hold_until,
        )
        .into_response()
    }

    #[test]
    fn platform_access_notice_is_emitted_once_per_restriction_window() {
        clear_client_toast_notices();
        let response = restricted_response("2026-08-30T12:00:00Z");
        record_platform_access_notice(&response);
        record_platform_access_notice(&response);
        let notices = drain_client_toast_notices_inner();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].code, "platform_access_restricted");
        assert_eq!(
            notices[0].restriction_until.as_deref(),
            Some("2026-08-30T12:00:00Z")
        );

        let next = restricted_response("2026-08-31T12:00:00Z");
        record_platform_access_notice(&next);
        assert_eq!(drain_client_toast_notices_inner().len(), 1);
        clear_client_toast_notices();
    }
}
