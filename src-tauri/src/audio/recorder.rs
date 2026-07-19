//! Microphone capture. cpal streams are !Send, so each recording runs on its
//! own thread: capture at the device's native format, downmix to mono f32,
//! meter RMS per buffer, and on stop resample everything to 16 kHz mono i16
//! and write a WAV for the transcription API.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, Receiver, Sender};

use super::normalizer::LiveAudioLevelNormalizer;
use super::resample;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("No microphone found")]
    NoDevice,
    #[error("Microphone error: {0}")]
    Device(String),
    #[error("No audio received from microphone")]
    NoBuffers,
    #[error("Recording produced no audio")]
    Empty,
    #[error("Recording was cancelled")]
    Cancelled,
}

enum Command {
    Stop,
    Cancel,
}

pub struct RecordingHandle {
    cmd_tx: Sender<Command>,
    result_rx: Receiver<Result<PathBuf, AudioError>>,
}

impl RecordingHandle {
    /// Stop and write the WAV. Blocks until the file is finalized.
    pub fn stop(self) -> Result<PathBuf, AudioError> {
        let _ = self.cmd_tx.send(Command::Stop);
        self.result_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or(Err(AudioError::Device("recorder thread hung".into())))
    }

    /// Discard the recording.
    pub fn cancel(self) {
        let _ = self.cmd_tx.send(Command::Cancel);
        let _ = self.result_rx.recv_timeout(Duration::from_secs(5));
    }
}

pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|devices| devices.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

fn find_device(name: Option<&str>) -> Result<cpal::Device, AudioError> {
    let host = cpal::default_host();
    if let Some(name) = name {
        if let Ok(mut devices) = host.input_devices() {
            if let Some(d) = devices.find(|d| d.name().map(|n| n == name).unwrap_or(false)) {
                return Ok(d);
            }
        }
        tracing::warn!("microphone '{name}' not found, falling back to default");
    }
    host.default_input_device().ok_or(AudioError::NoDevice)
}

/// Start recording. `on_level` receives the normalized 0..1 display level
/// (~30 Hz); `on_ready` fires on the first non-silent buffer; `on_error`
/// reports async stream failures (e.g. device unplugged).
pub fn start(
    device_name: Option<String>,
    on_level: impl Fn(f32) + Send + 'static,
    on_ready: impl Fn() + Send + 'static,
    on_error: impl Fn(String) + Send + 'static,
) -> Result<RecordingHandle, AudioError> {
    let (cmd_tx, cmd_rx) = bounded::<Command>(2);
    let (result_tx, result_rx) = bounded::<Result<PathBuf, AudioError>>(1);
    let (setup_tx, setup_rx) = bounded::<Result<(), AudioError>>(1);

    std::thread::Builder::new()
        .name("murmur-recorder".into())
        .spawn(move || {
            record_thread(device_name, cmd_rx, result_tx, setup_tx, on_level, on_ready, on_error);
        })
        .map_err(|e| AudioError::Device(e.to_string()))?;

    // Wait for stream setup so callers get immediate device errors.
    match setup_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(RecordingHandle { cmd_tx, result_rx }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(AudioError::Device("recorder setup timed out".into())),
    }
}

