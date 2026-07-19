#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod audio;
mod hotkeys;
mod tray;

use hotkeys::{ShortcutBinding, SessionEvent};
use tauri::{Emitter, Manager, RunEvent};
use tauri_plugin_autostart::MacosLauncher;

#[tauri::command]
fn ping() -> String {
    "pong".into()
}

fn show_settings(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "murmur=debug,info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_settings(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![ping])
        .setup(|app| {
            tray::init(app.handle())?;

            // Shortcut engine: Right Ctrl = hold-to-talk, F9 = tap-to-toggle.
            // Bindings become configurable in M8.
            let handle = app.handle().clone();
            let engine = hotkeys::spawn(
                ShortcutBinding::new(vec![hotkeys::bindings::VK_RCONTROL]),
                ShortcutBinding::new(vec![hotkeys::bindings::VK_F9]),
                0,
                move |event: SessionEvent| {
                    tracing::info!("session event: {event:?}");
                    let _ = handle.emit("debug://shortcut", format!("{event:?}"));
                },
            );
            app.manage(engine);
            Ok(())
        })
        .on_window_event(|window, event| {
            // The app lives in the tray: closing a window only hides it.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if let RunEvent::ExitRequested { api, code, .. } = event {
                // Keep running with no visible windows unless Quit was chosen.
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
