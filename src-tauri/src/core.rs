use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use tauri::Emitter;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::api::cleanup::{clean_with_fallback, CleanupError, CleanupOutcome, CleanupRequest};
use crate::api::cooldown::CooldownManager;
use crate::api::execute::{answer_with_fallback, ExecuteRequest};
use crate::api::transcription::{self, TranscriptionRequest};
use crate::audio::RecordingHandle;
use crate::commands::{self, CommandAction};
use crate::events::{OverlayPhase, PipelineResult, HISTORY_CHANGED, PIPELINE_RESULT};
use crate::history::{HistoryEntry, HistoryStore};
use crate::hotkeys::{SessionEvent, ShortcutEngineHandle, TriggerMode};
use crate::overlay::Overlay;
use crate::settings::model::{PublicSettings, SaveSettingsInput, Settings};
use crate::{audio, paste, settings, sound, tray};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimePhase {
    Idle,
    Starting,
    Recording,
    Processing,
}

pub(crate) enum CoreEvent {
    Shortcut(SessionEvent),
    AudioLevel {
        run: u64,
        level: f32,
    },
    AudioError {
        run: u64,
        message: String,
    },
    PipelineFinished {
        run: u64,
        result: PipelineCompletion,
    },
    Executing {
        run: u64,
    },
}

pub(crate) enum PipelineCompletion {
    Completed {
        result: PipelineResult,
        history_error: Option<String>,
        paste_error: Option<String>,
    },
    Empty,
    Cancelled,
    Error(String),
}

enum BlockingFinalization {
    Cancelled,
    Finished {
        history_error: Option<String>,
        paste_error: Option<String>,
    },
}

struct PipelineRunContext {
    cancel: CancellationToken,
    stage: Option<(mpsc::UnboundedSender<CoreEvent>, u64)>,
}

impl PipelineRunContext {
    #[cfg(test)]
    fn without_stage(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            stage: None,
        }
    }
}

enum CoordinatorState {
    Idle,
    Recording {
        run: u64,
        mode: TriggerMode,
        handle: RecordingHandle,
    },
    Processing {
        run: u64,
        cancel: CancellationToken,
    },
}

pub struct AppCore {
    app: tauri::AppHandle,
    tx: mpsc::UnboundedSender<CoreEvent>,
    settings: Arc<RwLock<Settings>>,
    api_key: Arc<RwLock<String>>,
    last_text: Arc<RwLock<Option<String>>>,
    phase: Arc<RwLock<RuntimePhase>>,
    overlay: Arc<Overlay>,
    client: reqwest::Client,
    cooldowns: Arc<CooldownManager>,
    history: Arc<Result<HistoryStore, String>>,
    engine: OnceLock<Arc<ShortcutEngineHandle>>,
}

impl AppCore {
    pub fn new(app: tauri::AppHandle) -> (Arc<Self>, mpsc::UnboundedReceiver<CoreEvent>) {
        let persisted = settings::store::load();
        let api_key = settings::dpapi::decrypt(&persisted.api_key_dpapi).unwrap_or_default();
        let (tx, rx) = mpsc::unbounded_channel();
        let history = HistoryStore::initialize(settings::store::history_path()).map_err(|error| {
            tracing::warn!("history unavailable: {error}");
            error
        });
        let core = Arc::new(Self {
            overlay: Arc::new(Overlay::new(app.clone())),
            client: reqwest::Client::new(),
            cooldowns: Arc::new(CooldownManager::new(Some(settings::store::state_path()))),
            history: Arc::new(history),
            settings: Arc::new(RwLock::new(persisted)),
            api_key: Arc::new(RwLock::new(api_key)),
            last_text: Arc::new(RwLock::new(None)),
            phase: Arc::new(RwLock::new(RuntimePhase::Idle)),
            engine: OnceLock::new(),
            app,
            tx,
        });
        (core, rx)
    }

    pub fn attach_engine(&self, engine: Arc<ShortcutEngineHandle>) {
        let _ = self.engine.set(engine);
    }

    pub fn initial_shortcuts(
        &self,
    ) -> (
        crate::hotkeys::ShortcutBinding,
        crate::hotkeys::ShortcutBinding,
        u64,
    ) {
        let settings = self.settings.read().unwrap();
        (
            settings.hold_shortcut.clone(),
            settings.toggle_shortcut.clone(),
            settings.start_delay_ms,
        )
    }

