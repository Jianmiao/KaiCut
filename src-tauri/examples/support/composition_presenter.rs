//! Thread-owned DXGI composition swap chain. The native-surface-probe feature
//! additionally permits explicit attachment to a borrowed same-process HWND.
//! Present acceptance is API submission evidence, never proof of screen output.

#[cfg(feature = "native-surface-probe")]
#[path = "composition_target.rs"]
mod composition_target;
#[cfg(feature = "native-surface-probe")]
pub use composition_target::{AttachResult, BindingInfo, DetachResult};
#[cfg(feature = "native-surface-probe")]
use windows::Win32::Foundation::HWND;

pub use super::d3d11_renderer::source_fit::{FitRect, fit_rect, nearest_letterbox_rgba};
use super::d3d11_renderer::{D3d11Renderer, RenderResult, RendererInfo};
use serde::Serialize;
use std::{
    marker::PhantomData,
    rc::Rc,
    thread::{self, ThreadId},
    time::Instant,
};
use windows::{
    Win32::{
        Foundation::DXGI_STATUS_OCCLUDED,
        Graphics::{
            Direct3D11::ID3D11Texture2D,
            Dxgi::{
                Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
                DXGI_ERROR_WAS_STILL_DRAWING, DXGI_PRESENT_DO_NOT_WAIT, DXGI_SCALING_STRETCH,
                DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGISwapChain, IDXGISwapChain1,
            },
        },
    },
    core::{HRESULT, Interface},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PresentDisposition {
    Accepted,
    /// Retained for truthful raw-HRESULT reporting, though flip-model chains
    /// are not documented to return OCCLUDED. Do not infer visibility from it.
    Occluded,
    NotReady,
    OtherStatus,
    Suspended,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenterInfo {
    pub renderer: RendererInfo,
    pub width: u32,
    pub height: u32,
    pub buffer_count: u32,
    pub format: &'static str,
    pub swap_effect: &'static str,
    pub scaling: &'static str,
    pub alpha_mode: &'static str,
    pub composition_attached: bool,
    pub suspended: bool,
}

pub struct PresentResult {
    pub hresult: Option<i32>,
    pub disposition: PresentDisposition,
    pub present_called: bool,
    pub requested_width: u32,
    pub requested_height: u32,
    pub upload_draw_ms: f64,
    pub present_call_ms: f64,
    /// Actual swap-chain backbuffer sampled before this Present call. Pixels
    /// are evidence of drawing only; they are not a captured displayed image.
    pub diagnostic: Option<RenderResult>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResizeResult {
    pub width: u32,
    pub height: u32,
    pub suspended: bool,
    pub resize_buffers_called: bool,
    pub elapsed_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizePolicy {
    Suspend,
    Resize { width: u32, height: u32 },
}

/// All construction, attachment, drawing, resize and destruction stay on the
/// creating thread. Unattached unless attach_to_window is explicitly called.
/// There is no WARP fallback or recovery that hides a device-removal failure.
pub struct CompositionPresenter {
    #[cfg(feature = "native-surface-probe")]
    binding: Option<composition_target::CompositionTarget>,
    #[cfg(feature = "native-surface-probe")]
    last_binding_info: BindingInfo,
    // Drop swap-chain references before the owning D3D device/context.
    chain: IDXGISwapChain1,
    renderer: D3d11Renderer,
    owner: ThreadId,
    requested_width: u32,
    requested_height: u32,
    suspended: bool,
    failed: Option<String>,
    _not_send: PhantomData<Rc<()>>,
}

impl CompositionPresenter {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        let description = composition_description(width, height)?;
        let renderer = D3d11Renderer::new(width, height)?;
        let chain = renderer.composition_swap_chain(&description)?;
        Ok(Self {
            #[cfg(feature = "native-surface-probe")]
            binding: None,
            #[cfg(feature = "native-surface-probe")]
            last_binding_info: BindingInfo::default(),
            chain,
            renderer,
            owner: thread::current().id(),
            requested_width: width,
            requested_height: height,
            suspended: false,
            failed: None,
            _not_send: PhantomData,
        })
    }

    pub fn info(&self) -> &RendererInfo {
        self.renderer.info()
    }

    pub fn composition_info(&self) -> PresenterInfo {
        PresenterInfo {
            renderer: self.renderer.info().clone(),
            width: self.requested_width,
            height: self.requested_height,
            buffer_count: 2,
            format: "R8G8B8A8_UNORM",
            swap_effect: "FLIP_SEQUENTIAL",
            scaling: "STRETCH",
            alpha_mode: "IGNORE",
            composition_attached: self.is_attached(),
            suspended: self.suspended,
        }
    }

    fn is_attached(&self) -> bool {
        #[cfg(feature = "native-surface-probe")]
        {
            self.binding_info().attached
        }
        #[cfg(not(feature = "native-surface-probe"))]
        {
            false
        }
    }

    #[cfg(feature = "native-surface-probe")]
    pub fn binding_info(&self) -> BindingInfo {
        self.binding
            .as_ref()
            .map_or_else(|| self.last_binding_info.clone(), |binding| binding.info())
    }

    /// Borrows a live, same-process HWND. Caller must detach/drop this presenter
    /// before destroying that window. A successful Commit is not scanout proof.
    #[cfg(feature = "native-surface-probe")]
    pub fn attach_to_window(&mut self, window: HWND) -> Result<AttachResult, String> {
        self.check_owner()?;
        composition_target::allow_attach(self.binding.is_some())?;
        let dxgi = self.renderer.composition_dxgi_device()?;
        let (binding, result) =
            composition_target::CompositionTarget::attach(&dxgi, &self.chain, window)?;
        self.binding = Some(binding);
        Ok(result)
    }

    /// Cleanup remains available after a draw/resize device failure. A failed
    /// detach retains ownership and its diagnostic so the caller can retry.
    #[cfg(feature = "native-surface-probe")]
    pub fn detach_from_window(&mut self) -> Result<DetachResult, String> {
        if thread::current().id() != self.owner {
            return Err("Composition presenter must stay on its creating thread".into());
        }
        let Some(binding) = self.binding.as_mut() else {
            return Ok(DetachResult::already_detached());
        };
        let result = binding.detach()?;
        self.last_binding_info = binding.info();
        self.binding = None;
        Ok(result)
    }

    pub fn render_and_present(
        &mut self,
        rgb: &[u8],
        diagnostic_readback: bool,
    ) -> Result<PresentResult, String> {
        self.render_frame(rgb, None, diagnostic_readback)
    }

    /// Render source-sized RGB using GPU integer aspect-fit with opaque black
    /// bars. Source texture/shaders stay cached and normal frames have no GPU
    /// readback. Diagnostic samples copy the same actual backbuffer as Present.
    pub fn render_source_and_present(
        &mut self,
        rgb: &[u8],
        source_width: u32,
        source_height: u32,
        diagnostic: bool,
    ) -> Result<PresentResult, String> {
        self.render_frame(rgb, Some((source_width, source_height)), diagnostic)
    }

    fn render_frame(
        &mut self,
        rgb: &[u8],
        source_dimensions: Option<(u32, u32)>,
        diagnostic_readback: bool,
    ) -> Result<PresentResult, String> {
        self.check_owner()?;
        if self.suspended {
            return Ok(PresentResult {
                hresult: None,
                disposition: PresentDisposition::Suspended,
                present_called: false,
                requested_width: self.requested_width,
                requested_height: self.requested_height,
                upload_draw_ms: 0.0,
                present_call_ms: 0.0,
                diagnostic: None,
            });
        }
        let result: Result<PresentResult, String> = (|| {
            // Acquire a fresh buffer after each flip. No persistent RTV or
            // backbuffer reference can obstruct ResizeBuffers later.
            let rendered = {
                let target: ID3D11Texture2D =
                    unsafe { self.chain.GetBuffer(0) }.map_err(|error| {
                        self.renderer
                            .device_failure("Get composition backbuffer", error)
                    })?;
                if let Some((width, height)) = source_dimensions {
                    self.renderer.render_source_to_texture(
                        &target,
                        rgb,
                        width,
                        height,
                        diagnostic_readback,
                    )?
                } else {
                    self.renderer
                        .render_rgb_to_texture(&target, rgb, diagnostic_readback)?
                }
            };
            let base: IDXGISwapChain = self
                .chain
                .cast()
                .map_err(|error| self.renderer.device_failure("Query base swap chain", error))?;
            let started = Instant::now();
            // Call raw HRESULT to retain every success/status code. There is no
            // TEST flag. Attachment is explicit and never implies visibility.
            let status = unsafe {
                (Interface::vtable(&base).Present)(
                    Interface::as_raw(&base),
                    0,
                    DXGI_PRESENT_DO_NOT_WAIT,
                )
            };
            let present_call_ms = started.elapsed().as_secs_f64() * 1000.0;
            let disposition = classify_present(status).map_err(|error| {
                self.renderer
                    .device_failure("Present composition swap chain", error)
            })?;
            Ok(PresentResult {
                hresult: Some(status.0),
                disposition,
                present_called: true,
                requested_width: self.requested_width,
                requested_height: self.requested_height,
                upload_draw_ms: rendered.upload_draw_ms,
                present_call_ms,
                diagnostic: rendered.diagnostic,
            })
        })();
        if let Err(error) = &result {
            self.failed = Some(error.clone());
        }
        result
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<ResizeResult, String> {
        self.check_owner()?;
        let policy = resize_policy(width, height)?;
        let started = Instant::now();
        if policy == ResizePolicy::Suspend {
            self.requested_width = width;
            self.requested_height = height;
            self.suspended = true;
            return Ok(ResizeResult {
                width,
                height,
                suspended: true,
                resize_buffers_called: false,
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            });
        }
        // All GetBuffer/RTV values were frame-local. Clear context bindings and
        // release them before ResizeBuffers; shaders are rebound by each draw.
        self.renderer.clear_bindings();
        let result: Result<ResizeResult, String> = (|| {
            unsafe {
                self.chain.ResizeBuffers(
                    2,
                    width,
                    height,
                    DXGI_FORMAT_R8G8B8A8_UNORM,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )
            }
            .map_err(|error| {
                self.renderer
                    .device_failure("ResizeBuffers composition swap chain", error)
            })?;
            self.renderer.resize_resources(width, height)?;
            self.requested_width = width;
            self.requested_height = height;
            self.suspended = false;
            Ok(ResizeResult {
                width,
                height,
                suspended: false,
                resize_buffers_called: true,
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            })
        })();
        if let Err(error) = &result {
            self.failed = Some(error.clone());
        }
        result
    }

    fn check_owner(&self) -> Result<(), String> {
        if thread::current().id() != self.owner {
            return Err("Composition presenter must stay on its creating thread".into());
        }
        if let Some(error) = &self.failed {
            return Err(format!(
                "Composition presenter is terminal after failure: {error}"
            ));
        }
        Ok(())
    }
}

impl Drop for CompositionPresenter {
    fn drop(&mut self) {
        #[cfg(feature = "native-surface-probe")]
        {
            let _ = self.detach_from_window();
            // Even if explicit detach failed, release DComp references while
            // the window and D3D device are still owned by their callers.
            self.binding = None;
        }
        self.renderer.clear_bindings();
    }
}

fn classify_present(status: HRESULT) -> Result<PresentDisposition, String> {
    if status == HRESULT(0) {
        Ok(PresentDisposition::Accepted)
    } else if status == DXGI_STATUS_OCCLUDED {
        Ok(PresentDisposition::Occluded)
    } else if status == DXGI_ERROR_WAS_STILL_DRAWING {
        Ok(PresentDisposition::NotReady)
    } else if status.is_err() {
        Err(format!(
            "Present returned failure HRESULT 0x{:08X}",
            status.0 as u32
        ))
    } else {
        Ok(PresentDisposition::OtherStatus)
    }
}

fn resize_policy(width: u32, height: u32) -> Result<ResizePolicy, String> {
    if width > 1920 || height > 1080 {
        return Err("Composition surface is bounded to 1920x1080".into());
    }
    if width == 0 || height == 0 {
        Ok(ResizePolicy::Suspend)
    } else {
        Ok(ResizePolicy::Resize { width, height })
    }
}

fn composition_description(width: u32, height: u32) -> Result<DXGI_SWAP_CHAIN_DESC1, String> {
    if !matches!(resize_policy(width, height)?, ResizePolicy::Resize { .. }) {
        return Err("Composition creation requires nonzero dimensions".into());
    }
    Ok(DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Dxgi::{DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET};

    #[test]
    fn raw_present_status_keeps_occlusion_not_ready_and_other_success_distinct() {
        assert_eq!(
            classify_present(HRESULT(0)).unwrap(),
            PresentDisposition::Accepted
        );
        assert_eq!(
            classify_present(DXGI_STATUS_OCCLUDED).unwrap(),
            PresentDisposition::Occluded
        );
        assert_eq!(
            classify_present(DXGI_ERROR_WAS_STILL_DRAWING).unwrap(),
            PresentDisposition::NotReady
        );
        assert_eq!(
            classify_present(HRESULT(1)).unwrap(),
            PresentDisposition::OtherStatus
        );
        assert!(classify_present(DXGI_ERROR_DEVICE_REMOVED).is_err());
        assert!(classify_present(DXGI_ERROR_DEVICE_RESET).is_err());
        assert!(classify_present(HRESULT(0x80004005_u32 as i32)).is_err());
    }

    #[test]
    fn resizing_zero_area_suspends_without_passing_zero_dimensions_to_dxgi() {
        assert_eq!(resize_policy(0, 0).unwrap(), ResizePolicy::Suspend);
        assert_eq!(resize_policy(0, 1080).unwrap(), ResizePolicy::Suspend);
        assert_eq!(resize_policy(1920, 0).unwrap(), ResizePolicy::Suspend);
        assert_eq!(
            resize_policy(161, 91).unwrap(),
            ResizePolicy::Resize {
                width: 161,
                height: 91
            }
        );
        for (width, height) in [(1921, 1080), (1920, 1081), (u32::MAX, 0)] {
            assert!(resize_policy(width, height).is_err());
        }
    }

    #[test]
    fn composition_description_is_flip_sequential_single_sample_and_unattached() {
        let desc = composition_description(161, 91).unwrap();
        assert_eq!((desc.Width, desc.Height), (161, 91));
        assert_eq!(desc.Format, DXGI_FORMAT_R8G8B8A8_UNORM);
        assert_eq!(desc.SampleDesc.Count, 1);
        assert_eq!(desc.SampleDesc.Quality, 0);
        assert_eq!(desc.BufferCount, 2);
        assert_eq!(desc.Scaling, DXGI_SCALING_STRETCH);
        assert_eq!(desc.SwapEffect, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL);
        assert_eq!(desc.AlphaMode, DXGI_ALPHA_MODE_IGNORE);
        assert_eq!(desc.Flags, 0);
        assert!(composition_description(0, 90).is_err());
        assert!(composition_description(1921, 90).is_err());
    }
}
