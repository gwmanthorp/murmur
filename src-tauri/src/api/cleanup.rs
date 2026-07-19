//! LLM cleanup of raw transcripts (port of FreeFlow's PostProcessingService):
//! chat/completions at temperature 0, primary → fallback model retry on
//! 429/empty/suspected-instruction-execution, EMPTY sentinel, per-model
//! cooldown circuit breaker, and graceful degradation to the raw transcript.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::cooldown::{rate_limit_cooldown, CooldownManager};
use super::models;
use super::prompts;

#[derive(Debug, thiserror::Error)]
pub enum CleanupError {
    #[error("Model {model} rate-limited — retry in {retry_after:.0}s")]
    RateLimited { model: String, retry_after: f64 },
    #[error("Cleanup failed with status {0}: {1}")]
    RequestFailed(u16, String),
    #[error("Invalid cleanup response: {0}")]
    InvalidResponse(String),
    #[error("Cleanup returned empty output")]
    EmptyOutput,
    #[error("Cleanup timed out")]
    TimedOut,
    #[error("Cleanup output looked like it answered the transcript instead of cleaning it")]
    SuspectedInstructionExecution,
}

pub struct CleanupRequest {
    pub base_url: String,
    pub api_key: String,
    pub primary_model: String,
    pub fallback_model: String,
    pub custom_system_prompt: String,
    pub custom_vocabulary: String,
    pub context_summary: String,
    pub instruction_guard_enabled: bool,
    pub timeout: Duration,
}

#[derive(Debug)]
pub struct CleanupOutcome {
    pub text: String,
    /// Full prompt for debugging/history; empty when cleanup was skipped.
    #[allow(dead_code)] // retained for the deferred history/settings surface
    pub prompt: String,
    pub degraded: bool,
}

/// Top-level entry: try primary then fallback; on unrecoverable failure the
/// caller should paste the raw transcript (we signal that via Err).
pub async fn clean_with_fallback(
    client: &reqwest::Client,
    cooldowns: &Arc<CooldownManager>,
    req: &CleanupRequest,
    transcript: &str,
) -> Result<CleanupOutcome, CleanupError> {
    let vocabulary = prompts::merged_vocabulary_terms(&req.custom_vocabulary);
    let primary = req.primary_model.trim();
    let primary = if primary.is_empty() {
        models::DEFAULT_CLEANUP_MODEL
    } else {
        primary
    };
    let fallback = req.fallback_model.trim();
    let fallback = if fallback.is_empty() {
        models::DEFAULT_CLEANUP_FALLBACK_MODEL
    } else {
        fallback
    };
    let retry_model = (fallback != primary).then_some(fallback);

    // Circuit breaker: skip a doomed request when both models are cooling.
    let Some(model) = cooldowns.effective_primary(primary, retry_model) else {
        tracing::warn!("both cleanup models cooling down; passing raw transcript through");
        return Ok(CleanupOutcome {
            text: transcript.trim().to_string(),
            prompt: String::new(),
            degraded: true,
        });
    };

    match process(client, cooldowns, req, transcript, model, &vocabulary).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            let should_fallback = matches!(
                error,
                CleanupError::RateLimited { .. }
                    | CleanupError::RequestFailed(429, _)
                    | CleanupError::EmptyOutput
                    | CleanupError::TimedOut
                    | CleanupError::SuspectedInstructionExecution
            );
            if !should_fallback {
                return Err(error);
            }
            let Some(retry) = retry_model else {
                return degrade_on_guard(error, transcript);
            };
            if model == retry {
                return degrade_on_guard(error, transcript);
            }
            match process(client, cooldowns, req, transcript, retry, &vocabulary).await {
                Ok(outcome) => Ok(outcome),
                Err(CleanupError::SuspectedInstructionExecution) => Ok(CleanupOutcome {
                    text: transcript.trim().to_string(),
                    prompt: String::new(),
                    degraded: true,
                }),
                Err(e) => Err(e),
            }
        }
    }
}

