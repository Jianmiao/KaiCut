//! Thread-affine, shared-mode WASAPI output for standalone playback probes.
//! Silent mode uses SILENT packets. PCM mode writes decoded samples and sets
//! mute only on its own unique, nonpersistent session. No endpoint changes.

use std::{
    marker::PhantomData,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    rc::Rc,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_NOPERSIST, IAudioClient,
            IAudioClock, IAudioRenderClient, IMMDeviceEnumerator, ISimpleAudioVolume,
            MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eConsole, eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize,
            },
            Threading::{CreateEventW, ResetEvent, WaitForSingleObject},
        },
    },
    core::{GUID, HRESULT, Interface, PCWSTR},
};

const REQUESTED_BUFFER_DURATION_100NS: i64 = 1_000_000;
const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SampleEncoding {
    Float32,
    Pcm16,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputMode {
    Silence,
    Pcm,
}

fn classify_format(base: WAVEFORMATEX, extension: Option<(u16, GUID)>) -> SampleEncoding {
    if base.nSamplesPerSec == 0 || base.nChannels == 0 {
        return SampleEncoding::Unsupported;
    }
    let (valid_bits, subtype) = match base.wFormatTag {
        1 => (base.wBitsPerSample, SUBTYPE_PCM),
        3 => (base.wBitsPerSample, SUBTYPE_FLOAT),
        0xfffe if base.cbSize >= 22 => match extension {
            Some(value) => value,
            None => return SampleEncoding::Unsupported,
        },
        _ => return SampleEncoding::Unsupported,
    };
    let encoding = if subtype == SUBTYPE_FLOAT && base.wBitsPerSample == 32 && valid_bits == 32 {
        SampleEncoding::Float32
    } else if subtype == SUBTYPE_PCM && base.wBitsPerSample == 16 && valid_bits == 16 {
        SampleEncoding::Pcm16
    } else {
        return SampleEncoding::Unsupported;
    };
    let bytes_per_sample = u32::from(base.wBitsPerSample / 8);
    let block_align = u32::from(base.nChannels) * bytes_per_sample;
    if block_align != u32::from(base.nBlockAlign)
        || base.nSamplesPerSec.checked_mul(block_align) != Some(base.nAvgBytesPerSec)
    {
        return SampleEncoding::Unsupported;
    }
    encoding
}

fn pack_pcm(
    samples: &[f32],
    channels: u16,
    encoding: SampleEncoding,
    available: u32,
) -> Result<(u32, Vec<u8>), String> {
    if channels == 0 || samples.len() % usize::from(channels) != 0 {
        return Err("PCM input must contain complete interleaved audio frames".into());
    }
    if samples
        .iter()
        .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
    {
        return Err("PCM samples must be finite and within [-1, 1]".into());
    }
    let sample_bytes = match encoding {
        SampleEncoding::Float32 => 4,
        SampleEncoding::Pcm16 => 2,
        SampleEncoding::Unsupported => return Err("Unsupported PCM device encoding".into()),
    };
    let frames = (samples.len() / usize::from(channels)).min(available as usize);
    let count = frames * usize::from(channels);
    let capacity = count
        .checked_mul(sample_bytes)
        .ok_or("PCM packet size overflow")?;
    let mut bytes = Vec::with_capacity(capacity);
    for sample in &samples[..count] {
        match encoding {
            SampleEncoding::Float32 => bytes.extend_from_slice(&sample.to_le_bytes()),
            SampleEncoding::Pcm16 => {
                let value = (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            SampleEncoding::Unsupported => unreachable!(),
        }
    }
    Ok((frames as u32, bytes))
}

fn validate_prequeued(mode: OutputMode, padding: u32) -> Result<(), String> {
    if mode == OutputMode::Pcm && padding == 0 {
        Err("Queue decoded PCM frames before starting the PCM stream".into())
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub block_align: u16,
    pub format_tag: u16,
    pub valid_bits_per_sample: u16,
    pub encoding: SampleEncoding,
    pub session_muted: bool,
    pub event_driven: bool,
    pub stream_flags: u32,
    pub buffer_frames: u32,
    pub requested_buffer_duration_100ns: i64,
    pub default_period_100ns: i64,
    pub minimum_period_100ns: i64,
    pub stream_latency_100ns: i64,
    pub clock_frequency: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct ClockSample {
    /// Device clock units; convert with frequency, never assume PCM frames.
    pub position: u64,
    pub frequency: u64,
    /// WASAPI's correlated QPC value, already converted to 100-ns units.
    pub qpc_100ns: u64,
    /// S_FALSE still returns a sample, but it is not a reliable clock reading.
    pub reliable: bool,
    pub hresult: i32,
    /// Host monotonic time immediately after the GetPosition call.
    pub wall: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamState {
    Stopped,
    Running,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WaitOutcome {
    Signaled,
    TimedOut,
}

fn event_wait_millis(
    event_driven: bool,
    state: StreamState,
    timeout: Duration,
) -> Result<u32, String> {
    if !event_driven {
        return Err("wait_ready requires an event-driven output stream".into());
    }
    if state != StreamState::Running {
        return Err("wait_ready requires a running output stream".into());
    }
    if timeout < Duration::from_millis(1) || timeout > Duration::from_millis(250) {
        return Err("Audio readiness wait must be within 1..=250 ms".into());
    }
    Ok(timeout.as_nanos().div_ceil(1_000_000) as u32)
}

fn requested_buffer_duration(event_driven: bool) -> i64 {
    // IAudioClient::Initialize explicitly requires 0/0 for shared event mode.
    // Keep the old polling request unchanged for a truthful comparison.
    if event_driven {
        0
    } else {
        REQUESTED_BUFFER_DURATION_100NS
    }
}

fn readiness_event() -> Result<OwnedHandle, String> {
    let handle = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
        .map_err(|error| format!("Cannot create auto-reset audio readiness event: {error}"))?;
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

impl StreamState {
    fn can_start(self) -> Result<(), String> {
        if self == Self::Stopped {
            Ok(())
        } else {
            Err("Silent output stream is already running".into())
        }
    }

    fn can_stop(self) -> Result<(), String> {
        if self == Self::Running {
            Ok(())
        } else {
            Err("Silent output stream is already stopped".into())
        }
    }

    fn can_reset(self) -> Result<(), String> {
        if self == Self::Stopped {
            Ok(())
        } else {
            Err("Stop the silent output stream before resetting its clock".into())
        }
    }
}

fn available_frames(capacity: u32, padding: u32) -> Result<u32, String> {
    capacity
        .checked_sub(padding)
        .ok_or_else(|| "Render padding exceeds the endpoint buffer capacity".into())
}

fn clock_result(
    status: HRESULT,
    position: u64,
    frequency: u64,
    qpc_100ns: u64,
    wall: Instant,
) -> Result<ClockSample, String> {
    if status.is_err() {
        return Err(format!("IAudioClock::GetPosition failed: {status:?}"));
    }
    if frequency == 0 {
        return Err("Cannot interpret a device clock with zero frequency".into());
    }
    Ok(ClockSample {
        position,
        frequency,
        qpc_100ns,
        reliable: status.0 == 0,
        hresult: status.0,
        wall,
    })
}

struct ComApartment;

impl ComApartment {
    fn initialize() -> Result<Self, String> {
        // S_OK and S_FALSE both increment this thread's COM initialization
        // count. RPC_E_CHANGED_MODE must not be balanced with CoUninitialize.
        let status = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        status
            .ok()
            .map_err(|error| format!("CoInitializeEx(MTA) failed: {error}"))?;
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct MixFormat(*mut WAVEFORMATEX);

impl Drop for MixFormat {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast())) };
    }
}

/// Release any acquired packet even if a subsequent validation/copy path
/// fails. Releasing zero frames discards the outstanding packet.
struct RenderPacket<'a> {
    render: &'a IAudioRenderClient,
    frames: u32,
    acquired: bool,
}

impl RenderPacket<'_> {
    fn commit(mut self, flags: u32) -> Result<(), String> {
        unsafe { self.render.ReleaseBuffer(self.frames, flags) }
            .map_err(|error| format!("Cannot release render buffer: {error}"))?;
        self.acquired = false;
        Ok(())
    }
}

impl Drop for RenderPacket<'_> {
    fn drop(&mut self) {
        if self.acquired {
            // A device-loss error may also reject this best-effort discard;
            // the owning IAudioClient is subsequently released on teardown.
            let _ = unsafe { self.render.ReleaseBuffer(0, 0) };
        }
    }
}

pub struct WasapiOutput {
    // Rust drops fields in declaration order after Drop::drop. Every COM
    // interface must be released before the apartment guard at the end.
    render: IAudioRenderClient,
    clock: IAudioClock,
    _session_volume: Option<ISimpleAudioVolume>,
    client: IAudioClient,
    // SetEventHandle keeps the handle live for the full client lifetime.
    // This field is deliberately dropped after all client/service COM refs.
    ready_event: Option<OwnedHandle>,
    info: OutputInfo,
    state: StreamState,
    mode: OutputMode,
    // COM and each GetBuffer/ReleaseBuffer pair stay on their creating thread.
    _thread_affinity: PhantomData<Rc<()>>,
    _apartment: ComApartment,
}

impl WasapiOutput {
    pub fn open() -> Result<Self, String> {
        Self::open_mode(OutputMode::Silence, false, false)
    }

    pub fn open_pcm_muted() -> Result<Self, String> {
        Self::open_mode(OutputMode::Pcm, false, true)
    }

    pub fn open_pcm_muted_event() -> Result<Self, String> {
        Self::open_pcm_event(true)
    }

    pub fn open_pcm_event(muted: bool) -> Result<Self, String> {
        Self::open_mode(OutputMode::Pcm, true, muted)
    }

    fn open_mode(mode: OutputMode, event_driven: bool, muted: bool) -> Result<Self, String> {
        // Declared first so every early-return path releases local COM refs
        // before balancing CoInitializeEx.
        let apartment = ComApartment::initialize()?;
        // Declare before the client so early-return cleanup also releases all
        // client references before closing its event, then uninitializes COM.
        let ready_event = if event_driven {
            Some(readiness_event()?)
        } else {
            None
        };
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|error| format!("Cannot create audio endpoint enumerator: {error}"))?;
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
            .map_err(|error| format!("Default render endpoint is unavailable: {error}"))?;
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .map_err(|error| format!("Cannot activate shared audio client: {error}"))?;
        let format = MixFormat(
            unsafe { client.GetMixFormat() }
                .map_err(|error| format!("Cannot read endpoint mix format: {error}"))?,
        );
        if format.0.is_null() {
            return Err("Endpoint returned a null mix format".into());
        }
        // Copy only the base fields for diagnostics. Initialize receives the
        // original allocation including cbSize and all extensible fields.
        let base = unsafe { std::ptr::read_unaligned(format.0) };
        if base.nSamplesPerSec == 0 || base.nChannels == 0 || base.nBlockAlign == 0 {
            return Err(
                "Endpoint mix format has invalid sample rate/channels/block alignment".into(),
            );
        }
        let extension = if base.wFormatTag == 0xfffe && base.cbSize >= 22 {
            // GetMixFormat owns the complete extensible allocation. Do not
            // read its extension unless cbSize declares all 22 bytes present.
            let extended =
                unsafe { std::ptr::read_unaligned(format.0.cast::<WAVEFORMATEXTENSIBLE>()) };
            Some((
                unsafe { extended.Samples.wValidBitsPerSample },
                extended.SubFormat,
            ))
        } else {
            None
        };
        let encoding = classify_format(base, extension);
        if mode == OutputMode::Pcm && encoding == SampleEncoding::Unsupported {
            return Err(
                "PCM probe supports only declared float32 or full-precision PCM16 mix formats"
                    .into(),
            );
        }
        // Isolate muting to this stream's own session, including from other
        // probe streams within the same process; never use the endpoint API.
        let session_guid = GUID::from_u128(uuid::Uuid::new_v4().as_u128());
        let mut default_period = 0;
        let mut minimum_period = 0;
        unsafe { client.GetDevicePeriod(Some(&mut default_period), Some(&mut minimum_period)) }
            .map_err(|error| format!("Cannot read device periods: {error}"))?;
        let stream_flags = AUDCLNT_STREAMFLAGS_NOPERSIST
            | if event_driven {
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            } else {
                0
            };
        let buffer_duration = requested_buffer_duration(event_driven);
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                stream_flags,
                buffer_duration,
                0,
                format.0,
                Some(&session_guid),
            )
        }
        .map_err(|error| format!("Cannot initialize shared silent render stream: {error}"))?;
        if let Some(event) = &ready_event {
            unsafe { client.SetEventHandle(HANDLE(event.as_raw_handle())) }
                .map_err(|error| format!("Cannot attach audio readiness event: {error}"))?;
        }
        let session_volume = if mode == OutputMode::Pcm {
            let volume: ISimpleAudioVolume = unsafe { client.GetService() }.map_err(|error| {
                format!("Cannot obtain this stream's audio session volume: {error}")
            })?;
            unsafe { volume.SetMute(muted, &session_guid) }.map_err(|error| {
                format!("Cannot set the source's own audio session mute: {error}")
            })?;
            if unsafe { volume.GetMute() }
                .map_err(|error| format!("Cannot verify probe session mute: {error}"))?
                .as_bool()
                != muted
            {
                return Err("Source audio session did not confirm its requested mute state".into());
            }
            Some(volume)
        } else {
            None
        };
        let buffer_frames = unsafe { client.GetBufferSize() }
            .map_err(|error| format!("Cannot query render buffer size: {error}"))?;
        if buffer_frames == 0 {
            return Err("Shared render buffer contains no frames".into());
        }
        let stream_latency = unsafe { client.GetStreamLatency() }
            .map_err(|error| format!("Cannot query stream latency: {error}"))?;
        let render: IAudioRenderClient = unsafe { client.GetService() }
            .map_err(|error| format!("Cannot obtain render service: {error}"))?;
        let clock: IAudioClock = unsafe { client.GetService() }
            .map_err(|error| format!("Cannot obtain device clock service: {error}"))?;
        let frequency = unsafe { clock.GetFrequency() }
            .map_err(|error| format!("Cannot query device clock frequency: {error}"))?;
        if frequency == 0 {
            return Err("Device clock frequency is zero".into());
        }
        let info = OutputInfo {
            sample_rate: base.nSamplesPerSec,
            channels: base.nChannels,
            bits_per_sample: base.wBitsPerSample,
            block_align: base.nBlockAlign,
            format_tag: base.wFormatTag,
            valid_bits_per_sample: extension.map_or(base.wBitsPerSample, |value| value.0),
            encoding,
            session_muted: session_volume.is_some() && muted,
            event_driven,
            stream_flags,
            buffer_frames,
            requested_buffer_duration_100ns: buffer_duration,
            default_period_100ns: default_period,
            minimum_period_100ns: minimum_period,
            stream_latency_100ns: stream_latency,
            clock_frequency: frequency,
        };
        // These are no longer needed. Keep only the stream's service refs.
        drop(format);
        drop(device);
        drop(enumerator);
        Ok(Self {
            render,
            clock,
            _session_volume: session_volume,
            client,
            ready_event,
            info,
            state: StreamState::Stopped,
            mode,
            _thread_affinity: PhantomData,
            _apartment: apartment,
        })
    }

    pub fn info(&self) -> &OutputInfo {
        &self.info
    }

    pub fn start(&mut self) -> Result<(), String> {
        self.state.can_start()?;
        // Prime the render buffer before every Start. After Stop, preserve
        // already queued silence; after Reset, refill the now-empty buffer.
        if self.mode == OutputMode::Silence {
            self.fill_silence()?;
        }
        validate_prequeued(self.mode, self.padding_frames()?)?;
        // Only reset while stopped, immediately before Start/Resume. Clearing
        // an event while running would discard a real readiness notification.
        if let Some(event) = &self.ready_event {
            unsafe { ResetEvent(HANDLE(event.as_raw_handle())) }
                .map_err(|error| format!("Cannot reset stopped audio readiness event: {error}"))?;
        }
        unsafe { self.client.Start() }
            .map_err(|error| format!("Cannot start shared render stream: {error}"))?;
        self.state = StreamState::Running;
        Ok(())
    }

    pub fn wait_ready(&self, timeout: Duration) -> Result<WaitOutcome, String> {
        let milliseconds = event_wait_millis(self.info.event_driven, self.state, timeout)?;
        let event = self
            .ready_event
            .as_ref()
            .ok_or("Event-driven output has no readiness event")?;
        let result = unsafe { WaitForSingleObject(HANDLE(event.as_raw_handle()), milliseconds) };
        if result == WAIT_OBJECT_0 {
            Ok(WaitOutcome::Signaled)
        } else if result == WAIT_TIMEOUT {
            Ok(WaitOutcome::TimedOut)
        } else if result == WAIT_FAILED {
            Err(format!(
                "Audio readiness wait failed: {}",
                windows::core::Error::from_thread()
            ))
        } else {
            Err(format!(
                "Unexpected audio readiness wait status: {}",
                result.0
            ))
        }
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.state.can_stop()?;
        unsafe { self.client.Stop() }
            .map_err(|error| format!("Cannot stop shared render stream: {error}"))?;
        self.state = StreamState::Stopped;
        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), String> {
        self.state.can_reset()?;
        unsafe { self.client.Reset() }
            .map_err(|error| format!("Cannot reset stopped render stream: {error}"))?;
        Ok(())
    }

    /// Refill all currently available render frames. Call continuously while
    /// running (e.g. every 5–20 ms), not just once at stream startup.
    pub fn fill_silence(&mut self) -> Result<u32, String> {
        if self.mode != OutputMode::Silence {
            return Err("PCM streams must be filled with decoded PCM, not SILENT packets".into());
        }
        let padding = self.padding_frames()?;
        let frames = available_frames(self.info.buffer_frames, padding)?;
        if frames == 0 {
            return Ok(0);
        }
        // No samples are copied or fabricated. SILENT instructs the audio
        // engine to treat exactly this acquired frame packet as silence.
        let _buffer = unsafe { self.render.GetBuffer(frames) }
            .map_err(|error| format!("Cannot acquire silent render buffer: {error}"))?;
        RenderPacket {
            render: &self.render,
            frames,
            acquired: true,
        }
        .commit(AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)?;
        Ok(frames)
    }

    pub fn padding_frames(&self) -> Result<u32, String> {
        let padding = unsafe { self.client.GetCurrentPadding() }
            .map_err(|error| format!("Cannot query render buffer padding: {error}"))?;
        available_frames(self.info.buffer_frames, padding)?;
        Ok(padding)
    }

    /// Queue real interleaved decoded samples; the return value counts audio
    /// frames, not scalar channel samples. The caller retains any unused tail.
    pub fn write_pcm(&mut self, samples: &[f32]) -> Result<u32, String> {
        if self.mode != OutputMode::Pcm {
            return Err("Open a muted PCM stream before writing decoded samples".into());
        }
        let available = available_frames(self.info.buffer_frames, self.padding_frames()?)?;
        // Validate and pack before GetBuffer: there is no fallible conversion
        // or allocation while holding the endpoint's outstanding packet.
        let (frames, bytes) = pack_pcm(samples, self.info.channels, self.info.encoding, available)?;
        if frames == 0 {
            return Ok(0);
        }
        let buffer = unsafe { self.render.GetBuffer(frames) }
            .map_err(|error| format!("Cannot acquire PCM render buffer: {error}"))?;
        let packet = RenderPacket {
            render: &self.render,
            frames,
            acquired: true,
        };
        if buffer.is_null() {
            return Err("Endpoint returned a null PCM render buffer".into());
        }
        // The recognized mix format guarantees bytes.len() == frames *
        // nBlockAlign. GetBuffer grants at least this packet's full capacity.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len()) };
        packet.commit(0)?;
        Ok(frames)
    }

    pub fn sample(&self) -> Result<ClockSample, String> {
        let mut position = 0;
        let mut qpc_100ns = 0;
        // windows 0.62's convenient GetPosition uses HRESULT::ok(), which
        // erases S_FALSE. Call the same COM vtable method to retain accuracy.
        let status = unsafe {
            (Interface::vtable(&self.clock).GetPosition)(
                Interface::as_raw(&self.clock),
                &mut position,
                &mut qpc_100ns,
            )
        };
        clock_result(
            status,
            position,
            self.info.clock_frequency,
            qpc_100ns,
            Instant::now(),
        )
    }
}