    pub fn start(self: &Arc<Self>, rx: mpsc::UnboundedReceiver<CoreEvent>) {
        let core = Arc::clone(self);
        tauri::async_runtime::spawn(async move { core.run(rx).await });
    }

    pub fn send_shortcut(&self, event: SessionEvent) {
        let _ = self.tx.send(CoreEvent::Shortcut(event));
    }

    pub fn manual_toggle(&self) {
        if let Some(engine) = self.engine.get() {
            engine.manual_toggle();
        }
    }

    fn set_phase(&self, phase: RuntimePhase) {
        *self.phase.write().unwrap() = phase;
        let paste_enabled = self.last_text.read().unwrap().is_some();
        tray::set_runtime_state(&self.app, phase, paste_enabled);
    }

    fn set_processing(&self, processing: bool) {
        if let Some(engine) = self.engine.get() {
            engine.set_transcribing(processing);
        }
    }

    fn reset_shortcuts(&self) {
        if let Some(engine) = self.engine.get() {
            engine.set_suspended(true);
            engine.set_suspended(false);
        }
    }

    fn show_error(&self, message: String) {
        let enabled = self.settings.read().unwrap().sounds_enabled;
        sound::play(sound::Cue::Error, enabled);
        self.overlay
            .set_state(OverlayPhase::Error, false, Some(message));
    }

    fn finish_completed_run(
        &self,
        result: PipelineResult,
        history_error: Option<String>,
        paste_error: Option<String>,
    ) {
        *self.last_text.write().unwrap() = Some(result.final_text.clone());
        tray::set_runtime_state(&self.app, RuntimePhase::Idle, true);
        let _ = self.app.emit(PIPELINE_RESULT, result);
        if history_error.is_none() {
            let _ = self.app.emit(HISTORY_CHANGED, ());
        }
        match completion_notice(paste_error.as_deref(), history_error.as_deref()) {
            None => self.overlay.hide(),
            Some(message) => self.show_error(message.into()),
        }
    }

