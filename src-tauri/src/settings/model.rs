use serde::{Deserialize, Serialize};

use crate::api::models;
use crate::hotkeys::bindings::{VK_F9, VK_RCONTROL};
use crate::hotkeys::ShortcutBinding;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DictationMode {
    Fast,
    #[default]
    Polished,
}

impl DictationMode {
    pub const fn transcription_model(self) -> &'static str {
        match self {
            Self::Fast => models::FAST_TRANSCRIPTION_MODEL,
            Self::Polished => models::DEFAULT_TRANSCRIPTION_MODEL,
        }
    }
}

/// Persisted settings (settings.json). The API key is stored as a DPAPI blob
/// in `api_key_dpapi`, never plaintext; the in-memory decrypted key lives
/// only in `Secrets`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub version: u32,
    pub api_key_dpapi: String,
    pub base_url: String,
    pub dictation_mode: DictationMode,
    pub transcription_model: String,
    pub cleanup_model: String,
    pub cleanup_fallback_model: String,
    /// ISO language hint for transcription; empty = auto-detect.
    pub language: String,
    pub custom_vocabulary: String,
    pub custom_system_prompt: String,
    pub hold_shortcut: ShortcutBinding,
    pub toggle_shortcut: ShortcutBinding,
    pub start_delay_ms: u64,
    pub preserve_clipboard: bool,
    pub commands_beta_enabled: bool,
    pub sounds_enabled: bool,
    pub launch_at_login: bool,
    pub mic_device: Option<String>,
    pub instruction_guard_enabled: bool,
    pub onboarding_complete: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            api_key_dpapi: String::new(),
            base_url: models::DEFAULT_BASE_URL.into(),
            dictation_mode: DictationMode::default(),
            transcription_model: models::DEFAULT_TRANSCRIPTION_MODEL.into(),
            cleanup_model: models::DEFAULT_CLEANUP_MODEL.into(),
            cleanup_fallback_model: models::DEFAULT_CLEANUP_FALLBACK_MODEL.into(),
            language: String::new(),
            custom_vocabulary: String::new(),
            custom_system_prompt: String::new(),
            hold_shortcut: ShortcutBinding::new(vec![VK_RCONTROL]),
            toggle_shortcut: ShortcutBinding::new(vec![VK_F9]),
            start_delay_ms: 0,
            preserve_clipboard: true,
            commands_beta_enabled: false,
            sounds_enabled: true,
            launch_at_login: false,
            mic_device: None,
            instruction_guard_enabled: true,
            onboarding_complete: false,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicSettings {
    pub api_key_configured: bool,
    pub base_url: String,
    pub dictation_mode: DictationMode,
    pub mic_device: Option<String>,
    pub mic_devices: Vec<String>,
    pub hold_shortcut: String,
    pub toggle_shortcut: String,
    pub preserve_clipboard: bool,
    pub commands_beta_enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSettingsInput {
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
    pub base_url: String,
    pub dictation_mode: DictationMode,
    pub mic_device: Option<String>,
    pub commands_beta_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polished_is_the_backwards_compatible_default() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.dictation_mode, DictationMode::Polished);
        assert!(!settings.commands_beta_enabled);
        assert_eq!(
            settings.dictation_mode.transcription_model(),
            "whisper-large-v3"
        );
    }

    #[test]
    fn fast_mode_selects_whisper_turbo() {
        assert_eq!(
            DictationMode::Fast.transcription_model(),
            "whisper-large-v3-turbo"
        );
    }

    #[test]
    fn dictation_mode_round_trips_as_lowercase_json() {
        let settings = Settings {
            dictation_mode: DictationMode::Fast,
            ..Default::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""dictationMode":"fast""#));
        let decoded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.dictation_mode, DictationMode::Fast);
    }

    #[test]
    fn commands_beta_round_trips() {
        let settings = Settings {
            commands_beta_enabled: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""commandsBetaEnabled":true"#));
        assert!(
            serde_json::from_str::<Settings>(&json)
                .unwrap()
                .commands_beta_enabled
        );
    }
}
