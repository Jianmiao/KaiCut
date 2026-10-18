#[cfg(windows)]
#[path = "support/async_renderer.rs"]
mod async_renderer;
#[cfg(windows)]
#[path = "support/audio_cursor.rs"]
mod audio_cursor;
#[cfg(windows)]
#[path = "support/audio_preview_adapter.rs"]
mod audio_preview_adapter;
#[cfg(windows)]
#[path = "support/audio_scheduling.rs"]
mod audio_scheduling;
#[cfg(windows)]
#[path = "support/audio_session_driver.rs"]
#[allow(dead_code)]
mod audio_session_driver;
#[cfg(windows)]
#[path = "support/audio_timeline.rs"]
mod audio_timeline;
#[cfg(windows)]
#[path = "support/audio_timing_driver.rs"]
#[allow(dead_code)]
mod audio_timing_driver;
#[cfg(windows)]
#[path = "support/audio_transfer.rs"]
#[allow(dead_code)]
mod audio_transfer;
#[cfg(windows)]
#[path = "support/av_session_driver.rs"]
mod av_session_driver;
#[cfg(windows)]
#[path = "support/d3d11_renderer.rs"]
mod d3d11_renderer;
#[cfg(windows)]
#[path = "support/frame_verification.rs"]
#[allow(dead_code)]
mod frame_verification;
#[cfg(windows)]
#[path = "support/stream_reference.rs"]
#[allow(dead_code)]
mod stream_reference;
#[cfg(windows)]
#[path = "support/streaming_decoder.rs"]
mod streaming_decoder;
#[cfg(windows)]
#[path = "support/wasapi_output.rs"]
#[allow(dead_code)]
mod wasapi_output;

#[cfg(windows)]
fn main() {
    if let Err(error) = av_session_driver::run() {
        eprintln!("AV session validation failed: {error}");
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("AV session validation requires Windows.");
    std::process::exit(1);
}
