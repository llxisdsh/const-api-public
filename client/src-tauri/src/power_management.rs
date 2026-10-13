//! Keep available supply and in-flight responses awake without keeping the display on.
//! The runtime task owns the OS request, so cancellation/exit always releases it.
use crate::model::AppState;

pub(crate) const SUPPORTED: bool = cfg!(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux"
));

#[tauri::command]
pub(crate) async fn set_prevent_sleep_while_supplying(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<bool, String> {
    if enabled && !SUPPORTED {
        return Err("Supply sleep prevention is unavailable on this operating system".into());
    }
    // Validate the OS boundary before accepting the setting. No power-plan changes.
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    let _checked_request = if enabled {
        Some(acquire_request().await?)
    } else {
        None
    };
    crate::commit_config_update(&state.config_path, &state.proxy_config, |config| {
        config.prevent_sleep_while_supplying = enabled;
        Ok(())
    })
    .map_err(|error| error.to_string())?;
    SETTINGS_CHANGED.notify_one();
    Ok(enabled)
}

static SETTINGS_CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

fn has_ready_supply(
    config: &crate::model::ClientConfig,
    readiness: &std::collections::HashMap<String, bool>,
    proxy_running: bool,
    supplier: &crate::model::SupplierRuntime,
) -> bool {
    config.channels.iter().any(|channel| {
        channel.enabled
            && readiness.get(&channel.id) == Some(&true)
            && (proxy_running
                || supplier
                    .channel_agents
                    .values()
                    .any(|agent| agent.channel_ids.contains(&channel.id)))
    })
}

async fn should_prevent_sleep(
    state: &AppState,
    active_requests: bool,
) -> Result<(bool, bool), String> {
    let enabled = state
        .proxy_config
        .lock()
        .map_err(|error| error.to_string())?
        .prevent_sleep_while_supplying;
    if !enabled {
        return Ok((false, false));
    }
    // Includes local/LAN inference, platform supply, pending output and replay ACKs.
    // Do not use `idle`: an update gate alone is not an active request.
    if active_requests {
        return Ok((true, true));
    }
    let proxy_running = state.proxy.lock().await.running;
    let supplier = state.supplier.lock().await;
    let config = state
        .proxy_config
        .lock()
        .map_err(|error| error.to_string())?;
    let readiness = state
        .local_channel_readiness
        .lock()
        .map_err(|error| error.to_string())?;
    // Use the same readiness decision as routing; transport reconnects do not
    // revoke an otherwise usable channel. Disabled/deleted channels cannot hold it.
    Ok((
        true,
        has_ready_supply(&config, &readiness, proxy_running, &supplier),
    ))
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
async fn acquire_request() -> Result<platform::SleepRequest, String> {
    // D-Bus may wait for a desktop service. Never block a request-processing worker.
    tokio::task::spawn_blocking(platform::SleepRequest::acquire)
        .await
        .map_err(|error| format!("Sleep prevention task: {error}"))?
}

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
pub(crate) async fn run(state: AppState) {
    let mut request = None;
    let mut last_error = String::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let mut enabled = false;
        let result = async {
            let active =
                crate::update_activity::update_install_readiness().active_logical_requests > 0;
            let (setting, needed) = should_prevent_sleep(&state, active).await?;
            enabled = setting;
            if needed && request.is_none() {
                request = Some(acquire_request().await?);
            } else if !needed {
                request = None;
            }
            Ok::<_, String>(())
        }
        .await;
        match result {
            Ok(()) => last_error.clear(),
            Err(error) => {
                if error != last_error {
                    log::warn!("[const-api][power] sleep prevention failed: {error}");
                    last_error = error;
                }
            }
        }
        tokio::select! {
            _ = tick.tick() => {},
            _ = SETTINGS_CHANGED.notified() => {},
            // Wake promptly for a request when idle; do not rescan configuration
            // for every streamed chunk while the power request is already held.
            _ = crate::update_activity::wait_for_update_activity_change(), if enabled && request.is_none() && last_error.is_empty() => {},
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::{
            Power::{
                PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest,
            },
            Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0},
        },
    };

    pub(super) struct SleepRequest(OwnedHandle);

    impl SleepRequest {
        pub(super) fn acquire() -> Result<Self, String> {
            let mut reason: Vec<u16> = "CONST API: available supply or active requests\0"
                .encode_utf16()
                .collect();
            let context = REASON_CONTEXT {
                Version: 0, // POWER_REQUEST_CONTEXT_VERSION
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 {
                    SimpleReasonString: reason.as_mut_ptr(),
                },
            };
            // PowerCreateRequest copies the reason; OwnedHandle is movable
            // across Tokio workers, unlike a thread execution-state flag.
            let raw = unsafe { PowerCreateRequest(&context) };
            if raw == INVALID_HANDLE_VALUE {
                return Err(format!(
                    "PowerCreateRequest: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            if unsafe { PowerSetRequest(handle.as_raw_handle(), PowerRequestSystemRequired) } == 0 {
                return Err(format!(
                    "PowerSetRequest: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self(handle))
        }
    }

    impl Drop for SleepRequest {
        fn drop(&mut self) {
            if unsafe { PowerClearRequest(self.0.as_raw_handle(), PowerRequestSystemRequired) } == 0
            {
                log::warn!(
                    "[const-api][power] PowerClearRequest: {}",
                    std::io::Error::last_os_error()
                );
            }
            // Closing the owned handle also releases the request if clearing fails.
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::{
        base::TCFType,
        string::{CFString, CFStringRef},
    };

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    pub(super) struct SleepRequest(u32);

    impl SleepRequest {
        pub(super) fn acquire() -> Result<Self, String> {
            let kind = CFString::new("PreventUserIdleSystemSleep");
            let reason = CFString::new("CONST API: available supply or active requests");
            let mut id = 0;
            let status = unsafe {
                IOPMAssertionCreateWithName(
                    kind.as_concrete_TypeRef(),
                    255,
                    reason.as_concrete_TypeRef(),
                    &mut id,
                )
            };
            if status != 0 {
                return Err(format!("IOPMAssertionCreateWithName: {status:#x}"));
            }
            Ok(Self(id))
        }
    }

    impl Drop for SleepRequest {
        fn drop(&mut self) {
            let status = unsafe { IOPMAssertionRelease(self.0) };
            if status != 0 {
                log::warn!("[const-api][power] IOPMAssertionRelease: {status:#x}");
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use dbus::blocking::Connection;
    use std::time::Duration;

    // Each request owns a private bus connection. Both desktop APIs remove
    // its inhibitors on disconnect, including task cancellation/process exit.
    pub(super) struct SleepRequest {
        _connection: Connection,
    }

    impl SleepRequest {
        pub(super) fn acquire() -> Result<Self, String> {
            let connection =
                Connection::new_session().map_err(|error| format!("Session D-Bus: {error}"))?;
            let reason = "CONST API: available supply or active requests";
            let gnome = connection.with_proxy(
                "org.gnome.SessionManager",
                "/org/gnome/SessionManager",
                Duration::from_secs(2),
            );
            // Suspend only (4), not idle (8): allow screen blanking and locking.
            let result: Result<(u32,), dbus::Error> = gnome.method_call(
                "org.gnome.SessionManager",
                "Inhibit",
                ("xin.const.api.client", 0u32, reason, 4u32),
            );
            if let Err(error) = result {
                if !matches!(
                    error.name(),
                    Some(
                        "org.freedesktop.DBus.Error.ServiceUnknown"
                            | "org.freedesktop.DBus.Error.NameHasNoOwner"
                    )
                ) {
                    return Err(format!("GNOME sleep inhibition: {error}"));
                }
                // KDE and other desktops implement the freedesktop power API.
                let power = connection.with_proxy(
                    "org.freedesktop.PowerManagement.Inhibit",
                    "/org/freedesktop/PowerManagement/Inhibit",
                    Duration::from_secs(2),
                );
                let _: (u32,) = power
                    .method_call(
                        "org.freedesktop.PowerManagement.Inhibit",
                        "Inhibit",
                        ("CONST API", reason),
                    )
                    .map_err(|error| format!("Desktop sleep inhibition unavailable: {error}"))?;
            }
            Ok(Self {
                _connection: connection,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    fn supplier() -> crate::model::SupplierRuntime {
        crate::model::SupplierRuntime {
            running: false,
            starting: false,
            node_id: String::new(),
            server_ws_url: String::new(),
            server_quic_url: String::new(),
            transport_preference: String::new(),
            transport_state: Arc::new(Mutex::new(Default::default())),
            nodes: vec![],
            channels: vec![],
            last_error: String::new(),
            channel_agents: HashMap::new(),
        }
    }

    #[test]
    fn ready_supply_requires_an_enabled_ready_channel_and_a_running_entry() {
        let mut config = crate::default_config();
        let channel = &mut config.channels[0];
        channel.enabled = true;
        let id = channel.id.clone();
        let mut readiness = HashMap::new();
        let runtime = supplier();
        assert!(!has_ready_supply(&config, &readiness, true, &runtime));
        readiness.insert(id.clone(), false);
        assert!(!has_ready_supply(&config, &readiness, true, &runtime));
        readiness.insert(id, true);
        assert!(!has_ready_supply(&config, &readiness, false, &runtime));
        assert!(has_ready_supply(&config, &readiness, true, &runtime));
        config.channels[0].enabled = false;
        assert!(!has_ready_supply(&config, &readiness, true, &runtime));
        config.channels.clear();
        assert!(!has_ready_supply(&config, &readiness, true, &runtime));
    }

    #[test]
    fn setting_defaults_on_and_preserves_an_explicit_opt_out() {
        let mut value = serde_json::to_value(crate::default_config()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("prevent_sleep_while_supplying");
        let mut config: crate::model::ClientConfig = serde_json::from_value(value).unwrap();
        assert!(config.prevent_sleep_while_supplying);
        config.prevent_sleep_while_supplying = false;
        let config: crate::model::ClientConfig =
            serde_json::from_value(serde_json::to_value(config).unwrap()).unwrap();
        assert!(!config.prevent_sleep_while_supplying);
    }

    #[tokio::test]
    async fn active_requests_keep_a_stopped_channel_awake_until_completion() {
        let temp = tempfile::tempdir().unwrap();
        let state = crate::runtime::create_runtime_state(
            temp.path().join("client.json"),
            crate::default_config(),
        );
        state
            .proxy_config
            .lock()
            .unwrap()
            .prevent_sleep_while_supplying = false;
        assert_eq!(
            should_prevent_sleep(&state, true).await.unwrap(),
            (false, false)
        );
        state
            .proxy_config
            .lock()
            .unwrap()
            .prevent_sleep_while_supplying = true;
        // No supply is online. Pending output/replay still counts, without
        // relying on global request state shared by concurrently running tests.
        assert_eq!(
            should_prevent_sleep(&state, true).await.unwrap(),
            (true, true)
        );
        assert_eq!(
            should_prevent_sleep(&state, false).await.unwrap(),
            (true, false)
        );
    }

    #[test]
    fn platform_channel_stays_eligible_during_transport_reconnect() {
        let mut config = crate::default_config();
        config.channels[0].enabled = true;
        let id = config.channels[0].id.clone();
        let readiness = HashMap::from([(id.clone(), true)]);
        let mut runtime = supplier();
        let (shutdown, _) = tokio::sync::oneshot::channel();
        let (register, _) = tokio::sync::mpsc::channel(1);
        runtime.channel_agents.insert(
            "test".into(),
            crate::model::SupplierAgentHandle {
                client_id: "test".into(),
                channel_ids: vec![id],
                node_ids: vec![],
                suppliers: vec![],
                agent_shutdown: shutdown,
                monitor_shutdown: None,
                register_update: register,
            },
        );
        // Neither a local listener nor a connected transport is necessary
        // while a started agent is reconnecting an otherwise ready channel.
        assert!(has_ready_supply(&config, &readiness, false, &runtime));
        runtime.channel_agents.clear();
        assert!(!has_ready_supply(&config, &readiness, false, &runtime));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_power_request_can_be_acquired_and_released() {
        // Local OS only: does not change settings, sleep the PC or contact an upstream.
        drop(platform::SleepRequest::acquire().expect("acquire system-required power request"));
    }
}
