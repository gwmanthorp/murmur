//! Overlay window controller: shows/hides the pill, toggles click-through
//! (clicks pass through except when the toggle-mode stop button is visible),
//! and auto-dismisses error toasts after 6s.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use crate::events::{OverlayPhase, OverlayState, OVERLAY_LEVEL, OVERLAY_STATE};
use crate::windows_ext;

pub struct Overlay {
    app: AppHandle,
    /// Bumped on every state change so a stale error auto-hide can't fire.
    generation: Arc<AtomicU64>,
}

impl Overlay {
    pub fn new(app: AppHandle) -> Self {
        if let Some(w) = app.get_webview_window("overlay") {
            windows_ext::make_unfocusable(&w);
            windows_ext::position_top_center(&w);
        }
        Self {
            app,
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set_state(&self, phase: OverlayPhase, toggle_mode: bool, message: Option<String>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let state = OverlayState {
            phase,
            toggle_mode,
            message,
        };
        let _ = self.app.emit(OVERLAY_STATE, state);

        if let Some(w) = self.app.get_webview_window("overlay") {
            match phase {
                OverlayPhase::Hidden => {
                    let _ = w.hide();
                }
                _ => {
                    windows_ext::position_top_center(&w);
                    // Clicks land only when the stop button is on screen
                    // (toggle-mode recording) or an error toast is shown.
                    let interactive = phase == OverlayPhase::Recording && toggle_mode;
                    let _ = w.set_ignore_cursor_events(!interactive);
                    if !w.is_visible().unwrap_or(false) {
                        let _ = w.show();
                        windows_ext::make_unfocusable(&w);
                    }
                }
            }
        }

        if phase == OverlayPhase::Error {
            // Auto-dismiss after 6s unless something newer replaced it.
            let app = self.app.clone();
            let gen_cell = self.generation.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                if gen_cell.load(Ordering::SeqCst) == generation {
                    let _ = app.emit(
                        OVERLAY_STATE,
                        OverlayState {
                            phase: OverlayPhase::Hidden,
                            toggle_mode: false,
                            message: None,
                        },
                    );
                    if let Some(w) = app.get_webview_window("overlay") {
                        let _ = w.hide();
                    }
                }
            });
        }
    }

    pub fn update_level(&self, level: f32) {
        let _ = self.app.emit(OVERLAY_LEVEL, level);
    }

    pub fn hide(&self) {
        self.set_state(OverlayPhase::Hidden, false, None);
    }
}
