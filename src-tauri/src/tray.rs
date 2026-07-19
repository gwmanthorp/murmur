use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

use crate::core::{AppCore, RuntimePhase};

pub struct TrayState {
    toggle: MenuItem<tauri::Wry>,
    paste_again: MenuItem<tauri::Wry>,
}

pub fn init(app: &AppHandle) -> tauri::Result<TrayState> {
    let toggle = MenuItem::with_id(app, "toggle", "Start Dictating", true, None::<&str>)?;
    let paste_again = MenuItem::with_id(app, "paste_again", "Paste Again", false, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings...", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Murmur", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &paste_again, &settings, &separator, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Murmur - idle")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => app.state::<std::sync::Arc<AppCore>>().manual_toggle(),
            "paste_again" => {
                let core = app.state::<std::sync::Arc<AppCore>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = core.paste_again().await {
                        tracing::warn!("paste again failed: {error}");
                    }
                });
            }
            "settings" => crate::show_settings(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                crate::show_settings(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(TrayState {
        toggle,
        paste_again,
    })
}

pub fn set_runtime_state(app: &AppHandle, phase: RuntimePhase, paste_enabled: bool) {
    if let Some(tray) = app.tray_by_id("main") {
        let tooltip = match phase {
            RuntimePhase::Idle => "Murmur - idle",
            RuntimePhase::Starting => "Murmur - starting microphone",
            RuntimePhase::Recording => "Murmur - recording",
            RuntimePhase::Processing => "Murmur - transcribing",
        };
        let _ = tray.set_tooltip(Some(tooltip));
    }
    if let Some(state) = app.try_state::<TrayState>() {
        let label = match phase {
            RuntimePhase::Recording | RuntimePhase::Starting => "Stop Dictating",
            RuntimePhase::Processing => "Processing...",
            RuntimePhase::Idle => "Start Dictating",
        };
        let _ = state.toggle.set_text(label);
        let _ = state.toggle.set_enabled(phase != RuntimePhase::Processing);
        let _ = state.paste_again.set_enabled(paste_enabled);
    }
}
