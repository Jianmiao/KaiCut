#[cfg(windows)]
#[path = "support/composition_presenter.rs"]
mod composition_presenter;
#[cfg(windows)]
#[path = "support/d3d11_renderer.rs"]
mod d3d11_renderer;
#[cfg(windows)]
#[path = "support/frame_verification.rs"]
mod frame_verification;
#[cfg(windows)]
#[path = "support/present_driver.rs"]
mod present_driver;
#[cfg(windows)]
#[path = "support/presentation_gate.rs"]
mod presentation_gate;

#[cfg(windows)]
fn main() {
    if let Err(error) = present_driver::run() {
        eprintln!("Composition presentation validation failed: {error}");
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Composition presentation probe requires Windows.");
    std::process::exit(1);
}
