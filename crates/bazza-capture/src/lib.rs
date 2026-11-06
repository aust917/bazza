//! Audio (and screen) capture for Bazza.
//!
//! * `devices` - enumerate mics / outputs (outputs double as loopback sources)
//! * `recorder` - mic + system audio to WAV, plus a live 16 kHz mono feed
//! * `video` - ffmpeg sidecar argument builders and process control
//! * `probe` - short level sampling for the audio-activity detector

pub mod capture_environment;
pub mod devices;
pub mod dsp;
pub mod monitors;
#[cfg(windows)]
mod owned_capture_child;
#[cfg(windows)]
pub mod owned_media_child;
pub mod probe;
#[cfg(windows)]
pub mod process_loopback;
pub mod recorder;
pub mod reference_residual;
pub mod speech_alignment;
pub mod stream;
pub mod video;
pub mod wav;

pub use devices::{find_device, input_devices, output_devices, AudioDevice};
pub use monitors::{list_monitors, MonitorInfo};
#[cfg(windows)]
pub use owned_media_child::{OwnedMediaChild, OwnedMediaStderr, OwnedMediaStdout};
pub use recorder::{
    CaptureEvent, RecordConfig, RecordError, RecordOutput, Recorder, ARCHIVE_HZ, ASR_HZ,
};
pub use video::{VideoRecorder, VideoTarget};
pub use wav::WavWriter;
