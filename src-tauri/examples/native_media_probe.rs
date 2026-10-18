//! Standalone decoded PCM/PTS probe; never opens a graphics or application window.
#[path = "support/native_media_driver.rs"]
mod native_media_driver;

fn main() {
    native_media_driver::run(false);
}
