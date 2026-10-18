#[cfg(windows)]
#[path = "support/audio_timeline.rs"]
mod audio_timeline;
#[cfg(windows)]
#[path = "support/audio_timing_driver.rs"]
mod audio_timing_driver;
#[cfg(windows)]
#[path = "support/stream_reference.rs"]
#[allow(dead_code)]
mod stream_reference;
#[cfg(windows)]
#[path = "support/streaming_decoder.rs"]
mod streaming_decoder;

#[cfg(windows)]
fn main() {
    if let Err(error) = audio_timing_driver::run() {
        eprintln!("Audio timing probe failed: {error}");
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Audio timing probe requires Windows sidecars.");
    std::process::exit(1);
}
