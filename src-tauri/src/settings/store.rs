use std::path::PathBuf;

use std::os::windows::ffi::OsStrExt;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

use super::model::Settings;

pub fn config_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("murmur")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn state_path() -> PathBuf {
    config_dir().join("state.json")
}

pub fn load() -> Settings {
    let path = settings_path();
    match std::fs::read_to_string(&path) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
            tracing::error!("settings.json is corrupt ({e}); using defaults");
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

/// Atomic write: temp file in the same directory, then rename over the target.
pub fn save(settings: &Settings) -> std::io::Result<()> {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(settings)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    let source: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(std::io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_serde() {
        let s = Settings::default();
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.base_url, s.base_url);
        assert_eq!(back.hold_shortcut, s.hold_shortcut);
        // Unknown/missing fields fall back to defaults (forward compat).
        let sparse: Settings = serde_json::from_str(r#"{"baseUrl": "http://x"}"#).unwrap();
        assert_eq!(sparse.base_url, "http://x");
        assert!(sparse.preserve_clipboard);
    }
}
