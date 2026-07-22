use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

use crate::core::{AppCore, RuntimePhase};

pub struct TrayState {
    update: MenuItem<tauri::Wry>,
}

pub fn init(app: &AppHandle) -> tauri::Result<TrayState> {
    let open = MenuItem::with_id(app, "open", "Open Murmur", true, None::<&str>)?;
    let update = MenuItem::with_id(app, "update", "Check for Updates...", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Murmur", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &update, &separator, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Murmur - idle")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => crate::show_settings(app),
            "update" => {
                let manager = app.state::<std::sync::Arc<crate::updater::UpdateManager>>();
                let core = app.state::<std::sync::Arc<AppCore>>();
                manager.manual_action(core.inner().clone());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Left-click opens the app; right-click surfaces the menu.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::show_settings(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(TrayState { update })
}

pub fn set_update_state(app: &AppHandle, state: &crate::updater::UpdateState) {
    if let Some(tray) = app.try_state::<TrayState>() {
        let (label, enabled) = state.tray_presentation();
        let _ = tray.update.set_text(label);
        let _ = tray.update.set_enabled(enabled);
    }
}

pub fn set_runtime_state(app: &AppHandle, phase: RuntimePhase, _paste_enabled: bool) {
    if let Some(tray) = app.tray_by_id("main") {
        let tooltip = match phase {
            RuntimePhase::Idle => "Murmur - idle",
            RuntimePhase::Starting => "Murmur - starting microphone",
            RuntimePhase::Recording => "Murmur - recording",
            RuntimePhase::Processing => "Murmur - transcribing",
        };
        let _ = tray.set_tooltip(Some(tooltip));
    }
}
