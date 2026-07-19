#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod audio;
mod core;
mod events;
mod hotkeys;
mod overlay;
mod paste;
mod settings;
mod sound;
mod tray;
mod windows_ext;

use std::sync::Arc;

use tauri::{Manager, RunEvent};
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
        .invoke_handler(tauri::generate_handler![
            ping,
            core::get_settings,
            core::save_settings,
            core::validate_credentials,
            core::stop_dictating,
            core::paste_again,
        ])
        .setup(|app| {
            let tray_state = tray::init(app.handle())?;
            app.manage(tray_state);

            let (core, rx) = core::AppCore::new(app.handle().clone());
            let (hold, toggle, delay) = core.initial_shortcuts();
            let events = core.clone();
            let engine = Arc::new(hotkeys::spawn(hold, toggle, delay, move |event| {
                tracing::info!("session event: {event:?}");
                events.send_shortcut(event);
            }));
            core.attach_engine(engine);
            core.start(rx);
            app.manage(core);
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
