//! Per-model rate-limit circuit breaker (port of FreeFlow's
//! LLMCooldownManager). Minute-level cooldowns live in memory; daily limits
//! (>= 1h, or an exhausted requests-per-day quota) persist to state.json so
//! they survive restarts.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAILY_LIMIT_THRESHOLD_SECS: f64 = 3600.0;
/// Fallback cooldown when a 429 carries no parseable timing header.
const DEFAULT_REPROBE_COOLDOWN_SECS: f64 = 60.0;

#[derive(Default)]
pub struct CooldownManager {
    /// model -> unix expiry seconds. Daily entries are also mirrored to disk.
    in_memory: Mutex<HashMap<String, f64>>,
    persisted: Mutex<HashMap<String, f64>>,
    state_path: Option<std::path::PathBuf>,
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
}

impl CooldownManager {
    pub fn new(state_path: Option<std::path::PathBuf>) -> Self {
        let persisted = state_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<HashMap<String, f64>>(&s).ok())
            .unwrap_or_default();
        Self {
            in_memory: Mutex::new(HashMap::new()),
            persisted: Mutex::new(persisted),
            state_path,
        }
    }

    pub fn is_in_cooldown(&self, model: &str) -> bool {
        let now = now_unix();
        {
            let mut mem = self.in_memory.lock().unwrap();
            if let Some(&until) = mem.get(model) {
                if now < until {
                    return true;
                }
                mem.remove(model);
            }
        }
        let mut persisted = self.persisted.lock().unwrap();
        if let Some(&until) = persisted.get(model) {
            if now < until {
                return true;
            }
            persisted.remove(model);
            self.save(&persisted);
        }
        false
    }

    pub fn set_cooldown(&self, model: &str, retry_after_secs: f64, persist: bool) {
        let expiry = now_unix() + retry_after_secs;
        if persist || retry_after_secs >= DAILY_LIMIT_THRESHOLD_SECS {
            let mut persisted = self.persisted.lock().unwrap();
            persisted.insert(model.to_string(), expiry);
            self.save(&persisted);
        } else {
            self.in_memory.lock().unwrap().insert(model.to_string(), expiry);
        }
    }

    /// The primary if it isn't cooling; else the fallback if it isn't; else
    /// None so the caller can skip a doomed request entirely.
    pub fn effective_primary<'a>(
        &self,
        primary: &'a str,
        fallback: Option<&'a str>,
    ) -> Option<&'a str> {
        if !self.is_in_cooldown(primary) {
            return Some(primary);
        }
        fallback.filter(|f| !self.is_in_cooldown(f))
    }

    /// Cooldown expiry (unix seconds) for a model, for the settings UI.
    pub fn expiry(&self, model: &str) -> Option<f64> {
        let now = now_unix();
        let mem = self.in_memory.lock().unwrap();
        let persisted = self.persisted.lock().unwrap();
        mem.get(model)
            .or_else(|| persisted.get(model))
            .copied()
            .filter(|&t| t > now)
    }

    fn save(&self, persisted: &HashMap<String, f64>) {
        if let Some(path) = &self.state_path {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_string_pretty(persisted) {
                let _ = std::fs::write(path, json);
            }
        }
    }
}

/// From a 429 response's headers: (cooldown seconds, is_daily).
/// Priority (FreeFlow): exhausted RPD quota (remaining-requests <= 0 with the
/// reset-requests duration) → retry-after → reset-tokens → short re-probe.
pub fn rate_limit_cooldown(headers: &reqwest::header::HeaderMap) -> (f64, bool) {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };

    let remaining_requests = get("x-ratelimit-remaining-requests").and_then(|v| v.parse::<f64>().ok());
    if let Some(remaining) = remaining_requests {
        if remaining <= 0.0 {
            if let Some(daily_reset) = get("x-ratelimit-reset-requests").and_then(parse_groq_duration)
            {
                return (daily_reset, true);
            }
        }
    }
    if let Some(v) = get("retry-after").and_then(parse_groq_duration) {
        return (v, false);
    }
    if let Some(v) = get("x-ratelimit-reset-tokens").and_then(parse_groq_duration) {
        return (v, false);
    }
    (DEFAULT_REPROBE_COOLDOWN_SECS, false)
}

