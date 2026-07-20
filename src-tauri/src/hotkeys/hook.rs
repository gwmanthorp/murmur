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
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KillTimer, SetTimer, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_SYSKEYDOWN, WM_TIMER,
};

use super::bindings::{is_modifier, modifier_bit, MODIFIER_VKS, VK_ESCAPE};
use super::state_machine::EngineInput;

/// Marker placed in `dwExtraInfo` of our own SendInput events so the hook
/// never swallows or reacts to Murmur's synthetic Ctrl+V / Enter.
pub const INJECT_SENTINEL: usize = 0x4D52_4D52; // "MRMR"

/// What the hook callback needs to know, swapped atomically by the consumer.
#[derive(Default, Clone)]
pub struct HookConfig {
    /// (vk, required_modifier_mask): swallow keydown of `vk` when the current
    /// modifier mask equals the required mask; keyups of `vk` are always
    /// swallowed for non-modifiers so apps never see a stray up for a down
    /// they never got. Modifier keyups pass through so Windows clears them.
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
struct PhysicalKeyState {
    down: [AtomicU64; 4],
    observed: [AtomicU64; 4],
}

impl PhysicalKeyState {
    const fn new() -> Self {
        Self {
            down: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            observed: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
        }
    }

    fn record(&self, vk: u16, down: bool) {
        let Some((word, bit)) = key_slot(vk) else {
            return;
        };
        self.observed[word].fetch_or(bit, Ordering::Relaxed);
        if down {
            self.down[word].fetch_or(bit, Ordering::Relaxed);
        } else {
            self.down[word].fetch_and(!bit, Ordering::Relaxed);
        }
    }

    fn get(&self, vk: u16) -> Option<bool> {
        let (word, bit) = key_slot(vk)?;
        if self.observed[word].load(Ordering::Relaxed) & bit == 0 {
            return None;
        }
        Some(self.down[word].load(Ordering::Relaxed) & bit != 0)
    }

    fn replace_with(&self, mut is_down: impl FnMut(u16) -> bool) {
        for vk in 0..=255u16 {
            self.record(vk, is_down(vk));
        }
    }
}

static PHYSICAL_KEYS: PhysicalKeyState = PhysicalKeyState::new();
static SWALLOWED: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

fn key_slot(vk: u16) -> Option<(usize, u64)> {
    (vk < 256).then(|| {
        let index = vk as usize;
        (index / 64, 1u64 << (index % 64))
    })
}

fn swallowed_slot(vk: u16) -> Option<(&'static AtomicU64, u64)> {
    key_slot(vk).map(|(word, bit)| (&SWALLOWED[word], bit))
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
        let matched_down = take_swallowed(vk);
        // A swallowed modifier release can leave Windows' asynchronous key
        // state stuck on "down". Let the physical key-up reach Windows while
        // still consuming our bookkeeping. Non-modifier shortcuts keep their
        // matching key-up swallowed so target apps never receive a stray up.
        matched_down && !is_modifier(vk)
    }
}

fn record_keyboard_event(state: &PhysicalKeyState, vk: u16, down: bool, injected: bool) {
    if !injected {
        state.record(vk, down);
    }
}

fn async_key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

fn resync_physical_state() {
    PHYSICAL_KEYS.replace_with(async_key_down);
    let mask = MODIFIER_VKS.iter().fold(0u8, |mask, &vk| {
        if PHYSICAL_KEYS.get(vk) == Some(true) {
            mask | modifier_bit(vk)
        } else {
            mask
        }
    });
    MOD_MASK.store(mask, Ordering::Relaxed);
}

/// Physical state observed by Murmur's hook. Unlike `GetAsyncKeyState`, this
/// remains accurate when Murmur deliberately swallows a shortcut keydown.
pub fn physical_key_down(vk: u16) -> bool {
    PHYSICAL_KEYS.get(vk).unwrap_or_else(|| async_key_down(vk))
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
    let vk = kb.vkCode as u16;
    let msg = wparam.0 as u32;
    let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    record_keyboard_event(&PHYSICAL_KEYS, vk, down, injected);
    if injected {
        return CallNextHookEx(None, code, wparam, lparam);
    }

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
            resync_physical_state();
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
                            resync_physical_state();
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
    fn modifier_keyup_clears_bookkeeping_but_passes_through() {
        let _ = take_swallowed(VK_RCONTROL);
        let cfg = HookConfig {
            swallow_rules: vec![(VK_RCONTROL, modifier_bit(VK_RCONTROL))],
            ..Default::default()
        };
        assert!(decide_swallow(
            &cfg,
            VK_RCONTROL,
            true,
            modifier_bit(VK_RCONTROL)
        ));
        assert!(!decide_swallow(&cfg, VK_RCONTROL, false, 0));
        assert!(!take_swallowed(VK_RCONTROL));
    }

    #[test]
    fn non_modifier_keyup_remains_swallowed() {
        let _ = take_swallowed(VK_F9);
        let cfg = HookConfig {
            swallow_rules: vec![(VK_F9, 0)],
            ..Default::default()
        };
        assert!(decide_swallow(&cfg, VK_F9, true, 0));
        assert!(decide_swallow(&cfg, VK_F9, false, 0));
        assert!(!take_swallowed(VK_F9));
    }

    #[test]
    fn physical_state_records_down_up_and_can_resync() {
        const TEST_KEY: u16 = 0x71;
        let state = PhysicalKeyState::new();
        assert_eq!(state.get(TEST_KEY), None);
        state.record(TEST_KEY, true);
        assert_eq!(state.get(TEST_KEY), Some(true));
        state.record(TEST_KEY, false);
        assert_eq!(state.get(TEST_KEY), Some(false));
        state.replace_with(|vk| vk == VK_RCONTROL);
        assert_eq!(state.get(VK_RCONTROL), Some(true));
        assert_eq!(state.get(TEST_KEY), Some(false));
    }

    #[test]
    fn injected_events_do_not_change_physical_state() {
        const TEST_KEY: u16 = 0x72;
        let state = PhysicalKeyState::new();
        record_keyboard_event(&state, TEST_KEY, true, true);
        assert_eq!(state.get(TEST_KEY), None);
        record_keyboard_event(&state, TEST_KEY, true, false);
        assert_eq!(state.get(TEST_KEY), Some(true));
        record_keyboard_event(&state, TEST_KEY, false, true);
        assert_eq!(state.get(TEST_KEY), Some(true));
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
