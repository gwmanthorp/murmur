use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::cooldown::{rate_limit_cooldown, CooldownManager};
use super::models;

const SYSTEM_PROMPT: &str = "Answer the user's request directly and concisely. Return only the answer text that should be inserted at the cursor. Do not add a preamble, mention these instructions, or describe your reasoning.";

#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error("Model {model} rate-limited — retry in {retry_after:.0}s")]
    RateLimited { model: String, retry_after: f64 },
    #[error("Answer request failed with status {0}")]
    RequestFailed(u16),
    #[error("Invalid answer response: {0}")]
    InvalidResponse(String),
    #[error("The model returned an empty answer")]
    EmptyOutput,
    #[error("Answer request timed out")]
    TimedOut,
    #[error("Both answer models are temporarily unavailable")]
    ModelsCoolingDown,
}

pub struct ExecuteRequest {
    pub base_url: String,
    pub api_key: String,
    pub primary_model: String,
    pub fallback_model: String,
    pub timeout: Duration,
}

pub async fn answer_with_fallback(
    client: &reqwest::Client,
    cooldowns: &Arc<CooldownManager>,
    req: &ExecuteRequest,
    request_text: &str,
) -> Result<String, ExecuteError> {
    let primary = nonempty_model(&req.primary_model, models::DEFAULT_CLEANUP_MODEL);
    let fallback = nonempty_model(&req.fallback_model, models::DEFAULT_CLEANUP_FALLBACK_MODEL);
    let retry = (fallback != primary).then_some(fallback);
    let Some(model) = cooldowns.effective_primary(primary, retry) else {
        return Err(ExecuteError::ModelsCoolingDown);
    };

    match answer(client, cooldowns, req, request_text, model).await {
        Ok(text) => Ok(text),
        Err(error) if should_fallback(&error) => {
            let Some(retry_model) = retry.filter(|candidate| *candidate != model) else {
                return Err(error);
            };
            answer(client, cooldowns, req, request_text, retry_model).await
        }
        Err(error) => Err(error),
    }
}

fn nonempty_model<'a>(configured: &'a str, default: &'a str) -> &'a str {
    let configured = configured.trim();
    if configured.is_empty() {
        default
    } else {
        configured
    }
}

fn should_fallback(error: &ExecuteError) -> bool {
    matches!(
        error,
        ExecuteError::RateLimited { .. }
            | ExecuteError::RequestFailed(0 | 429 | 500..=599)
            | ExecuteError::EmptyOutput
            | ExecuteError::TimedOut
    )
}

async fn answer(
    client: &reqwest::Client,
    cooldowns: &Arc<CooldownManager>,
    req: &ExecuteRequest,
    request_text: &str,
    model: &str,
) -> Result<String, ExecuteError> {
    let config = models::config_for(model);
    let mut payload = json!({
        "model": model,
        "temperature": 0.0,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": request_text},
        ],
    });
    let object = payload.as_object_mut().expect("JSON payload is an object");
    if let Some(max) = config.max_completion_tokens {
        object.insert("max_completion_tokens".into(), json!(max.min(1024)));
    }
    if let Some(effort) = config.reasoning_effort {
        object.insert("reasoning_effort".into(), json!(effort));
    }
    if let Some(include) = config.include_reasoning {
        object.insert("include_reasoning".into(), json!(include));
    }

    let url = format!("{}/chat/completions", req.base_url.trim_end_matches('/'));
    let response = client
        .post(url)
        .bearer_auth(&req.api_key)
        .json(&payload)
        .timeout(req.timeout)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                ExecuteError::TimedOut
            } else {
                ExecuteError::RequestFailed(0)
            }
        })?;
    let status = response.status().as_u16();
    if status == 429 {
        let (seconds, is_daily) = rate_limit_cooldown(response.headers());
        cooldowns.set_cooldown(model, seconds, is_daily);
        return Err(ExecuteError::RateLimited {
            model: model.to_string(),
            retry_after: seconds,
        });
    }
    if status != 200 {
        return Err(ExecuteError::RequestFailed(status));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| ExecuteError::InvalidResponse(error.to_string()))?;
    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| ExecuteError::InvalidResponse("Missing answer content".into()))?;
    let text = if config.strip_think_tags {
        models::strip_think_tags(content)
    } else {
        content.trim().to_string()
    };
    if text.is_empty() {
        Err(ExecuteError::EmptyOutput)
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httptest::{matchers::*, responders::*, Expectation, Server};

    fn request(server: &Server) -> ExecuteRequest {
        ExecuteRequest {
            base_url: server.url_str("/"),
            api_key: "test-key".into(),
            primary_model: models::DEFAULT_CLEANUP_MODEL.into(),
            fallback_model: models::DEFAULT_CLEANUP_FALLBACK_MODEL.into(),
            timeout: Duration::from_secs(2),
        }
    }

    fn response(text: &str) -> serde_json::Value {
        json!({"choices": [{"message": {"content": text}}]})
    }

    #[tokio::test]
    async fn returns_direct_answer() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/chat/completions"))
                .respond_with(json_encoded(response("520"))),
        );
        let result = answer_with_fallback(
            &reqwest::Client::new(),
            &Arc::new(CooldownManager::new(None)),
            &request(&server),
            "what is 10 times 52",
        )
        .await
        .unwrap();
        assert_eq!(result, "520");
    }

    #[tokio::test]
    async fn retries_transient_failure_and_strips_reasoning() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/chat/completions"))
                .times(2)
                .respond_with(cycle![
                    status_code(500),
                    json_encoded(response("<think>math</think>520")),
                ]),
        );
        let result = answer_with_fallback(
            &reqwest::Client::new(),
            &Arc::new(CooldownManager::new(None)),
            &request(&server),
            "calculate it",
        )
        .await
        .unwrap();
        assert_eq!(result, "520");
    }

    #[tokio::test]
    async fn empty_primary_uses_fallback() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/chat/completions"))
                .times(2)
                .respond_with(cycle![
                    json_encoded(response("  ")),
                    json_encoded(response("Fallback answer")),
                ]),
        );
        let result = answer_with_fallback(
            &reqwest::Client::new(),
            &Arc::new(CooldownManager::new(None)),
            &request(&server),
            "answer me",
        )
        .await
        .unwrap();
        assert_eq!(result, "Fallback answer");
    }

    #[tokio::test]
    async fn timeout_uses_fallback() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/chat/completions"))
                .times(2)
                .respond_with(cycle![
                    delay_and_then(
                        Duration::from_millis(80),
                        json_encoded(response("Too late")),
                    ),
                    json_encoded(response("Fallback answer")),
                ]),
        );
        let mut req = request(&server);
        req.timeout = Duration::from_millis(20);
        let result = answer_with_fallback(
            &reqwest::Client::new(),
            &Arc::new(CooldownManager::new(None)),
            &req,
            "answer me",
        )
        .await
        .unwrap();
        assert_eq!(result, "Fallback answer");
    }

    #[tokio::test]
    async fn both_models_failing_returns_an_error() {
        let server = Server::run();
        server.expect(
            Expectation::matching(request::method_path("POST", "/chat/completions"))
                .times(2)
                .respond_with(status_code(500)),
        );
        let error = answer_with_fallback(
            &reqwest::Client::new(),
            &Arc::new(CooldownManager::new(None)),
            &request(&server),
            "answer me",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ExecuteError::RequestFailed(500)));
    }
}
