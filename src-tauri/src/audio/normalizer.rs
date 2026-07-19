//! Direct port of FreeFlow's LiveAudioLevelNormalizer (adaptive noise floor /
//! peak ceiling with attack/release smoothing) producing a 0..1 display level
//! for the overlay waveform.

const MINIMUM_RMS: f32 = 0.00001;
const MIN_SPAN_DB: f32 = 18.0;
const PEAK_HEADROOM_DB: f32 = 8.0;
const SPEECH_GATE_MARGIN_DB: f32 = 3.0;
const MINIMUM_VISIBLE_ACTIVE_LEVEL: f32 = 0.12;
const NOISE_GATE_NORMALIZED_THRESHOLD: f32 = 0.06;
const FLOOR_RISE_WINDOW_DB: f32 = 4.0;
const FLOOR_FALL_BLEND: f32 = 0.12;
const FLOOR_RISE_BLEND: f32 = 0.02;
const PEAK_ATTACK_BLEND: f32 = 0.55;
const PEAK_RELEASE_BLEND: f32 = 0.04;
const DISPLAY_ATTACK_BLEND: f32 = 0.45;
const DISPLAY_RELEASE_BLEND: f32 = 0.12;

pub struct LiveAudioLevelNormalizer {
    noise_floor_db: f32,
    peak_ceiling_db: f32,
    display_level: f32,
}

impl Default for LiveAudioLevelNormalizer {
    fn default() -> Self {
        Self {
            noise_floor_db: -55.0,
            peak_ceiling_db: -37.0,
            display_level: 0.0,
        }
    }
}

fn mix(current: f32, target: f32, blend: f32) -> f32 {
    current + (target - current) * blend
}

impl LiveAudioLevelNormalizer {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn normalized_level(&mut self, rms: f32) -> f32 {
        let level_db = 20.0 * rms.max(MINIMUM_RMS).log10();

        self.update_noise_floor(level_db);
        self.update_peak_ceiling(level_db);

        let display_ceiling_db = self.peak_ceiling_db + PEAK_HEADROOM_DB;
        let dynamic_span =
            (display_ceiling_db - self.noise_floor_db).max(MIN_SPAN_DB + PEAK_HEADROOM_DB);
        let mut normalized = ((level_db - self.noise_floor_db) / dynamic_span).clamp(0.0, 1.0);
        let is_active_speech = level_db >= self.noise_floor_db + SPEECH_GATE_MARGIN_DB;

        if normalized < NOISE_GATE_NORMALIZED_THRESHOLD
            && level_db <= self.noise_floor_db + SPEECH_GATE_MARGIN_DB
        {
            normalized = 0.0;
        } else if is_active_speech {
            normalized = normalized.max(MINIMUM_VISIBLE_ACTIVE_LEVEL);
        }

        let blend = if normalized > self.display_level {
            DISPLAY_ATTACK_BLEND
        } else {
            DISPLAY_RELEASE_BLEND
        };
        self.display_level = mix(self.display_level, normalized, blend);
        self.display_level
    }

    fn update_noise_floor(&mut self, level_db: f32) {
        let ceiling_limited_level = level_db.min(self.peak_ceiling_db - MIN_SPAN_DB);

        if ceiling_limited_level <= self.noise_floor_db {
            self.noise_floor_db = mix(self.noise_floor_db, ceiling_limited_level, FLOOR_FALL_BLEND);
        } else if ceiling_limited_level <= self.noise_floor_db + FLOOR_RISE_WINDOW_DB {
            self.noise_floor_db = mix(self.noise_floor_db, ceiling_limited_level, FLOOR_RISE_BLEND);
        }
    }

    fn update_peak_ceiling(&mut self, level_db: f32) {
        let minimum_ceiling = self.noise_floor_db + MIN_SPAN_DB;

        if level_db >= self.peak_ceiling_db {
            self.peak_ceiling_db = mix(self.peak_ceiling_db, level_db, PEAK_ATTACK_BLEND);
        } else {
            self.peak_ceiling_db = mix(
                self.peak_ceiling_db,
                level_db.max(minimum_ceiling),
                PEAK_RELEASE_BLEND,
            );
        }

        self.peak_ceiling_db = self.peak_ceiling_db.max(minimum_ceiling);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_stays_at_zero() {
        let mut n = LiveAudioLevelNormalizer::default();
        for _ in 0..50 {
            let level = n.normalized_level(0.00001);
            assert!(level <= 0.05, "silence should stay near zero, got {level}");
        }
    }

    #[test]
    fn speech_rises_above_visible_minimum() {
        let mut n = LiveAudioLevelNormalizer::default();
        // Simulate background noise then speech.
        for _ in 0..20 {
            n.normalized_level(0.0005);
        }
        let mut peak: f32 = 0.0;
        for _ in 0..20 {
            peak = peak.max(n.normalized_level(0.05));
        }
        assert!(peak >= 0.12, "speech should reach visible level, got {peak}");
    }

    #[test]
    fn output_is_always_in_unit_range() {
        let mut n = LiveAudioLevelNormalizer::default();
        for rms in [0.0, 0.00001, 0.001, 0.1, 0.5, 1.0, 2.0] {
            for _ in 0..10 {
                let v = n.normalized_level(rms);
                assert!((0.0..=1.0).contains(&v));
            }
        }
    }
}
