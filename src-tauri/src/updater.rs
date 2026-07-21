use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::core::{AppCore, RuntimePhase};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateState {
    Idle,
    Checking,
    Downloading,
    Ready { version: String },
    Installing,
    Failed,
}

impl UpdateState {
    pub fn tray_presentation(&self) -> (String, bool) {
        match self {
            Self::Idle | Self::Failed => ("Check for Updates...".into(), true),
            Self::Checking => ("Checking for Updates...".into(), false),
            Self::Downloading => ("Downloading Update...".into(), false),
            Self::Ready { version } => (format!("Install Update v{version}..."), true),
            Self::Installing => ("Installing Update...".into(), false),
        }
    }
}

#[derive(Clone)]
struct PendingUpdate {
    update: Update,
    bytes: Vec<u8>,
}

pub struct UpdateManager {
    app: AppHandle,
    state: RwLock<UpdateState>,
    pending: Mutex<Option<PendingUpdate>>,
    checking: AtomicBool,
    prompt_open: AtomicBool,
}

impl UpdateManager {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            state: RwLock::new(UpdateState::Idle),
            pending: Mutex::new(None),
            checking: AtomicBool::new(false),
            prompt_open: AtomicBool::new(false),
        }
    }

    pub fn start(self: &Arc<Self>, core: Arc<AppCore>) {
        if cfg!(debug_assertions) {
            tracing::debug!("automatic update checks are disabled in development builds");
            return;
        }
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            manager.check(core, false).await;
        });
    }

    pub fn manual_action(self: &Arc<Self>, core: Arc<AppCore>) {
        if matches!(&*self.state.read().unwrap(), UpdateState::Ready { .. }) {
            self.prompt_install(core);
            return;
        }
        let manager = self.clone();
        tauri::async_runtime::spawn(async move { manager.check(core, true).await });
    }

    async fn check(self: Arc<Self>, core: Arc<AppCore>, user_initiated: bool) {
        if self.checking.swap(true, Ordering::SeqCst) {
            return;
        }
        self.set_state(UpdateState::Checking);

        let result = self.fetch_update().await;
        self.checking.store(false, Ordering::SeqCst);
        match result {
            Ok(Some(version)) => {
                self.set_state(UpdateState::Ready { version });
                while core.runtime_phase() != RuntimePhase::Idle {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                self.prompt_install(core);
            }
            Ok(None) => {
                self.set_state(UpdateState::Idle);
                if user_initiated {
                    self.show_message(
                        "Murmur is up to date",
                        "You already have the latest version of Murmur.",
                        MessageDialogKind::Info,
                    );
                }
            }
            Err(error) => {
                tracing::warn!("update check failed: {error}");
                self.set_state(UpdateState::Failed);
                if user_initiated {
                    self.show_message(
                        "Could not check for updates",
                        &format!("Murmur could not reach the update service.\n\n{error}"),
                        MessageDialogKind::Error,
                    );
                }
            }
        }
    }

    async fn fetch_update(&self) -> Result<Option<String>, String> {
        let updater = self
            .app
            .updater()
            .map_err(|error| format!("Could not initialize the updater: {error}"))?;
        let Some(update) = updater
            .check()
            .await
            .map_err(|error| format!("Could not read the latest release: {error}"))?
        else {
            return Ok(None);
        };

        self.set_state(UpdateState::Downloading);
        let bytes = update
            .download(|_, _| {}, || {})
            .await
            .map_err(|error| format!("Could not download or verify the update: {error}"))?;
        let version = update.version.clone();
        *self.pending.lock().unwrap() = Some(PendingUpdate { update, bytes });
        Ok(Some(version))
    }

    fn prompt_install(self: &Arc<Self>, core: Arc<AppCore>) {
        if self.prompt_open.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(version) = self.pending_version() else {
            self.prompt_open.store(false, Ordering::SeqCst);
            self.set_state(UpdateState::Idle);
            return;
        };

        let manager = self.clone();
        self.app
            .dialog()
            .message(format!(
                "Murmur v{version} has been downloaded and verified. Restart Murmur to install it now?"
            ))
            .title("Murmur update ready")
            .kind(MessageDialogKind::Info)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Restart Now".into(),
                "Later".into(),
            ))
            .show(move |restart| {
                manager.prompt_open.store(false, Ordering::SeqCst);
                if restart {
                    let install_manager = manager.clone();
                    tauri::async_runtime::spawn(async move {
                        install_manager.install(core).await;
                    });
                }
            });
    }

    async fn install(self: Arc<Self>, core: Arc<AppCore>) {
        if !core.try_prepare_for_update() {
            self.show_message(
                "Finish dictating first",
                "Murmur will keep the update ready. Install it from the tray after dictation finishes.",
                MessageDialogKind::Info,
            );
            return;
        }

        let Some(pending) = self.pending.lock().unwrap().clone() else {
            core.update_install_failed();
            self.set_state(UpdateState::Idle);
            return;
        };
        let version = pending.update.version.clone();
        self.set_state(UpdateState::Installing);
        let result =
            tokio::task::spawn_blocking(move || pending.update.install(pending.bytes)).await;

        match result {
            Ok(Ok(())) => self.app.restart(),
            Ok(Err(error)) => {
                core.update_install_failed();
                self.set_state(UpdateState::Ready { version });
                self.show_message(
                    "Could not install the update",
                    &format!("The update remains available in the tray.\n\n{error}"),
                    MessageDialogKind::Error,
                );
            }
            Err(error) => {
                core.update_install_failed();
                self.set_state(UpdateState::Ready { version });
                self.show_message(
                    "Could not install the update",
                    &format!("The update worker stopped unexpectedly.\n\n{error}"),
                    MessageDialogKind::Error,
                );
            }
        }
    }

    fn pending_version(&self) -> Option<String> {
        self.pending
            .lock()
            .unwrap()
            .as_ref()
            .map(|pending| pending.update.version.clone())
    }

    fn set_state(&self, state: UpdateState) {
        *self.state.write().unwrap() = state.clone();
        crate::tray::set_update_state(&self.app, &state);
    }

    fn show_message(&self, title: &str, message: &str, kind: MessageDialogKind) {
        self.app
            .dialog()
            .message(message)
            .title(title)
            .kind(kind)
            .show(|_| {});
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_labels_are_actionable() {
        assert_eq!(
            UpdateState::Idle.tray_presentation(),
            ("Check for Updates...".into(), true)
        );
        assert_eq!(
            UpdateState::Checking.tray_presentation(),
            ("Checking for Updates...".into(), false)
        );
        assert_eq!(
            UpdateState::Ready {
                version: "1.2.3".into()
            }
            .tray_presentation(),
            ("Install Update v1.2.3...".into(), true)
        );
        assert_eq!(
            UpdateState::Installing.tray_presentation(),
            ("Installing Update...".into(), false)
        );
    }
}