impl Drop for WasapiOutput {
    fn drop(&mut self) {
        if self.state == StreamState::Running {
            // Best effort only during teardown; releasing the client still
            // ends our session. Explicit stop() reports device-loss errors.
            let _ = unsafe { self.client.Stop() };
            self.state = StreamState::Stopped;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_wait_requires_running_event_stream_and_a_bounded_timeout() {
        assert!(event_wait_millis(false, StreamState::Running, Duration::from_millis(10)).is_err());
        assert!(event_wait_millis(true, StreamState::Stopped, Duration::from_millis(10)).is_err());
        for timeout in [
            Duration::ZERO,
            Duration::from_micros(999),
            Duration::from_millis(251),
            Duration::from_secs(u32::MAX as u64),
        ] {
            assert!(event_wait_millis(true, StreamState::Running, timeout).is_err());
        }
        assert_eq!(
            event_wait_millis(true, StreamState::Running, Duration::from_millis(1)).unwrap(),
            1
        );
        assert_eq!(
            event_wait_millis(true, StreamState::Running, Duration::from_micros(1500)).unwrap(),
            2
        );
        assert_eq!(
            event_wait_millis(true, StreamState::Running, Duration::from_millis(250)).unwrap(),
            250
        );
    }

    #[test]
    fn shared_event_buffer_request_is_zero_and_polling_retains_100ms() {
        assert_eq!(requested_buffer_duration(true), 0);
        assert_eq!(requested_buffer_duration(false), 1_000_000);
    }

    fn format(tag: u16, bits: u16) -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: tag,
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * u32::from(bits / 8 * 2),
            nBlockAlign: bits / 8 * 2,
            wBitsPerSample: bits,
            cbSize: if tag == 0xfffe { 22 } else { 0 },
        }
    }

