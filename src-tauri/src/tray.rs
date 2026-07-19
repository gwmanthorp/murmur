use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

pub fn init(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Murmur", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&settings, &separator, &quit])?;

    let tray = TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Murmur — idle")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
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

    Ok(tray)
}
