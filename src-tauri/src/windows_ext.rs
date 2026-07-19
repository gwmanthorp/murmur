//! Win32 window styling for the overlay: never steal focus, stay out of the
//! taskbar/alt-tab, and sit top-center of the primary display.

use tauri::{PhysicalPosition, WebviewWindow};
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

/// Top-center of the primary monitor, just below the top edge.
pub fn position_top_center(window: &WebviewWindow) {
    let Ok(Some(monitor)) = window.primary_monitor() else {
        return;
    };
    let screen = monitor.size();
    let win = window.outer_size().unwrap_or(tauri::PhysicalSize {
        width: 320,
        height: 64,
    });
    let x = monitor.position().x + ((screen.width as i32 - win.width as i32) / 2);
    let y = monitor.position().y + 8;
    let _ = window.set_position(PhysicalPosition { x, y });
}
