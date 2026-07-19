//! Low-level keyboard hook (WH_KEYBOARD_LL).
//!
//! The callback must stay tiny: Windows silently removes hooks whose callback
//! exceeds the LowLevelHooksTimeout (~300ms). It only filters injected events,
//! tracks a modifier bitmask, pushes the event to the consumer thread, and
//! decides whether to swallow the key. All real logic lives in
//! `state_machine.rs` on the consumer thread.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;
use crossbeam_channel::Sender;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KillTimer, SetTimer, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_SYSKEYDOWN, WM_TIMER,
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
static SWALLOWED: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

fn swallowed_slot(vk: u16) -> Option<(&'static AtomicU64, u64)> {
    (vk < 256).then(|| {
        let index = vk as usize;
        (&SWALLOWED[index / 64], 1u64 << (index % 64))
    })
}

fn remember_swallowed(vk: u16) {
    if let Some((slot, bit)) = swallowed_slot(vk) {
        slot.fetch_or(bit, Ordering::Relaxed);
    }
}

fn take_swallowed(vk: u16) -> bool {
    swallowed_slot(vk).is_some_and(|(slot, bit)| {
        let previous = slot.fetch_and(!bit, Ordering::Relaxed);
        previous & bit != 0
    })
}

fn matches_keydown(cfg: &HookConfig, vk: u16, mask: u8) -> bool {
    (vk == VK_ESCAPE && cfg.swallow_esc)
        || cfg
            .swallow_rules
            .iter()
            .any(|&(rule_vk, required_mask)| rule_vk == vk && mask == required_mask)
}

fn decide_swallow(cfg: &HookConfig, vk: u16, down: bool, mask: u8) -> bool {
    if cfg.suspended {
        if !down {
            let _ = take_swallowed(vk);
        }
        return false;
    }
    if down {
        let matched = matches_keydown(cfg, vk, mask);
        if matched {
            remember_swallowed(vk);
        }
        matched
    } else {
        take_swallowed(vk)
    }
}

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
    if !cfg.suspended {
        if let Some(tx) = TX.get() {
            let _ = tx.try_send(EngineInput::Key { vk, down });
        }
    }

    let mask = MOD_MASK.load(Ordering::Relaxed);
    let swallow = decide_swallow(&cfg, vk, down, mask);
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
            let mut hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), None, 0)
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!("failed to install keyboard hook: {e}");
                    return;
                }
            };
            tracing::info!("keyboard hook installed ({hook:?})");
            // Reinstall periodically on this same message-pump thread. Windows
            // may silently remove a low-level hook after a callback timeout;
            // this bounds recovery without adding work to the callback.
            let timer_id = SetTimer(None, 0, 60_000, None);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_TIMER && msg.wParam.0 == timer_id {
                    match SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), None, 0) {
                        Ok(reinstalled) => {
                            let previous = hook;
                            hook = reinstalled;
                            let _ = UnhookWindowsHookEx(previous);
                            tracing::debug!("keyboard hook health reinstall complete");
                        }
                        Err(error) => tracing::error!("keyboard hook reinstall failed: {error}"),
                    }
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = KillTimer(None, timer_id);
            let _ = UnhookWindowsHookEx(hook);
        })
        .expect("failed to spawn hook thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::bindings::{modifier_bit, VK_F9, VK_RCONTROL};

    #[test]
    fn modifier_only_rule_requires_exact_mask() {
        let cfg = HookConfig {
            swallow_rules: vec![(VK_RCONTROL, modifier_bit(VK_RCONTROL))],
            ..Default::default()
        };
        assert!(matches_keydown(
            &cfg,
            VK_RCONTROL,
            modifier_bit(VK_RCONTROL)
        ));
        assert!(!matches_keydown(
            &cfg,
            VK_RCONTROL,
            modifier_bit(VK_RCONTROL) | 1
        ));
    }

    #[test]
    fn swallowed_keyup_must_match_a_swallowed_keydown() {
        let _ = take_swallowed(VK_F9);
        assert!(!take_swallowed(VK_F9));
        remember_swallowed(VK_F9);
        assert!(take_swallowed(VK_F9));
        assert!(!take_swallowed(VK_F9));
    }

    #[test]
    fn suspended_keyup_clears_swallowed_bookkeeping_without_swallowing() {
        const TEST_KEY: u16 = 0x70;
        let _ = take_swallowed(TEST_KEY);
        remember_swallowed(TEST_KEY);
        let cfg = HookConfig {
            suspended: true,
            ..Default::default()
        };
        assert!(!decide_swallow(&cfg, TEST_KEY, false, 0));
        assert!(!take_swallowed(TEST_KEY));
    }
}
