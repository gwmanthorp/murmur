//! Mono f32 at an arbitrary source rate → 16 kHz mono i16.

use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

use super::TARGET_SAMPLE_RATE;

/// Resample a whole recording. `samples` are mono f32 in [-1, 1].
pub fn to_16k_mono_i16(samples: &[f32], source_rate: u32) -> Vec<i16> {
    let mono: Vec<f32> = if source_rate == TARGET_SAMPLE_RATE {
        samples.to_vec()
    } else {
        resample(samples, source_rate)
    };
    mono.iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect()
}

fn resample(samples: &[f32], source_rate: u32) -> Vec<f32> {
    let ratio = TARGET_SAMPLE_RATE as f64 / source_rate as f64;
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    const CHUNK: usize = 1024;
    let mut resampler = match SincFixedIn::<f32>::new(ratio, 1.1, params, CHUNK, 1) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("resampler init failed: {e}");
            return Vec::new();
        }
    };

    let output_delay = resampler.output_delay();
    let expected_len = (samples.len() as f64 * ratio).round() as usize;
    let mut out = Vec::with_capacity(expected_len + 2 * CHUNK);
    let mut pos = 0;
    while pos + CHUNK <= samples.len() {
        match resampler.process(&[&samples[pos..pos + CHUNK]], None) {
            Ok(mut chunks) => out.append(&mut chunks[0]),
            Err(e) => {
                tracing::error!("resample failed: {e}");
                return out;
            }
        }
        pos += CHUNK;
    }
    // Flush the tail (padded with silence internally).
    let remaining = &samples[pos..];
    if !remaining.is_empty() {
        if let Ok(mut chunks) = resampler.process_partial(Some(&[remaining]), None) {
            out.append(&mut chunks[0]);
        }
    }
    if let Ok(mut chunks) = resampler.process_partial::<&[f32]>(None, None) {
        out.append(&mut chunks[0]);
    }
    // Drop the filter's group delay from the front and the flush padding from
    // the back so output length matches the input duration.
    let trimmed: Vec<f32> = out
        .into_iter()
        .skip(output_delay)
        .take(expected_len)
        .collect();
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_at_16k() {
        let samples = vec![0.5f32; 1600];
        let out = to_16k_mono_i16(&samples, 16_000);
        assert_eq!(out.len(), 1600);
        assert!((out[0] as f32 / i16::MAX as f32 - 0.5).abs() < 0.001);
    }

    #[test]
    fn downsamples_48k_to_16k() {
        // 1 second of a 440 Hz sine at 48 kHz should come out ~16000 samples.
        let samples: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 440.0 * 2.0 * std::f32::consts::PI / 48_000.0).sin() * 0.5)
            .collect();
        let out = to_16k_mono_i16(&samples, 48_000);
        let len = out.len() as i64;
        assert!(
            (len - 16_000).abs() < 200,
            "expected ~16000 samples, got {len}"
        );
        // Signal should retain energy.
        let rms: f64 = (out.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / len as f64).sqrt();
        assert!(rms > 5000.0, "rms too low: {rms}");
    }
}
