//! Synthetic keystrokes via SendInput, marked with the hook sentinel so our
//! own keyboard hook passes them through untouched.

use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, VIRTUAL_KEY,
};

use crate::hotkeys::hook::INJECT_SENTINEL;

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_LWIN: u16 = 0x5B;
const VK_RWIN: u16 = 0x5C;
const VK_V: u16 = 0x56;
const VK_LCONTROL: u16 = 0xA2;
const VK_RCONTROL: u16 = 0xA3;
const RELEASE_POLL_ATTEMPTS: usize = 80;

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

/// Wait until the trigger keys and all generic modifiers are physically
/// released, so a still-held Right Ctrl doesn't corrupt the synthetic Ctrl+V.
/// Polls for up to two seconds, then applies a 30ms settle delay.
pub fn wait_for_keys_released(binding_vks: &[u16]) -> bool {
    wait_for_keys_released_with(binding_vks, key_down, std::thread::sleep)
}

fn wait_for_keys_released_with(
    binding_vks: &[u16],
    is_down: impl Fn(u16) -> bool,
    sleep: impl Fn(Duration),
) -> bool {
    let mut watch: Vec<u16> = binding_vks.to_vec();
    watch.extend_from_slice(&[
        VK_SHIFT,
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_MENU,
        VK_LWIN,
        VK_RWIN,
    ]);
    watch.sort_unstable();
    watch.dedup();
    for attempt in 0..=RELEASE_POLL_ATTEMPTS {
        if watch.iter().all(|&vk| !is_down(vk)) {
            sleep(Duration::from_millis(30));
            return true;
        }
        if attempt < RELEASE_POLL_ATTEMPTS {
            sleep(Duration::from_millis(25));
        }
    }
    false
}

fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: INJECT_SENTINEL,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> u32 {
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        tracing::error!("SendInput delivered {sent}/{} events", inputs.len());
    }
    sent
}

pub fn send_ctrl_v() -> bool {
    send_ctrl_v_with(send)
}

fn send_ctrl_v_with(mut sender: impl FnMut(&[INPUT]) -> u32) -> bool {
    let inputs = [
        key_input(VK_LCONTROL, false),
        key_input(VK_V, false),
        key_input(VK_V, true),
        key_input(VK_LCONTROL, true),
    ];
    let sent = sender(&inputs).min(inputs.len() as u32);
    if sent == inputs.len() as u32 {
        return true;
    }

    if sent == 2 {
        let _ = sender(&[key_input(VK_V, true)]);
    }
    if sent >= 1 {
        let _ = sender(&[key_input(VK_LCONTROL, true)]);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn event(input: &INPUT) -> (u16, bool, usize) {
        let keyboard = unsafe { input.Anonymous.ki };
        (
            keyboard.wVk.0,
            keyboard.dwFlags.contains(KEYEVENTF_KEYUP),
            keyboard.dwExtraInfo,
        )
    }

    #[test]
    fn full_paste_sequence_uses_left_control_and_sentinel() {
        let calls = RefCell::new(Vec::<Vec<(u16, bool, usize)>>::new());
        assert!(send_ctrl_v_with(|inputs| {
            calls.borrow_mut().push(inputs.iter().map(event).collect());
            inputs.len() as u32
        }));
        assert_eq!(
            calls.borrow()[0],
            vec![
                (VK_LCONTROL, false, INJECT_SENTINEL),
                (VK_V, false, INJECT_SENTINEL),
                (VK_V, true, INJECT_SENTINEL),
                (VK_LCONTROL, true, INJECT_SENTINEL),
            ]
        );
    }

    #[test]
    fn partial_prefixes_release_only_murmur_owned_keys() {
        for delivered in 0..4u32 {
            let calls = RefCell::new(Vec::<Vec<(u16, bool, usize)>>::new());
            let first = Cell::new(true);
            assert!(!send_ctrl_v_with(|inputs| {
                calls.borrow_mut().push(inputs.iter().map(event).collect());
                if first.replace(false) {
                    delivered
                } else {
                    inputs.len() as u32
                }
            }));
            let calls = calls.into_inner();
            assert!(calls.iter().flatten().all(|(vk, _, _)| *vk != VK_RCONTROL));
            assert_eq!(
                calls
                    .iter()
                    .skip(1)
                    .flatten()
                    .last()
                    .map(|event| (event.0, event.1)),
                (delivered >= 1).then_some((VK_LCONTROL, true))
            );
            if delivered == 2 {
                assert_eq!((calls[1][0].0, calls[1][0].1), (VK_V, true));
            }
        }
    }

    #[test]
    fn modifier_timeout_returns_false_without_settle_delay() {
        let sleeps = Cell::new(0usize);
        assert!(!wait_for_keys_released_with(
            &[],
            |vk| vk == VK_CONTROL,
            |_| sleeps.set(sleeps.get() + 1),
        ));
        assert_eq!(sleeps.get(), RELEASE_POLL_ATTEMPTS);
    }

    #[test]
    fn released_keys_receive_settle_delay() {
        let durations = RefCell::new(Vec::new());
        assert!(wait_for_keys_released_with(
            &[VK_RCONTROL],
            |_| false,
            |delay| durations.borrow_mut().push(delay),
        ));
        assert_eq!(durations.into_inner(), vec![Duration::from_millis(30)]);
    }
}