    async fn run(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<CoreEvent>) {
        let mut state = CoordinatorState::Idle;
        let mut next_run = 0u64;

        while let Some(event) = rx.recv().await {
            match event {
                CoreEvent::Shortcut(SessionEvent::Begin(mode)) => {
                    if !matches!(state, CoordinatorState::Idle) {
                        continue;
                    }
                    if self.api_key.read().unwrap().trim().is_empty() {
                        self.reset_shortcuts();
                        self.show_error("Add and validate your Groq API key in Settings.".into());
                        crate::show_settings(&self.app);
                        continue;
                    }

                    next_run = next_run.wrapping_add(1);
                    let run = next_run;
                    self.set_phase(RuntimePhase::Starting);
                    self.overlay.set_state(
                        OverlayPhase::Initializing,
                        mode == TriggerMode::Toggle,
                        None,
                    );
                    let config = self.settings.read().unwrap().clone();
                    let level_tx = self.tx.clone();
                    let error_tx = self.tx.clone();
                    let start_result = tokio::task::spawn_blocking(move || {
                        audio::recorder::start(
                            config.mic_device,
                            move |level| {
                                let _ = level_tx.send(CoreEvent::AudioLevel { run, level });
                            },
                            || {},
                            move |message| {
                                let _ = error_tx.send(CoreEvent::AudioError { run, message });
                            },
                        )
                    })
                    .await;

                    match start_result {
                        Ok(Ok(handle)) => {
                            let enabled = self.settings.read().unwrap().sounds_enabled;
                            sound::play(sound::Cue::Start, enabled);
                            self.set_phase(RuntimePhase::Recording);
                            self.overlay.set_state(
                                OverlayPhase::Recording,
                                mode == TriggerMode::Toggle,
                                None,
                            );
                            state = CoordinatorState::Recording { run, mode, handle };
                        }
                        Ok(Err(error)) => {
                            self.reset_shortcuts();
                            self.set_phase(RuntimePhase::Idle);
                            self.show_error(error.to_string());
                        }
                        Err(error) => {
                            self.reset_shortcuts();
                            self.set_phase(RuntimePhase::Idle);
                            self.show_error(format!("Could not start the microphone: {error}"));
                        }
                    }
                }
                CoreEvent::Shortcut(SessionEvent::Latched) => {
                    if let CoordinatorState::Recording { mode, .. } = &mut state {
                        *mode = TriggerMode::Toggle;
                        self.overlay.set_state(OverlayPhase::Recording, true, None);
                    }
                }
                CoreEvent::Shortcut(SessionEvent::Stop) => {
                    let CoordinatorState::Recording { run, handle, .. } =
                        std::mem::replace(&mut state, CoordinatorState::Idle)
                    else {
                        continue;
                    };
                    let enabled = self.settings.read().unwrap().sounds_enabled;
                    sound::play(sound::Cue::Stop, enabled);
                    self.set_phase(RuntimePhase::Processing);
                    self.set_processing(true);
                    self.overlay
                        .set_state(OverlayPhase::Transcribing, false, None);
                    let cancel = CancellationToken::new();
                    state = CoordinatorState::Processing {
                        run,
                        cancel: cancel.clone(),
                    };
                    self.spawn_pipeline(run, handle, cancel);
                }
                CoreEvent::Shortcut(SessionEvent::Cancel) => {
                    if let CoordinatorState::Recording { handle, .. } =
                        std::mem::replace(&mut state, CoordinatorState::Idle)
                    {
                        let _ = tokio::task::spawn_blocking(move || handle.cancel()).await;
                        self.set_phase(RuntimePhase::Idle);
                        self.overlay.hide();
                    }
                }
                CoreEvent::Shortcut(SessionEvent::CancelTranscription) => {
                    if let CoordinatorState::Processing { cancel, .. } = &state {
                        cancel.cancel();
                        self.overlay.hide();
                    }
                }
                CoreEvent::AudioLevel { run, level } => {
                    if matches!(state, CoordinatorState::Recording { run: active, .. } if active == run)
                    {
                        self.overlay.update_level(level);
                    }
                }
                CoreEvent::AudioError { run, message } => {
                    if matches!(state, CoordinatorState::Recording { run: active, .. } if active == run)
                    {
                        if let CoordinatorState::Recording { handle, .. } =
                            std::mem::replace(&mut state, CoordinatorState::Idle)
                        {
                            let _ = tokio::task::spawn_blocking(move || handle.cancel()).await;
                        }
                        self.set_phase(RuntimePhase::Idle);
                        self.reset_shortcuts();
                        self.show_error(format!("Microphone disconnected: {message}"));
                    }
                }
                CoreEvent::Executing { run } => {
                    if matches!(state, CoordinatorState::Processing { run: active, .. } if active == run)
                    {
                        self.overlay.set_state(OverlayPhase::Executing, false, None);
                    }
                }
                CoreEvent::PipelineFinished { run, result } => {
                    if !matches!(state, CoordinatorState::Processing { run: active, .. } if active == run)
                    {
                        continue;
                    }
                    state = CoordinatorState::Idle;
                    self.set_processing(false);
                    self.set_phase(RuntimePhase::Idle);
                    match result {
                        PipelineCompletion::Completed {
                            result,
                            history_error,
                            paste_error,
                        } => {
                            self.finish_completed_run(result, history_error, paste_error);
                        }
                        PipelineCompletion::Empty | PipelineCompletion::Cancelled => {
                            self.overlay.hide();
                        }
                        PipelineCompletion::Error(message) => self.show_error(message),
                    }
                }
            }
        }
    }

    fn spawn_pipeline(&self, run: u64, recording: RecordingHandle, cancel: CancellationToken) {
        let tx = self.tx.clone();
        let settings = self.settings.read().unwrap().clone();
        let api_key = self.api_key.read().unwrap().clone();
        let client = self.client.clone();
        let cooldowns = self.cooldowns.clone();
        let history = self.history.clone();
        let stage_tx = tx.clone();

        tauri::async_runtime::spawn(async move {
            let completion = run_pipeline(
                recording,
                settings,
                api_key,
                client,
                cooldowns,
                history,
                PipelineRunContext {
                    cancel,
                    stage: Some((stage_tx, run)),
                },
            )
            .await;
            let _ = tx.send(CoreEvent::PipelineFinished {
                run,
                result: completion,
            });
        });
    }

