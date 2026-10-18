//! Hardware-only shader rendering for diagnostic examples. Render targets may
//! be offscreen textures or caller-provided composition swap-chain buffers.

#[path = "source_fit.rs"]
pub mod source_fit;

use source_fit::{FitRect, fit_rect};
use std::{
    marker::PhantomData,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::{
                D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0,
                D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
                Fxc::{D3DCOMPILE_ENABLE_STRICTNESS, D3DCompile},
                ID3DBlob,
            },
            Direct3D11::*,
            Dxgi::{
                Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
                DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_WAS_STILL_DRAWING, DXGI_SWAP_CHAIN_DESC1,
                IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory2, IDXGIOutput,
                IDXGISwapChain1,
            },
        },
    },
    core::{HRESULT, Interface, PCSTR, s},
};

const READBACK_TIMEOUT: Duration = Duration::from_secs(2);
const VERTEX_SHADER: &str = r#"
float4 main(uint vertexId : SV_VertexID) : SV_Position {
    float2 uv = float2((vertexId << 1) & 2, vertexId & 2);
    return float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
}
"#;
const PIXEL_SHADER: &str = r#"
Texture2D<float4> sourceTexture : register(t0);
float4 main(float4 position : SV_Position) : SV_Target {
    return sourceTexture.Load(int3(uint2(position.xy), 0));
}
"#;
const FIT_PIXEL_SHADER: &str = r#"
Texture2D<float4> sourceTexture : register(t0);
cbuffer FitGeometry : register(b0) {
    uint2 sourceSize;
    uint2 fitOrigin;
    uint2 fitSize;
    uint2 padding;
};
float4 main(float4 position : SV_Position) : SV_Target {
    uint2 pixel = uint2(position.xy);
    if (any(pixel < fitOrigin) || any(pixel >= fitOrigin + fitSize)) {
        return float4(0.0, 0.0, 0.0, 1.0);
    }
    uint2 sourcePixel = ((pixel - fitOrigin) * sourceSize) / fitSize;
    return sourceTexture.Load(int3(sourcePixel, 0));
}
"#;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RendererInfo {
    pub adapter_description: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub adapter_flags: u32,
    pub luid_low: u32,
    pub luid_high: i32,
    pub feature_level: u32,
    pub width: u32,
    pub height: u32,
}

pub struct RenderResult {
    pub rgba: Vec<u8>,
    pub row_pitch: u32,
    /// CPU wall time including upload, queueing, draw, wait, and row copy.
    /// This is not a GPU timestamp or screen-presentation measurement.
    pub upload_draw_readback_ms: f64,
    pub readback_wait_ms: f64,
}

pub(crate) struct SurfaceRenderResult {
    /// CPU upload/draw submission cost; this is not GPU execution time.
    pub upload_draw_ms: f64,
    pub diagnostic: Option<RenderResult>,
}

struct SizedResources {
    target: ID3D11Texture2D,
    target_view: ID3D11RenderTargetView,
    staging: ID3D11Texture2D,
}

struct SourceResources {
    texture: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
    width: u32,
    height: u32,
}

pub struct D3d11Renderer {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    // Kept across target-only resizes; recreated only when source size changes.
    source: Option<SourceResources>,
    target: ID3D11Texture2D,
    target_view: ID3D11RenderTargetView,
    staging: ID3D11Texture2D,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    fit_pixel_shader: ID3D11PixelShader,
    fit_constants: ID3D11Buffer,
    rasterizer: ID3D11RasterizerState,
    completion: ID3D11Query,
    info: RendererInfo,
    failed: bool,
    _thread_affinity: PhantomData<Rc<()>>,
}

fn gpu_error(device: &ID3D11Device, operation: &str, error: impl std::fmt::Display) -> String {
    let removed = unsafe { device.GetDeviceRemovedReason() };
    match removed {
        Ok(()) => format!("{operation}: {error}; device removed reason S_OK"),
        Err(reason) => format!("{operation}: {error}; device removed reason: {reason}"),
    }
}

