//! Whisper transcription via the OpenAI-compatible /audio/transcriptions
//! endpoint (Groq by default), with FreeFlow's silence-hallucination filter.

use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum TranscriptionError {
    #[error("{0}")]
    Http(String),
    #[error("Transcription timed out — try again")]
    TimedOut,
    #[error("No internet — check connection")]
    Offline,
    #[error("Invalid transcription response: {0}")]
    InvalidResponse(String),
}

#[derive(Deserialize)]
struct Segment {
    no_speech_prob: Option<f64>,
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
    segments: Option<Vec<Segment>>,
}

// Whisper hallucinates these on silence/background noise; drop them when
// whisper itself reports a high no_speech_prob on the first segment.
const HALLUCINATION_PHRASES: &[&str] = &[
    "thank you",
    "thank you for watching",
    "thank you very much",
    "thank you so much",
    "thanks for watching",
    "please subscribe",
    "like and subscribe",
    "subtitles by",
    "subtitles by the amara.org community",
    "you",
];
const HALLUCINATION_NO_SPEECH_THRESHOLD: f64 = 0.1;

fn friendly_http_message(status: u16, body: &str) -> String {
    match status {
        401 => "Invalid API key".into(),
        403 => "API key doesn't have access to this endpoint".into(),
        404 => "Transcription endpoint not found — check the base URL".into(),
        413 => "Recording too large for the provider".into(),
        429 => "Rate limited — try again shortly".into(),
        500..=599 => "Provider error — try again".into(),
        _ => {
            let snippet: String = body.chars().take(200).collect();
            format!("Transcription failed ({status}): {snippet}")
        }
    }
}

fn classify_reqwest_error(e: reqwest::Error) -> TranscriptionError {
    if e.is_timeout() {
        TranscriptionError::TimedOut
    } else if e.is_connect() {
        TranscriptionError::Offline
    } else {
        TranscriptionError::Http(e.to_string())
    }
}

pub struct TranscriptionRequest<'a> {
    pub base_url: &'a str,
    pub api_key: &'a str,
    pub model: &'a str,
    /// Optional ISO language hint (e.g. "en"); empty = auto-detect.
    pub language: &'a str,
    /// Optional decoding hint biasing Whisper toward custom vocabulary;
    /// empty = omit the field.
    pub prompt: &'a str,
    pub timeout: Duration,
}

/// Upload a WAV and return the transcript (may be empty after the
/// hallucination filter).
pub async fn transcribe(
    client: &reqwest::Client,
    req: TranscriptionRequest<'_>,
    wav_bytes: Vec<u8>,
) -> Result<String, TranscriptionError> {
    let is_whisper = req.model.to_lowercase().contains("whisper");
    let response_format = if is_whisper { "verbose_json" } else { "json" };

    let mut form = reqwest::multipart::Form::new()
        .text("model", req.model.to_string())
        .text("response_format", response_format.to_string())
        .part(
            "file",
            reqwest::multipart::Part::bytes(wav_bytes)
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .map_err(|e| TranscriptionError::InvalidResponse(e.to_string()))?,
        );
    if !req.language.trim().is_empty() {
        form = form.text("language", req.language.trim().to_string());
    }
    if !req.prompt.trim().is_empty() {
        form = form.text("prompt", req.prompt.trim().to_string());
    }

    let url = format!(
        "{}/audio/transcriptions",
        req.base_url.trim_end_matches('/')
    );
    let response = client
        .post(&url)
        .bearer_auth(req.api_key)
        .multipart(form)
        .timeout(req.timeout)
        .send()
        .await
        .map_err(classify_reqwest_error)?;

    let status = response.status().as_u16();
    let body = response.text().await.map_err(classify_reqwest_error)?;
    if status != 200 {
        return Err(TranscriptionError::Http(friendly_http_message(
            status, &body,
        )));
    }

    let parsed: TranscriptionResponse = serde_json::from_str(&body)
        .map_err(|e| TranscriptionError::InvalidResponse(e.to_string()))?;

    if is_hallucination(&parsed) {
        tracing::info!("filtered whisper hallucination: {:?}", parsed.text);
        return Ok(String::new());
    }
    // On near-silence Whisper sometimes regurgitates its own decoding hint.
    if echoes_prompt(&parsed.text, req.prompt) {
        tracing::info!("filtered vocabulary prompt echo: {:?}", parsed.text);
        return Ok(String::new());
    }
    Ok(parsed.text)
}

