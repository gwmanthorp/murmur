//! Shortcut session state machine (port of FreeFlow's
//! DictationShortcutSessionController + ShortcutMatcher semantics).
//!
//! Behaviors:
//! - Hold: record while the binding is held; releasing any of its keys stops.
//! - Toggle: full press starts; a second *full* press (all keys released,
//!   then pressed again) stops — holding the combo doesn't insta-stop.
//! - Latch: while a hold session is active (or pending), activating the
//!   toggle binding switches the session to toggle mode so keys can be
//!   released while recording continues.
//! - Start delay (0–500ms): recording begins only after the binding has been
//!   held for the delay; releasing earlier cancels the pending start.
//! - Esc cancels toggle sessions and in-flight transcription only.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};

use super::bindings::{is_modifier, modifier_bit, ShortcutBinding, VK_ESCAPE};
use super::hook::{self, HookConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerMode {
    Hold,
    Toggle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// Start recording.
    Begin(TriggerMode),
    /// A hold session latched into toggle mode (recording continues).
    Latched,
    /// Stop recording and run the pipeline.
    Stop,
    /// Cancel recording, discard audio.
    Cancel,
    /// Esc pressed while a transcription is in flight.
    CancelTranscription,
}

#[derive(Debug)]
pub enum EngineInput {
    Key {
        vk: u16,
        down: bool,
    },
    #[allow(dead_code)] // used by shortcut capture/rebinding in M8
    SetBindings {
        hold: ShortcutBinding,
        toggle: ShortcutBinding,
        start_delay_ms: u64,
    },
    SetTranscribing(bool),
    SetSuspended(bool),
    ManualToggle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Pending {
        mode: TriggerMode,
        deadline: Instant,
    },
    Active {
        mode: TriggerMode,
        toggle_rearmed: bool,
    },
}

pub struct Engine {
    hold: ShortcutBinding,
    toggle: ShortcutBinding,
    start_delay: Duration,
    down: HashSet<u16>,
    state: State,
    transcribing: bool,
    suspended: bool,
}

impl Engine {
    pub fn new(hold: ShortcutBinding, toggle: ShortcutBinding, start_delay_ms: u64) -> Self {
        Self {
            hold,
            toggle,
            start_delay: Duration::from_millis(start_delay_ms),
            down: HashSet::new(),
            state: State::Idle,
            transcribing: false,
            suspended: false,
        }
    }

    fn mod_mask(&self) -> u8 {
        self.down
            .iter()
            .filter(|vk| is_modifier(**vk))
            .fold(0u8, |acc, vk| acc | modifier_bit(*vk))
    }

    /// A binding is satisfied when all its keys are down and no modifiers
    /// outside the binding (minus `ignore_mask`) are held.
    fn satisfied(&self, binding: &ShortcutBinding, ignore_mask: u8) -> bool {
        !binding.is_empty()
            && binding.vks.iter().all(|vk| self.down.contains(vk))
            && (self.mod_mask() & !binding.modifier_mask() & !ignore_mask) == 0
    }

    fn in_hold_phase(&self) -> bool {
        matches!(
            self.state,
            State::Pending {
                mode: TriggerMode::Hold,
                ..
            } | State::Active {
                mode: TriggerMode::Hold,
                ..
            }
        )
    }

    fn begin(&mut self, mode: TriggerMode, now: Instant, out: &mut Vec<SessionEvent>) {
        if self.start_delay.is_zero() {
            self.state = State::Active {
                mode,
                toggle_rearmed: false,
            };
            out.push(SessionEvent::Begin(mode));
        } else {
            self.state = State::Pending {
                mode,
                deadline: now + self.start_delay,
            };
        }
    }

    pub fn handle_key(&mut self, vk: u16, down: bool, now: Instant) -> Vec<SessionEvent> {
        let mut out = Vec::new();
        if self.suspended {
            return out;
        }

        if down {
            // Auto-repeat: ignore keydowns for keys already held.
            if !self.down.insert(vk) {
                return out;
            }

            if vk == VK_ESCAPE {
                if self.transcribing {
                    out.push(SessionEvent::CancelTranscription);
                } else if matches!(
                    self.state,
                    State::Active {
                        mode: TriggerMode::Toggle,
                        ..
                    } | State::Pending {
                        mode: TriggerMode::Toggle,
                        ..
                    }
                ) {
                    self.state = State::Idle;
                    out.push(SessionEvent::Cancel);
                }
                return out;
            }

            // Processing is exclusive: keep the configured keys swallowed at
            // the hook, but never begin a second recording until it finishes.
            if self.transcribing {
                return out;
            }

            // Toggle binding: latch from a hold phase ignores the hold
            // binding's modifiers (FreeFlow: toggle extends hold).
            let latch_ignore = if self.in_hold_phase() {
                self.hold.modifier_mask()
            } else {
                0
            };
            if self.toggle.contains(vk) && self.satisfied(&self.toggle, latch_ignore) {
                match self.state {
                    State::Active {
                        mode: TriggerMode::Hold,
                        ..
                    } => {
                        self.state = State::Active {
                            mode: TriggerMode::Toggle,
                            toggle_rearmed: false,
                        };
                        out.push(SessionEvent::Latched);
                    }
                    State::Pending {
                        mode: TriggerMode::Hold,
                        deadline,
                    } => {
                        self.state = State::Pending {
                            mode: TriggerMode::Toggle,
                            deadline,
                        };
                    }
                    State::Active {
                        mode: TriggerMode::Toggle,
                        toggle_rearmed: true,
                    } => {
                        self.state = State::Idle;
                        out.push(SessionEvent::Stop);
                    }
                    State::Active {
                        mode: TriggerMode::Toggle,
                        ..
                    }
                    | State::Pending {
                        mode: TriggerMode::Toggle,
                        ..
                    } => {}
                    State::Idle => self.begin(TriggerMode::Toggle, now, &mut out),
                }
                return out;
            }

            if self.hold.contains(vk) && self.satisfied(&self.hold, 0) && self.state == State::Idle
            {
                self.begin(TriggerMode::Hold, now, &mut out);
            }
        } else {
            self.down.remove(&vk);

            match self.state {
                State::Active {
                    mode: TriggerMode::Hold,
                    ..
                } if self.hold.contains(vk) => {
                    self.state = State::Idle;
                    out.push(SessionEvent::Stop);
                }
                State::Pending {
                    mode: TriggerMode::Hold,
                    ..
                } if self.hold.contains(vk) => {
                    self.state = State::Idle;
                }
                State::Pending {
                    mode: TriggerMode::Toggle,
                    ..
                } if self.toggle.contains(vk) => {
                    self.state = State::Idle;
                }
                State::Active {
                    mode: TriggerMode::Toggle,
                    toggle_rearmed: false,
                } if self.toggle.contains(vk)
                    && !self.toggle.vks.iter().any(|k| self.down.contains(k)) =>
                {
                    self.state = State::Active {
                        mode: TriggerMode::Toggle,
                        toggle_rearmed: true,
                    };
                }
                _ => {}
            }
        }
        out
    }

    /// Fires pending starts whose deadline has passed.
    pub fn tick(&mut self, now: Instant) -> Vec<SessionEvent> {
        let mut out = Vec::new();
        if let State::Pending { mode, deadline } = self.state {
            if now >= deadline {
                self.state = State::Active {
                    mode,
                    toggle_rearmed: false,
                };
                out.push(SessionEvent::Begin(mode));
            }
        }
        out
    }

    pub fn handle_input(&mut self, input: EngineInput, now: Instant) -> Vec<SessionEvent> {
        match input {
            EngineInput::Key { vk, down } => self.handle_key(vk, down, now),
            EngineInput::SetBindings {
                hold,
                toggle,
                start_delay_ms,
            } => {
                self.hold = hold;
                self.toggle = toggle;
                self.start_delay = Duration::from_millis(start_delay_ms);
                // Rebinding mid-session: drop back to idle to avoid stuck state.
                let mut out = Vec::new();
                if matches!(self.state, State::Active { .. }) {
                    out.push(SessionEvent::Cancel);
                }
                self.state = State::Idle;
                out
            }
            EngineInput::SetTranscribing(t) => {
                self.transcribing = t;
                Vec::new()
            }
            EngineInput::SetSuspended(s) => {
                self.suspended = s;
                self.down.clear();
                let mut out = Vec::new();
                if s && matches!(self.state, State::Active { .. }) {
                    out.push(SessionEvent::Cancel);
                }
                self.state = State::Idle;
                out
            }
            EngineInput::ManualToggle => {
                let mut out = Vec::new();
                match self.state {
                    State::Idle => {
                        self.state = State::Active {
                            mode: TriggerMode::Toggle,
                            toggle_rearmed: true,
                        };
                        out.push(SessionEvent::Begin(TriggerMode::Toggle));
                    }
                    State::Active { .. } => {
                        self.state = State::Idle;
                        out.push(SessionEvent::Stop);
                    }
                    State::Pending { .. } => {
                        self.state = State::Idle;
                    }
                }
                out
            }
        }
    }

    /// Deadline the consumer loop should wake at, if any.
    pub fn next_deadline(&self) -> Option<Instant> {
        match self.state {
            State::Pending { deadline, .. } => Some(deadline),
            _ => None,
        }
    }

    /// What the hook callback needs right now.
    pub fn hook_config(&self) -> HookConfig {
        let mut rules = Vec::new();
        if let Some(key) = self.hold.key() {
            rules.push((key, self.hold.modifier_mask()));
        } else {
            // Modifier-only bindings (the default Right Ctrl hold shortcut)
            // must swallow the modifier itself when the exact combo matches.
            for &modifier in &self.hold.vks {
                rules.push((modifier, self.hold.modifier_mask()));
            }
        }
        if let Some(key) = self.toggle.key() {
            rules.push((key, self.toggle.modifier_mask()));
            // Latch: the toggle key may be pressed while the (modifier-only)
            // hold binding is still held.
            let hold_mods = self.hold.modifier_mask();
            if self.hold.key().is_none() && hold_mods != 0 {
                rules.push((key, self.toggle.modifier_mask() | hold_mods));
            }
        }
        HookConfig {
            swallow_rules: rules,
            swallow_esc: self.transcribing
                || matches!(
                    self.state,
                    State::Active {
                        mode: TriggerMode::Toggle,
                        ..
                    } | State::Pending {
                        mode: TriggerMode::Toggle,
                        ..
                    }
                ),
            suspended: self.suspended,
        }
    }
}

pub fn spawn_consumer(
    rx: Receiver<EngineInput>,
    hold: ShortcutBinding,
    toggle: ShortcutBinding,
    start_delay_ms: u64,
    on_event: impl Fn(SessionEvent) + Send + 'static,
) {
    std::thread::Builder::new()
        .name("murmur-shortcut-engine".into())
        .spawn(move || {
            let mut engine = Engine::new(hold, toggle, start_delay_ms);
            hook::store_config(engine.hook_config());
            loop {
                let now = Instant::now();
                let input = match engine.next_deadline() {
                    Some(deadline) => {
                        let timeout = deadline.saturating_duration_since(now);
                        match rx.recv_timeout(timeout) {
                            Ok(i) => Some(i),
                            Err(RecvTimeoutError::Timeout) => None,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    None => match rx.recv() {
                        Ok(i) => Some(i),
                        Err(_) => return,
                    },
                };
                let now = Instant::now();
                let events = match input {
                    Some(input) => engine.handle_input(input, now),
                    None => engine.tick(now),
                };
                hook::store_config(engine.hook_config());
                for ev in events {
                    tracing::debug!("shortcut event: {ev:?}");
                    on_event(ev);
                }
            }
        })
        .expect("failed to spawn shortcut engine thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::bindings::{VK_F9, VK_RCONTROL};

    fn engine() -> Engine {
        Engine::new(
            ShortcutBinding::new(vec![VK_RCONTROL]),
            ShortcutBinding::new(vec![VK_F9]),
            0,
        )
    }

    fn key(e: &mut Engine, vk: u16, down: bool) -> Vec<SessionEvent> {
        e.handle_key(vk, down, Instant::now())
    }

    #[test]
    fn hold_press_release() {
        let mut e = engine();
        assert_eq!(
            key(&mut e, VK_RCONTROL, true),
            vec![SessionEvent::Begin(TriggerMode::Hold)]
        );
        assert_eq!(key(&mut e, VK_RCONTROL, false), vec![SessionEvent::Stop]);
    }

    #[test]
    fn hold_with_extra_modifier_does_not_trigger() {
        let mut e = engine();
        assert!(key(&mut e, 0xA0 /* LShift */, true).is_empty());
        assert!(key(&mut e, VK_RCONTROL, true).is_empty());
    }

    #[test]
    fn toggle_tap_start_tap_stop() {
        let mut e = engine();
        assert_eq!(
            key(&mut e, VK_F9, true),
            vec![SessionEvent::Begin(TriggerMode::Toggle)]
        );
        // Holding F9 down (auto-repeat) must not stop.
        assert!(key(&mut e, VK_F9, true).is_empty());
        assert!(key(&mut e, VK_F9, false).is_empty());
        assert_eq!(key(&mut e, VK_F9, true), vec![SessionEvent::Stop]);
    }

    #[test]
    fn latch_hold_into_toggle() {
        let mut e = engine();
        assert_eq!(
            key(&mut e, VK_RCONTROL, true),
            vec![SessionEvent::Begin(TriggerMode::Hold)]
        );
        // Press toggle while holding: latch, no stop on hold release.
        assert_eq!(key(&mut e, VK_F9, true), vec![SessionEvent::Latched]);
        assert!(key(&mut e, VK_F9, false).is_empty());
        assert!(key(&mut e, VK_RCONTROL, false).is_empty());
        // Full toggle press stops.
        assert_eq!(key(&mut e, VK_F9, true), vec![SessionEvent::Stop]);
    }

    #[test]
    fn esc_cancels_toggle_but_not_hold() {
        let mut e = engine();
        key(&mut e, VK_F9, true);
        assert_eq!(key(&mut e, VK_ESCAPE, true), vec![SessionEvent::Cancel]);
        key(&mut e, VK_ESCAPE, false);

        key(&mut e, VK_RCONTROL, true);
        assert!(key(&mut e, VK_ESCAPE, true).is_empty());
        assert_eq!(key(&mut e, VK_RCONTROL, false), vec![SessionEvent::Stop]);
    }

    #[test]
    fn esc_cancels_transcription() {
        let mut e = engine();
        e.transcribing = true;
        assert_eq!(
            key(&mut e, VK_ESCAPE, true),
            vec![SessionEvent::CancelTranscription]
        );
    }

    #[test]
    fn start_delay_release_before_deadline_cancels() {
        let mut e = Engine::new(
            ShortcutBinding::new(vec![VK_RCONTROL]),
            ShortcutBinding::new(vec![VK_F9]),
            200,
        );
        let t0 = Instant::now();
        assert!(e.handle_key(VK_RCONTROL, true, t0).is_empty());
        assert!(e.next_deadline().is_some());
        // Released before the deadline: silent cancel.
        assert!(e.handle_key(VK_RCONTROL, false, t0).is_empty());
        assert!(e.next_deadline().is_none());
        assert!(e.tick(t0 + Duration::from_millis(300)).is_empty());
    }

    #[test]
    fn start_delay_elapsed_begins() {
        let mut e = Engine::new(
            ShortcutBinding::new(vec![VK_RCONTROL]),
            ShortcutBinding::new(vec![VK_F9]),
            200,
        );
        let t0 = Instant::now();
        e.handle_key(VK_RCONTROL, true, t0);
        assert_eq!(
            e.tick(t0 + Duration::from_millis(200)),
            vec![SessionEvent::Begin(TriggerMode::Hold)]
        );
        assert_eq!(
            e.handle_key(VK_RCONTROL, false, t0),
            vec![SessionEvent::Stop]
        );
    }

    #[test]
    fn pending_hold_latches_to_pending_toggle() {
        let mut e = Engine::new(
            ShortcutBinding::new(vec![VK_RCONTROL]),
            ShortcutBinding::new(vec![VK_F9]),
            200,
        );
        let t0 = Instant::now();
        e.handle_key(VK_RCONTROL, true, t0);
        assert!(e.handle_key(VK_F9, true, t0).is_empty());
        // Releasing hold no longer cancels: the pending session is toggle now.
        assert!(e.handle_key(VK_RCONTROL, false, t0).is_empty());
        assert_eq!(
            e.tick(t0 + Duration::from_millis(200)),
            vec![SessionEvent::Begin(TriggerMode::Toggle)]
        );
    }

    #[test]
    fn manual_toggle_roundtrip() {
        let mut e = engine();
        assert_eq!(
            e.handle_input(EngineInput::ManualToggle, Instant::now()),
            vec![SessionEvent::Begin(TriggerMode::Toggle)]
        );
        assert_eq!(
            e.handle_input(EngineInput::ManualToggle, Instant::now()),
            vec![SessionEvent::Stop]
        );
    }

    #[test]
    fn hook_config_latch_rule_for_modifier_hold() {
        let e = engine();
        let cfg = e.hook_config();
        // F9 alone, plus F9-while-RCtrl-held (latch).
        assert!(cfg.swallow_rules.contains(&(VK_F9, 0)));
        assert!(cfg
            .swallow_rules
            .contains(&(VK_F9, modifier_bit(VK_RCONTROL))));
        assert!(cfg
            .swallow_rules
            .contains(&(VK_RCONTROL, modifier_bit(VK_RCONTROL))));
        assert!(!cfg.swallow_esc);
    }

    #[test]
    fn transcription_blocks_new_recording() {
        let mut e = engine();
        e.transcribing = true;
        assert!(key(&mut e, VK_RCONTROL, true).is_empty());
        assert!(key(&mut e, VK_RCONTROL, false).is_empty());
        assert!(key(&mut e, VK_F9, true).is_empty());
    }
}