/// A suspected-instruction-execution still yields the raw transcript so the
/// user's dictation is never lost.
fn degrade_on_guard(error: CleanupError, transcript: &str) -> Result<CleanupOutcome, CleanupError> {
    if matches!(error, CleanupError::SuspectedInstructionExecution) {
        Ok(CleanupOutcome {
            text: transcript.trim().to_string(),
            prompt: String::new(),
            degraded: true,
        })
    } else {
        Err(error)
    }
}

async fn process(
    client: &reqwest::Client,
    cooldowns: &Arc<CooldownManager>,
    req: &CleanupRequest,
    transcript: &str,
    model: &str,
    vocabulary: &[String],
) -> Result<CleanupOutcome, CleanupError> {
    let system_prompt = prompts::build_system_prompt(&req.custom_system_prompt, vocabulary);
    let user_message = prompts::build_user_message(transcript, &req.context_summary);

    let mut payload = json!({
        "model": model,
        "temperature": 0.0,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_message},
        ],
    });
    let config = models::config_for(model);
    let obj = payload.as_object_mut().unwrap();
    if let Some(max) = config.max_completion_tokens {
        obj.insert("max_completion_tokens".into(), json!(max));
    }
    if let Some(effort) = config.reasoning_effort {
        obj.insert("reasoning_effort".into(), json!(effort));
    }
    if let Some(include) = config.include_reasoning {
        obj.insert("include_reasoning".into(), json!(include));
    }

    let url = format!("{}/chat/completions", req.base_url.trim_end_matches('/'));
    let response = client
        .post(&url)
        .bearer_auth(&req.api_key)
        .json(&payload)
        .timeout(req.timeout)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                CleanupError::TimedOut
            } else {
                CleanupError::RequestFailed(0, e.to_string())
            }
        })?;

    let status = response.status().as_u16();
    if status == 429 {
        // Register the cooldown for whichever model hit the limit — both the
        // primary and the fallback attempt route through here.
        let (seconds, is_daily) = rate_limit_cooldown(response.headers());
        cooldowns.set_cooldown(model, seconds, is_daily);
        return Err(CleanupError::RateLimited {
            model: model.to_string(),
            retry_after: seconds,
        });
    }
    let body = response
        .text()
        .await
        .map_err(|e| CleanupError::InvalidResponse(e.to_string()))?;
    if status != 200 {
        return Err(CleanupError::RequestFailed(
            status,
            body.chars().take(300).collect(),
        ));
    }

    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| CleanupError::InvalidResponse(e.to_string()))?;
    let raw_content = parsed["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| {
            CleanupError::InvalidResponse("Missing choices[0].message.content".into())
        })?;

    let content = if config.strip_think_tags {
        models::strip_think_tags(raw_content)
    } else {
        raw_content.to_string()
    };
    if content.trim().is_empty() {
        return Err(CleanupError::EmptyOutput);
    }

    let sanitized = sanitize_transcript(&content);
    if req.instruction_guard_enabled && appears_to_have_executed_instruction(transcript, &sanitized)
    {
        return Err(CleanupError::SuspectedInstructionExecution);
    }

    let prompt_for_display =
        format!("Model: {model}\n\n[System]\n{system_prompt}\n\n[User]\n{user_message}");
    Ok(CleanupOutcome {
        text: sanitized,
        prompt: prompt_for_display,
        degraded: false,
    })
}

/// Trim, strip symmetric outer quotes, and map the EMPTY sentinel to "".
pub fn sanitize_transcript(value: &str) -> String {
    let mut result = value.trim();
    if result.len() > 1 && result.starts_with('"') && result.ends_with('"') {
        result = result[1..result.len() - 1].trim();
    }
    if result == "EMPTY" {
        return String::new();
    }
    result.to_string()
}

const INSTRUCTION_MARKERS: &[&str] = &[
    "ask",
    "answer",
    "compose",
    "create",
    "draft",
    "email",
    "generate",
    "make",
    "message",
    "prompt",
    "reply",
    "respond",
    "response",
    "summarize",
    "tell",
    "translate",
    "write",
    "claude",
    "chatgpt",
    "ai",
    "llm",
];