fn shader(source: &str, profile: PCSTR) -> Result<Vec<u8>, String> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            s!("kaicut-fixed-offscreen-shader"),
            None,
            None,
            s!("main"),
            profile,
            D3DCOMPILE_ENABLE_STRICTNESS,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(error) = result {
        let details = errors
            .as_ref()
            .map(|blob| unsafe {
                let length = blob.GetBufferSize().min(4096);
                let pointer = blob.GetBufferPointer().cast::<u8>();
                if pointer.is_null() || length == 0 {
                    String::new()
                } else {
                    String::from_utf8_lossy(std::slice::from_raw_parts(pointer, length))
                        .into_owned()
                }
            })
            .unwrap_or_default();
        return Err(format!("Fixed HLSL compilation failed: {error}: {details}"));
    }
    let code = code.ok_or("Shader compilation returned no bytecode")?;
    let (pointer, length) = unsafe { (code.GetBufferPointer().cast::<u8>(), code.GetBufferSize()) };
    if pointer.is_null() || length == 0 || length > 1024 * 1024 {
        return Err("Compiled shader bytecode has an invalid size".into());
    }
    Ok(unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec())
}

fn texture(device: &ID3D11Device, desc: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D, String> {
    let mut output = None;
    unsafe { device.CreateTexture2D(desc, None, Some(&mut output)) }
        .map_err(|error| gpu_error(device, "CreateTexture2D", error))?;
    output.ok_or_else(|| "CreateTexture2D returned no texture".into())
}

fn sized_resources(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<SizedResources, String> {
    image_bytes(width, height, 4)?;
    let base = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        ..Default::default()
    };
    let target = texture(
        device,
        &D3D11_TEXTURE2D_DESC {
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..base
        },
    )?;
    let staging = texture(
        device,
        &D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..base
        },
    )?;
    let mut target_view = None;
    unsafe { device.CreateRenderTargetView(&target, None, Some(&mut target_view)) }
        .map_err(|error| gpu_error(device, "CreateRenderTargetView", error))?;
    Ok(SizedResources {
        target,
        staging,
        target_view: target_view.ok_or("No render target view")?,
    })
}

fn source_resources(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<SourceResources, String> {
    image_bytes(width, height, 4)?;
    let texture = texture(
        device,
        &D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        },
    )?;
    let mut view = None;
    unsafe { device.CreateShaderResourceView(&texture, None, Some(&mut view)) }
        .map_err(|error| gpu_error(device, "Create source shader resource view", error))?;
    Ok(SourceResources {
        texture,
        view: view.ok_or("No source shader resource view")?,
        width,
        height,
    })
}

struct MappedTexture<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
}

impl Drop for MappedTexture<'_> {
    fn drop(&mut self) {
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}