    pub fn public_settings(&self) -> PublicSettings {
        let settings = self.settings.read().unwrap().clone();
        PublicSettings {
            api_key_configured: !self.api_key.read().unwrap().trim().is_empty(),
            base_url: settings.base_url,
            dictation_mode: settings.dictation_mode,
            mic_device: settings.mic_device,
            mic_devices: audio::list_input_devices(),
            hold_shortcut: settings.hold_shortcut.label(),
            toggle_shortcut: settings.toggle_shortcut.label(),
            preserve_clipboard: settings.preserve_clipboard,
            commands_beta_enabled: settings.commands_beta_enabled,
        }
    }

    pub fn save_public_settings(&self, input: SaveSettingsInput) -> Result<PublicSettings, String> {
        let base_url = input.base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err("Base URL cannot be empty".into());
        }
        reqwest::Url::parse(&base_url).map_err(|_| "Base URL must be a valid URL".to_string())?;

        let mut settings = self.settings.read().unwrap().clone();
        let mut key = self.api_key.read().unwrap().clone();
        if input.clear_api_key {
            key.clear();
            settings.api_key_dpapi.clear();
        } else if let Some(replacement) = input.api_key.map(|value| value.trim().to_string()) {
            if !replacement.is_empty() {
                settings.api_key_dpapi = settings::dpapi::encrypt(&replacement)
                    .ok_or_else(|| "Windows could not protect the API key".to_string())?;
                key = replacement;
            }
        }
        settings.base_url = base_url;
        settings.dictation_mode = input.dictation_mode;
        settings.transcription_model = input.dictation_mode.transcription_model().into();
        settings.mic_device = input.mic_device.filter(|value| !value.trim().is_empty());
        settings.commands_beta_enabled = input.commands_beta_enabled;
        settings::store::save(&settings).map_err(|error| error.to_string())?;
        *self.settings.write().unwrap() = settings;
        *self.api_key.write().unwrap() = key;
        let public = self.public_settings();
        let _ = self
            .app
            .emit(crate::events::SETTINGS_CHANGED, public.clone());
        Ok(public)
    }

    pub async fn validate_credentials(
        &self,
        api_key: String,
        base_url: String,
    ) -> Result<(), String> {
        let key = if api_key.trim().is_empty() {
            self.api_key.read().unwrap().clone()
        } else {
            api_key.trim().to_string()
        };
        if key.is_empty() {
            return Err("Enter an API key first".into());
        }
        transcription::validate_api_key(&self.client, base_url.trim(), &key).await
    }

    pub async fn paste_again(&self) -> Result<(), String> {
        let text = self
            .last_text
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| "Nothing has been dictated yet".to_string())?;
        let settings = self.settings.read().unwrap().clone();
        let binding_vks = binding_vks(&settings);
        tokio::task::spawn_blocking(move || {
            paste::paste_text(&text, &binding_vks, settings.preserve_clipboard)
        })
        .await
        .map_err(|error| error.to_string())?
    }

    fn history_store(&self) -> Result<HistoryStore, String> {
        match self.history.as_ref() {
            Ok(store) => Ok(store.clone()),
            Err(error) => Err(error.clone()),
        }
    }

    pub async fn history_entries(&self) -> Result<Vec<HistoryEntry>, String> {
        let store = self.history_store()?;
        tokio::task::spawn_blocking(move || store.list())
            .await
            .map_err(|error| format!("Could not load History: {error}"))?
    }

    pub async fn copy_history(&self, id: i64) -> Result<(), String> {
        let store = self.history_store()?;
        tokio::task::spawn_blocking(move || {
            let text = store.text(id)?;
            if paste::clipboard::set_text(&text) {
                Ok(())
            } else {
                Err("Could not write the dictation to the clipboard".into())
            }
        })
        .await
        .map_err(|error| format!("Could not copy from History: {error}"))?
    }

    pub async fn delete_history(&self, id: i64) -> Result<(), String> {
        let store = self.history_store()?;
        tokio::task::spawn_blocking(move || store.delete(id))
            .await
            .map_err(|error| format!("Could not delete from History: {error}"))??;
        let _ = self.app.emit(HISTORY_CHANGED, ());
        Ok(())
    }

    pub async fn clear_history_entries(&self) -> Result<(), String> {
        let store = self.history_store()?;
        tokio::task::spawn_blocking(move || store.clear())
            .await
            .map_err(|error| format!("Could not clear History: {error}"))??;
        let _ = self.app.emit(HISTORY_CHANGED, ());
        Ok(())
    }
}