/// Groq duration grammar: bare seconds ("2", "7.66"), suffixed units
/// ("7.66s", "120ms"), compound forms ("2m59.56s", "1h0m0s"). Rejects
/// negative/NaN/trailing-unitless input.
pub fn parse_groq_duration(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(seconds) = trimmed.parse::<f64>() {
        return (seconds.is_finite() && seconds >= 0.0).then_some(seconds);
    }
    let mut total = 0.0f64;
    let mut number_buffer = String::new();
    let mut matched_any_unit = false;
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_digit() || c == '.' {
            number_buffer.push(c);
            i += 1;
            continue;
        }
        let number: f64 = number_buffer.parse().ok()?;
        number_buffer.clear();
        if trimmed[i..].starts_with("ms") {
            total += number / 1000.0;
            i += 2;
        } else if c == 'h' {
            total += number * 3600.0;
            i += 1;
        } else if c == 'm' {
            total += number * 60.0;
            i += 1;
        } else if c == 's' {
            total += number;
            i += 1;
        } else {
            return None;
        }
        matched_any_unit = true;
    }
    if !number_buffer.is_empty() || !matched_any_unit {
        return None;
    }
    (total.is_finite() && total >= 0.0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_groq_duration_grammar() {
        assert_eq!(parse_groq_duration("2"), Some(2.0));
        assert_eq!(parse_groq_duration("7.66"), Some(7.66));
        assert_eq!(parse_groq_duration("7.66s"), Some(7.66));
        assert_eq!(parse_groq_duration("120ms"), Some(0.12));
        assert_eq!(parse_groq_duration("2m59.56s"), Some(179.56));
        assert_eq!(parse_groq_duration("1h0m0s"), Some(3600.0));
        assert_eq!(parse_groq_duration("1h2m3.5s"), Some(3723.5));
        assert_eq!(parse_groq_duration(""), None);
        assert_eq!(parse_groq_duration("-3"), None);
        assert_eq!(parse_groq_duration("nan"), None);
        assert_eq!(parse_groq_duration("1h30"), None);
        assert_eq!(parse_groq_duration("5x"), None);
    }

    #[test]
    fn cooldown_lifecycle() {
        let mgr = CooldownManager::new(None);
        assert!(!mgr.is_in_cooldown("m1"));
        mgr.set_cooldown("m1", 30.0, false);
        assert!(mgr.is_in_cooldown("m1"));
        assert_eq!(mgr.effective_primary("m1", Some("m2")), Some("m2"));
        mgr.set_cooldown("m2", 30.0, false);
        assert_eq!(mgr.effective_primary("m1", Some("m2")), None);
        assert!(mgr.expiry("m1").is_some());
    }

    #[test]
    fn expired_cooldown_clears() {
        let mgr = CooldownManager::new(None);
        mgr.set_cooldown("m1", -1.0, false); // already expired
        assert!(!mgr.is_in_cooldown("m1"));
    }

    #[test]
    fn daily_persists_to_disk() {
        let dir = std::env::temp_dir().join("murmur-test-cooldown");
        let path = dir.join("state.json");
        let _ = std::fs::remove_file(&path);
        {
            let mgr = CooldownManager::new(Some(path.clone()));
            mgr.set_cooldown("daily-model", 7200.0, false);
        }
        let mgr2 = CooldownManager::new(Some(path.clone()));
        assert!(mgr2.is_in_cooldown("daily-model"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn header_priority_rpd_first() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-ratelimit-remaining-requests", "0".parse().unwrap());
        headers.insert("x-ratelimit-reset-requests", "10m".parse().unwrap());
        headers.insert("retry-after", "5".parse().unwrap());
        let (secs, daily) = rate_limit_cooldown(&headers);
        assert_eq!(secs, 600.0);
        assert!(daily);

        let mut headers2 = reqwest::header::HeaderMap::new();
        headers2.insert("retry-after", "5".parse().unwrap());
        assert_eq!(rate_limit_cooldown(&headers2), (5.0, false));

        let empty = reqwest::header::HeaderMap::new();
        assert_eq!(rate_limit_cooldown(&empty), (60.0, false));
    }
}
