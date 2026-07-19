pub mod bindings;
pub mod hook;
pub mod state_machine;

pub use bindings::ShortcutBinding;
pub use state_machine::{EngineInput, SessionEvent, TriggerMode};

use crossbeam_channel::Sender;

/// Handle to the running shortcut engine (hook thread + consumer thread).
pub struct ShortcutEngineHandle {
    input_tx: Sender<EngineInput>,
}

impl ShortcutEngineHandle {
    pub fn send(&self, input: EngineInput) {
        let _ = self.input_tx.send(input);
    }

    pub fn set_transcribing(&self, transcribing: bool) {
        self.send(EngineInput::SetTranscribing(transcribing));
    }

    pub fn set_suspended(&self, suspended: bool) {
        self.send(EngineInput::SetSuspended(suspended));
    }

    #[allow(dead_code)] // used by shortcut capture/rebinding in M8
    pub fn set_bindings(
        &self,
        hold: ShortcutBinding,
        toggle: ShortcutBinding,
        start_delay_ms: u64,
    ) {
        self.send(EngineInput::SetBindings {
            hold,
            toggle,
            start_delay_ms,
        });
    }

    /// Tray "Start/Stop Dictating" — behaves like a toggle-mode session.
    pub fn manual_toggle(&self) {
        self.send(EngineInput::ManualToggle);
    }
}

/// Spawn the low-level hook and the consumer state machine.
/// `on_event` is called from the consumer thread — it must hand off to the
/// pipeline quickly (send to a channel / spawn a task), never block.
pub fn spawn(
    hold: ShortcutBinding,
    toggle: ShortcutBinding,
    start_delay_ms: u64,
    on_event: impl Fn(SessionEvent) + Send + 'static,
) -> ShortcutEngineHandle {
    let (input_tx, input_rx) = crossbeam_channel::unbounded::<EngineInput>();

    // Key events from the hook flow into the same input channel.
    let key_tx = input_tx.clone();
    hook::spawn_hook(key_tx);

    state_machine::spawn_consumer(input_rx, hold, toggle, start_delay_ms, on_event);

    ShortcutEngineHandle { input_tx }
}
