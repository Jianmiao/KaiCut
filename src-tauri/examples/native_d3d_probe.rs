//! Standalone D3D11 offscreen rendering probe. No swap chain or desktop control.
#[path = "support/native_media_driver.rs"]
mod native_media_driver;

fn main() {
    native_media_driver::run(true);
}