fn binding_vks(settings: &Settings) -> Vec<u16> {
    let mut keys = settings.hold_shortcut.vks.clone();
    for key in &settings.toggle_shortcut.vks {
        if !keys.contains(key) {
            keys.push(*key);
        }
    }
    keys
}

fn completion_notice(
    paste_error: Option<&str>,
    history_error: Option<&str>,
) -> Option<&'static str> {
    match (paste_error, history_error) {
        (None, None) => None,
        (Some(error), None) if error == paste::MODIFIER_RELEASE_ERROR => {
            Some(paste::MODIFIER_RELEASE_ERROR)
        }
        (Some(error), None) if error == paste::SUBMIT_ERROR => Some(paste::SUBMIT_ERROR),
        (Some(_), None) => Some("Couldn't paste — the dictation is saved in History."),
        (None, Some(_)) => Some("Dictation pasted, but it couldn't be saved to History."),
        (Some(_), Some(_)) => {
            Some("Couldn't paste or save to History. Use Paste Again before quitting.")
        }
    }
}

fn persist_then_paste<Save, Cancelled, Rollback, Paste>(
    save: Save,
    cancelled: Cancelled,
    rollback: Rollback,
    paste: Paste,
) -> BlockingFinalization
where
    Save: FnOnce() -> Result<i64, String>,
    Cancelled: FnOnce() -> bool,
    Rollback: FnOnce(i64),
    Paste: FnOnce() -> Result<(), String>,
{
    let history_result = save();
    if cancelled() {
        if let Ok(id) = history_result {
            rollback(id);
        }
        return BlockingFinalization::Cancelled;
    }
    BlockingFinalization::Finished {
        history_error: history_result.err(),
        paste_error: paste().err(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httptest::{matchers::*, responders::*, Expectation, Server};

    fn dummy_wav() -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "murmur-core-test-{}-{stamp}.wav",
            std::process::id()
        ));
        std::fs::write(&path, b"RIFF-test").unwrap();
        path
    }

    fn unavailable_history() -> Result<HistoryStore, String> {
        Err("History unavailable in this test".into())
    }

    #[test]
    fn paste_waits_for_unique_keys_from_both_bindings() {
        let mut settings = Settings::default();
        settings.hold_shortcut.vks = vec![0xA3];
        settings.toggle_shortcut.vks = vec![0xA3, 0x78];
        assert_eq!(binding_vks(&settings), vec![0xA3, 0x78]);
    }

    #[test]
    fn cleanup_failure_degrades_to_raw_text() {
        let (text, degraded) = resolve_cleanup(
            "raw dictation",
            Err(CleanupError::RequestFailed(500, "boom".into())),
        );
        assert_eq!(text, "raw dictation");
        assert!(degraded);
    }

    #[test]
    fn completion_notices_cover_partial_failures() {
        assert_eq!(completion_notice(None, None), None);
        assert_eq!(
            completion_notice(Some("paste"), None),
            Some("Couldn't paste — the dictation is saved in History.")
        );
        assert_eq!(
            completion_notice(None, Some("history")),
            Some("Dictation pasted, but it couldn't be saved to History.")
        );
        assert_eq!(
            completion_notice(Some("paste"), Some("history")),
            Some("Couldn't paste or save to History. Use Paste Again before quitting.")
        );
        assert_eq!(
            completion_notice(Some(paste::MODIFIER_RELEASE_ERROR), None),
            Some("Release modifier keys, then use Paste Again.")
        );
    }

    #[test]
    fn finalization_persists_before_paste_even_when_history_fails() {
        let calls = std::sync::Mutex::new(Vec::new());
        let completion = persist_then_paste(
            || {
                calls.lock().unwrap().push("history");
                Err("disk full".into())
            },
            || false,
            |_| calls.lock().unwrap().push("rollback"),
            || {
                calls.lock().unwrap().push("paste");
                Ok(())
            },
        );
        assert_eq!(*calls.lock().unwrap(), vec!["history", "paste"]);
        assert!(matches!(
            completion,
            BlockingFinalization::Finished {
                history_error: Some(_),
                paste_error: None
            }
        ));
    }

    #[test]
    fn cancellation_after_save_rolls_back_without_pasting() {
        let calls = std::sync::Mutex::new(Vec::new());
        let completion = persist_then_paste(
            || {
                calls.lock().unwrap().push("history");
                Ok(42)
            },
            || true,
            |id| {
                calls
                    .lock()
                    .unwrap()
                    .push(if id == 42 { "rollback" } else { "wrong" })
            },
            || {
                calls.lock().unwrap().push("paste");
                Ok(())
            },
        );
        assert_eq!(*calls.lock().unwrap(), vec!["history", "rollback"]);
        assert!(matches!(completion, BlockingFinalization::Cancelled));
    }

    #[tokio::test]
    async fn silence_stops_before_cleanup_or_paste() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/audio/transcriptions"))
                .respond_with(json_encoded(
                    serde_json::json!({"text": "", "segments": []}),
                )),
        );
        let path = dummy_wav();
        let settings = Settings {
            base_url: server.url_str("/"),
            ..Default::default()
        };
        let result = process_wav(
            &path,
            &settings,
            "test-key",
            &reqwest::Client::new(),
            &Arc::new(CooldownManager::new(None)),
            &unavailable_history(),
            &PipelineRunContext::without_stage(CancellationToken::new()),
        )
        .await;
        assert!(matches!(result, PipelineCompletion::Empty));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn invalid_key_becomes_pipeline_error() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/audio/transcriptions"))
                .respond_with(status_code(401)),
        );
        let path = dummy_wav();
        let settings = Settings {
            base_url: server.url_str("/"),
            ..Default::default()
        };
        let result = process_wav(
            &path,
            &settings,
            "bad-key",
            &reqwest::Client::new(),
            &Arc::new(CooldownManager::new(None)),
            &unavailable_history(),
            &PipelineRunContext::without_stage(CancellationToken::new()),
        )
        .await;
        assert!(
            matches!(result, PipelineCompletion::Error(message) if message == "Invalid API key")
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn pre_cancelled_run_never_reaches_network_or_paste() {
        let path = dummy_wav();
        let settings = Settings {
            base_url: "http://127.0.0.1:9".into(),
            ..Default::default()
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = process_wav(
            &path,
            &settings,
            "test-key",
            &reqwest::Client::new(),
            &Arc::new(CooldownManager::new(None)),
            &unavailable_history(),
            &PipelineRunContext::without_stage(cancel),
        )
        .await;
        assert!(matches!(result, PipelineCompletion::Cancelled));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn command_without_a_body_stops_before_cleanup_or_history() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/audio/transcriptions"))
                .respond_with(json_encoded(
                    serde_json::json!({"text": "execute...", "segments": []}),
                )),
        );
        let path = dummy_wav();
        let settings = Settings {
            base_url: server.url_str("/"),
            commands_beta_enabled: true,
            ..Default::default()
        };
        let result = process_wav(
            &path,
            &settings,
            "test-key",
            &reqwest::Client::new(),
            &Arc::new(CooldownManager::new(None)),
            &unavailable_history(),
            &PipelineRunContext::without_stage(CancellationToken::new()),
        )
        .await;
        assert!(matches!(
            result,
            PipelineCompletion::Error(message) if message == "Say something before execute."
        ));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn execute_failure_stops_before_history_and_paste() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/audio/transcriptions"))
                .respond_with(json_encoded(
                    serde_json::json!({"text": "what is ten times 52 execute", "segments": []}),
                )),
        );
        server.expect(
            Expectation::matching(request::method_path("POST", "/chat/completions"))
                .times(3)
                .respond_with(cycle![
                    json_encoded(serde_json::json!({"choices": [{"message": {"content": "What is ten times 52?"}}]})),
                    status_code(500),
                    status_code(500),
                ]),
        );
        let path = dummy_wav();
        let settings = Settings {
            base_url: server.url_str("/"),
            commands_beta_enabled: true,
            ..Default::default()
        };
        let result = process_wav(
            &path,
            &settings,
            "test-key",
            &reqwest::Client::new(),
            &Arc::new(CooldownManager::new(None)),
            &unavailable_history(),
            &PipelineRunContext::without_stage(CancellationToken::new()),
        )
        .await;
        assert!(matches!(
            result,
            PipelineCompletion::Error(message) if message.starts_with("Couldn't answer that request:")
        ));
        let _ = std::fs::remove_file(path);
    }
}

