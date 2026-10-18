//! Explicit background audio/render concurrency probe; no window or Present.
#[path = "support/native_media_driver.rs"]
mod native_media_driver;

fn main() {
    native_media_driver::run_async();
}
