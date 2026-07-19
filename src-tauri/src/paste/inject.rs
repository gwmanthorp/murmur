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

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

/// Wait until the trigger keys and all generic modifiers are physically
/// released, so a still-held Right Ctrl doesn't corrupt the synthetic Ctrl+V.
/// Polls up to 24 × 25ms (mirrors FreeFlow), then a 30ms settle delay.
pub fn wait_for_keys_released(binding_vks: &[u16]) {
    let mut watch: Vec<u16> = binding_vks.to_vec();
    watch.extend_from_slice(&[VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]);
    for _ in 0..24 {
        if watch.iter().all(|&vk| !key_down(vk)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(30));
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

fn send(inputs: &[INPUT]) -> bool {
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        tracing::error!("SendInput delivered {sent}/{} events", inputs.len());
    }
    sent == inputs.len() as u32
}

pub fn send_ctrl_v() -> bool {
    send(&[
        key_input(VK_CONTROL, false),
        key_input(VK_V, false),
        key_input(VK_V, true),
        key_input(VK_CONTROL, true),
    ])
}
