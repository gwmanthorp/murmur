pub mod clipboard;
pub mod inject;

use std::time::Duration;

pub const MODIFIER_RELEASE_ERROR: &str = "Release modifier keys, then use Paste Again.";

/// Paste `text` at the cursor of the focused app (port of FreeFlow's
/// pasteAtCursor pipeline):
/// 1. wait for physical shortcut and modifier release,
/// 2. snapshot the clipboard (if preserving),
/// 3. write the text (with a trailing space after sentence punctuation so the
///    next dictation doesn't jam against it),
/// 4. synthetic Ctrl+V,
/// 5. restore the snapshot after ~1s — only if the clipboard still holds our
///    text (i.e. the user didn't copy something else meanwhile).
///
/// Blocking; call from a blocking-ok thread.
pub fn paste_text(text: &str, binding_vks: &[u16], preserve_clipboard: bool) -> Result<(), String> {
    if !inject::wait_for_keys_released(binding_vks) {
        return Err(MODIFIER_RELEASE_ERROR.into());
    }
    let to_paste = text_for_paste(text);

    let snapshot = if preserve_clipboard {
        clipboard::snapshot()
    } else {
        None
    };

    if !clipboard::set_text(&to_paste) {
        return Err("Could not write to the clipboard".into());
    }
    let seq_after_write = clipboard::sequence_number();

    let injected = inject::send_ctrl_v();

    if let Some(snapshot) = snapshot {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            if should_restore(clipboard::sequence_number(), seq_after_write) {
                if !clipboard::restore(&snapshot) {
                    tracing::warn!("clipboard restore failed");
                }
            } else {
                tracing::debug!("clipboard changed since paste; skipping restore");
            }
        });
    }
    if injected {
        Ok(())
    } else {
        Err("Windows could not send Ctrl+V".into())
    }
}

fn text_for_paste(text: &str) -> String {
    let mut output = text.to_string();
    if output.ends_with(['.', '!', '?']) {
        output.push(' ');
    }
    output
}

fn should_restore(current_sequence: u32, our_sequence: u32) -> bool {
    current_sequence == our_sequence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentence_punctuation_gets_a_trailing_space() {
        assert_eq!(text_for_paste("Hello."), "Hello. ");
        assert_eq!(text_for_paste("Really?"), "Really? ");
        assert_eq!(text_for_paste("fragment"), "fragment");
    }

    #[test]
    fn clipboard_restore_requires_unchanged_sequence() {
        assert!(should_restore(42, 42));
        assert!(!should_restore(43, 42));
    }

    #[test]
    fn modifier_timeout_message_is_actionable() {
        assert_eq!(
            MODIFIER_RELEASE_ERROR,
            "Release modifier keys, then use Paste Again."
        );
    }
}
