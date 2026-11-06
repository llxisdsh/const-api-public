//! Background policy for the main WebView only. Keep the document, drafts and
//! unfinished JS/IPC workflows alive; never destroy, navigate or TrySuspend it.
//! Platform differences and the rollback switch stay here, outside the runtime.

use std::sync::atomic::{AtomicU8, Ordering};
use tauri::Manager;

// One source-level switch rolls back both native visibility and memory hints.
// The renderer's independent visibility-aware display polling remains useful.
const ENABLED: bool = true;

#[derive(Default)]
struct BackgroundState(AtomicU8);

impl BackgroundState {
    fn apply_if_changed<E>(
        &self,
        background: bool,
        apply: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        let target = if background { 2 } else { 1 };
        // All applications run on the GUI thread. Record before invoking native
        // APIs so a reentrant focus event cannot repeatedly hide/show the view.
        if self.0.swap(target, Ordering::Relaxed) == target {
            return Ok(());
        }
        if let Err(error) = apply() {
            self.0.store(0, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }
}

pub(crate) fn configure(mut context: tauri::Context<tauri::Wry>) -> tauri::Context<tauri::Wry> {
    if ENABLED {
        for window in &mut context.config_mut().app.windows {
            if window.label == "main" {
                // Tauri/Wry applies this public policy on macOS 14+ and leaves
                // older WebKit versions alone. Windows/Linux ignore this option.
                // Throttle preserves ongoing interactive authorization/IPC.
                window.background_throttling =
                    Some(tauri::utils::config::BackgroundThrottlingPolicy::Throttle);
            }
        }
    }
    context
}

pub(crate) fn install(app: &tauri::AppHandle) {
    if ENABLED {
        app.manage(BackgroundState::default());
    }
}

pub(crate) fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if window.label() == "main"
        && matches!(
            event,
            tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Focused(_)
        )
    {
        // Losing focus alone is not a reason to throttle a still-visible UI.
        update(window.app_handle());
    }
}

pub(crate) fn update(app: &tauri::AppHandle) {
    if !ENABLED {
        return;
    }
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || update_on_main_thread(&handle)) {
        log::debug!("[const-api][window] background policy dispatch failed: {error}");
    }
}

fn update_on_main_thread(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Some(state) = app.try_state::<BackgroundState>() else {
        return;
    };
    // Read at execution time, not before dispatch: a queued hide must not hide
    // the WebView again after the native window has already been restored.
    let background =
        window.is_visible().is_ok_and(|visible| !visible) || window.is_minimized().unwrap_or(false);
    if let Err(error) = state.apply_if_changed(background, || {
        let webview: &tauri::Webview = window.as_ref();
        if background {
            webview.hide()?;
        } else {
            webview.show()?;
        }
        // hide/show uses WebView2 IsVisible, WKWebView setHidden and GTK widget
        // visibility respectively. WebKit owns its normal background throttling;
        // no process-wide cache changes or private suspension APIs are needed.
        update_memory_target(&window, background);
        Ok::<_, tauri::Error>(())
    }) {
        log::warn!("[const-api][window] background policy failed: {error}");
    }
}

fn update_memory_target(window: &tauri::WebviewWindow, background: bool) {
    #[cfg(target_os = "windows")]
    {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL, ICoreWebView2_19,
        };
        use windows::core::Interface;
        if let Err(error) = window.with_webview(move |view| {
            let result = (|| -> windows::core::Result<()> {
                let webview: ICoreWebView2_19 =
                    unsafe { view.controller().CoreWebView2()? }.cast()?;
                unsafe {
                    webview.SetMemoryUsageTargetLevel(if background {
                        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
                    } else {
                        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
                    })
                }
            })();
            if let Err(error) = result {
                // Older WebView2 runtimes may not expose this optional interface.
                log::debug!("[const-api][window] memory target unavailable: {error}");
            }
        }) {
            log::debug!("[const-api][window] memory target dispatch failed: {error}");
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = (window, background);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_transitions_preserve_restore_and_skip_duplicate_events() {
        let state = BackgroundState::default();
        let mut transitions = Vec::new();
        for background in [false, false, true, true, false, false, true, false] {
            state
                .apply_if_changed(background, || {
                    transitions.push(background);
                    Ok::<_, ()>(())
                })
                .unwrap();
        }
        assert_eq!(transitions, [false, true, false, true, false]);
    }

    #[test]
    fn failed_restore_can_be_retried_by_the_next_focus_event() {
        let state = BackgroundState::default();
        state.apply_if_changed(true, || Ok::<_, &str>(())).unwrap();
        assert_eq!(
            state.apply_if_changed(false, || Err("native error")),
            Err("native error")
        );
        let mut restored = false;
        state
            .apply_if_changed(false, || {
                restored = true;
                Ok::<_, &str>(())
            })
            .unwrap();
        assert!(restored);
    }

    #[test]
    fn native_focus_reentry_does_not_repeat_the_same_transition() {
        let state = BackgroundState::default();
        state
            .apply_if_changed(true, || {
                state
                    .apply_if_changed::<()>(true, || panic!("duplicate native visibility call"))?;
                Ok::<_, ()>(())
            })
            .unwrap();
    }

    // Opt-in desktop check: an off-screen about:blank window with a disposable
    // WebView profile. It never loads the renderer or the user's configuration.
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires a Windows desktop and installed WebView2 runtime"]
    fn native_webview_visibility_restores_after_hide_and_minimize() {
        use tauri::utils::config::WebviewUrl;

        let profile = tempfile::tempdir().unwrap();
        let profile_path = profile.path().to_path_buf();
        let mut context = tauri::generate_context!();
        context.config_mut().identifier = "xin.const.api.test.webview-background".into();
        context.config_mut().app.windows.clear();
        let (sender, receiver) = std::sync::mpsc::channel();
        let app = tauri::Builder::default()
            .any_thread()
            .setup(move |app| {
                install(app.handle());
                tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    WebviewUrl::External("about:blank".parse().unwrap()),
                )
                .data_directory(profile_path)
                .visible(false)
                .focused(false)
                .position(-20_000.0, -20_000.0)
                .build()?;
                Ok(())
            })
            .on_window_event(on_window_event)
            .build(configure(context))
            .unwrap();
        app.run_return(move |app, event| {
            if !matches!(event, tauri::RunEvent::Ready) {
                return;
            }
            let result = (|| -> Result<Vec<bool>, String> {
                let window = app.get_webview_window("main").unwrap();
                let mut observed = Vec::new();
                for step in 0..7 {
                    match step {
                        0 | 2 => window.show(),
                        1 | 5 => window.hide(),
                        3 => window.minimize(),
                        4 => window.unminimize(),
                        _ => window.show(),
                    }
                    .map_err(|error| error.to_string())?;
                    update(app);
                    let (tx, rx) = std::sync::mpsc::channel();
                    window
                        .with_webview(move |view| {
                            let mut visible = windows::core::BOOL::default();
                            let value = unsafe { view.controller().IsVisible(&mut visible) }
                                .map(|()| visible.as_bool())
                                .map_err(|error| error.to_string());
                            let _ = tx.send(value);
                        })
                        .map_err(|error| error.to_string())?;
                    observed.push(rx.try_recv().map_err(|error| error.to_string())??);
                }
                Ok(observed)
            })();
            let _ = sender.send(result);
            app.exit(0);
        });
        assert_eq!(
            receiver.try_recv().unwrap().unwrap(),
            [true, false, true, false, true, false, true]
        );
    }
}
