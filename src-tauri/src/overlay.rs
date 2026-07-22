//! Overlay controller for the always-on indicator pill. The pill is permanently
//! visible, click-through, and anchored bottom-center of whichever monitor the
//! cursor is on (a background loop follows the cursor while idle and freezes in
//! place during a dictation). Phase changes only update the emitted visual
//! state; the window is never shown or hidden after startup.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::events::{OverlayPhase, OverlayState, OVERLAY_LEVEL, OVERLAY_STATE};
use crate::windows_ext;

pub struct Overlay {
    app: AppHandle,
    /// Bumped on every state change so a stale error auto-dismiss can't fire.
    generation: Arc<AtomicU64>,
    /// True while a dictation is in progress; freezes the cursor-follow loop so
    /// the pill doesn't hop monitors mid-session.
    active: Arc<AtomicBool>,
}

impl Overlay {
    pub fn new(app: AppHandle) -> Self {
        if let Some(w) = app.get_webview_window("overlay") {
            windows_ext::make_unfocusable(&w);
            // The pill is purely an indicator — clicks pass through, always.
            let _ = w.set_ignore_cursor_events(true);
            windows_ext::position_under_cursor(&w);
            let _ = w.show();
        }
        let overlay = Self {
            app,
            generation: Arc::new(AtomicU64::new(0)),
            active: Arc::new(AtomicBool::new(false)),
        };
        overlay.spawn_cursor_follow();
        overlay
    }

    /// While idle, keep the pill on the monitor under the cursor. Only moves when
    /// the cursor crosses to a different monitor to avoid needless repositioning.
    fn spawn_cursor_follow(&self) {
        let app = self.app.clone();
        let active = self.active.clone();
        tauri::async_runtime::spawn(async move {
            let mut last_monitor: Option<(i32, i32)> = None;
            loop {
                tokio::time::sleep(Duration::from_millis(400)).await;
                if active.load(Ordering::SeqCst) {
                    continue;
                }
                let Some(w) = app.get_webview_window("overlay") else {
                    continue;
                };
                let Ok(pos) = w.cursor_position() else {
                    continue;
                };
                let Ok(Some(monitor)) = w.monitor_from_point(pos.x, pos.y) else {
                    continue;
                };
                let key = (monitor.position().x, monitor.position().y);
                if last_monitor != Some(key) {
                    windows_ext::position_bottom_center(&w, &monitor);
                    last_monitor = Some(key);
                }
            }
        });
    }

    pub fn set_state(&self, phase: OverlayPhase, toggle_mode: bool, message: Option<String>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.app.emit(
            OVERLAY_STATE,
            OverlayState {
                phase,
                toggle_mode,
                message,
            },
        );

        let is_active = phase != OverlayPhase::Hidden;
        let was_active = self.active.swap(is_active, Ordering::SeqCst);
        if is_active && !was_active {
            // Anchor the pill where the cursor is as the dictation begins; the
            // follow loop then leaves it put until we return to idle.
            if let Some(w) = self.app.get_webview_window("overlay") {
                windows_ext::position_under_cursor(&w);
            }
        }

        if phase == OverlayPhase::Error {
            // Auto-return to the idle pill after 6s unless something newer replaced it.
            let app = self.app.clone();
            let gen_cell = self.generation.clone();
            let active = self.active.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(6)).await;
                if gen_cell.load(Ordering::SeqCst) == generation {
                    active.store(false, Ordering::SeqCst);
                    let _ = app.emit(
                        OVERLAY_STATE,
                        OverlayState {
                            phase: OverlayPhase::Hidden,
                            toggle_mode: false,
                            message: None,
                        },
                    );
                }
            });
        }
    }

    pub fn update_level(&self, level: f32) {
        let _ = self.app.emit(OVERLAY_LEVEL, level);
    }

    /// Return the pill to its idle state (it stays on screen).
    pub fn hide(&self) {
        self.set_state(OverlayPhase::Hidden, false, None);
    }
}