impl D3d11Renderer {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        image_bytes(width, height, 4)?;
        let mut device = None;
        let mut context = None;
        let mut feature_level = D3D_FEATURE_LEVEL_11_0;
        // A null adapter plus HARDWARE selects the system's default adapter.
        // No WARP/reference fallback is allowed to masquerade as hardware.
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )
        }
        .map_err(|error| {
            format!("Cannot create hardware D3D11 device (feature level 11 required): {error}")
        })?;
        let device: ID3D11Device = device.ok_or("D3D11CreateDevice returned no device")?;
        let context = context.ok_or("D3D11CreateDevice returned no immediate context")?;
        let dxgi: IDXGIDevice = device
            .cast()
            .map_err(|error| gpu_error(&device, "Query IDXGIDevice", error))?;
        let adapter = unsafe { dxgi.GetAdapter() }
            .map_err(|error| gpu_error(&device, "GetAdapter", error))?;
        let adapter1: IDXGIAdapter1 = adapter
            .cast()
            .map_err(|error| gpu_error(&device, "Query IDXGIAdapter1", error))?;
        let desc = unsafe { adapter1.GetDesc1() }
            .map_err(|error| gpu_error(&device, "GetDesc1", error))?;
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            return Err(
                "Offscreen probe requires a hardware adapter; software adapters are rejected"
                    .into(),
            );
        }
        let info = RendererInfo {
            adapter_description: String::from_utf16_lossy(
                &desc.Description[..desc
                    .Description
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(desc.Description.len())],
            ),
            vendor_id: desc.VendorId,
            device_id: desc.DeviceId,
            adapter_flags: desc.Flags,
            luid_low: desc.AdapterLuid.LowPart,
            luid_high: desc.AdapterLuid.HighPart,
            feature_level: feature_level.0 as u32,
            width,
            height,
        };
        let resources = sized_resources(&device, width, height)?;
        let vs = shader(VERTEX_SHADER, s!("vs_5_0"))?;
        let ps = shader(PIXEL_SHADER, s!("ps_5_0"))?;
        let fit_ps = shader(FIT_PIXEL_SHADER, s!("ps_5_0"))?;
        let mut vertex_shader = None;
        unsafe { device.CreateVertexShader(&vs, None, Some(&mut vertex_shader)) }
            .map_err(|error| gpu_error(&device, "CreateVertexShader", error))?;
        let mut pixel_shader = None;
        unsafe { device.CreatePixelShader(&ps, None, Some(&mut pixel_shader)) }
            .map_err(|error| gpu_error(&device, "CreatePixelShader", error))?;
        let mut fit_pixel_shader = None;
        unsafe { device.CreatePixelShader(&fit_ps, None, Some(&mut fit_pixel_shader)) }
            .map_err(|error| gpu_error(&device, "Create aspect-fit pixel shader", error))?;
        let mut fit_constants = None;
        unsafe {
            device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 32,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut fit_constants),
            )
        }
        .map_err(|error| gpu_error(&device, "Create aspect-fit constants", error))?;
        let mut rasterizer = None;
        unsafe {
            device.CreateRasterizerState(
                &D3D11_RASTERIZER_DESC {
                    FillMode: D3D11_FILL_SOLID,
                    CullMode: D3D11_CULL_NONE,
                    DepthClipEnable: true.into(),
                    ..Default::default()
                },
                Some(&mut rasterizer),
            )
        }
        .map_err(|error| gpu_error(&device, "CreateRasterizerState", error))?;
        let mut completion = None;
        unsafe {
            device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut completion),
            )
        }
        .map_err(|error| gpu_error(&device, "CreateQuery(EVENT)", error))?;
        Ok(Self {
            device,
            context,
            source: None,
            target: resources.target,
            staging: resources.staging,
            target_view: resources.target_view,
            vertex_shader: vertex_shader.ok_or("No vertex shader")?,
            pixel_shader: pixel_shader.ok_or("No pixel shader")?,
            fit_pixel_shader: fit_pixel_shader.ok_or("No aspect-fit pixel shader")?,
            fit_constants: fit_constants.ok_or("No aspect-fit constant buffer")?,
            rasterizer: rasterizer.ok_or("No rasterizer state")?,
            completion: completion.ok_or("No completion query")?,
            info,
            failed: false,
            _thread_affinity: PhantomData,
        })
    }

    pub fn info(&self) -> &RendererInfo {
        &self.info
    }

    pub(crate) fn composition_swap_chain(
        &self,
        desc: &DXGI_SWAP_CHAIN_DESC1,
    ) -> Result<IDXGISwapChain1, String> {
        let dxgi: IDXGIDevice = self
            .device
            .cast()
            .map_err(|error| gpu_error(&self.device, "Query IDXGIDevice", error))?;
        let adapter = unsafe { dxgi.GetAdapter() }
            .map_err(|error| gpu_error(&self.device, "GetAdapter", error))?;
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }
            .map_err(|error| gpu_error(&self.device, "Get IDXGIFactory2", error))?;
        unsafe { factory.CreateSwapChainForComposition(&self.device, desc, None::<&IDXGIOutput>) }
            .map_err(|error| gpu_error(&self.device, "CreateSwapChainForComposition", error))
    }

    #[cfg(feature = "native-surface-probe")]
    pub(crate) fn composition_dxgi_device(&self) -> Result<IDXGIDevice, String> {
        self.device
            .cast()
            .map_err(|error| gpu_error(&self.device, "Query composition IDXGIDevice", error))
    }

    pub(crate) fn device_failure(&self, operation: &str, status: impl std::fmt::Display) -> String {
        gpu_error(&self.device, operation, status)
    }

    pub(crate) fn clear_bindings(&self) {
        unsafe {
            self.context.ClearState();
            self.context.Flush();
        }
    }

    /// Recreate target-size offscreen/readback resources while retaining source
    /// texture, device, context, compiled shaders and the fit constant buffer.
    pub(crate) fn resize_resources(&mut self, width: u32, height: u32) -> Result<(), String> {
        if self.failed {
            return Err("Cannot resize a failed renderer".into());
        }
        image_bytes(width, height, 4)?;
        let resources = match sized_resources(&self.device, width, height) {
            Ok(resources) => resources,
            Err(error) => {
                self.failed = true;
                return Err(error);
            }
        };
        self.clear_bindings();
        self.target = resources.target;
        self.target_view = resources.target_view;
        self.staging = resources.staging;
        self.info.width = width;
        self.info.height = height;
        Ok(())
    }

    /// Shared upload/shader path targeting the caller's actual RGBA8 backbuffer.
    /// The target view is temporary and unbound before returning. Diagnostic
    /// readback copies this target, never the renderer's old offscreen target.
    pub(crate) fn render_rgb_to_texture(
        &mut self,
        target: &ID3D11Texture2D,
        rgb: &[u8],
        readback: bool,
    ) -> Result<SurfaceRenderResult, String> {
        self.render_to_texture(
            target,
            rgb,
            self.info.width,
            self.info.height,
            readback,
            None,
        )
    }

    /// Draw a source-sized RGB frame into the caller's full-sized backbuffer.
    /// Scaling and opaque black letterboxing happen in the pixel shader; the
    /// CPU only expands RGB to RGBA before uploading at source resolution.
    pub(crate) fn render_source_to_texture(
        &mut self,
        target: &ID3D11Texture2D,
        rgb: &[u8],
        source_width: u32,
        source_height: u32,
        readback: bool,
    ) -> Result<SurfaceRenderResult, String> {
        let fit = fit_rect(
            source_width,
            source_height,
            self.info.width,
            self.info.height,
        )?;
        self.render_to_texture(
            target,
            rgb,
            source_width,
            source_height,
            readback,
            Some(fit),
        )
    }

    fn render_to_texture(
        &mut self,
        target: &ID3D11Texture2D,
        rgb: &[u8],
        source_width: u32,
        source_height: u32,
        readback: bool,
        fit: Option<FitRect>,
    ) -> Result<SurfaceRenderResult, String> {
        if self.failed {
            return Err("Renderer is unavailable after a previous GPU/readback failure".into());
        }
        let started = Instant::now();
        let rgba = rgb_to_rgba(rgb, source_width, source_height)?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { target.GetDesc(&mut desc) };
        if desc.Width != self.info.width
            || desc.Height != self.info.height
            || desc.Format != DXGI_FORMAT_R8G8B8A8_UNORM
            || desc.SampleDesc.Count != 1
            || desc.ArraySize != 1
            || desc.MipLevels != 1
        {
            return Err(
                "External render target does not match the validated RGBA8 geometry".into(),
            );
        }
        let result = (|| {
            self.ensure_source(source_width, source_height)?;
            let mut view = None;
            unsafe {
                self.device
                    .CreateRenderTargetView(target, None, Some(&mut view))
            }
            .map_err(|error| {
                gpu_error(&self.device, "Create backbuffer render target view", error)
            })?;
            let view = view.ok_or("No backbuffer render target view")?;
            self.draw(&rgba, &view, fit)?;
            let upload_draw_ms = started.elapsed().as_secs_f64() * 1000.0;
            let diagnostic = if readback {
                Some(self.readback(target, started)?)
            } else {
                None
            };
            // Query the removal reason without waiting for GPU completion.
            unsafe { self.device.GetDeviceRemovedReason() }
                .map_err(|error| gpu_error(&self.device, "Draw target device health", error))?;
            Ok(SurfaceRenderResult {
                upload_draw_ms,
                diagnostic,
            })
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    pub fn render_rgb(&mut self, rgb: &[u8]) -> Result<RenderResult, String> {
        if self.failed {
            return Err("Renderer is unavailable after a previous GPU/readback failure; create a new instance".into());
        }
        let started = Instant::now();
        let rgba = rgb_to_rgba(rgb, self.info.width, self.info.height)?;
        let result = (|| {
            self.ensure_source(self.info.width, self.info.height)?;
            self.draw_and_readback(&rgba, started)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn draw_and_readback(&self, rgba: &[u8], started: Instant) -> Result<RenderResult, String> {
        self.draw(rgba, &self.target_view, None)?;
        self.readback(&self.target, started)
    }

    fn ensure_source(&mut self, width: u32, height: u32) -> Result<(), String> {
        if self
            .source
            .as_ref()
            .is_some_and(|source| source.width == width && source.height == height)
        {
            return Ok(());
        }
        self.source = Some(source_resources(&self.device, width, height)?);
        Ok(())
    }

    fn draw(
        &self,
        rgba: &[u8],
        target_view: &ID3D11RenderTargetView,
        fit: Option<FitRect>,
    ) -> Result<(), String> {
        let source = self
            .source
            .as_ref()
            .ok_or("Source texture is not initialized")?;
        unsafe {
            self.context.UpdateSubresource(
                &source.texture,
                0,
                None,
                rgba.as_ptr().cast(),
                source.width * 4,
                0,
            );
            // Legacy calibration uses magenta to expose missing draw coverage;
            // source fitting uses opaque black bars outside the image rectangle.
            self.context.ClearRenderTargetView(
                target_view,
                &if fit.is_some() {
                    [0.0, 0.0, 0.0, 1.0]
                } else {
                    [1.0, 0.0, 1.0, 0.25]
                },
            );
            self.context
                .OMSetRenderTargets(Some(&[Some(target_view.clone())]), None);
            self.context.RSSetState(&self.rasterizer);
            self.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                Width: self.info.width as f32,
                Height: self.info.height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
                ..Default::default()
            }]));
            self.context.IASetInputLayout(None);
            self.context
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.context.VSSetShader(&self.vertex_shader, None);
            if let Some(fit) = fit {
                // Two complete 16-byte HLSL constant registers. All coordinate
                // arithmetic in the shader is integer after SV_Position casts.
                let constants = [
                    source.width,
                    source.height,
                    fit.x,
                    fit.y,
                    fit.width,
                    fit.height,
                    0,
                    0,
                ];
                self.context.UpdateSubresource(
                    &self.fit_constants,
                    0,
                    None,
                    constants.as_ptr().cast(),
                    0,
                    0,
                );
                self.context
                    .PSSetConstantBuffers(0, Some(&[Some(self.fit_constants.clone())]));
                self.context.PSSetShader(&self.fit_pixel_shader, None);
            } else {
                self.context.PSSetShader(&self.pixel_shader, None);
            }
            self.context
                .PSSetShaderResources(0, Some(&[Some(source.view.clone())]));
            self.context.Draw(3, 0);
            self.context.PSSetShaderResources(0, Some(&[None]));
            self.context.PSSetConstantBuffers(0, Some(&[None]));
            self.context.OMSetRenderTargets(None, None);
        }
        Ok(())
    }

    fn readback(&self, target: &ID3D11Texture2D, started: Instant) -> Result<RenderResult, String> {
        unsafe {
            self.context.CopyResource(&self.staging, target);
            // EVENT queries use End only, never Begin.
            self.context.End(&self.completion);
            self.context.Flush();
        }
        let wait_started = Instant::now();
        loop {
            let mut ready = 0i32;
            // The projected GetData calls HRESULT::ok(), erasing S_FALSE.
            // Preserve it so pending work never appears completed.
            let status = unsafe {
                (Interface::vtable(&self.context).GetData)(
                    Interface::as_raw(&self.context),
                    Interface::as_raw(&self.completion),
                    (&mut ready as *mut i32).cast(),
                    std::mem::size_of::<i32>() as u32,
                    D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                )
            };
            if status == HRESULT(0) && ready != 0 {
                break;
            }
            if status.is_err() {
                return Err(gpu_error(&self.device, "GetData(EVENT)", status));
            }
            if wait_started.elapsed() >= READBACK_TIMEOUT {
                return Err(gpu_error(
                    &self.device,
                    "GPU completion deadline",
                    "exceeded two seconds",
                ));
            }
            thread::sleep(Duration::from_millis(1));
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        loop {
            match unsafe {
                self.context.Map(
                    &self.staging,
                    0,
                    D3D11_MAP_READ,
                    D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                    Some(&mut mapped),
                )
            } {
                Ok(()) => break,
                Err(error)
                    if error.code() == DXGI_ERROR_WAS_STILL_DRAWING
                        && wait_started.elapsed() < READBACK_TIMEOUT =>
                {
                    thread::sleep(Duration::from_millis(1))
                }
                Err(error) => return Err(gpu_error(&self.device, "Map staging texture", error)),
            }
        }
        let readback_wait_ms = wait_started.elapsed().as_secs_f64() * 1000.0;
        let _mapped_guard = MappedTexture {
            context: &self.context,
            texture: &self.staging,
        };
        if mapped.pData.is_null() || mapped.RowPitch < self.info.width * 4 {
            return Err("Mapped staging texture returned an invalid pointer or row pitch".into());
        }
        // Only the last row's active pixels are required, not its trailing
        // padding. DepthPitch is not used as proof of allocation size.
        let length = (mapped.RowPitch as usize)
            .checked_mul(self.info.height as usize - 1)
            .and_then(|value| value.checked_add(self.info.width as usize * 4))
            .filter(|value| *value <= isize::MAX as usize)
            .ok_or("Mapped texture byte span overflow")?;
        let bytes = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), length) };
        let rgba = dense_rgba(bytes, self.info.width, self.info.height, mapped.RowPitch)?;
        Ok(RenderResult {
            rgba,
            row_pitch: mapped.RowPitch,
            upload_draw_readback_ms: started.elapsed().as_secs_f64() * 1000.0,
            readback_wait_ms,
        })
    }
}