    #[test]
    fn mix_format_recognizes_only_declared_f32_or_full_precision_pcm16() {
        assert_eq!(
            classify_format(format(3, 32), None),
            SampleEncoding::Float32
        );
        assert_eq!(classify_format(format(1, 16), None), SampleEncoding::Pcm16);
        assert_eq!(
            classify_format(format(0xfffe, 32), Some((32, SUBTYPE_FLOAT))),
            SampleEncoding::Float32
        );
        assert_eq!(
            classify_format(format(0xfffe, 16), Some((16, SUBTYPE_PCM))),
            SampleEncoding::Pcm16
        );
        for (base, extension) in [
            (format(1, 24), None),
            (format(3, 64), None),
            (format(0xfffe, 32), None),
            (format(0xfffe, 32), Some((24, SUBTYPE_PCM))),
            (format(0xfffe, 32), Some((32, GUID::zeroed()))),
            (format(0xfffe, 16), Some((12, SUBTYPE_PCM))),
            (
                WAVEFORMATEX {
                    nBlockAlign: 1,
                    ..format(3, 32)
                },
                None,
            ),
            (
                WAVEFORMATEX {
                    cbSize: 0,
                    ..format(0xfffe, 32)
                },
                Some((32, SUBTYPE_FLOAT)),
            ),
        ] {
            assert_eq!(
                classify_format(base, extension),
                SampleEncoding::Unsupported
            );
        }
    }

