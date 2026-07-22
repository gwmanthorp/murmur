//! Event names + payloads shared with the webviews (mirrored in src/shared/ipc.ts).

use serde::Serialize;

pub const OVERLAY_STATE: &str = "overlay://state";
pub const OVERLAY_LEVEL: &str = "overlay://level";
pub const NAVIGATE: &str = "nav://goto";
pub const SETTINGS_CHANGED: &str = "settings://changed";
pub const PIPELINE_RESULT: &str = "pipeline://result";
pub const HISTORY_CHANGED: &str = "history://changed";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayPhase {
    Hidden,
    Initializing,
    Recording,
    Transcribing,
    Executing,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayState {
    pub phase: OverlayPhase,
    pub toggle_mode: bool,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineResult {
    pub raw_transcript: String,
    pub final_text: String,
    pub degraded: bool,
}
