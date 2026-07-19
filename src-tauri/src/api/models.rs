//! Per-model request parameters (port of FreeFlow's ModelConfiguration).

pub const DEFAULT_BASE_URL: &str = "https://api.groq.com/openai/v1";
pub const DEFAULT_TRANSCRIPTION_MODEL: &str = "whisper-large-v3";
pub const DEFAULT_CLEANUP_MODEL: &str = "openai/gpt-oss-20b";
pub const DEFAULT_CLEANUP_FALLBACK_MODEL: &str = "qwen/qwen3.6-27b";

#[allow(dead_code)] // exposed by the deferred model-picker UI
pub const TRANSCRIPTION_MODELS: &[&str] = &["whisper-large-v3", "whisper-large-v3-turbo"];
#[allow(dead_code)] // exposed by the deferred model-picker UI
pub const LLM_MODELS: &[&str] = &[
    "openai/gpt-oss-20b",
    "openai/gpt-oss-120b",
    "qwen/qwen3.6-27b",
    "llama-3.3-70b-versatile",
    "llama-3.1-8b-instant",
];

#[derive(Debug, Clone, Default)]
pub struct ModelConfig {
    pub max_completion_tokens: Option<u32>,
    pub reasoning_effort: Option<&'static str>,
    pub include_reasoning: Option<bool>,
    pub strip_think_tags: bool,
}

pub fn config_for(model: &str) -> ModelConfig {
    let mut clean = model.trim().to_lowercase();
    // Normalize providerless aliases.
    clean = match clean.as_str() {
        "qwen3-32b" => "qwen/qwen3-32b".into(),
        "qwen3.6-27b" => "qwen/qwen3.6-27b".into(),
        "gpt-oss-20b" => "openai/gpt-oss-20b".into(),
        "gpt-oss-120b" => "openai/gpt-oss-120b".into(),
        _ => clean,
    };
    match clean.as_str() {
        "openai/gpt-oss-20b" => ModelConfig {
            max_completion_tokens: Some(4096),
            reasoning_effort: Some("low"),
            include_reasoning: Some(false),
            strip_think_tags: false,
        },
        "qwen/qwen3.6-27b" => ModelConfig {
            max_completion_tokens: None,
            reasoning_effort: Some("none"),
            include_reasoning: Some(false),
            strip_think_tags: true,
        },
        "qwen/qwen3-32b" => ModelConfig {
            strip_think_tags: true,
            ..Default::default()
        },
        _ => ModelConfig::default(),
    }
}

/// Remove `<think>…</think>` blocks emitted by reasoning models.
pub fn strip_think_tags(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end_rel) => rest = &rest[start + end_rel + "</think>".len()..],
            None => {
                // Unclosed think tag: drop everything after it.
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpt_oss_config() {
        let c = config_for("openai/gpt-oss-20b");
        assert_eq!(c.max_completion_tokens, Some(4096));
        assert_eq!(c.reasoning_effort, Some("low"));
        assert_eq!(c.include_reasoning, Some(false));
        assert!(!c.strip_think_tags);
    }

    #[test]
    fn qwen_alias_and_think_strip() {
        assert!(config_for("qwen3.6-27b").strip_think_tags);
        assert_eq!(
            config_for("QWEN/qwen3.6-27b").reasoning_effort,
            Some("none")
        );
    }

    #[test]
    fn strip_think() {
        assert_eq!(
            strip_think_tags("<think>reasoning here</think>\nHello world"),
            "Hello world"
        );
        assert_eq!(strip_think_tags("no tags"), "no tags");
        assert_eq!(strip_think_tags("keep <think>drop"), "keep");
        assert_eq!(
            strip_think_tags("a<think>x</think>b<think>y</think>c"),
            "abc"
        );
    }
}