impl Drop for D3d11Renderer {
    fn drop(&mut self) {
        unsafe {
            self.context.ClearState();
            self.context.Flush();
        }
    }
}

fn image_bytes(width: u32, height: u32, channels: usize) -> Result<usize, String> {
    if width == 0 || height == 0 || width > 1920 || height > 1080 {
        return Err("Offscreen probe dimensions must be within 1..1920 by 1..1080".into());
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or_else(|| "Image allocation size overflow".into())
}

fn rgb_to_rgba(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if rgb.len() != image_bytes(width, height, 3)? {
        return Err("RGB input length does not match the declared dimensions".into());
    }
    let mut rgba = Vec::with_capacity(image_bytes(width, height, 4)?);
    for pixel in rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    Ok(rgba)
}

fn dense_rgba(mapped: &[u8], width: u32, height: u32, row_pitch: u32) -> Result<Vec<u8>, String> {
    let capacity = image_bytes(width, height, 4)?;
    let row_bytes = width as usize * 4;
    let pitch = row_pitch as usize;
    let minimum_length = pitch
        .checked_mul(height as usize - 1)
        .and_then(|bytes| bytes.checked_add(row_bytes))
        .ok_or("Readback size overflow")?;
    if pitch < row_bytes || mapped.len() < minimum_length {
        return Err("Mapped texture rows are too small for the declared image".into());
    }
    let mut rgba = Vec::with_capacity(capacity);
    for row in 0..height as usize {
        rgba.extend_from_slice(&mapped[row * pitch..row * pitch + row_bytes]);
    }
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_and_input_lengths_are_bounded_before_allocations() {
        for (width, height) in [
            (0, 1),
            (1, 0),
            (1921, 1080),
            (1920, 1081),
            (u32::MAX, u32::MAX),
        ] {
            assert!(image_bytes(width, height, 4).is_err());
        }
        assert_eq!(image_bytes(1920, 1080, 4).unwrap(), 8_294_400);
        assert!(rgb_to_rgba(&[1, 2], 1, 1).is_err());
        assert!(rgb_to_rgba(&[1, 2, 3, 4], 1, 1).is_err());
    }

    #[test]
    fn rgb_upload_keeps_channel_order_orientation_and_opaque_alpha() {
        assert_eq!(
            rgb_to_rgba(&[255, 0, 7, 0, 128, 9, 11, 13, 17, 19, 23, 29], 2, 2).unwrap(),
            vec![
                255, 0, 7, 255, 0, 128, 9, 255, 11, 13, 17, 255, 19, 23, 29, 255
            ]
        );
    }

    #[test]
    fn readback_honors_padding_without_copying_guard_bytes() {
        let mapped = [
            1, 2, 3, 255, 4, 5, 6, 255, 99, 99, 99, 99, 7, 8, 9, 255, 10, 11, 12, 255,
        ];
        assert_eq!(
            dense_rgba(&mapped, 2, 2, 12).unwrap(),
            vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
        assert!(dense_rgba(&mapped, 2, 2, 7).is_err());
        assert!(dense_rgba(&mapped[..19], 2, 2, 12).is_err());
    }

    #[test]
    fn odd_width_rows_preserve_the_last_pixel_and_final_row() {
        let width = 161;
        let height = 91;
        let pitch = 768;
        let mut mapped = vec![222; pitch * height];
        for row in 0..height {
            for column in 0..width {
                let offset = row * pitch + column * 4;
                mapped[offset..offset + 4].copy_from_slice(&[
                    column as u8,
                    row as u8,
                    (column ^ row) as u8,
                    255,
                ]);
            }
        }
        let dense = dense_rgba(&mapped, width as u32, height as u32, pitch as u32).unwrap();
        assert_eq!(dense.len(), width * height * 4);
        assert_eq!(&dense[dense.len() - 4..], &[160, 90, 250, 255]);
    }
}
