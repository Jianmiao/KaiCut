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
#[path = "support/hidden_surface_host.rs"]
mod hidden_surface_host;
#[cfg(windows)]
#[path = "support/surface_driver.rs"]
mod surface_driver;

#[cfg(windows)]
fn main() {
    if let Err(error) = surface_driver::run() {
        eprintln!("Hidden surface validation failed: {error}");
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Hidden composition surface validation requires Windows.");
    std::process::exit(1);
}
