use super::*;
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

struct Pending {
    verifier: String,
    expires: Instant,
}
type Sessions = Mutex<HashMap<String, Arc<tokio::sync::Mutex<Pending>>>>;
static SESSIONS: OnceLock<Sessions> = OnceLock::new();

#[derive(Debug, Serialize)]
pub(crate) struct AuthError {
    code: &'static str,
    status: Option<u16>,
}
fn failure(code: &'static str, status: Option<u16>) -> AuthError {
    AuthError { code, status }
}

#[tauri::command]
pub(crate) fn create_cursor_auth_session() -> std::result::Result<Value, AuthError> {
    let verifier = crate::detection::base64_url_no_pad(
        &crate::detection::random_bytes(64).map_err(|_| failure("invalid_response", None))?,
    );
    let id = uuid::Uuid::new_v4().to_string();
    let mut url = reqwest::Url::parse("https://cursor.com/loginDeepControl").unwrap();
    url.query_pairs_mut()
        .append_pair("challenge", &crate::detection::pkce_challenge(&verifier))
        .append_pair("uuid", &id)
        .append_pair("mode", "login")
        .append_pair("redirectTarget", "cli");
    let mut sessions = SESSIONS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| failure("invalid_response", None))?;
    sessions.retain(|_, pending| {
        pending
            .try_lock()
            .map_or(true, |p| p.expires > Instant::now())
    });
    if sessions.len() >= 8 {
        return Err(failure("slow_down", None));
    }
    sessions.insert(
        id.clone(),
        Arc::new(tokio::sync::Mutex::new(Pending {
            verifier,
            expires: Instant::now() + Duration::from_secs(600),
        })),
    );
    Ok(
        json!({"device_code":id,"user_code":"","verification_uri":url.as_str(),
        "interval_seconds":3,"expires_at_unix_ms":(crate::now_unix()+600)*1000}),
    )
}

#[tauri::command]
pub(crate) async fn poll_cursor_auth_session(
    device_code: String,
) -> std::result::Result<Value, AuthError> {
    let slot = SESSIONS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| failure("invalid_response", None))?
        .get(&device_code)
        .cloned()
        .ok_or_else(|| failure("expired", None))?;
    let mut pending = slot.lock().await;
    if pending.expires <= Instant::now() {
        return Err(failure("expired", None));
    }
    let client = http_client().map_err(|_| failure("network", None))?;
    // The current CLI/SDK accepts POST. Never place the verifier in a URL,
    // where a proxy or upstream access log would retain it.
    let response = match send(
        client
            .post(format!("{API}/auth/poll"))
            .json(&json!({"uuid":device_code,"verifier":pending.verifier}))
            .timeout(Duration::from_secs(15)),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => return Ok(json!({"state":"retry","after_seconds":5})),
    };
    let status = response.status().as_u16();
    if status == 429 || status >= 500 {
        return Ok(json!({"state":"retry","after_seconds":10}));
    }
    if status == 404 {
        let raw = bounded_body(response)
            .await
            .map_err(|_| failure("invalid_response", Some(status)))?;
        return if raw.trim_ascii() == b"Not found" {
            Ok(json!({"state":"pending"}))
        } else {
            Err(failure("upstream_error", Some(status)))
        };
    }
    if status != 200 {
        return Err(failure("upstream_error", Some(status)));
    }
    let raw = bounded_body(response)
        .await
        .map_err(|_| failure("invalid_response", Some(status)))?;
    let value: Value =
        serde_json::from_slice(&raw).map_err(|_| failure("invalid_response", Some(status)))?;
    let token =
        normalize_token(value, None).map_err(|_| failure("invalid_response", Some(status)))?;
    pending.expires = Instant::now();
    pending.verifier.clear();
    SESSIONS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| failure("invalid_response", None))?
        .remove(&device_code);
    // Same explicit import/finalization path as the other account subscriptions.
    Ok(json!({"state":"authorized","api_key":token.to_string()}))
}

#[tauri::command]
pub(crate) fn cancel_cursor_auth_session(
    device_code: String,
) -> std::result::Result<(), AuthError> {
    SESSIONS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| failure("invalid_response", None))?
        .remove(&device_code);
    Ok(())
}
