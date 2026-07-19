//! Integration tests for the cleanup fallback chain against a local mock
//! HTTP server (httptest).

use std::sync::Arc;
use std::time::Duration;

use httptest::{matchers::*, responders::*, Expectation, Server};
use serde_json::json;

use super::cleanup::{clean_with_fallback, CleanupError, CleanupRequest};
use super::cooldown::CooldownManager;

fn request(base_url: String) -> CleanupRequest {
    CleanupRequest {
        base_url,
        api_key: "test-key".into(),
        primary_model: "openai/gpt-oss-20b".into(),
        fallback_model: "qwen/qwen3.6-27b".into(),
        custom_system_prompt: String::new(),
        custom_vocabulary: String::new(),
        context_summary: String::new(),
        instruction_guard_enabled: true,
        timeout: Duration::from_secs(5),
    }
}

fn chat_response(content: &str) -> serde_json::Value {
    json!({"choices": [{"message": {"role": "assistant", "content": content}}]})
}

#[tokio::test]
async fn happy_path_returns_cleaned_text() {
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("POST", "/chat/completions"))
            .respond_with(json_encoded(chat_response("Hello world."))),
    );
    let client = reqwest::Client::new();
    let cooldowns = Arc::new(CooldownManager::new(None));
    let req = request(server.url_str("/"));
    let out = clean_with_fallback(&client, &cooldowns, &req, "um hello world")
        .await
        .unwrap();
    assert_eq!(out.text, "Hello world.");
    assert!(out.prompt.contains("openai/gpt-oss-20b"));
}

#[tokio::test]
async fn empty_sentinel_yields_empty_text() {
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("POST", "/chat/completions"))
            .respond_with(json_encoded(chat_response("EMPTY"))),
    );
    let client = reqwest::Client::new();
    let cooldowns = Arc::new(CooldownManager::new(None));
    let req = request(server.url_str("/"));
    let out = clean_with_fallback(&client, &cooldowns, &req, "uh")
        .await
        .unwrap();
    assert_eq!(out.text, "");
}

#[tokio::test]
async fn think_tags_stripped_for_qwen() {
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("POST", "/chat/completions")).respond_with(
            json_encoded(chat_response(
                "<think>the user wants cleanup</think>Hello there.",
            )),
        ),
    );
    let client = reqwest::Client::new();
    let cooldowns = Arc::new(CooldownManager::new(None));
    let mut req = request(server.url_str("/"));
    req.primary_model = "qwen/qwen3.6-27b".into();
    req.fallback_model = "qwen/qwen3.6-27b".into();
    let out = clean_with_fallback(&client, &cooldowns, &req, "hello there")
        .await
        .unwrap();
    assert_eq!(out.text, "Hello there.");
}

#[tokio::test]
async fn rate_limit_falls_back_and_registers_cooldown() {
    let server = Server::run();
    // First call (primary) → 429 with retry-after; second call (fallback) → 200.
    server.expect(
        Expectation::matching(request::method_path("POST", "/chat/completions"))
            .times(2)
            .respond_with(cycle![
                status_code(429).append_header("retry-after", "30"),
                json_encoded(chat_response("Cleaned by fallback.")),
            ]),
    );
    let client = reqwest::Client::new();
    let cooldowns = Arc::new(CooldownManager::new(None));
    let req = request(server.url_str("/"));
    let out = clean_with_fallback(&client, &cooldowns, &req, "some words here")
        .await
        .unwrap();
    assert_eq!(out.text, "Cleaned by fallback.");
    // Primary is now cooling; next call goes straight to the fallback.
    assert!(cooldowns.is_in_cooldown("openai/gpt-oss-20b"));
    assert!(!cooldowns.is_in_cooldown("qwen/qwen3.6-27b"));
}

#[tokio::test]
async fn both_models_cooling_passes_raw_through() {
    let client = reqwest::Client::new();
    let cooldowns = Arc::new(CooldownManager::new(None));
    cooldowns.set_cooldown("openai/gpt-oss-20b", 300.0, false);
    cooldowns.set_cooldown("qwen/qwen3.6-27b", 300.0, false);
    // No server needed: the breaker short-circuits before any request.
    let req = request("http://127.0.0.1:9".into());
    let out = clean_with_fallback(&client, &cooldowns, &req, "  raw words  ")
        .await
        .unwrap();
    assert_eq!(out.text, "raw words");
    assert!(out.prompt.is_empty());
}

#[tokio::test]
async fn empty_output_retries_on_fallback() {
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("POST", "/chat/completions"))
            .times(2)
            .respond_with(cycle![
                json_encoded(chat_response("   ")),
                json_encoded(chat_response("Fallback fixed it.")),
            ]),
    );
    let client = reqwest::Client::new();
    let cooldowns = Arc::new(CooldownManager::new(None));
    let req = request(server.url_str("/"));
    let out = clean_with_fallback(&client, &cooldowns, &req, "test words")
        .await
        .unwrap();
    assert_eq!(out.text, "Fallback fixed it.");
}

#[tokio::test]
async fn timeout_retries_on_fallback() {
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("POST", "/chat/completions"))
            .times(2)
            .respond_with(cycle![
                delay_and_then(
                    Duration::from_millis(80),
                    json_encoded(chat_response("Too late.")),
                ),
                json_encoded(chat_response("Fallback after timeout.")),
            ]),
    );
    let client = reqwest::Client::new();
    let cooldowns = Arc::new(CooldownManager::new(None));
    let mut req = request(server.url_str("/"));
    req.timeout = Duration::from_millis(20);
    let out = clean_with_fallback(&client, &cooldowns, &req, "fallback after timeout")
        .await
        .unwrap();
    assert_eq!(out.text, "Fallback after timeout.");
}

#[tokio::test]
async fn server_error_bubbles_up() {
    let server = Server::run();
    server.expect(
        Expectation::matching(request::method_path("POST", "/chat/completions"))
            .respond_with(status_code(500).body("boom")),
    );
    let client = reqwest::Client::new();
    let cooldowns = Arc::new(CooldownManager::new(None));
    let req = request(server.url_str("/"));
    let err = clean_with_fallback(&client, &cooldowns, &req, "test words")
        .await
        .unwrap_err();
    assert!(matches!(err, CleanupError::RequestFailed(500, _)));
}

#[tokio::test]
async fn guard_falls_back_then_degrades_to_raw() {
    let server = Server::run();
    // Both models "answer" the instruction → guard trips twice → raw passthrough.
    server.expect(
        Expectation::matching(request::method_path("POST", "/chat/completions"))
            .times(2)
            .respond_with(json_encoded(chat_response(
                "Sure, here's a poem: Roses are red...",
            ))),
    );
    let client = reqwest::Client::new();
    let cooldowns = Arc::new(CooldownManager::new(None));
    let req = request(server.url_str("/"));
    let out = clean_with_fallback(&client, &cooldowns, &req, "write a poem about roses")
        .await
        .unwrap();
    assert_eq!(out.text, "write a poem about roses");
}