/// Normalize to lowercase alphanumeric words for comparison.
fn normalize_words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// True only when the transcript is *exactly* the prompt, so a real sentence
/// that happens to use vocabulary terms is never dropped.
fn echoes_prompt(text: &str, prompt: &str) -> bool {
    if prompt.trim().is_empty() || text.trim().is_empty() {
        return false;
    }
    normalize_words(text) == normalize_words(prompt)
}

fn is_hallucination(resp: &TranscriptionResponse) -> bool {
    let normalized: String = resp
        .text
        .to_lowercase()
        .trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .to_string();
    if !HALLUCINATION_PHRASES.contains(&normalized.as_str()) {
        return false;
    }
    // Only filter when the provider actually reports no_speech metadata.
    let Some(no_speech_prob) = resp
        .segments
        .as_ref()
        .and_then(|s| s.first())
        .and_then(|s| s.no_speech_prob)
    else {
        return false;
    };
    no_speech_prob >= HALLUCINATION_NO_SPEECH_THRESHOLD
}

/// GET {base}/models with the bearer key; 200 = valid.
pub async fn validate_api_key(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<(), String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let response = client
        .get(&url)
        .bearer_auth(api_key)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                "No internet — check connection".to_string()
            } else {
                format!("Could not reach provider: {e}")
            }
        })?;
    let status = response.status().as_u16();
    if status == 200 {
        Ok(())
    } else {
        Err(friendly_http_message(status, ""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(text: &str, no_speech: Option<f64>) -> TranscriptionResponse {
        TranscriptionResponse {
            text: text.into(),
            segments: no_speech.map(|p| {
                vec![Segment {
                    no_speech_prob: Some(p),
                }]
            }),
        }
    }

    #[test]
    fn filters_known_phrase_with_high_no_speech() {
        assert!(is_hallucination(&resp("Thank you.", Some(0.4))));
        assert!(is_hallucination(&resp(" thanks for watching! ", Some(0.1))));
        assert!(is_hallucination(&resp("You", Some(0.9))));
    }

    #[test]
    fn keeps_real_speech() {
        // Real "thank you" speech has low no_speech_prob.
        assert!(!is_hallucination(&resp("Thank you.", Some(0.05))));
        // Unknown phrases never filtered.
        assert!(!is_hallucination(&resp(
            "Thank you for the report",
            Some(0.9)
        )));
        // Missing metadata: skip the filter.
        assert!(!is_hallucination(&resp("Thank you.", None)));
    }

    #[test]
    fn parses_verbose_json_fixture() {
        let body = r#"{
            "text": "Hello world.",
            "segments": [{"id": 0, "no_speech_prob": 0.01, "text": "Hello world."}],
            "language": "en"
        }"#;
        let parsed: TranscriptionResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.text, "Hello world.");
        assert!(!is_hallucination(&parsed));
    }

    #[test]
    fn filters_exact_prompt_echo() {
        assert!(echoes_prompt("Groq, WASAPI, DPAPI.", "Groq, WASAPI, DPAPI"));
        assert!(echoes_prompt(" groq wasapi dpapi ", "Groq, WASAPI, DPAPI"));
    }

    #[test]
    fn keeps_speech_that_merely_uses_vocabulary() {
        assert!(!echoes_prompt(
            "Let's switch Groq for WASAPI.",
            "Groq, WASAPI, DPAPI"
        ));
        assert!(!echoes_prompt("Groq", "Groq, WASAPI"));
        assert!(!echoes_prompt("Anything", ""));
        assert!(!echoes_prompt("", "Groq"));
    }

    #[test]
    fn parses_plain_json_without_segments() {
        let body = r#"{"text": "Hi."}"#;
        let parsed: TranscriptionResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.text, "Hi.");
    }
}
