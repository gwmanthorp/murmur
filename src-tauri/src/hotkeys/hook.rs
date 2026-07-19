//! Low-level keyboard hook (WH_KEYBOARD_LL).
//!
//! The callback must stay tiny: Windows silently removes hooks whose callback
//! exceeds the LowLevelHooksTimeout (~300ms). It only filters injected events,
//! tracks a modifier bitmask, pushes the event to the consumer thread, and
//! decides whether to swallow the key. All real logic lives in
//! `state_machine.rs` on the consumer thread.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;
use crossbeam_channel::Sender;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use super::bindings::{is_modifier, modifier_bit, VK_ESCAPE};
use super::state_machine::EngineInput;

/// Marker placed in `dwExtraInfo` of our own SendInput events so the hook
/// never swallows or reacts to Murmur's synthetic Ctrl+V / Enter.
pub const INJECT_SENTINEL: usize = 0x4D52_4D52; // "MRMR"

/// What the hook callback needs to know, swapped atomically by the consumer.
#[derive(Default, Clone)]
pub struct HookConfig {
    /// (vk, required_modifier_mask): swallow keydown of `vk` when the current
    /// modifier mask equals the required mask; keyups of `vk` are always
    /// swallowed so apps never see a stray up for a down they never got.
    pub swallow_rules: Vec<(u16, u8)>,
    /// Swallow Esc keydown (active toggle session or in-flight transcription).
    pub swallow_esc: bool,
    /// Shortcut capture in progress in the settings UI: pass everything
    /// through and send nothing.
    pub suspended: bool,
}

static CONFIG: OnceLock<ArcSwap<HookConfig>> = OnceLock::new();
static TX: OnceLock<Sender<EngineInput>> = OnceLock::new();
static MOD_MASK: AtomicU8 = AtomicU8::new(0);

pub fn store_config(cfg: HookConfig) {
    CONFIG
        .get_or_init(|| ArcSwap::from_pointee(HookConfig::default()))
        .store(Arc::new(cfg));
}

unsafe extern "system" fn ll_keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    let injected = (kb.flags.0 & LLKHF_INJECTED.0) != 0 || kb.dwExtraInfo == INJECT_SENTINEL;
    if injected {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let vk = kb.vkCode as u16;
    let msg = wparam.0 as u32;
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;

    if is_modifier(vk) {
        let bit = modifier_bit(vk);
        if down {
            MOD_MASK.fetch_or(bit, Ordering::Relaxed);
        } else {
            MOD_MASK.fetch_and(!bit, Ordering::Relaxed);
        }
    }

    let Some(cfg_cell) = CONFIG.get() else {
        return CallNextHookEx(None, code, wparam, lparam);
    };
    let cfg = cfg_cell.load();
    if cfg.suspended {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    if let Some(tx) = TX.get() {
        let _ = tx.try_send(EngineInput::Key { vk, down });
    }

    let mask = MOD_MASK.load(Ordering::Relaxed);
    let swallow = (vk == VK_ESCAPE && cfg.swallow_esc && down)
        || cfg
            .swallow_rules
            .iter()
            .any(|&(rvk, rmask)| rvk == vk && (!down || mask == rmask));
    if swallow {
        return LRESULT(1);
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// Install the hook on a dedicated thread with its own message pump.
pub fn spawn_hook(tx: Sender<EngineInput>) {
    let _ = TX.set(tx);
    CONFIG.get_or_init(|| ArcSwap::from_pointee(HookConfig::default()));

    std::thread::Builder::new()
        .name("murmur-kbd-hook".into())
        .spawn(|| unsafe {
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), None, 0) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!("failed to install keyboard hook: {e}");
                    return;
                }
            };
            tracing::info!("keyboard hook installed ({hook:?})");
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .expect("failed to spawn hook thread");
}
