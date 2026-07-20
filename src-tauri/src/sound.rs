//! Feedback tones generated in memory (no bundled assets) and played via
//! PlaySoundW with SND_MEMORY | SND_ASYNC.

use std::io::Cursor;
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_SYNC};

pub enum Cue {
    Start,
    Stop,
    Error,
}

fn tone(frequencies: &[(f32, f32)]) -> Vec<u8> {
    // 22.05 kHz mono i16 with a short fade to avoid clicks.
    const RATE: u32 = 22_050;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
        for &(freq, dur_s) in frequencies {
            let n = (RATE as f32 * dur_s) as usize;
            for i in 0..n {
                let t = i as f32 / RATE as f32;
                let fade = (1.0 - (i as f32 / n as f32)).powf(0.5)
                    * (i as f32 / (RATE as f32 * 0.005)).min(1.0);
                let sample = (t * freq * 2.0 * std::f32::consts::PI).sin() * 0.25 * fade;
                writer
                    .write_sample((sample * i16::MAX as f32) as i16)
                    .unwrap();
            }
        }
        writer.finalize().unwrap();
    }
    cursor.into_inner()
}

fn cue_bytes(cue: &Cue) -> &'static Vec<u8> {
    static START: OnceLock<Vec<u8>> = OnceLock::new();
    static STOP: OnceLock<Vec<u8>> = OnceLock::new();
    static ERROR: OnceLock<Vec<u8>> = OnceLock::new();
    match cue {
        Cue::Start => START.get_or_init(|| tone(&[(660.0, 0.06), (880.0, 0.08)])),
        Cue::Stop => STOP.get_or_init(|| tone(&[(880.0, 0.06), (660.0, 0.08)])),
        Cue::Error => ERROR.get_or_init(|| tone(&[(220.0, 0.12), (185.0, 0.15)])),
    }
}

pub fn play(cue: Cue, enabled: bool) {
    if !enabled {
        return;
    }
    let bytes = cue_bytes(&cue);
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(bytes.as_ptr() as *const u16),
            None,
            SND_MEMORY | SND_ASYNC,
        );
    }
}

/// Plays a cue to completion. Used before recording starts so playback can be
/// muted only after the cue finishes, preventing Murmur from recording itself.
pub fn play_blocking(cue: Cue, enabled: bool) {
    if !enabled {
        return;
    }
    let bytes = cue_bytes(&cue);
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(bytes.as_ptr() as *const u16),
            None,
            SND_MEMORY | SND_SYNC,
        );
    }
}
