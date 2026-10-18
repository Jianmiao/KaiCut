#[cfg(windows)]
#[path = "support/async_renderer.rs"]
mod async_renderer;
#[cfg(windows)]
#[path = "support/audio_scheduling.rs"]
mod audio_scheduling;
#[cfg(windows)]
#[path = "support/d3d11_renderer.rs"]
mod d3d11_renderer;
#[cfg(windows)]
#[path = "support/frame_verification.rs"]
#[allow(dead_code)] // Calibration helpers are exercised by the D3D probe/tests.
mod frame_verification;
#[cfg(windows)]
#[path = "support/hd_reference.rs"]
mod hd_reference;
#[cfg(windows)]
#[path = "support/presentation_clock.rs"]
#[allow(dead_code)] // Shared probe module also contains the synthetic scheduler.
mod presentation_clock;
#[cfg(windows)]
#[path = "support/preview_session.rs"]
mod preview_session;
#[cfg(windows)]
#[path = "support/stream_reference.rs"]
mod stream_reference;
#[cfg(windows)]
#[path = "support/streaming_decoder.rs"]
mod streaming_decoder;
#[cfg(windows)]
#[path = "support/streaming_driver.rs"]
mod streaming_driver;
#[cfg(windows)]
#[path = "support/wasapi_output.rs"]
#[allow(dead_code)] // The separate clock probe uses the silent output constructor.
mod wasapi_output;

#[cfg(windows)]
fn main() {
    if let Err(error) = streaming_driver::run() {
        eprintln!("Native streaming probe failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Native streaming probe requires Windows.");
    std::process::exit(1);
}
