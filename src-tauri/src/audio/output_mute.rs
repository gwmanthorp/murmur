use std::sync::mpsc;

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

enum Command {
    Mute(mpsc::Sender<Result<(), String>>),
    Restore(mpsc::Sender<Result<(), String>>),
    Shutdown,
}

/// Owns the Windows Core Audio interfaces on one COM-initialized thread.
///
/// Keeping the original endpoint alive lets us restore the exact device Murmur
/// muted even if Windows changes the default output while dictation is active.
pub struct OutputMuteController {
    tx: mpsc::Sender<Command>,
}

impl OutputMuteController {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("murmur-output-mute".into())
            .spawn(move || run_windows_worker(rx))
            .expect("could not start output mute worker");
        Self { tx }
    }

    /// Mutes the default playback endpoint and remembers its prior mute state.
    /// Calling this more than once during the same recording is harmless.
    pub fn mute(&self) -> Result<(), String> {
        self.request(Command::Mute)
    }

    /// Restores the endpoint to the state it had before `mute` was called.
    pub fn restore(&self) -> Result<(), String> {
        self.request(Command::Restore)
    }

    fn request(
        &self,
        command: impl FnOnce(mpsc::Sender<Result<(), String>>) -> Command,
    ) -> Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(command(reply_tx))
            .map_err(|_| "The Windows audio worker stopped unexpectedly".to_string())?;
        reply_rx
            .recv()
            .map_err(|_| "The Windows audio worker did not respond".to_string())?
    }
}

impl Drop for OutputMuteController {
    fn drop(&mut self) {
        // Restore first so a normal application exit cannot leave Windows muted.
        let _ = self.restore();
        let _ = self.tx.send(Command::Shutdown);
    }
}

trait EndpointMute {
    fn is_muted(&self) -> Result<bool, String>;
    fn set_muted(&self, muted: bool) -> Result<(), String>;
}

trait EndpointProvider {
    type Endpoint: EndpointMute;

    fn default_output(&self) -> Result<Self::Endpoint, String>;
}

struct ActiveMute<E> {
    endpoint: E,
    originally_muted: bool,
}

fn begin_mute<P: EndpointProvider>(
    provider: &P,
    active: &mut Option<ActiveMute<P::Endpoint>>,
) -> Result<(), String> {
    if active.is_some() {
        return Ok(());
    }

    let endpoint = provider.default_output()?;
    let originally_muted = endpoint.is_muted()?;
    if !originally_muted {
        endpoint.set_muted(true)?;
    }
    *active = Some(ActiveMute {
        endpoint,
        originally_muted,
    });
    Ok(())
}

fn restore_mute<E: EndpointMute>(active: &mut Option<ActiveMute<E>>) -> Result<(), String> {
    let Some(session) = active.as_ref() else {
        return Ok(());
    };
    if !session.originally_muted {
        // Retain the session when restoration fails so shutdown can retry.
        session.endpoint.set_muted(false)?;
    }
    *active = None;
    Ok(())
}

struct WindowsProvider {
    enumerator: IMMDeviceEnumerator,
}

struct WindowsEndpoint(IAudioEndpointVolume);

impl WindowsProvider {
    fn new() -> Result<Self, String> {
        let enumerator = unsafe {
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|error| format!("Could not access Windows playback devices: {error}"))?
        };
        Ok(Self { enumerator })
    }
}

impl EndpointProvider for WindowsProvider {
    type Endpoint = WindowsEndpoint;

    fn default_output(&self) -> Result<Self::Endpoint, String> {
        let device = unsafe {
            self.enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|error| format!("Could not find the default playback device: {error}"))?
        };
        let volume = unsafe {
            device
                .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
                .map_err(|error| format!("Could not control playback volume: {error}"))?
        };
        Ok(WindowsEndpoint(volume))
    }
}

impl EndpointMute for WindowsEndpoint {
    fn is_muted(&self) -> Result<bool, String> {
        unsafe {
            self.0
                .GetMute()
                .map(|value| value.as_bool())
                .map_err(|error| format!("Could not read playback mute state: {error}"))
        }
    }

    fn set_muted(&self, muted: bool) -> Result<(), String> {
        unsafe {
            self.0
                .SetMute(muted, std::ptr::null())
                .map_err(|error| format!("Could not change playback mute state: {error}"))
        }
    }
}