    #[test]
    fn pcm_packing_preserves_real_float_samples_and_consumes_whole_bounded_frames() {
        let (frames, bytes) =
            pack_pcm(&[-1.0, 0.5, 0.25, 1.0], 2, SampleEncoding::Float32, 1).unwrap();
        assert_eq!(frames, 1);
        assert_eq!(
            bytes,
            [(-1.0f32).to_le_bytes(), 0.5f32.to_le_bytes()].concat()
        );
        assert_eq!(
            pack_pcm(&[0.25, -0.25], 2, SampleEncoding::Float32, 0).unwrap(),
            (0, vec![])
        );
        assert_eq!(
            pack_pcm(&[], 2, SampleEncoding::Float32, 4800).unwrap(),
            (0, vec![])
        );
    }

    #[test]
    fn pcm16_packing_maps_endpoints_without_wrapping_and_rejects_bad_samples() {
        let (frames, bytes) =
            pack_pcm(&[-1.0, -0.5, 0.0, 0.5, 1.0], 1, SampleEncoding::Pcm16, 5).unwrap();
        assert_eq!(frames, 5);
        assert_eq!(
            bytes,
            [-32768i16, -16384, 0, 16384, 32767]
                .into_iter()
                .flat_map(i16::to_le_bytes)
                .collect::<Vec<_>>()
        );
        for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.01, -1.01] {
            assert!(pack_pcm(&[sample], 1, SampleEncoding::Pcm16, 1).is_err());
        }
        assert!(pack_pcm(&[0.0], 2, SampleEncoding::Float32, 1).is_err());
        assert!(pack_pcm(&[0.0], 0, SampleEncoding::Float32, 1).is_err());
        assert!(pack_pcm(&[0.0], 1, SampleEncoding::Unsupported, 1).is_err());
    }

    #[test]
    fn pcm_mode_requires_prequeued_data_without_replacing_it_with_silence() {
        assert!(validate_prequeued(OutputMode::Pcm, 0).is_err());
        assert!(validate_prequeued(OutputMode::Pcm, 128).is_ok());
        assert!(validate_prequeued(OutputMode::Silence, 0).is_ok());
    }

    #[test]
    fn stopped_and_running_streams_reject_invalid_lifecycle_operations() {
        assert!(StreamState::Stopped.can_start().is_ok());
        assert!(StreamState::Stopped.can_stop().is_err());
        assert!(StreamState::Stopped.can_reset().is_ok());
        assert!(StreamState::Running.can_start().is_err());
        assert!(StreamState::Running.can_stop().is_ok());
        assert!(StreamState::Running.can_reset().is_err());
    }

    #[test]
    fn padding_must_not_exceed_the_buffer_and_full_buffers_need_no_write() {
        assert_eq!(available_frames(4800, 1200).unwrap(), 3600);
        assert_eq!(available_frames(4800, 4800).unwrap(), 0);
        assert!(available_frames(4800, 4801).is_err());
    }

    #[test]
    fn s_false_is_retained_as_an_unreliable_device_clock_sample() {
        let wall = Instant::now();
        let sample = clock_result(HRESULT(1), 64, 48_000, 987, wall).unwrap();
        assert!(!sample.reliable);
        assert_eq!(sample.hresult, 1);
        assert_eq!(sample.position, 64);
        assert_eq!(sample.frequency, 48_000);
        assert_eq!(sample.qpc_100ns, 987);
        assert_eq!(sample.wall, wall);
        assert!(
            clock_result(HRESULT(0), 64, 48_000, 987, wall)
                .unwrap()
                .reliable
        );
    }

    #[test]
    fn failed_clock_calls_and_zero_frequency_never_become_valid_timestamps() {
        assert!(
            clock_result(
                HRESULT(0x8889_0004_u32 as i32),
                0,
                48_000,
                0,
                Instant::now()
            )
            .is_err()
        );
        assert!(clock_result(HRESULT(0), 0, 0, 0, Instant::now()).is_err());
    }
}