async fn run_pipeline(
    recording: RecordingHandle,
    settings: Settings,
    api_key: String,
    client: reqwest::Client,
    cooldowns: Arc<CooldownManager>,
    history: Arc<Result<HistoryStore, String>>,
    run_context: PipelineRunContext,
) -> PipelineCompletion {
    let wav_path = match tokio::task::spawn_blocking(move || recording.stop()).await {
        Ok(Ok(path)) => path,
        Ok(Err(error)) => return PipelineCompletion::Error(error.to_string()),
        Err(error) => {
            return PipelineCompletion::Error(format!("Could not finish recording: {error}"))
        }
    };

    let result = process_wav(
        &wav_path,
        &settings,
        &api_key,
        &client,
        &cooldowns,
        history.as_ref(),
        &run_context,
    )
    .await;
    let _ = tokio::fs::remove_file(&wav_path).await;
    result
}

async fn process_wav(
    wav_path: &PathBuf,
    settings: &Settings,
    api_key: &str,
    client: &reqwest::Client,
    cooldowns: &Arc<CooldownManager>,
    history: &Result<HistoryStore, String>,
    run_context: &PipelineRunContext,
) -> PipelineCompletion {
    let cancel = &run_context.cancel;
    let wav_bytes = match tokio::fs::read(wav_path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return PipelineCompletion::Error(format!("Could not read recording: {error}"))
        }
    };
    let transcribe = transcription::transcribe(
        client,
        TranscriptionRequest {
            base_url: &settings.base_url,
            api_key,
            model: settings.dictation_mode.transcription_model(),
            language: &settings.language,
            timeout: Duration::from_secs(20),
        },
        wav_bytes,
    );
    let raw = tokio::select! {
        _ = cancel.cancelled() => return PipelineCompletion::Cancelled,
        result = transcribe => match result {
            Ok(text) => text.trim().to_string(),
            Err(error) => return PipelineCompletion::Error(error.to_string()),
        }
    };
    if raw.is_empty() {
        return PipelineCompletion::Empty;
    }

    let parsed = commands::parse(&raw, settings.commands_beta_enabled);
    if parsed.action != CommandAction::Paste && parsed.body.is_empty() {
        let command = match parsed.action {
            CommandAction::Dispatch => "dispatch",
            CommandAction::Execute => "execute",
            CommandAction::Paste => unreachable!(),
        };
        return PipelineCompletion::Error(format!("Say something before {command}."));
    }

    let cleanup_input = parsed.body;
    let cleanup_request = CleanupRequest {
        base_url: settings.base_url.clone(),
        api_key: api_key.to_string(),
        primary_model: settings.cleanup_model.clone(),
        fallback_model: settings.cleanup_fallback_model.clone(),
        custom_system_prompt: settings.custom_system_prompt.clone(),
        custom_vocabulary: settings.custom_vocabulary.clone(),
        context_summary: String::new(),
        instruction_guard_enabled: settings.instruction_guard_enabled,
        timeout: Duration::from_secs(20),
    };
    let cleanup = clean_with_fallback(client, cooldowns, &cleanup_request, &cleanup_input);
    let (cleaned_text, degraded) = tokio::select! {
        _ = cancel.cancelled() => return PipelineCompletion::Cancelled,
        result = cleanup => resolve_cleanup(&cleanup_input, result)
    };
    let final_text = if parsed.action == CommandAction::Execute {
        if let Some((tx, run)) = &run_context.stage {
            let _ = tx.send(CoreEvent::Executing { run: *run });
        }
        let request_text = if cleaned_text.trim().is_empty() {
            cleanup_input.clone()
        } else {
            cleaned_text
        };
        let execute_request = ExecuteRequest {
            base_url: settings.base_url.clone(),
            api_key: api_key.to_string(),
            primary_model: settings.cleanup_model.clone(),
            fallback_model: settings.cleanup_fallback_model.clone(),
            timeout: Duration::from_secs(20),
        };
        let execute = answer_with_fallback(client, cooldowns, &execute_request, &request_text);
        tokio::select! {
            _ = cancel.cancelled() => return PipelineCompletion::Cancelled,
            result = execute => match result {
                Ok(answer) => answer,
                Err(error) => return PipelineCompletion::Error(format!("Couldn't answer that request: {error}")),
            }
        }
    } else {
        cleaned_text
    };
    if final_text.trim().is_empty() {
        return PipelineCompletion::Empty;
    }
    if cancel.is_cancelled() {
        return PipelineCompletion::Cancelled;
    }

    let result = PipelineResult {
        raw_transcript: raw,
        final_text: final_text.clone(),
        degraded,
    };
    let history_store = match history {
        Ok(store) => Ok(store.clone()),
        Err(error) => Err(error.clone()),
    };
    let rollback_store = history.as_ref().ok().cloned();
    let history_text = final_text.clone();
    let paste_text = final_text;
    let binding_vks = binding_vks(settings);
    let preserve_clipboard = settings.preserve_clipboard;
    let worker_cancel = cancel.clone();
    let finalization = tokio::task::spawn_blocking(move || {
        persist_then_paste(
            || match history_store {
                Ok(store) => store.insert(&history_text),
                Err(error) => Err(error),
            },
            || worker_cancel.is_cancelled(),
            |id| {
                if let Some(store) = rollback_store {
                    let _ = store.delete(id);
                }
            },
            || {
                paste::paste_text_with_submit(
                    &paste_text,
                    &binding_vks,
                    preserve_clipboard,
                    parsed.action == CommandAction::Dispatch,
                )
            },
        )
    })
    .await;
    match finalization {
        Ok(BlockingFinalization::Cancelled) => PipelineCompletion::Cancelled,
        Ok(BlockingFinalization::Finished {
            history_error,
            paste_error,
        }) => PipelineCompletion::Completed {
            result,
            history_error,
            paste_error,
        },
        Err(error) => PipelineCompletion::Completed {
            result,
            history_error: Some(format!("Could not finish History: {error}")),
            paste_error: Some(format!("Could not finish paste: {error}")),
        },
    }
}

