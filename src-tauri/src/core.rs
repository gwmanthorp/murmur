use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use tauri::Emitter;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::api::cleanup::{clean_with_fallback, CleanupError, CleanupOutcome, CleanupRequest};
use crate::api::cooldown::CooldownManager;
use crate::api::transcription::{self, TranscriptionRequest};
use crate::audio::RecordingHandle;
use crate::events::{OverlayPhase, PipelineResult, PIPELINE_RESULT};
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
}

pub(crate) enum PipelineCompletion {
    Success(PipelineResult),
    Empty,
    Cancelled,
    Error(String),
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
    engine: OnceLock<Arc<ShortcutEngineHandle>>,
}

impl AppCore {
    pub fn new(app: tauri::AppHandle) -> (Arc<Self>, mpsc::UnboundedReceiver<CoreEvent>) {
        let persisted = settings::store::load();
        let api_key = settings::dpapi::decrypt(&persisted.api_key_dpapi).unwrap_or_default();
        let (tx, rx) = mpsc::unbounded_channel();
        let core = Arc::new(Self {
            overlay: Arc::new(Overlay::new(app.clone())),
            client: reqwest::Client::new(),
            cooldowns: Arc::new(CooldownManager::new(Some(settings::store::state_path()))),
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
                CoreEvent::PipelineFinished { run, result } => {
                    if !matches!(state, CoordinatorState::Processing { run: active, .. } if active == run)
                    {
                        continue;
                    }
                    state = CoordinatorState::Idle;
                    self.set_processing(false);
                    self.set_phase(RuntimePhase::Idle);
                    match result {
                        PipelineCompletion::Success(result) => {
                            *self.last_text.write().unwrap() = Some(result.final_text.clone());
                            tray::set_runtime_state(&self.app, RuntimePhase::Idle, true);
                            let _ = self.app.emit(PIPELINE_RESULT, result);
                            self.overlay.hide();
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

        tauri::async_runtime::spawn(async move {
            let completion =
                run_pipeline(recording, settings, api_key, client, cooldowns, cancel).await;
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
            &CancellationToken::new(),
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
            &CancellationToken::new(),
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
            &cancel,
        )
        .await;
        assert!(matches!(result, PipelineCompletion::Cancelled));
        let _ = std::fs::remove_file(path);
    }
}

async fn run_pipeline(
    recording: RecordingHandle,
    settings: Settings,
    api_key: String,
    client: reqwest::Client,
    cooldowns: Arc<CooldownManager>,
    cancel: CancellationToken,
) -> PipelineCompletion {
    let wav_path = match tokio::task::spawn_blocking(move || recording.stop()).await {
        Ok(Ok(path)) => path,
        Ok(Err(error)) => return PipelineCompletion::Error(error.to_string()),
        Err(error) => {
            return PipelineCompletion::Error(format!("Could not finish recording: {error}"))
        }
    };

    let result = process_wav(&wav_path, &settings, &api_key, &client, &cooldowns, &cancel).await;
    let _ = tokio::fs::remove_file(&wav_path).await;
    result
}

async fn process_wav(
    wav_path: &PathBuf,
    settings: &Settings,
    api_key: &str,
    client: &reqwest::Client,
    cooldowns: &Arc<CooldownManager>,
    cancel: &CancellationToken,
) -> PipelineCompletion {
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
    let cleanup = clean_with_fallback(client, cooldowns, &cleanup_request, &raw);
    let (final_text, degraded) = tokio::select! {
        _ = cancel.cancelled() => return PipelineCompletion::Cancelled,
        result = cleanup => resolve_cleanup(&raw, result)
    };
    if final_text.trim().is_empty() {
        return PipelineCompletion::Empty;
    }
    if cancel.is_cancelled() {
        return PipelineCompletion::Cancelled;
    }

    let paste_text = final_text.clone();
    let binding_vks = binding_vks(settings);
    let preserve_clipboard = settings.preserve_clipboard;
    let paste_result = tokio::task::spawn_blocking(move || {
        paste::paste_text(&paste_text, &binding_vks, preserve_clipboard)
    })
    .await;
    match paste_result {
        Ok(Ok(())) => PipelineCompletion::Success(PipelineResult {
            raw_transcript: raw,
            final_text,
            degraded,
        }),
        Ok(Err(error)) => PipelineCompletion::Error(error),
        Err(error) => PipelineCompletion::Error(format!("Could not paste: {error}")),
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
