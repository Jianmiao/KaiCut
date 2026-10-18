#[cfg(windows)]
#[path = "support/compat_driver.rs"]
mod compat_driver;
#[cfg(windows)]
#[path = "support/stream_reference.rs"]
#[allow(dead_code)] // Only the exact PCM range verifier is needed here.
mod stream_reference;
#[cfg(windows)]
#[path = "support/streaming_decoder.rs"]
mod streaming_decoder;

#[cfg(windows)]
fn main() {
    if let Err(error) = compat_driver::run() {
        eprintln!("Native compatibility probe failed: {error}");
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Native compatibility probe requires Windows sidecars.");
    std::process::exit(1);
}
