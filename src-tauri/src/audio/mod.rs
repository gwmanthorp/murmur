pub mod normalizer;
pub mod recorder;
pub mod resample;

pub use recorder::{list_input_devices, RecordingHandle};

pub const TARGET_SAMPLE_RATE: u32 = 16_000;