fn resolve_cleanup(raw: &str, result: Result<CleanupOutcome, CleanupError>) -> (String, bool) {
    match result {
        Ok(outcome) => (outcome.text, outcome.degraded),
        Err(error) => {
            tracing::warn!("cleanup failed; using raw transcript: {error}");
            (raw.to_string(), true)
        }
    }
}

#[tauri::command]
pub fn get_settings(core: tauri::State<'_, Arc<AppCore>>) -> PublicSettings {
    core.public_settings()
}

#[tauri::command]
pub fn save_settings(
    core: tauri::State<'_, Arc<AppCore>>,
    input: SaveSettingsInput,
) -> Result<PublicSettings, String> {
    core.save_public_settings(input)
}

#[tauri::command]
pub async fn validate_credentials(
    core: tauri::State<'_, Arc<AppCore>>,
    api_key: String,
    base_url: String,
) -> Result<(), String> {
    core.validate_credentials(api_key, base_url).await
}

#[tauri::command]
pub fn stop_dictating(core: tauri::State<'_, Arc<AppCore>>) {
    core.manual_toggle();
}

#[tauri::command]
pub async fn paste_again(core: tauri::State<'_, Arc<AppCore>>) -> Result<(), String> {
    core.paste_again().await
}

#[tauri::command]
pub async fn get_history(
    core: tauri::State<'_, Arc<AppCore>>,
) -> Result<Vec<HistoryEntry>, String> {
    core.history_entries().await
}

#[tauri::command]
pub async fn copy_history_entry(
    core: tauri::State<'_, Arc<AppCore>>,
    id: i64,
) -> Result<(), String> {
    core.copy_history(id).await
}

#[tauri::command]
pub async fn delete_history_entry(
    core: tauri::State<'_, Arc<AppCore>>,
    id: i64,
) -> Result<(), String> {
    core.delete_history(id).await
}

#[tauri::command]
pub async fn clear_history(core: tauri::State<'_, Arc<AppCore>>) -> Result<(), String> {
    core.clear_history_entries().await
}
