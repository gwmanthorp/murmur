//! Win32 window styling for the overlay: never steal focus, stay out of the
//! taskbar/alt-tab, and sit bottom-center of the monitor under the cursor.

use tauri::{Monitor, PhysicalPosition, WebviewWindow};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// Tauri's `focus: false` alone doesn't survive every show/hide path — OR the
/// styles in directly so the overlay can never be activated.
pub fn make_unfocusable(window: &WebviewWindow) {
    if let Ok(hwnd) = window.hwnd() {
        let hwnd = HWND(hwnd.0);
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                style | (WS_EX_NOACTIVATE.0 as isize) | (WS_EX_TOOLWINDOW.0 as isize),
            );
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }
}

/// Gap between the pill and the bottom of the monitor's work area.
const BOTTOM_GAP: i32 = 6;

/// Bottom-center of `monitor`'s work area (which already excludes the taskbar),
/// leaving a small gap above the bottom edge.
pub fn position_bottom_center(window: &WebviewWindow, monitor: &Monitor) {
    let area = monitor.work_area();
    let win = window.outer_size().unwrap_or(tauri::PhysicalSize {
        width: 160,
        height: 76,
    });
    let x = area.position.x + ((area.size.width as i32 - win.width as i32) / 2);
    let y = area.position.y + area.size.height as i32 - win.height as i32 - BOTTOM_GAP;
    let _ = window.set_position(PhysicalPosition { x, y });
}

/// Place the pill on whichever monitor the mouse cursor is currently on.
pub fn position_under_cursor(window: &WebviewWindow) {
    let monitor = window
        .cursor_position()
        .ok()
        .and_then(|p| window.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());
    if let Some(monitor) = monitor {
        position_bottom_center(window, &monitor);
    }
}