fn run_windows_worker(rx: mpsc::Receiver<Command>) {
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if let Err(error) = initialized.ok() {
        respond_unavailable(
            rx,
            format!("Could not initialize Windows audio control: {error}"),
        );
        return;
    }

    match WindowsProvider::new() {
        Ok(provider) => run_worker(&provider, rx),
        Err(error) => respond_unavailable(rx, error),
    }

    unsafe { CoUninitialize() };
}

fn run_worker<P: EndpointProvider>(provider: &P, rx: mpsc::Receiver<Command>) {
    let mut active = None;
    while let Ok(command) = rx.recv() {
        match command {
            Command::Mute(reply) => {
                let _ = reply.send(begin_mute(provider, &mut active));
            }
            Command::Restore(reply) => {
                let _ = reply.send(restore_mute(&mut active));
            }
            Command::Shutdown => {
                if let Err(error) = restore_mute(&mut active) {
                    tracing::warn!("could not restore playback during shutdown: {error}");
                }
                break;
            }
        }
    }
}

fn respond_unavailable(rx: mpsc::Receiver<Command>, error: String) {
    while let Ok(command) = rx.recv() {
        match command {
            Command::Mute(reply) | Command::Restore(reply) => {
                let _ = reply.send(Err(error.clone()));
            }
            Command::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeEndpoint {
        muted: Arc<Mutex<bool>>,
        changes: Arc<Mutex<Vec<bool>>>,
        fail_restore: Arc<Mutex<bool>>,
    }

    impl EndpointMute for FakeEndpoint {
        fn is_muted(&self) -> Result<bool, String> {
            Ok(*self.muted.lock().unwrap())
        }

        fn set_muted(&self, muted: bool) -> Result<(), String> {
            if !muted && *self.fail_restore.lock().unwrap() {
                return Err("restore failed".into());
            }
            *self.muted.lock().unwrap() = muted;
            self.changes.lock().unwrap().push(muted);
            Ok(())
        }
    }

    struct FakeProvider(FakeEndpoint);

    impl EndpointProvider for FakeProvider {
        type Endpoint = FakeEndpoint;

        fn default_output(&self) -> Result<Self::Endpoint, String> {
            Ok(self.0.clone())
        }
    }

    fn fixture(initially_muted: bool) -> (FakeProvider, Arc<Mutex<Vec<bool>>>) {
        let changes = Arc::new(Mutex::new(Vec::new()));
        let endpoint = FakeEndpoint {
            muted: Arc::new(Mutex::new(initially_muted)),
            changes: changes.clone(),
            fail_restore: Arc::new(Mutex::new(false)),
        };
        (FakeProvider(endpoint), changes)
    }

    #[test]
    fn mute_and_restore_preserve_an_unmuted_endpoint() {
        let (provider, changes) = fixture(false);
        let mut active = None;
        begin_mute(&provider, &mut active).unwrap();
        restore_mute(&mut active).unwrap();
        assert_eq!(*changes.lock().unwrap(), vec![true, false]);
        assert!(active.is_none());
    }

    #[test]
    fn an_already_muted_endpoint_is_never_unmuted() {
        let (provider, changes) = fixture(true);
        let mut active = None;
        begin_mute(&provider, &mut active).unwrap();
        restore_mute(&mut active).unwrap();
        assert!(changes.lock().unwrap().is_empty());
    }

    #[test]
    fn repeated_mute_is_idempotent() {
        let (provider, changes) = fixture(false);
        let mut active = None;
        begin_mute(&provider, &mut active).unwrap();
        begin_mute(&provider, &mut active).unwrap();
        assert_eq!(*changes.lock().unwrap(), vec![true]);
    }

    #[test]
    fn failed_restore_keeps_state_for_a_retry() {
        let (provider, changes) = fixture(false);
        let mut active = None;
        begin_mute(&provider, &mut active).unwrap();
        *provider.0.fail_restore.lock().unwrap() = true;
        assert!(restore_mute(&mut active).is_err());
        assert!(active.is_some());
        *provider.0.fail_restore.lock().unwrap() = false;
        restore_mute(&mut active).unwrap();
        assert_eq!(*changes.lock().unwrap(), vec![true, false]);
    }
}