#[allow(clippy::too_many_arguments)]
fn record_thread(
    device_name: Option<String>,
    cmd_rx: Receiver<Command>,
    result_tx: Sender<Result<PathBuf, AudioError>>,
    setup_tx: Sender<Result<(), AudioError>>,
    on_level: impl Fn(f32) + Send + 'static,
    on_ready: impl Fn() + Send + 'static,
    on_error: impl Fn(String) + Send + 'static,
) {
    let device = match find_device(device_name.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            let _ = setup_tx.send(Err(e));
            return;
        }
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = setup_tx.send(Err(AudioError::Device(e.to_string())));
            return;
        }
    };
    let source_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    tracing::debug!(
        "recording from '{}' at {source_rate} Hz, {channels} ch, {:?}",
        device.name().unwrap_or_default(),
        config.sample_format()
    );

    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let got_buffer = Arc::new(AtomicBool::new(false));
    let normalizer = Arc::new(Mutex::new(LiveAudioLevelNormalizer::default()));
    let last_emit = Arc::new(Mutex::new(std::time::Instant::now()));
    let ready_fired = Arc::new(AtomicBool::new(false));

    let data_handler = {
        let samples = samples.clone();
        let got_buffer = got_buffer.clone();
        let normalizer = normalizer.clone();
        let last_emit = last_emit.clone();
        let ready_fired = ready_fired.clone();
        move |mono: Vec<f32>| {
            got_buffer.store(true, Ordering::Relaxed);
            let rms = if mono.is_empty() {
                0.0
            } else {
                (mono.iter().map(|s| s * s).sum::<f32>() / mono.len() as f32).sqrt()
            };
            if rms > 0.0 && !ready_fired.swap(true, Ordering::Relaxed) {
                on_ready();
            }
            let level = normalizer.lock().unwrap().normalized_level(rms);
            {
                let mut last = last_emit.lock().unwrap();
                if last.elapsed() >= Duration::from_millis(33) {
                    *last = std::time::Instant::now();
                    on_level(level);
                }
            }
            samples.lock().unwrap().extend_from_slice(&mono);
        }
    };

    let err_fn = move |e: cpal::StreamError| {
        tracing::error!("audio stream error: {e}");
        on_error(e.to_string());
    };

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.clone().into(),
            move |data: &[f32], _| data_handler(downmix(data, channels)),
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.clone().into(),
            move |data: &[i16], _| {
                let f: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                data_handler(downmix(&f, channels))
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.clone().into(),
            move |data: &[u16], _| {
                let f: Vec<f32> = data
                    .iter()
                    .map(|&s| (s as f32 - 32768.0) / 32768.0)
                    .collect();
                data_handler(downmix(&f, channels))
            },
            err_fn,
            None,
        ),
        other => {
            let _ = setup_tx.send(Err(AudioError::Device(format!(
                "unsupported sample format {other:?}"
            ))));
            return;
        }
    };

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let _ = setup_tx.send(Err(AudioError::Device(e.to_string())));
            return;
        }
    };
    if let Err(e) = stream.play() {
        let _ = setup_tx.send(Err(AudioError::Device(e.to_string())));
        return;
    }
    let _ = setup_tx.send(Ok(()));

    // Watchdog: if no buffers arrive within 2s, report and bail.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let outcome = loop {
        match cmd_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(cmd) => break Some(cmd),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if !got_buffer.load(Ordering::Relaxed) && std::time::Instant::now() >= deadline {
                    drop(stream);
                    let _ = result_tx.send(Err(AudioError::NoBuffers));
                    return;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break None,
        }
    };

    drop(stream); // stops capture, flushes callbacks

    match outcome {
        Some(Command::Stop) => {
            let samples = samples.lock().unwrap();
            if samples.is_empty() {
                let _ = result_tx.send(Err(AudioError::Empty));
                return;
            }
            let pcm = resample::to_16k_mono_i16(&samples, source_rate);
            let _ = result_tx.send(write_wav(&pcm));
        }
        Some(Command::Cancel) | None => {
            let _ = result_tx.send(Err(AudioError::Cancelled));
        }
    }
}

fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

fn write_wav(pcm: &[i16]) -> Result<PathBuf, AudioError> {
    let dir = std::env::temp_dir().join("murmur");
    std::fs::create_dir_all(&dir).map_err(|e| AudioError::Device(e.to_string()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("recording-{stamp}.wav"));

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: super::TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(&path, spec).map_err(|e| AudioError::Device(e.to_string()))?;
    for &s in pcm {
        writer
            .write_sample(s)
            .map_err(|e| AudioError::Device(e.to_string()))?;
    }
    writer
        .finalize()
        .map_err(|e| AudioError::Device(e.to_string()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_stereo_averages() {
        let stereo = [1.0f32, 0.0, 0.5, 0.5, -1.0, 1.0];
        assert_eq!(downmix(&stereo, 2), vec![0.5, 0.5, 0.0]);
    }

    /// Real-hardware smoke test: records 1.5s from the default microphone.
    /// Run explicitly with: cargo test real_mic -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_mic_capture() {
        let devices = list_input_devices();
        println!("input devices: {devices:?}");
        let handle = start(
            None,
            |level| println!("level: {level:.3}"),
            || println!("ready: first non-silent buffer"),
            |e| println!("stream error: {e}"),
        )
        .expect("failed to start recording");
        std::thread::sleep(Duration::from_millis(1500));
        let path = handle.stop().expect("stop failed");
        let reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        let duration_s = reader.duration() as f32 / spec.sample_rate as f32;
        println!("wrote {path:?}: {spec:?}, {duration_s:.2}s");
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.channels, 1);
        assert!((1.2..=1.9).contains(&duration_s), "duration {duration_s}");
    }

    #[test]
    fn wav_roundtrip() {
        let pcm: Vec<i16> = (0..16_000).map(|i| ((i % 100) * 300) as i16).collect();
        let path = write_wav(&pcm).unwrap();
        let mut reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.bits_per_sample, 16);
        let read: Vec<i16> = reader.samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(read, pcm);
        let _ = std::fs::remove_file(path);
    }
}