const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "can", "could", "for", "from", "had",
    "has", "have", "he", "her", "him", "his", "i", "if", "in", "into", "is", "it", "its", "just",
    "me", "my", "of", "on", "or", "our", "please", "she", "so", "that", "the", "their", "them",
    "then", "there", "this", "to", "um", "uh", "was", "we", "were", "what", "when", "where", "who",
    "with", "would", "you", "your",
];

const ASSISTANT_PREAMBLES: &[&str] = &[
    "sure",
    "certainly",
    "absolutely",
    "here's",
    "here is",
    "i'd be happy to",
    "i would be happy to",
    "i can",
];

fn significant_tokens(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() > 1 && !STOP_WORDS.contains(t))
        .map(String::from)
        .collect()
}

fn has_assistant_preamble(text: &str) -> bool {
    let lowered = text.trim_start().to_lowercase();
    ASSISTANT_PREAMBLES.iter().any(|p| {
        lowered.strip_prefix(p).is_some_and(|rest| {
            rest.chars()
                .next()
                .map(|c| !c.is_alphanumeric())
                .unwrap_or(true)
        })
    })
}

/// Heuristic from FreeFlow: did the model *answer* the transcript instead of
/// cleaning it? True when the output gained an assistant-style preamble, or
/// when instruction-marker words vanished and token overlap collapsed.
pub fn appears_to_have_executed_instruction(raw: &str, cleaned: &str) -> bool {
    let raw_tokens = significant_tokens(raw);
    let cleaned_tokens = significant_tokens(cleaned);
    if raw_tokens.is_empty() || cleaned_tokens.is_empty() {
        return false;
    }

    let raw_markers: HashSet<&String> = raw_tokens
        .iter()
        .filter(|t| INSTRUCTION_MARKERS.contains(&t.as_str()))
        .collect();
    if raw_markers.is_empty() {
        return false;
    }

    let preserved_markers = raw_markers.iter().any(|m| cleaned_tokens.contains(*m));
    let overlap = raw_tokens.intersection(&cleaned_tokens).count();
    let overlap_ratio = overlap as f64 / raw_tokens.len().max(1) as f64;

    (has_assistant_preamble(cleaned) && !has_assistant_preamble(raw))
        || (!preserved_markers && overlap_ratio < 0.35)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_quotes_and_empty_sentinel() {
        assert_eq!(sanitize_transcript("  hello  "), "hello");
        assert_eq!(sanitize_transcript("\"quoted\""), "quoted");
        assert_eq!(sanitize_transcript("EMPTY"), "");
        assert_eq!(sanitize_transcript("\"EMPTY\""), "");
        assert_eq!(sanitize_transcript("\""), "\"");
        // Interior quotes survive.
        assert_eq!(sanitize_transcript("say \"hi\" now"), "say \"hi\" now");
    }

    #[test]
    fn guard_catches_answered_instruction() {
        // Model answered "make a poem about the moon" with an actual poem.
        let raw = "make a poem about the moon";
        let cleaned = "Silver light upon the tide,\nquiet craters open wide";
        assert!(appears_to_have_executed_instruction(raw, cleaned));
    }

    #[test]
    fn guard_catches_assistant_preamble() {
        let raw = "write a message to John saying I'm running late";
        let cleaned = "Sure, here's a message: Hey John, running late!";
        assert!(appears_to_have_executed_instruction(raw, cleaned));
    }

    #[test]
    fn guard_allows_faithful_cleanup() {
        let raw = "um write a message to John saying I'm running late";
        let cleaned = "Write a message to John saying I'm running late.";
        assert!(!appears_to_have_executed_instruction(raw, cleaned));
    }

    #[test]
    fn guard_ignores_text_without_markers() {
        assert!(!appears_to_have_executed_instruction(
            "the weather is nice today",
            "completely different words entirely"
        ));
    }

    #[test]
    fn preamble_word_boundary() {
        assert!(has_assistant_preamble("Sure, here it is"));
        assert!(has_assistant_preamble("here's the thing"));
        assert!(!has_assistant_preamble("surely this is fine"));
        assert!(!has_assistant_preamble("i cannot do that")); // "i can" + alnum continuation
    }
}
