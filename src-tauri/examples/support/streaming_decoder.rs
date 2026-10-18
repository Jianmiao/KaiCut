//! Bounded streaming fixture decoder. Sequential restart remains the default;
//! indexed seeking is admitted only for controlled zero-origin 30 fps fixtures.
#[path = "seek_plan.rs"]
mod seek_plan;
#[path = "stream_process.rs"]
mod stream_process;
#[path = "timestamp_index.rs"]
mod timestamp_index;
pub use seek_plan::SeekMode;
use seek_plan::{IndexedAudioMetadata, IndexedSeekPlan, IndexedVideoMetadata, index_for_pts};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Instant,
};
use stream_process::{
    PipeProcess, ProcessInfo, bounded_line, bounded_output, command, small_output,
};
use timestamp_index::{MAX_INDEX_BYTES, TimestampIndex, TimestampMetadata};

#[cfg(test)]
const FRAME_BYTES: usize = 160 * 90 * 3;
const AUDIO_CAPACITY: usize = 8;
const VIDEO_CAPACITY: usize = 4;
const AUDIO_BLOCK_FRAMES: usize = 480;
const MAX_SECONDS: u64 = 24 * 60 * 60;

#[derive(Clone, Copy)]
struct VideoFormat {
    width: u32,
    height: u32,
    frame_bytes: usize,
}
impl VideoFormat {
    fn new(width: u32, height: u32) -> Result<Self, String> {
        if !(1..=1920).contains(&width) || !(1..=1080).contains(&height) {
            return Err("Streaming RGB dimensions must be within 1..=1920 by 1..=1080".into());
        }
        let frame_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or("Streaming RGB frame byte length overflow")?;
        if frame_bytes > 1920 * 1080 * 3 {
            return Err("Streaming RGB frame exceeds 1080p byte limit".into());
        }
        Ok(Self {
            width,
            height,
            frame_bytes,
        })
    }
}

struct VideoMetadata {
    numerator: u32,
    denominator: u32,
    width: u32,
    height: u32,
    coded_width: Option<u32>,
    coded_height: Option<u32>,
}

#[derive(Clone)]
pub struct StreamConfig {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub video: PathBuf,
    pub audio: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub video_width: u32,
    pub video_height: u32,
    pub seek_mode: SeekMode,
    pub epoch: u64,
    pub start_audio_frame: u64,
    pub start_video_ns: u64,
}
pub struct AudioBlock {
    pub epoch: u64,
    pub source_start_frame: u64,
    pub samples: Vec<f32>,
}
pub struct VideoFrame {
    pub epoch: u64,
    pub index: usize,
    pub pts_ns: u64,
    pub rgb: Arc<[u8]>,
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecoderStats {
    pub epoch: u64,
    /// Requested logical source time is independent of the earlier decode anchor.
    pub requested_video_ns: u64,
    pub source_playback: bool,
    pub seek_mode: SeekMode,
    pub video_seek_strategy: &'static str,
    pub audio_seek_strategy: &'static str,
    pub source_index_frames: usize,
    pub index_build_ms: f64,
    pub index_rebuilt_this_session: bool,
    pub source_start_pts: Option<i64>,
    pub source_duration_ns: Option<u64>,
    pub audio_decode_start_frame: u64,
    pub video_decode_start_index: u64,
    pub video_decode_start_ns: u64,
    /// Actual decoded frames read in this session, not absolute source offsets.
    pub audio_frames_read: u64,
    pub video_frames_read: u64,
    pub video_pts_read: u64,
    pub video_pts_skipped: u64,
    pub first_delivered_audio_frame: Option<u64>,
    pub first_delivered_video_index: Option<u64>,
    pub audio_eof: bool,
    pub video_eof: bool,
    pub audio_blocks_produced: u64,
    pub audio_frames_produced: u64,
    pub audio_frames_skipped: u64,
    pub audio_source_frames: u64,
    pub video_frames_produced: u64,
    pub video_frames_skipped: u64,
    pub video_source_frames: u64,
    pub audio_pending: usize,
    pub video_pending: usize,
    pub audio_high_water: usize,
    pub video_high_water: usize,
    pub video_high_water_bytes: usize,
    pub source_width: u32,
    pub source_height: u32,
    pub source_coded_width: Option<u32>,
    pub source_coded_height: Option<u32>,
    pub output_width: u32,
    pub output_height: u32,
    pub frame_bytes: usize,
    /// Capacity of the video queue itself; readers and downstream owners may
    /// additionally hold bounded in-flight frames outside this queue.
    pub max_queued_video_bytes: usize,
    pub source_timebase_numerator: u32,
    pub source_timebase_denominator: u32,
    pub source_first_pts: Option<i64>,
    pub processes: Vec<ProcessInfo>,
    pub closed: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecoderShutdown {
    pub joined: bool,
    pub processes_reaped: bool,
    pub elapsed_ms: f64,
    pub stats: DecoderStats,
    /// Teardown failures only; a source decode failure remains in stats/error.
    pub cleanup_errors: Vec<String>,
    pub error: Option<String>,
}
struct State {
    audio: VecDeque<AudioBlock>,
    video: VecDeque<VideoFrame>,
    stats: DecoderStats,
    frame_bytes: usize,
    indexed_seek: Option<IndexedSeekPlan>,
    timestamp_seek: Option<TimestampSeekPlan>,
    pcm_seek: Option<PcmSeekPlan>,
}
#[derive(Clone)]
struct TimestampSeekPlan {
    index: Arc<TimestampIndex>,
    anchor: usize,
    input_seconds: String,
    probe_interval: String,
}
#[derive(Clone)]
struct PcmSeekPlan {
    decode_start_frame: u64,
    frames_to_discard: u64,
    input_seconds: String,
    total_frames: u64,
}
struct Shared {
    state: Mutex<State>,
    space: Condvar,
    stop: AtomicBool,
    processes: Vec<Arc<PipeProcess>>,
}
pub struct StreamingDecoder {
    shared: Arc<Shared>,
    readers: Vec<JoinHandle<()>>,
}

fn initial_video_state(epoch: u64, source: &VideoMetadata, output: VideoFormat) -> State {
    State {
        audio: VecDeque::with_capacity(AUDIO_CAPACITY),
        video: VecDeque::with_capacity(VIDEO_CAPACITY),
        frame_bytes: output.frame_bytes,
        indexed_seek: None,
        timestamp_seek: None,
        pcm_seek: None,
        stats: DecoderStats {
            epoch,
            seek_mode: SeekMode::Sequential,
            video_seek_strategy: "sequential-decoded",
            audio_seek_strategy: "sequential-decoded",
            source_index_frames: 0,
            requested_video_ns: 0,
            source_playback: false,
            index_build_ms: 0.0,
            index_rebuilt_this_session: false,
            source_start_pts: None,
            source_duration_ns: None,
            audio_decode_start_frame: 0,
            video_decode_start_index: 0,
            video_decode_start_ns: 0,
            audio_frames_read: 0,
            video_frames_read: 0,
            video_pts_read: 0,
            video_pts_skipped: 0,
            first_delivered_audio_frame: None,
            first_delivered_video_index: None,
            audio_eof: false,
            video_eof: false,
            audio_blocks_produced: 0,
            audio_frames_produced: 0,
            audio_frames_skipped: 0,
            audio_source_frames: 0,
            video_frames_produced: 0,
            video_frames_skipped: 0,
            video_source_frames: 0,
            audio_pending: 0,
            video_pending: 0,
            audio_high_water: 0,
            video_high_water: 0,
            video_high_water_bytes: 0,
            source_width: source.width,
            source_height: source.height,
            source_coded_width: source.coded_width,
            source_coded_height: source.coded_height,
            output_width: output.width,
            output_height: output.height,
            frame_bytes: output.frame_bytes,
            max_queued_video_bytes: output.frame_bytes * VIDEO_CAPACITY,
            source_timebase_numerator: source.numerator,
            source_timebase_denominator: source.denominator,
            source_first_pts: None,
            processes: vec![],
            closed: false,
            error: None,
        },
    }
}
#[cfg(test)]
fn initial_state(epoch: u64, n: u32, d: u32) -> State {
    initial_video_state(
        epoch,
        &VideoMetadata {
            numerator: n,
            denominator: d,
            width: 160,
            height: 90,
            coded_width: None,
            coded_height: None,
        },
        VideoFormat::new(160, 90).unwrap(),
    )
}
fn push_audio(shared: &Shared, block: AudioBlock, channels: u16) -> Result<bool, String> {
    let mut state = shared.state.lock().map_err(|_| "Decoder state poisoned")?;
    while state.audio.len() >= AUDIO_CAPACITY && !shared.stop.load(Ordering::Acquire) {
        state = shared
            .space
            .wait(state)
            .map_err(|_| "Decoder queue wait poisoned")?;
    }
    if shared.stop.load(Ordering::Acquire) {
        return Ok(false);
    }
    if block.epoch != state.stats.epoch
        || channels == 0
        || block.samples.len() % usize::from(channels) != 0
        || block.samples.len() > AUDIO_BLOCK_FRAMES * usize::from(channels)
    {
        return Err("Invalid bounded PCM queue block".into());
    }
    state.stats.audio_blocks_produced += 1;
    state
        .stats
        .first_delivered_audio_frame
        .get_or_insert(block.source_start_frame);
    state.stats.audio_frames_produced += (block.samples.len() / usize::from(channels)) as u64;
    state.audio.push_back(block);
    state.stats.audio_high_water = state.stats.audio_high_water.max(state.audio.len());
    Ok(true)
}
fn push_video(shared: &Shared, frame: VideoFrame) -> Result<bool, String> {
    let mut state = shared.state.lock().map_err(|_| "Decoder state poisoned")?;
    while state.video.len() >= VIDEO_CAPACITY && !shared.stop.load(Ordering::Acquire) {
        state = shared
            .space
            .wait(state)
            .map_err(|_| "Decoder queue wait poisoned")?;
    }
    if shared.stop.load(Ordering::Acquire) {
        return Ok(false);
    }
    if frame.epoch != state.stats.epoch || frame.rgb.len() != state.frame_bytes {
        return Err("Invalid bounded RGB queue frame".into());
    }
    state.stats.video_frames_produced += 1;
    state
        .stats
        .first_delivered_video_index
        .get_or_insert(frame.index as u64);
    state.video.push_back(frame);
    state.stats.video_high_water = state.stats.video_high_water.max(state.video.len());
    state.stats.video_high_water_bytes = state.stats.video_high_water * state.frame_bytes;
    Ok(true)
}

fn audited_binary(path: &Path, name: &str, expected_hash: &str) -> Result<PathBuf, String> {
    checked_binary(path, name, expected_hash, false)
}
fn checked_binary(
    path: &Path,
    name: &str,
    expected_hash: &str,
    allow_packaged: bool,
) -> Result<PathBuf, String> {
    let expected = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!("{name}-x86_64-pc-windows-msvc.exe"))
        .canonicalize()
        .ok();
    let actual = path.canonicalize().map_err(|e| e.to_string())?;
    let executable = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if expected.as_ref() != Some(&actual)
        && !(allow_packaged && packaged_location(&actual, &executable, name))
    {
        return Err("Streaming probes require the checkout's fixed audited sidecars".into());
    }
    let mut file = File::open(&actual).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 32768];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if format!("{:x}", hash.finalize()) != expected_hash {
        return Err(format!("Audited {name} SHA-256 does not match"));
    }
    Ok(actual)
}
fn packaged_location(actual: &Path, exe: &Path, name: &str) -> bool {
    let Some(parent) = exe.parent() else {
        return false;
    };
    actual == parent.join(format!("{name}.exe"))
        || actual == parent.join("binaries").join(format!("{name}.exe"))
}
fn input(path: &Path, extension: &str) -> Result<PathBuf, String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 16 * 1024 * 1024 * 1024 {
        return Err("Streaming fixture must be a regular nonempty file of at most 16 GiB".into());
    }
    let ext = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !(ext == extension || (extension == "mp4" && ext == "mov")) {
        return Err(format!("Expected a local {extension} fixture"));
    }
    Ok(path)
}
fn parse_video_metadata(bytes: &[u8]) -> Result<VideoMetadata, String> {
    let data: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let streams = data["streams"]
        .as_array()
        .ok_or("Missing stream metadata")?;
    if streams.len() != 1 {
        return Err("Expected exactly one selected video stream".into());
    }
    let (n, d) = streams[0]["time_base"]
        .as_str()
        .ok_or("Missing video timebase")?
        .split_once('/')
        .ok_or("Invalid video timebase")?;
    let n: u32 = n.parse().map_err(|_| "Invalid timebase numerator")?;
    let d: u32 = d.parse().map_err(|_| "Invalid timebase denominator")?;
    if n == 0 || d == 0 {
        return Err("Zero video timebase".into());
    }
    let dimension = |name: &str, optional: bool| -> Result<Option<u32>, String> {
        let value = &streams[0][name];
        if optional && (value.is_null() || value.as_u64() == Some(0)) {
            return Ok(None);
        }
        let dimension = value
            .as_u64()
            .filter(|value| (1..=16384).contains(value))
            .ok_or_else(|| format!("Video {name} must be an integer dimension within 1..=16384"))?;
        Ok(Some(dimension as u32))
    };
    Ok(VideoMetadata {
        numerator: n,
        denominator: d,
        width: dimension("width", false)?.ok_or("Missing video width")?,
        height: dimension("height", false)?.ok_or("Missing video height")?,
        coded_width: dimension("coded_width", true)?,
        coded_height: dimension("coded_height", true)?,
    })
}

fn indexed_video(bytes: &[u8], source: &VideoMetadata) -> Result<IndexedVideoMetadata, String> {
    let data: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let stream = &data["streams"][0];
    let rational = |field: &str| -> Result<(u32, u32), String> {
        let (n, d) = stream[field]
            .as_str()
            .ok_or_else(|| format!("Indexed seek requires {field}"))?
            .split_once('/')
            .ok_or("Invalid indexed frame rate")?;
        Ok((
            n.parse().map_err(|_| "Invalid indexed rate numerator")?,
            d.parse().map_err(|_| "Invalid indexed rate denominator")?,
        ))
    };
    let rate = rational("r_frame_rate")?;
    let average = rational("avg_frame_rate")?;
    Ok(IndexedVideoMetadata {
        timebase_numerator: source.numerator,
        timebase_denominator: source.denominator,
        start_pts: stream["start_pts"]
            .as_i64()
            .ok_or("Indexed seek requires explicit video start_pts")?,
        rate_numerator: rate.0,
        rate_denominator: rate.1,
        average_rate_numerator: average.0,
        average_rate_denominator: average.1,
        duration_ticks: stream["duration_ts"]
            .as_u64()
            .ok_or("Indexed seek requires video duration_ts")?,
        frame_count: stream["nb_frames"]
            .as_str()
            .ok_or("Indexed seek requires declared frame count")?
            .parse()
            .map_err(|_| "Invalid video frame count")?,
    })
}

fn indexed_audio(bytes: &[u8]) -> Result<IndexedAudioMetadata, String> {
    let data: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let streams = data["streams"]
        .as_array()
        .ok_or("Missing indexed audio stream metadata")?;
    if streams.len() != 1 {
        return Err("Indexed seek requires exactly one selected audio stream".into());
    }
    let stream = &streams[0];
    let (n, d) = stream["time_base"]
        .as_str()
        .ok_or("Missing PCM timebase")?
        .split_once('/')
        .ok_or("Invalid PCM timebase")?;
    Ok(IndexedAudioMetadata {
        codec_name: stream["codec_name"]
            .as_str()
            .ok_or("Missing PCM codec")?
            .into(),
        sample_rate: stream["sample_rate"]
            .as_str()
            .ok_or("Missing PCM sample rate")?
            .parse()
            .map_err(|_| "Invalid PCM sample rate")?,
        channels: u16::try_from(stream["channels"].as_u64().ok_or("Missing PCM channels")?)
            .map_err(|_| "PCM channels overflow")?,
        bits_per_sample: u16::try_from(
            stream["bits_per_sample"]
                .as_u64()
                .ok_or("Missing PCM bit depth")?,
        )
        .map_err(|_| "PCM bit depth overflow")?,
        timebase_numerator: n.parse().map_err(|_| "Invalid PCM timebase numerator")?,
        timebase_denominator: d.parse().map_err(|_| "Invalid PCM timebase denominator")?,
        duration_ticks: stream["duration_ts"]
            .as_u64()
            .ok_or("Missing PCM duration_ts")?,
    })
}

fn indexed_probe_interval(plan: &IndexedSeekPlan) -> String {
    // FFprobe 7.1's open-ended interval parser leaves end_is_offset/end unset
    // in its non-zeroed ReadInterval allocation. Supply an absolute end so all
    // fields used by read_interval_packets are initialized. One microsecond
    // beyond ceil(duration) preserves every admitted final-frame packet; the
    // reader still requires exact declared EOF and never fabricates timestamps.
    let end_micros = (u128::from(plan.video_total_frames) * 1_000_000).div_ceil(30) + 1;
    format!(
        "{}%{}.{:06}",
        plan.video_input_seconds,
        end_micros / 1_000_000,
        end_micros % 1_000_000
    )
}

fn timestamp_metadata(bytes: &[u8], source: &VideoMetadata) -> Result<TimestampMetadata, String> {
    let data: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let stream = &data["streams"][0];
    Ok(TimestampMetadata {
        numerator: source.numerator,
        denominator: source.denominator,
        start_pts: stream["start_pts"]
            .as_i64()
            .ok_or("PTS-index seek requires explicit stream start_pts")?,
        duration_ticks: stream["duration_ts"]
            .as_u64()
            .ok_or("PTS-index seek requires explicit positive stream duration_ts")?,
    })
}

fn source_fingerprint(path: &Path) -> Result<(u64, std::time::SystemTime, String), String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let info = file.metadata().map_err(|error| error.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let size = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok((
        info.len(),
        info.modified().map_err(|error| error.to_string())?,
        format!("{:x}", hash.finalize()),
    ))
}

fn pcm_seek_plan(
    metadata: &IndexedAudioMetadata,
    target: u64,
    output_rate: u32,
    output_channels: u16,
) -> Result<PcmSeekPlan, String> {
    if metadata.codec_name != "pcm_s16le"
        || metadata.sample_rate != 48000
        || metadata.channels != 2
        || metadata.bits_per_sample != 16
        || output_rate != 48000
        || output_channels != 2
        || metadata.timebase_numerator == 0
        || metadata.timebase_denominator == 0
    {
        return Err(
            "Indexed native PCM seek requires 48 kHz stereo PCM16 without resampling".into(),
        );
    }
    let scaled = u128::from(metadata.duration_ticks)
        .checked_mul(u128::from(metadata.timebase_numerator))
        .and_then(|value| value.checked_mul(48000))
        .ok_or("PCM duration overflow")?;
    let denominator = u128::from(metadata.timebase_denominator);
    if scaled == 0
        || scaled % denominator != 0
        || scaled / denominator > u128::from(MAX_SECONDS * 48000)
    {
        return Err("Indexed PCM duration is not bounded whole frames".into());
    }
    let total_frames = u64::try_from(scaled / denominator).map_err(|_| "PCM duration overflow")?;
    if target >= total_frames {
        return Err("PCM seek target must precede source EOF".into());
    }
    let seconds = target / 48000;
    let decode_start_frame = seconds * 48000;
    Ok(PcmSeekPlan {
        decode_start_frame,
        frames_to_discard: target - decode_start_frame,
        input_seconds: seconds.to_string(),
        total_frames,
    })
}

fn validate_compressed_audio(bytes: &[u8], rate: u32, channels: u16) -> Result<(), String> {
    let data: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let streams = data["streams"]
        .as_array()
        .ok_or("Missing compressed audio metadata")?;
    if streams.len() != 1 || streams[0]["codec_name"] != "aac" || rate != 48000 || channels != 2 {
        return Err(
            "PTS-index compressed audio is limited to AAC in M4A decoded to 48 kHz stereo".into(),
        );
    }
    let stream = &streams[0];
    let sample_rate: u32 = stream["sample_rate"]
        .as_str()
        .ok_or("Missing AAC sample rate")?
        .parse()
        .map_err(|_| "Invalid AAC sample rate")?;
    if !(8000..=192000).contains(&sample_rate)
        || !stream["channels"]
            .as_u64()
            .is_some_and(|value| (1..=8).contains(&value))
    {
        return Err("Unsupported AAC channel/sample-rate metadata".into());
    }
    // AAC duration metadata may exclude decoder delay/padding. Actual decoded
    // output-frame count comes only from sequential PCM EOF, never duration*rate.
    Ok(())
}

/// True EOF is distinct from a truncated frame/header.
fn packet(reader: &mut impl Read, length: usize) -> Result<Option<Vec<u8>>, String> {
    let mut bytes = vec![0; length];
    let mut used = 0;
    while used < length {
        match reader.read(&mut bytes[used..]) {
            Ok(0) if used == 0 => return Ok(None),
            Ok(0) => return Err("Truncated decoder packet".into()),
            Ok(n) => used += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(Some(bytes))
}
fn required(reader: &mut impl Read, length: usize) -> Result<Vec<u8>, String> {
    packet(reader, length)?.ok_or_else(|| "Unexpected decoder EOF".into())
}
fn le16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or("Truncated WAV field")?
            .try_into()
            .unwrap(),
    ))
}
fn le32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or("Truncated WAV field")?
            .try_into()
            .unwrap(),
    ))
}

struct WavStream<R> {
    reader: R,
    remaining: Option<u64>,
    riff_end: Option<u64>,
    position: u64,
    stride: usize,
    finished: bool,
}
impl<R: Read> WavStream<R> {
    fn new(mut reader: R, rate: u32, channels: u16) -> Result<Self, String> {
        let header = required(&mut reader, 12)?;
        if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
            return Err("Expected RIFF/WAVE pipe output".into());
        }
        let length = le32(&header, 4)?;
        let riff_end = if length == u32::MAX {
            None
        } else {
            Some(u64::from(length) + 8)
        };
        let mut position = 12u64;
        let mut format_seen = false;
        loop {
            let chunk = required(&mut reader, 8)?;
            position += 8;
            let len = le32(&chunk, 4)?;
            if &chunk[..4] == b"data" {
                if !format_seen {
                    return Err("WAV data precedes its format".into());
                }
                let remaining = if len == u32::MAX {
                    None
                } else {
                    Some(u64::from(len))
                };
                if riff_end.is_some() && remaining.is_none() {
                    return Err("Unknown WAV data length requires an unknown RIFF length".into());
                }
                if let (Some(end), Some(data)) = (riff_end, remaining) {
                    if position.checked_add(data).is_none_or(|v| v > end) {
                        return Err("WAV data exceeds declared RIFF length".into());
                    }
                }
                return Ok(Self {
                    reader,
                    remaining,
                    riff_end,
                    position,
                    stride: usize::from(channels) * 2,
                    finished: false,
                });
            }
            if len > 4096 || position + u64::from(len) > 65536 {
                return Err("WAV header exceeds bounded chunk limits".into());
            }
            let bytes = required(&mut reader, len as usize)?;
            position += u64::from(len);
            if &chunk[..4] == b"fmt " {
                if format_seen {
                    return Err("Duplicate WAV format".into());
                }
                format_seen = true;
                let tag = le16(&bytes, 0)?;
                if bytes.len() != 16
                    && (bytes.len() < 18 || usize::from(le16(&bytes, 16)?) + 18 != bytes.len())
                {
                    return Err("WAV format extension length is inconsistent".into());
                }
                if tag == 0xfffe {
                    const PCM: [u8; 16] = [
                        1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
                    ];
                    if bytes.len() < 40
                        || le16(&bytes, 16)? < 22
                        || le16(&bytes, 18)? != 16
                        || bytes.get(24..40) != Some(PCM.as_slice())
                    {
                        return Err("Unsupported extensible PCM WAV format".into());
                    }
                } else if tag != 1 {
                    return Err("WAV pipe must contain PCM16".into());
                }
                if le16(&bytes, 2)? != channels
                    || le32(&bytes, 4)? != rate
                    || le16(&bytes, 14)? != 16
                    || le16(&bytes, 12)? != channels * 2
                    || le32(&bytes, 8)? != rate * u32::from(channels) * 2
                {
                    return Err("WAV pipe does not match device PCM format".into());
                }
            }
            if len & 1 != 0 {
                required(&mut reader, 1)?;
                position += 1;
            }
        }
    }
    fn next(&mut self) -> Result<Option<Vec<i16>>, String> {
        if self.finished {
            return Ok(None);
        }
        if self.remaining == Some(0) {
            self.finish_known()?;
            return Ok(None);
        }
        let limit = (AUDIO_BLOCK_FRAMES * self.stride)
            .min(self.remaining.unwrap_or(u64::MAX).min(usize::MAX as u64) as usize);
        let mut data = vec![0; limit];
        let mut used = 0;
        while used < limit {
            match self.reader.read(&mut data[used..]) {
                Ok(0) => break,
                Ok(n) => used += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        if used < limit && self.remaining.is_some() {
            return Err("Truncated WAV data chunk".into());
        }
        if used % self.stride != 0 {
            return Err("WAV pipe ends inside an audio frame".into());
        }
        self.position += used as u64;
        if let Some(left) = &mut self.remaining {
            *left -= used as u64;
        }
        if used == 0 {
            self.finished = true;
            return Ok(None);
        }
        data.truncate(used);
        Ok(Some(
            data.chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect(),
        ))
    }
    fn finish_known(&mut self) -> Result<(), String> {
        let mut trailer = 0u64;
        loop {
            let Some(header) = packet(&mut self.reader, 8)? else {
                break;
            };
            self.position += 8;
            trailer += 8;
            if &header[..4] == b"data" || &header[..4] == b"fmt " {
                return Err("Duplicate WAV data/format trailer".into());
            }
            let length = u64::from(le32(&header, 4)?);
            trailer += length + (length & 1);
            if trailer > 65536 {
                return Err("WAV trailer exceeds 64 KiB".into());
            }
            required(&mut self.reader, (length + (length & 1)) as usize)?;
            self.position += length + (length & 1);
        }
        if self.riff_end.is_some_and(|end| end != self.position) {
            return Err("RIFF length does not match pipe EOF".into());
        }
        self.finished = true;
        Ok(())
    }
}

fn timestamp(line: &str) -> Result<i64, String> {
    let mut found = None;
    for part in line.trim_end().split('|') {
        if let Some(value) = part.strip_prefix("best_effort_timestamp=") {
            if found.is_some() {
                return Err("Duplicate timestamp key".into());
            }
            found = Some(
                value
                    .parse::<i64>()
                    .map_err(|_| "Missing/invalid best_effort_timestamp")?,
            );
        }
    }
    found.ok_or_else(|| "Frame record lacks best_effort_timestamp".into())
}
fn timestamp_ns(value: i64, first: i64, n: u32, d: u32) -> Result<u64, String> {
    let delta = u128::try_from(i128::from(value) - i128::from(first))
        .map_err(|_| "Video PTS precedes first PTS")?;
    let ns = delta
        .checked_mul(u128::from(n))
        .and_then(|v| v.checked_mul(1_000_000_000))
        .ok_or("Video PTS overflow")?
        / u128::from(d);
    if ns > u128::from(MAX_SECONDS) * 1_000_000_000 {
        return Err("Streaming video exceeds 24 hours".into());
    }
    u64::try_from(ns).map_err(|_| "Video PTS overflow".into())
}

fn source_video_anchor(
    index: &TimestampIndex,
    target: u64,
    source: bool,
) -> Result<timestamp_index::IndexedTimestamp, String> {
    if source && target >= index.duration_ns() {
        index
            .frame(
                index
                    .len()
                    .checked_sub(1)
                    .ok_or("Source video index is empty")?,
            )
            .ok_or_else(|| "Source video index lacks its final frame".into())
    } else {
        index.anchor(target)
    }
}

impl StreamingDecoder {
    pub fn start(config: StreamConfig) -> Result<Self, String> {
        Self::configured(config, false)
    }
    /// Source preview permits holding the final indexed video frame at its
    /// exclusive endpoint (or while a longer audio tail finishes). The logical
    /// query time is retained; only the decoder anchor selects an earlier PTS.
    pub fn start_source(config: StreamConfig) -> Result<Self, String> {
        if config.seek_mode != SeekMode::IndexedPts || config.start_video_ns > 60_000_000_000 {
            return Err("Source playback requires indexed PTS within 60 seconds".into());
        }
        Self::configured(config, true)
    }
    fn configured(config: StreamConfig, source_playback: bool) -> Result<Self, String> {
        let output_format = VideoFormat::new(config.video_width, config.video_height)?;
        if !(8000..=192000).contains(&config.sample_rate)
            || !(1..=8).contains(&config.channels)
            || config.epoch == 0
            || config.start_audio_frame > u64::from(config.sample_rate) * MAX_SECONDS
            || config.start_video_ns > MAX_SECONDS * 1_000_000_000
        {
            return Err("Invalid stream format, epoch or 24-hour source bounds".into());
        }
        let ffmpeg = checked_binary(
            &config.ffmpeg,
            "ffmpeg",
            "9752fd44c9e1867c8d26702ef81df1aad1442c1099888d02fed7c29de63aff39",
            source_playback,
        )?;
        let ffprobe = checked_binary(
            &config.ffprobe,
            "ffprobe",
            "3e3dfe5c77e7f3b8f9de1c8ec2ccfea4ec1e9f34e930b6f918894c9d2838219a",
            source_playback,
        )?;
        let video_extension = config
            .video
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(video_extension.as_str(), "mp4" | "mov") {
            return Err("Video source must be MP4 or MOV".into());
        }
        let video = input(&config.video, &video_extension)?;
        let audio_extension = config
            .audio
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let compressed_audio = matches!(audio_extension.as_str(), "m4a" | "mp4" | "mov");
        if source_playback && !compressed_audio {
            return Err("Source playback currently requires AAC audio".into());
        }
        if compressed_audio && config.seek_mode != SeekMode::IndexedPts {
            return Err(
                "AAC/M4A input is currently limited to explicit IndexedPts decoding".into(),
            );
        }
        let audio = input(
            &config.audio,
            if compressed_audio {
                &audio_extension
            } else {
                "wav"
            },
        )?;
        let audio_demuxer = if compressed_audio { "mov" } else { "wav" };
        let index_started = Instant::now();
        let index_source_before = if config.seek_mode == SeekMode::IndexedPts {
            Some(source_fingerprint(&video)?)
        } else {
            None
        };
        let mut metadata = command(&ffprobe);
        metadata
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file",
                "-f",
                "mov",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=time_base,width,height,coded_width,coded_height,start_pts,r_frame_rate,avg_frame_rate,duration_ts,nb_frames",
                "-of",
                "json",
            ])
            .arg(&video);
        let metadata_bytes = small_output(metadata)?;
        let source_metadata = parse_video_metadata(&metadata_bytes)?;
        let (n, d) = (source_metadata.numerator, source_metadata.denominator);
        let mut timestamp_seek = None;
        let mut pcm_seek = None;
        let mut index_build_ms = 0.0;
        let mut pts_start = None;
        if config.seek_mode == SeekMode::IndexedPts {
            let metadata = timestamp_metadata(&metadata_bytes, &source_metadata)?;
            pts_start = Some(metadata.start_pts);
            let mut index_command = command(&ffprobe);
            index_command
                .args([
                    "-v",
                    "error",
                    "-protocol_whitelist",
                    "file",
                    "-f",
                    "mov",
                    "-select_streams",
                    "v:0",
                    "-show_entries",
                    "frame=best_effort_timestamp",
                    "-of",
                    "json",
                ])
                .arg(&video);
            let bytes = bounded_output(index_command, MAX_INDEX_BYTES)?;
            let index = Arc::new(TimestampIndex::parse(&bytes, metadata)?);
            if Some(source_fingerprint(&video)?) != index_source_before {
                return Err("Video source changed while building its decoded PTS index".into());
            }
            let anchor = source_video_anchor(&index, config.start_video_ns, source_playback)?;
            if anchor.raw_pts < 0 {
                return Err("Negative absolute PTS seek anchors are parsed but not admitted by the current FFmpeg seek adapter".into());
            }
            let input_seconds = index.absolute_microseconds(anchor.raw_pts, false)?;
            let probe_interval = index.probe_interval(anchor.index)?;
            timestamp_seek = Some(TimestampSeekPlan {
                index,
                anchor: anchor.index,
                input_seconds,
                probe_interval,
            });
            index_build_ms = index_started.elapsed().as_secs_f64() * 1000.0;
            let mut audio_metadata = command(&ffprobe);
            audio_metadata
                .args([
                    "-v",
                    "error",
                    "-protocol_whitelist",
                    "file",
                    "-f",
                    audio_demuxer,
                    "-select_streams",
                    "a:0",
                    "-show_entries",
                    "stream=codec_name,sample_rate,channels,bits_per_sample,time_base,duration_ts",
                    "-of",
                    "json",
                ])
                .arg(&audio);
            let bytes = small_output(audio_metadata)?;
            if compressed_audio {
                validate_compressed_audio(&bytes, config.sample_rate, config.channels)?;
            } else {
                pcm_seek = Some(pcm_seek_plan(
                    &indexed_audio(&bytes)?,
                    config.start_audio_frame,
                    config.sample_rate,
                    config.channels,
                )?);
            }
        }
        let indexed_seek = if config.seek_mode == SeekMode::IndexedCfr {
            let mut audio_metadata = command(&ffprobe);
            audio_metadata
                .args([
                    "-v",
                    "error",
                    "-protocol_whitelist",
                    "file",
                    "-f",
                    "wav",
                    "-select_streams",
                    "a:0",
                    "-show_entries",
                    "stream=codec_name,sample_rate,channels,bits_per_sample,time_base,duration_ts",
                    "-of",
                    "json",
                ])
                .arg(&audio);
            Some(IndexedSeekPlan::new(
                config.start_video_ns,
                config.start_audio_frame,
                config.sample_rate,
                config.channels,
                &indexed_video(&metadata_bytes, &source_metadata)?,
                &indexed_audio(&small_output(audio_metadata)?)?,
            )?)
        } else {
            None
        };
        let scale = format!("scale={}:{}", output_format.width, output_format.height);
        let mut video_command = command(&ffmpeg);
        video_command.args([
            "-hide_banner",
            "-v",
            "error",
            "-nostdin",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-protocol_whitelist",
            "file",
            "-f",
            "mov",
        ]);
        if let Some(plan) = &indexed_seek {
            video_command.args(["-ss", &plan.video_input_seconds]);
        } else if let Some(plan) = &timestamp_seek {
            video_command.args(["-seek_timestamp", "1", "-ss", &plan.input_seconds]);
        }
        video_command.arg("-i").arg(&video).args([
            "-map",
            "0:v:0",
            "-an",
            "-vf",
            &scale,
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "rgb24",
            "-c:v",
            "rawvideo",
            "-threads",
            "1",
            "-f",
            "rawvideo",
            "pipe:1",
        ]);
        let (video_child, video_pipe) = PipeProcess::spawn(video_command, "stream-rgb")?;
        let mut pts_command = command(&ffprobe);
        pts_command.args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-f",
            "mov",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=best_effort_timestamp",
            "-of",
            "compact=p=0:nk=0",
        ]);
        if let Some(plan) = &indexed_seek {
            pts_command
                .arg("-read_intervals")
                .arg(indexed_probe_interval(plan));
        } else if let Some(plan) = &timestamp_seek {
            pts_command.arg("-read_intervals").arg(&plan.probe_interval);
        }
        pts_command.arg(&video);
        let (pts_child, pts_pipe) = PipeProcess::spawn(pts_command, "stream-pts")?;
        let mut audio_command = command(&ffmpeg);
        audio_command.args([
            "-hide_banner",
            "-v",
            "error",
            "-nostdin",
            "-threads",
            "1",
            "-protocol_whitelist",
            "file",
            "-f",
            audio_demuxer,
        ]);
        if let Some(plan) = &indexed_seek {
            audio_command.args(["-ss", &plan.audio_input_seconds]);
        } else if let Some(plan) = &pcm_seek {
            audio_command.args(["-ss", &plan.input_seconds]);
        }
        audio_command
            .arg("-i")
            .arg(audio)
            .args(["-map", "0:a:0", "-vn", "-c:a", "pcm_s16le", "-ar"])
            .arg(config.sample_rate.to_string())
            .arg("-ac")
            .arg(config.channels.to_string())
            .args(["-f", "wav", "pipe:1"]);
        let (audio_child, audio_pipe) = PipeProcess::spawn(audio_command, "stream-pcm")?;
        let mut state = initial_video_state(config.epoch, &source_metadata, output_format);
        state.stats.requested_video_ns = config.start_video_ns;
        state.stats.source_playback = source_playback;
        if let Some(plan) = &indexed_seek {
            state.stats.seek_mode = SeekMode::IndexedCfr;
            state.stats.video_seek_strategy = "indexed-cfr-absolute-grid";
            state.stats.audio_seek_strategy = "indexed-native-pcm";
            state.stats.audio_decode_start_frame = plan.audio_decode_start_frame;
            state.stats.video_decode_start_index = plan.video_anchor_index;
            state.stats.video_decode_start_ns = plan.video_anchor_ns;
            state.stats.source_first_pts = Some(0);
        }
        if let Some(plan) = &timestamp_seek {
            let anchor = plan
                .index
                .frame(plan.anchor)
                .ok_or("Missing selected PTS anchor")?;
            state.stats.seek_mode = SeekMode::IndexedPts;
            state.stats.video_seek_strategy = "full-decoded-pts-index-absolute-seek";
            state.stats.audio_seek_strategy = if compressed_audio {
                "sequential-decoded-compressed"
            } else {
                "indexed-native-pcm"
            };
            state.stats.source_index_frames = plan.index.len();
            state.stats.index_build_ms = index_build_ms;
            state.stats.index_rebuilt_this_session = true;
            state.stats.video_decode_start_index = anchor.index as u64;
            state.stats.video_decode_start_ns = anchor.pts_ns;
            state.stats.source_first_pts = Some(plan.index.first_pts());
            state.stats.source_start_pts = pts_start;
            state.stats.source_duration_ns = Some(plan.index.duration_ns());
            state.stats.audio_decode_start_frame =
                pcm_seek.as_ref().map_or(0, |plan| plan.decode_start_frame);
        }
        state.indexed_seek = indexed_seek;
        state.timestamp_seek = timestamp_seek;
        state.pcm_seek = pcm_seek;
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![video_child.clone(), pts_child.clone(), audio_child.clone()],
        });
        let mut decoder = Self {
            shared: shared.clone(),
            readers: Vec::new(),
        };
        let epoch = config.epoch;
        let channels = config.channels;
        let rate = config.sample_rate;
        let start_audio = config.start_audio_frame;
        let start_video = config.start_video_ns;
        let audio_shared = shared.clone();
        let audio_worker = thread::Builder::new()
            .name("stream-pcm-reader".into())
            .spawn(move || {
                reader_boundary(&audio_shared, || {
                    read_audio(
                        audio_pipe,
                        &audio_shared,
                        epoch,
                        rate,
                        channels,
                        start_audio,
                    )?;
                    if !audio_shared.stop.load(Ordering::Acquire) {
                        audio_child.wait_success()?;
                        audio_shared
                            .state
                            .lock()
                            .map_err(|_| "Decoder state poisoned")?
                            .stats
                            .audio_eof = true;
                    }
                    Ok(())
                })
            })
            .map_err(|e| format!("Cannot start PCM reader: {e}"))?;
        decoder.readers.push(audio_worker);
        let video_shared = shared;
        let video_worker = thread::Builder::new()
            .name("stream-rgb-pts-reader".into())
            .spawn(move || {
                reader_boundary(&video_shared, || {
                    read_video(
                        video_pipe,
                        pts_pipe,
                        &video_shared,
                        epoch,
                        n,
                        d,
                        start_video,
                    )?;
                    if !video_shared.stop.load(Ordering::Acquire) {
                        video_child.wait_success()?;
                        pts_child.wait_success()?;
                        video_shared
                            .state
                            .lock()
                            .map_err(|_| "Decoder state poisoned")?
                            .stats
                            .video_eof = true;
                    }
                    Ok(())
                })
            })
            .map_err(|e| format!("Cannot start video reader: {e}"))?;
        decoder.readers.push(video_worker);
        Ok(decoder)
    }
    pub fn poll_audio(&mut self) -> Result<Option<AudioBlock>, String> {
        let mut state = match self.shared.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
            Err(_) => return Err("Decoder state poisoned".into()),
        };
        if let Some(error) = &state.stats.error {
            return Err(error.clone());
        }
        let block = state.audio.pop_front();
        drop(state);
        if block.is_some() {
            self.shared.space.notify_all()
        }
        Ok(block)
    }
    pub fn poll_video(&mut self) -> Result<Option<VideoFrame>, String> {
        let mut state = match self.shared.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
            Err(_) => return Err("Decoder state poisoned".into()),
        };
        if let Some(error) = &state.stats.error {
            return Err(error.clone());
        }
        let frame = state.video.pop_front();
        drop(state);
        if frame.is_some() {
            self.shared.space.notify_all()
        }
        Ok(frame)
    }
    /// A validation caller may retain the owned process handle, never an
    /// externally supplied PID. Merely creating this closure changes no state.
    #[cfg(debug_assertions)]
    pub fn validation_video_failure(
        &self,
    ) -> Result<Box<dyn Fn() -> Result<serde_json::Value, String> + Send>, String> {
        let process = self
            .shared
            .processes
            .iter()
            .find(|p| p.info().is_ok_and(|i| i.label == "stream-rgb"))
            .cloned()
            .ok_or("Missing owned RGB decoder")?;
        Ok(Box::new(move || {
            let before = process.info()?;
            if before.exited {
                return Err("RGB decoder already exited before injection".into());
            }
            process.terminate()?;
            let after = process.info()?;
            Ok(
                serde_json::json!({"pid":after.pid,"label":after.label,"exited":after.exited,"exitCode":after.exit_code,"before":before,"ownedHandle":true}),
            )
        }))
    }
    pub fn stats(&self) -> Result<DecoderStats, String> {
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| "Decoder state poisoned")?;
        let mut stats = state.stats.clone();
        stats.audio_pending = state.audio.len();
        stats.video_pending = state.video.len();
        drop(state);
        stats.processes = self
            .shared
            .processes
            .iter()
            .map(|process| process.info())
            .collect::<Result<_, _>>()?;
        Ok(stats)
    }
    pub fn close(&mut self) -> Result<DecoderShutdown, String> {
        let started = Instant::now();
        self.shared.stop.store(true, Ordering::Release);
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stats.closed = true;
            state.audio.clear();
            state.video.clear();
        }
        self.shared.space.notify_all();
        let mut errors = Vec::new();
        for process in &self.shared.processes {
            if let Err(error) = process.terminate() {
                errors.push(error)
            }
        }
        for reader in self.readers.drain(..) {
            if reader.join().is_err() {
                errors.push("Decoder reader thread panicked outside its boundary".into())
            }
        }
        for process in &self.shared.processes {
            if let Err(error) = process.join_stderr() {
                errors.push(error)
            }
        }
        let stats = self.stats()?;
        let cleanup_errors = errors.clone();
        if let Some(error) = &stats.error {
            errors.push(error.clone())
        }
        let reaped = stats.processes.iter().all(|process| process.exited);
        Ok(DecoderShutdown {
            joined: true,
            processes_reaped: reaped,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            stats,
            cleanup_errors,
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("; "))
            },
        })
    }
}
impl Drop for StreamingDecoder {
    fn drop(&mut self) {
        if !self.readers.is_empty() {
            let _ = self.close();
        }
    }
}

fn reader_boundary(shared: &Shared, work: impl FnOnce() -> Result<(), String>) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err("Streaming decoder reader panicked".into()));
    if let Err(error) = result {
        if !shared.stop.swap(true, Ordering::AcqRel) {
            {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.stats.error = Some(error.chars().take(65536).collect());
                state.stats.closed = true;
                state.audio.clear();
                state.video.clear();
            }
            shared.space.notify_all();
            for process in &shared.processes {
                let _ = process.terminate();
            }
        }
    }
}
fn read_audio(
    reader: impl Read,
    shared: &Shared,
    epoch: u64,
    rate: u32,
    channels: u16,
    start: u64,
) -> Result<(), String> {
    let mut wav = WavStream::new(reader, rate, channels)?;
    let indexed = {
        let state = shared.state.lock().map_err(|_| "Decoder state poisoned")?;
        state.pcm_seek.clone().or_else(|| {
            state.indexed_seek.as_ref().map(|plan| PcmSeekPlan {
                decode_start_frame: plan.audio_decode_start_frame,
                frames_to_discard: plan.audio_frames_to_discard,
                input_seconds: plan.audio_input_seconds.clone(),
                total_frames: plan.audio_total_frames,
            })
        })
    };
    if indexed.as_ref().is_some_and(|plan| {
        plan.decode_start_frame.checked_add(plan.frames_to_discard) != Some(start)
    }) {
        return Err("Indexed PCM discard plan does not match requested source frame".into());
    }
    let mut cursor = indexed.as_ref().map_or(0, |plan| plan.decode_start_frame);
    let mut read_frames = 0u64;
    while !shared.stop.load(Ordering::Acquire) {
        let Some(samples) = wav.next()? else {
            if read_frames == 0 {
                return Err("Decoded audio stream is empty".into());
            }
            if cursor < start {
                return Err("Audio seek target exceeds source EOF".into());
            }
            if indexed
                .as_ref()
                .is_some_and(|plan| cursor != plan.total_frames)
            {
                return Err("Indexed audio EOF does not match declared PCM duration".into());
            }
            return Ok(());
        };
        let frames = (samples.len() / usize::from(channels)) as u64;
        read_frames = read_frames
            .checked_add(frames)
            .ok_or("Decoded audio read counter overflow")?;
        let end = cursor
            .checked_add(frames)
            .ok_or("Audio frame count overflow")?;
        if end > u64::from(rate) * MAX_SECONDS {
            return Err("Streaming audio exceeds 24 hours".into());
        }
        if indexed.as_ref().is_some_and(|plan| end > plan.total_frames) {
            return Err("Indexed audio exceeded declared source duration".into());
        }
        let skipped = start.saturating_sub(cursor).min(frames);
        {
            let mut state = shared.state.lock().map_err(|_| "Decoder state poisoned")?;
            state.stats.audio_source_frames = end;
            state.stats.audio_frames_read = read_frames;
            state.stats.audio_frames_skipped += skipped;
        }
        if skipped < frames {
            let samples = samples[skipped as usize * usize::from(channels)..]
                .iter()
                .map(|sample| f32::from(*sample) / 32768.0)
                .collect();
            if !push_audio(
                shared,
                AudioBlock {
                    epoch,
                    source_start_frame: cursor + skipped,
                    samples,
                },
                channels,
            )? {
                return Ok(());
            }
        }
        cursor = end;
    }
    Ok(())
}
fn read_video(
    mut rgb: impl Read,
    pts: impl Read,
    shared: &Shared,
    epoch: u64,
    n: u32,
    d: u32,
    start: u64,
) -> Result<(), String> {
    let (frame_bytes, indexed, timestamp_seek) = {
        let state = shared.state.lock().map_err(|_| "Decoder state poisoned")?;
        (
            state.frame_bytes,
            state.indexed_seek.clone(),
            state.timestamp_seek.clone(),
        )
    };
    if let Some(plan) = timestamp_seek {
        return read_timestamp_video(rgb, pts, shared, epoch, &plan, frame_bytes);
    }
    if let Some(plan) = indexed {
        return read_indexed_video(rgb, pts, shared, epoch, n, d, &plan, frame_bytes);
    }
    let mut pts = BufReader::new(pts);
    let mut first = None;
    let mut previous = None;
    let mut index = 0usize;
    let mut held: Option<VideoFrame> = None;
    let mut selected = start == 0;
    while !shared.stop.load(Ordering::Acquire) {
        let bytes = packet(&mut rgb, frame_bytes)?;
        let mut empty_lines = 0;
        let line = loop {
            let line = bounded_line(&mut pts)?;
            if line.as_ref().is_some_and(|line| line.trim().is_empty()) {
                empty_lines += 1;
                if empty_lines > 16 {
                    return Err("Unexpected blank timestamp records".into());
                }
                continue;
            }
            break line;
        };
        match (bytes, line) {
            (None, None) => {
                if index == 0 {
                    return Err("Decoded video stream is empty".into());
                }
                if let Some(frame) = held.take() {
                    if !push_video(shared, frame)? {
                        return Ok(());
                    }
                }
                return Ok(());
            }
            (Some(bytes), Some(line)) => {
                let value = timestamp(&line)?;
                let initial = *first.get_or_insert(value);
                if previous.is_some_and(|old| value <= old) {
                    return Err("Video PTS must be strictly increasing".into());
                }
                previous = Some(value);
                let ns = timestamp_ns(value, initial, n, d)?;
                {
                    let mut state = shared.state.lock().map_err(|_| "Decoder state poisoned")?;
                    state.stats.source_first_pts = Some(initial);
                    state.stats.video_source_frames = index as u64 + 1;
                    state.stats.video_frames_read += 1;
                    state.stats.video_pts_read += 1;
                }
                let frame = VideoFrame {
                    epoch,
                    index,
                    pts_ns: ns,
                    rgb: Arc::from(bytes),
                };
                index += 1;
                if selected {
                    if !push_video(shared, frame)? {
                        return Ok(());
                    }
                } else if ns <= start {
                    if held.replace(frame).is_some() {
                        shared
                            .state
                            .lock()
                            .map_err(|_| "Decoder state poisoned")?
                            .stats
                            .video_frames_skipped += 1;
                    }
                } else {
                    selected = true;
                    if let Some(earlier) = held.take() {
                        if !push_video(shared, earlier)? {
                            return Ok(());
                        }
                    }
                    if !push_video(shared, frame)? {
                        return Ok(());
                    }
                }
            }
            _ => {
                return Err(
                    "RGB frame stream and timestamp stream ended at different frame counts".into(),
                );
            }
        }
    }
    Ok(())
}

fn next_pts(reader: &mut impl std::io::BufRead) -> Result<Option<i64>, String> {
    for _ in 0..=16 {
        match bounded_line(reader)? {
            None => return Ok(None),
            Some(line) if line.trim().is_empty() => continue,
            Some(line) => return timestamp(&line).map(Some),
        }
    }
    Err("Unexpected blank timestamp records".into())
}

fn read_timestamp_video(
    mut rgb: impl Read,
    pts: impl Read,
    shared: &Shared,
    epoch: u64,
    plan: &TimestampSeekPlan,
    frame_bytes: usize,
) -> Result<(), String> {
    let mut pts = BufReader::new(pts);
    let mut previous = None;
    let mut expected_index = plan.anchor;
    while !shared.stop.load(Ordering::Acquire) {
        let selected = loop {
            let Some(raw) = next_pts(&mut pts)? else {
                break None;
            };
            let frame = plan.index.frame_by_raw(raw)?;
            if previous.is_some_and(|old| frame.index != old + 1) {
                return Err("Streaming PTS skipped or reordered a real source-index frame".into());
            }
            previous = Some(frame.index);
            {
                let mut state = shared.state.lock().map_err(|_| "Decoder state poisoned")?;
                state.stats.video_pts_read += 1;
                if frame.index < plan.anchor {
                    state.stats.video_pts_skipped += 1;
                }
            }
            if frame.index < plan.anchor {
                continue;
            }
            break Some(frame);
        };
        let bytes = packet(&mut rgb, frame_bytes)?;
        match (bytes, selected) {
            (None, None) => {
                if expected_index != plan.index.len() {
                    return Err(
                        "Indexed PTS/RGB ended before the complete real source index".into(),
                    );
                }
                return Ok(());
            }
            (Some(bytes), Some(frame)) => {
                if frame.index != expected_index {
                    return Err(
                        "Accurate RGB seek did not begin at the selected real PTS anchor".into(),
                    );
                }
                {
                    let mut state = shared.state.lock().map_err(|_| "Decoder state poisoned")?;
                    state.stats.video_frames_read += 1;
                    state.stats.video_source_frames = frame.index as u64 + 1;
                }
                if !push_video(
                    shared,
                    VideoFrame {
                        epoch,
                        index: frame.index,
                        pts_ns: frame.pts_ns,
                        rgb: Arc::from(bytes),
                    },
                )? {
                    return Ok(());
                }
                expected_index += 1;
            }
            (bytes, frame) => {
                return Err(format!(
                    "Real-index RGB/PTS count mismatch at source frame {expected_index}: rgbBytes={:?}, selectedFrame={:?}, totalIndexFrames={}",
                    bytes.as_ref().map(Vec::len),
                    frame.map(|value| value.index),
                    plan.index.len()
                ));
            }
        }
    }
    Ok(())
}

fn read_indexed_video(
    mut rgb: impl Read,
    pts: impl Read,
    shared: &Shared,
    epoch: u64,
    n: u32,
    d: u32,
    plan: &IndexedSeekPlan,
    frame_bytes: usize,
) -> Result<(), String> {
    let mut pts = BufReader::new(pts);
    let mut previous_index = None;
    let mut expected_index = plan.video_anchor_index;
    let mut delivered = false;
    while !shared.stop.load(Ordering::Acquire) {
        // ffprobe seeks to a preceding keyframe; FFmpeg accurate input seeking
        // discards those frames. Filter only PTS records before pairing RGB.
        let selected = loop {
            let Some(value) = next_pts(&mut pts)? else {
                break None;
            };
            let index = index_for_pts(value, n, d)?;
            if previous_index.is_some_and(|previous| index != previous + 1) {
                return Err("Indexed source PTS is not consecutive 30 fps CFR".into());
            }
            previous_index = Some(index);
            if index >= plan.video_total_frames {
                return Err("Indexed video PTS exceeds declared source frame count".into());
            }
            {
                let mut state = shared.state.lock().map_err(|_| "Decoder state poisoned")?;
                state.stats.video_pts_read += 1;
                if index < plan.video_anchor_index {
                    state.stats.video_pts_skipped += 1;
                }
            }
            if index < plan.video_anchor_index {
                continue;
            }
            break Some((value, index));
        };
        let bytes = packet(&mut rgb, frame_bytes)?;
        match (bytes, selected) {
            (None, None) => {
                if !delivered || expected_index != plan.video_total_frames {
                    return Err(
                        "Indexed RGB/PTS ended before its declared source frame count".into(),
                    );
                }
                return Ok(());
            }
            (Some(bytes), Some((value, index))) => {
                if index != expected_index {
                    return Err(
                        "Accurate RGB seek did not align with the exact admitted frame anchor"
                            .into(),
                    );
                }
                let ns = timestamp_ns(value, 0, n, d)?;
                if !delivered && ns != plan.video_anchor_ns {
                    return Err(
                        "Indexed first frame PTS did not match its exact source anchor".into(),
                    );
                }
                {
                    let mut state = shared.state.lock().map_err(|_| "Decoder state poisoned")?;
                    state.stats.source_first_pts = Some(0);
                    state.stats.video_source_frames = index + 1;
                    state.stats.video_frames_read += 1;
                }
                let frame = VideoFrame {
                    epoch,
                    index: usize::try_from(index).map_err(|_| "Source frame index overflow")?,
                    pts_ns: ns,
                    rgb: Arc::from(bytes),
                };
                if !push_video(shared, frame)? {
                    return Ok(());
                }
                expected_index += 1;
                delivered = true;
            }
            (bytes, selected) => {
                let stats = shared
                    .state
                    .lock()
                    .map_err(|_| "Decoder state poisoned")?
                    .stats
                    .clone();
                let mut detail = format!(
                    "Indexed RGB and filtered frame PTS ended at different counts: rgbBytes={:?}, selectedPts={selected:?}, expectedIndex={expected_index}, totalFrames={}, rgbRead={}, ptsRead={}, ptsSkipped={}",
                    bytes.as_ref().map(Vec::len),
                    plan.video_total_frames,
                    stats.video_frames_read,
                    stats.video_pts_read,
                    stats.video_pts_skipped
                );
                for process in &shared.processes {
                    if let Ok(info) = process.info() {
                        detail.push_str(&format!(
                            "; {} pid={} exited={} code={:?}",
                            info.label, info.pid, info.exited, info.exit_code
                        ));
                        if info.exited {
                            let stderr = process.join_stderr().unwrap_or_else(|error| error);
                            detail.push_str(&format!(
                                " stderr={:?}",
                                stderr.chars().take(8192).collect::<String>()
                            ));
                        }
                    }
                }
                return Err(detail);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn failed_decoder_fixture(error: &str, panic_reader: bool) -> StreamingDecoder {
    let mut state = initial_state(17, 1, 30);
    state.stats.error = Some(error.to_owned());
    state.stats.closed = true;
    StreamingDecoder {
        shared: Arc::new(Shared {
            state: Mutex::new(state),
            space: Condvar::new(),
            stop: AtomicBool::new(true),
            processes: Vec::new(),
        }),
        readers: if panic_reader {
            vec![thread::spawn(|| {
                panic!("controlled decoder reader failure")
            })]
        } else {
            Vec::new()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_endpoint_uses_final_real_anchor_without_relaxing_diagnostic_seek() {
        let index = TimestampIndex::parse(
            br#"{"frames":[{"best_effort_timestamp":0},{"best_effort_timestamp":33},{"best_effort_timestamp":66}]}"#,
            TimestampMetadata { numerator: 1, denominator: 1000, start_pts: 0, duration_ticks: 100 },
        ).unwrap();
        assert!(source_video_anchor(&index, 100_000_000, false).is_err());
        assert_eq!(
            source_video_anchor(&index, 100_000_000, true)
                .unwrap()
                .index,
            2
        );
        assert_eq!(
            source_video_anchor(&index, 150_000_000, true)
                .unwrap()
                .pts_ns,
            66_000_000
        );
        assert_eq!(
            source_video_anchor(&index, 50_000_000, true).unwrap().index,
            1
        );
    }
    use std::io::Cursor;

    fn test_timestamp_plan() -> TimestampSeekPlan {
        let index = Arc::new(TimestampIndex::parse(br#"{"frames":[{"best_effort_timestamp":2000},{"best_effort_timestamp":2033},{"best_effort_timestamp":2100},{"best_effort_timestamp":2350}]}"#,
            TimestampMetadata { numerator: 1, denominator: 1000, start_pts: 2000, duration_ticks: 1000 }).unwrap());
        TimestampSeekPlan {
            anchor: 2,
            input_seconds: "2.100000".into(),
            probe_interval: "2.100000%3.000001".into(),
            index,
        }
    }

    #[test]
    fn real_pts_reader_keeps_vfr_global_indices_and_nonzero_origin_without_average_fps() {
        let mut state = initial_state(4, 1, 1000);
        state.timestamp_seek = Some(test_timestamp_plan());
        let shared = Shared {
            state: Mutex::new(state),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        };
        let rgb = [vec![2; FRAME_BYTES], vec![3; FRAME_BYTES]].concat();
        read_video(Cursor::new(rgb), Cursor::new(b"best_effort_timestamp=2000\nbest_effort_timestamp=2033\nbest_effort_timestamp=2100\nbest_effort_timestamp=2350\n"), &shared, 4, 1, 1000, 120_000_000).unwrap();
        let state = shared.state.lock().unwrap();
        assert_eq!(
            (state.video[0].index, state.video[0].pts_ns),
            (2, 100_000_000)
        );
        assert_eq!(
            (state.video[1].index, state.video[1].pts_ns),
            (3, 350_000_000)
        );
        assert_eq!(state.stats.video_frames_read, 2);
        assert_eq!(state.stats.video_pts_skipped, 2);
        assert_eq!(state.stats.video_source_frames, 4);
    }

    #[test]
    fn real_pts_reader_rejects_missing_index_entries_and_unpaired_eof() {
        for timestamps in [
            b"best_effort_timestamp=2110\n".as_slice(),
            b"best_effort_timestamp=2350\n".as_slice(),
            b"best_effort_timestamp=2100\n".as_slice(),
        ] {
            let mut state = initial_state(4, 1, 1000);
            state.timestamp_seek = Some(test_timestamp_plan());
            let shared = Shared {
                state: Mutex::new(state),
                space: Condvar::new(),
                stop: AtomicBool::new(false),
                processes: vec![],
            };
            assert!(
                read_video(
                    Cursor::new(vec![0; FRAME_BYTES * 2]),
                    Cursor::new(timestamps),
                    &shared,
                    4,
                    1,
                    1000,
                    120_000_000
                )
                .is_err()
            );
            assert!(!shared.state.lock().unwrap().stats.video_eof);
        }
    }

    #[test]
    fn compressed_audio_origin_reading_retains_padding_instead_of_metadata_duration_truncation() {
        let state = initial_state(5, 1, 1000);
        let shared = Shared {
            state: Mutex::new(state),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        };
        let samples: Vec<i16> = (0..1024).map(|value| value as i16).collect();
        read_audio(Cursor::new(wav(&samples, true)), &shared, 5, 48000, 2, 480).unwrap();
        let state = shared.state.lock().unwrap();
        assert_eq!(state.stats.audio_source_frames, 512);
        assert_eq!(state.stats.audio_frames_read, 512);
        assert_eq!(state.stats.audio_frames_skipped, 480);
        assert_eq!(state.stats.audio_frames_produced, 32);
        assert_eq!(state.audio[0].source_start_frame, 480);
        assert_eq!(state.audio[0].samples[0], 960.0 / 32768.0);
        assert!(
            validate_compressed_audio(
                br#"{"streams":[{"codec_name":"aac","sample_rate":"44100","channels":1}]}"#,
                48000,
                2
            )
            .is_ok()
        );
        assert!(
            validate_compressed_audio(
                br#"{"streams":[{"codec_name":"mp3","sample_rate":"48000","channels":2}]}"#,
                48000,
                2
            )
            .is_err()
        );
    }

    fn test_indexed_plan() -> IndexedSeekPlan {
        IndexedSeekPlan {
            video_anchor_index: 901,
            video_anchor_ns: 30_033_333_333,
            video_input_seconds: "30.033333".into(),
            video_total_frames: 903,
            audio_decode_start_frame: 48000,
            audio_frames_to_discard: 1,
            audio_input_seconds: "1".into(),
            audio_total_frames: 48004,
        }
    }

    #[test]
    fn indexed_probe_uses_explicit_absolute_end_past_all_admitted_frame_timestamps() {
        let plan = test_indexed_plan();
        assert_eq!(indexed_probe_interval(&plan), "30.033333%30.100001");
        let mut fractional = plan.clone();
        fractional.video_total_frames = 902;
        assert_eq!(indexed_probe_interval(&fractional), "30.033333%30.066668");
        let mut full = plan;
        full.video_total_frames = 1800;
        full.video_input_seconds = "59.000000".into();
        assert_eq!(indexed_probe_interval(&full), "59.000000%60.000001");
    }

    #[test]
    fn indexed_reader_filters_preceding_keyframe_pts_without_discarding_rgb_anchor() {
        let mut state = initial_state(3, 1, 15360);
        state.indexed_seek = Some(test_indexed_plan());
        let shared = Shared {
            state: Mutex::new(state),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        };
        let rgb = [vec![91; FRAME_BYTES], vec![92; FRAME_BYTES]].concat();
        read_video(Cursor::new(rgb), Cursor::new(b"best_effort_timestamp=460800\nbest_effort_timestamp=461312\nbest_effort_timestamp=461824\n"), &shared, 3, 1, 15360, 30_033_333_333).unwrap();
        let state = shared.state.lock().unwrap();
        assert_eq!(state.video.len(), 2);
        assert_eq!(state.video[0].index, 901);
        assert_eq!(state.video[0].rgb[0], 91);
        assert_eq!(state.video[0].pts_ns, 30_033_333_333);
        assert_eq!(state.video[1].index, 902);
        assert_eq!(state.video[1].rgb[0], 92);
        assert_eq!(state.stats.video_frames_read, 2);
        assert_eq!(state.stats.video_source_frames, 903);
        assert_eq!(state.stats.video_pts_read, 3);
        assert_eq!(state.stats.video_pts_skipped, 1);
        assert_eq!(state.stats.source_first_pts, Some(0));
    }

    #[test]
    fn indexed_reader_rejects_missing_anchor_grid_gaps_and_unpaired_eof() {
        for timestamps in [
            b"best_effort_timestamp=461824\n".as_slice(),
            b"best_effort_timestamp=461313\n".as_slice(),
            b"best_effort_timestamp=461312\nbest_effort_timestamp=462336\n".as_slice(),
            b"best_effort_timestamp=461312\n".as_slice(),
        ] {
            let mut state = initial_state(3, 1, 15360);
            state.indexed_seek = Some(test_indexed_plan());
            let shared = Shared {
                state: Mutex::new(state),
                space: Condvar::new(),
                stop: AtomicBool::new(false),
                processes: vec![],
            };
            assert!(
                read_video(
                    Cursor::new(vec![1; FRAME_BYTES * 2]),
                    Cursor::new(timestamps),
                    &shared,
                    3,
                    1,
                    15360,
                    30_033_333_333
                )
                .is_err()
            );
            assert!(!shared.state.lock().unwrap().stats.video_eof);
        }
    }

    #[test]
    fn indexed_audio_uses_absolute_anchor_and_counts_only_actual_read_frames() {
        let mut state = initial_state(3, 1, 30);
        state.indexed_seek = Some(test_indexed_plan());
        let shared = Shared {
            state: Mutex::new(state),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        };
        read_audio(
            Cursor::new(wav(&[100, 101, 200, 201, 300, 301, 400, 401], true)),
            &shared,
            3,
            48000,
            2,
            48001,
        )
        .unwrap();
        let state = shared.state.lock().unwrap();
        assert_eq!(state.audio[0].source_start_frame, 48001);
        assert_eq!(state.audio[0].samples[0], 200.0 / 32768.0);
        assert_eq!(state.stats.audio_source_frames, 48004);
        assert_eq!(state.stats.audio_frames_read, 4);
        assert_eq!(state.stats.audio_frames_skipped, 1);
        assert_eq!(state.stats.audio_frames_produced, 3);
        assert_eq!(state.stats.first_delivered_audio_frame, Some(48001));
    }

    #[test]
    fn output_video_dimensions_bound_frame_allocations_before_decoding() {
        assert_eq!(VideoFormat::new(160, 90).unwrap().frame_bytes, 43_200);
        assert_eq!(VideoFormat::new(1920, 1080).unwrap().frame_bytes, 6_220_800);
        for (width, height) in [
            (0, 90),
            (160, 0),
            (1921, 1080),
            (1920, 1081),
            (u32::MAX, u32::MAX),
        ] {
            assert!(VideoFormat::new(width, height).is_err());
        }
    }

    #[test]
    fn metadata_requires_one_real_video_size_and_preserves_coded_dimensions() {
        let metadata = parse_video_metadata(br#"{"streams":[{"time_base":"1/15360","width":1920,"height":1080,"coded_width":1920,"coded_height":1088}]}"#).unwrap();
        assert_eq!((metadata.numerator, metadata.denominator), (1, 15360));
        assert_eq!((metadata.width, metadata.height), (1920, 1080));
        assert_eq!(
            (metadata.coded_width, metadata.coded_height),
            (Some(1920), Some(1088))
        );
        for invalid in [
            r#"{"streams":[]}"#,
            r#"{"streams":[{"time_base":"1/30","width":1920}] }"#,
            r#"{"streams":[{"time_base":"1/30","width":0,"height":1080}]}"#,
            r#"{"streams":[{"time_base":"1/30","width":1920.5,"height":1080}]}"#,
            r#"{"streams":[{"time_base":"1/30","width":1920,"height":1080},{"time_base":"1/30","width":1,"height":1}]}"#,
        ] {
            assert!(parse_video_metadata(invalid.as_bytes()).is_err());
        }
    }

    #[test]
    fn full_hd_reader_queues_exact_frame_payloads_and_keeps_four_frame_byte_budget() {
        let output = VideoFormat::new(1920, 1080).unwrap();
        let metadata = VideoMetadata {
            numerator: 1,
            denominator: 30,
            width: 1920,
            height: 1080,
            coded_width: Some(1920),
            coded_height: Some(1080),
        };
        let shared = Shared {
            state: Mutex::new(initial_video_state(9, &metadata, output)),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        };
        let bytes = vec![73_u8; 6_220_800];
        read_video(
            Cursor::new(bytes),
            Cursor::new(b"best_effort_timestamp=0\n"),
            &shared,
            9,
            1,
            30,
            0,
        )
        .unwrap();
        let frame = {
            let state = shared.state.lock().unwrap();
            assert_eq!(state.video[0].rgb.len(), 6_220_800);
            assert_eq!(
                (state.stats.source_width, state.stats.source_height),
                (1920, 1080)
            );
            assert_eq!(
                (state.stats.output_width, state.stats.output_height),
                (1920, 1080)
            );
            assert_eq!(state.stats.frame_bytes, 6_220_800);
            assert_eq!(state.stats.max_queued_video_bytes, 24_883_200);
            Arc::clone(&state.video[0].rgb)
        };
        for index in 1..4 {
            push_video(
                &shared,
                VideoFrame {
                    epoch: 9,
                    index,
                    pts_ns: index as u64 * 33_333_333,
                    rgb: Arc::clone(&frame),
                },
            )
            .unwrap();
        }
        let stats = shared.state.lock().unwrap().stats.clone();
        assert_eq!(stats.video_high_water, 4);
        assert_eq!(stats.video_high_water_bytes, 24_883_200);
        shared.stop.store(true, Ordering::Release);
        assert!(
            !push_video(
                &shared,
                VideoFrame {
                    epoch: 9,
                    index: 4,
                    pts_ns: 133_333_333,
                    rgb: frame
                }
            )
            .unwrap()
        );
    }

    #[test]
    fn full_hd_queue_rejects_toy_frames_and_truncated_full_hd_packets() {
        let output = VideoFormat::new(1920, 1080).unwrap();
        let metadata = VideoMetadata {
            numerator: 1,
            denominator: 30,
            width: 1920,
            height: 1080,
            coded_width: None,
            coded_height: None,
        };
        let shared = Shared {
            state: Mutex::new(initial_video_state(1, &metadata, output)),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        };
        assert!(
            push_video(
                &shared,
                VideoFrame {
                    epoch: 1,
                    index: 0,
                    pts_ns: 0,
                    rgb: Arc::from(vec![0; 43_200])
                }
            )
            .is_err()
        );
        assert!(
            read_video(
                Cursor::new(vec![0; 6_220_799]),
                Cursor::new(b"best_effort_timestamp=0\n"),
                &shared,
                1,
                1,
                30,
                0
            )
            .is_err()
        );
        assert_eq!(shared.state.lock().unwrap().stats.video_frames_produced, 0);
    }
    fn wav(samples: &[i16], unknown: bool) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(
            &(if unknown {
                u32::MAX
            } else {
                36 + samples.len() as u32 * 2
            })
            .to_le_bytes(),
        );
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&48000u32.to_le_bytes());
        bytes.extend_from_slice(&192000u32.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(
            &(if unknown {
                u32::MAX
            } else {
                samples.len() as u32 * 2
            })
            .to_le_bytes(),
        );
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes())
        }
        bytes
    }
    #[test]
    fn wav_pipe_unknown_lengths_are_read_in_bounded_whole_frames() {
        let source = vec![1234i16; 1000];
        let mut reader = WavStream::new(Cursor::new(wav(&source, true)), 48000, 2).unwrap();
        assert_eq!(reader.next().unwrap().unwrap(), vec![1234; 960]);
        assert_eq!(reader.next().unwrap().unwrap(), vec![1234; 40]);
        assert!(reader.next().unwrap().is_none());
    }
    #[test]
    fn wav_finite_and_truncated_packets_are_distinguished() {
        let mut reader = WavStream::new(Cursor::new(wav(&[1, 2, 3, 4], false)), 48000, 2).unwrap();
        assert_eq!(reader.next().unwrap().unwrap(), [1, 2, 3, 4]);
        assert!(reader.next().unwrap().is_none());
        for unknown in [false, true] {
            let mut source = wav(&[1, 2, 3, 4], unknown);
            source.pop();
            let mut reader = WavStream::new(Cursor::new(source), 48000, 2).unwrap();
            assert!(reader.next().is_err());
        }
    }
    #[test]
    fn pts_records_keep_real_ticks_and_reject_missing_duplicate_or_invalid_values() {
        assert_eq!(
            timestamp("best_effort_timestamp=-1001|side_data_type=known\n").unwrap(),
            -1001
        );
        assert_eq!(timestamp_ns(1001, -1001, 1, 30000).unwrap(), 66_733_333);
        for line in [
            "side_data_type=x\n",
            "best_effort_timestamp=N/A\n",
            "best_effort_timestamp=1|best_effort_timestamp=2\n",
        ] {
            assert!(timestamp(line).is_err())
        }
        assert!(timestamp_ns(-2, -1, 1, 30).is_err());
    }
    #[test]
    fn raw_rgb_eof_requires_a_complete_frame() {
        assert!(packet(&mut Cursor::new(vec![0; FRAME_BYTES - 1]), FRAME_BYTES).is_err());
        assert!(
            packet(&mut Cursor::new(Vec::<u8>::new()), FRAME_BYTES)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            packet(&mut Cursor::new(vec![0; FRAME_BYTES]), FRAME_BYTES)
                .unwrap()
                .unwrap()
                .len(),
            FRAME_BYTES
        );
    }
    #[test]
    fn contradictory_wav_length_and_format_extensions_are_rejected() {
        let mut bytes = wav(&[1, 2], true);
        bytes[4..8].copy_from_slice(&40u32.to_le_bytes());
        assert!(WavStream::new(Cursor::new(bytes), 48000, 2).is_err());
        let mut bytes = wav(&[1, 2], true);
        bytes[16..20].copy_from_slice(&18u32.to_le_bytes());
        bytes.splice(36..36, [2, 0]);
        assert!(WavStream::new(Cursor::new(bytes), 48000, 2).is_err());
    }
    #[test]
    fn bounded_audio_queue_backpressures_the_reader_and_close_wakes_it() {
        let shared = Arc::new(Shared {
            state: Mutex::new(initial_state(7, 1, 30)),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        });
        for index in 0..8 {
            assert!(
                push_audio(
                    &shared,
                    AudioBlock {
                        epoch: 7,
                        source_start_frame: index * 480,
                        samples: vec![0.1; 960]
                    },
                    2
                )
                .unwrap()
            );
        }
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let worker_shared = shared.clone();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            push_audio(
                &worker_shared,
                AudioBlock {
                    epoch: 7,
                    source_start_frame: 3840,
                    samples: vec![0.1; 960],
                },
                2,
            )
            .unwrap()
        });
        started_rx.recv().unwrap();
        assert_eq!(shared.state.lock().unwrap().audio.len(), 8);
        shared.stop.store(true, Ordering::Release);
        shared.space.notify_all();
        assert!(!worker.join().unwrap());
        assert_eq!(shared.state.lock().unwrap().stats.audio_high_water, 8);
    }
    #[test]
    fn bounded_video_queue_keeps_source_epoch_index_and_pts() {
        let shared = Shared {
            state: Mutex::new(initial_state(8, 1, 30)),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        };
        assert!(
            push_video(
                &shared,
                VideoFrame {
                    epoch: 8,
                    index: 91,
                    pts_ns: 3_033_333_333,
                    rgb: Arc::from(vec![9; FRAME_BYTES])
                }
            )
            .unwrap()
        );
        let state = shared.state.lock().unwrap();
        assert_eq!(state.video[0].epoch, 8);
        assert_eq!(state.video[0].index, 91);
        assert_eq!(state.video[0].pts_ns, 3_033_333_333);
    }
    #[test]
    fn audio_seek_discards_exact_resampled_frames_without_restarting_block_alignment() {
        let samples: Vec<i16> = (0..2000).map(|n| n as i16).collect();
        let shared = Shared {
            state: Mutex::new(initial_state(9, 1, 30)),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        };
        read_audio(Cursor::new(wav(&samples, true)), &shared, 9, 48000, 2, 479).unwrap();
        let state = shared.state.lock().unwrap();
        assert_eq!(state.audio[0].source_start_frame, 479);
        assert_eq!(state.audio[0].samples.len(), 2);
        assert_eq!(state.audio[0].samples[0], 958.0 / 32768.0);
        assert_eq!(state.audio[1].source_start_frame, 480);
        assert_eq!(state.stats.audio_frames_skipped, 479);
        assert_eq!(state.stats.audio_frames_produced, 521);
        assert_eq!(state.stats.audio_source_frames, 1000);
    }
    #[test]
    fn video_seek_preserves_preceding_real_pts_and_original_source_indices() {
        let shared = Shared {
            state: Mutex::new(initial_state(10, 1, 1000)),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        };
        let rgb = [
            vec![1; FRAME_BYTES],
            vec![2; FRAME_BYTES],
            vec![3; FRAME_BYTES],
        ]
        .concat();
        read_video(Cursor::new(rgb),Cursor::new(b"best_effort_timestamp=-100|\n\nbest_effort_timestamp=0\nbest_effort_timestamp=300\n"),&shared,10,1,1000,150_000_000).unwrap();
        let state = shared.state.lock().unwrap();
        assert_eq!(state.video.len(), 2);
        assert_eq!(state.video[0].index, 1);
        assert_eq!(state.video[0].pts_ns, 100_000_000);
        assert_eq!(state.video[0].rgb[0], 2);
        assert_eq!(state.video[1].pts_ns, 400_000_000);
        assert_eq!(state.stats.video_frames_skipped, 1);
        assert_eq!(state.stats.source_first_pts, Some(-100));
    }
    #[test]
    fn mismatched_rgb_and_pts_eof_never_becomes_successful_decode() {
        let shared = Shared {
            state: Mutex::new(initial_state(1, 1, 30)),
            space: Condvar::new(),
            stop: AtomicBool::new(false),
            processes: vec![],
        };
        assert!(
            read_video(
                Cursor::new(vec![1; FRAME_BYTES]),
                Cursor::new(Vec::<u8>::new()),
                &shared,
                1,
                1,
                30,
                0
            )
            .is_err()
        );
        assert!(!shared.state.lock().unwrap().stats.video_eof);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires audited HD fixtures in KAICUT_INDEXED_TEST_FIXTURES; bounded decoder-only checks, no GPU/audio device"]
    fn real_indexed_sidecars_match_reference_frames_and_unaligned_pcm() {
        use std::time::Duration;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures = PathBuf::from(
            std::env::var_os("KAICUT_INDEXED_TEST_FIXTURES")
                .expect("indexed fixture directory required"),
        );
        let ledger: serde_json::Value =
            serde_json::from_slice(&fs::read(fixtures.join("video-reference.json")).unwrap())
                .unwrap();
        let cases = [
            (0_u64, 1_u64, 0_usize),
            (1_500_000_000, 72001, 45),
            (30_000_000_000, 1440001, 900),
            (30_033_333_333, 1441601, 901),
            (30_020_000_000, 1440961, 900),
            (59_000_000_000, 2832001, 1770),
        ];
        for (start_video_ns, start_audio_frame, first_index) in cases {
            let mut decoder = StreamingDecoder::start(StreamConfig {
                ffmpeg: root.join("binaries/ffmpeg-x86_64-pc-windows-msvc.exe"),
                ffprobe: root.join("binaries/ffprobe-x86_64-pc-windows-msvc.exe"),
                video: fixtures.join("video-1080p60s.mp4"),
                audio: fixtures.join("audio-60s.wav"),
                sample_rate: 48000,
                channels: 2,
                video_width: 1920,
                video_height: 1080,
                seek_mode: SeekMode::IndexedCfr,
                epoch: 1,
                start_audio_frame,
                start_video_ns,
            })
            .unwrap();
            let mut video_count = 0;
            let mut audio_count = 0_u64;
            let mut audio_hash = Sha256::new();
            let deadline = Instant::now() + Duration::from_secs(15);
            let drain_tail = first_index == 1770;
            loop {
                if drain_tail || video_count < 3 {
                    if let Some(frame) = decoder.poll_video().unwrap() {
                        assert_eq!(frame.index, first_index + video_count);
                        let expected = &ledger["frames"][frame.index];
                        assert_eq!(
                            frame.pts_ns.to_string(),
                            expected["ptsNs"].as_str().unwrap()
                        );
                        assert_eq!(
                            format!("{:x}", Sha256::digest(&frame.rgb)),
                            expected["rgbSha256"].as_str().unwrap()
                        );
                        video_count += 1;
                    }
                }
                if drain_tail || audio_count < 960 {
                    if let Some(block) = decoder.poll_audio().unwrap() {
                        assert_eq!(block.source_start_frame, start_audio_frame + audio_count);
                        audio_count += (block.samples.len() / 2) as u64;
                        for sample in block.samples {
                            audio_hash.update(sample.to_le_bytes());
                        }
                    }
                }
                if drain_tail {
                    let stats = decoder.stats().unwrap();
                    if stats.audio_eof
                        && stats.video_eof
                        && stats.audio_pending == 0
                        && stats.video_pending == 0
                    {
                        assert_eq!(video_count, 30);
                        assert_eq!(audio_count, 47_999);
                        assert_eq!(stats.video_frames_read, 30);
                        assert_eq!(stats.audio_frames_read, 48_000);
                        assert_eq!(stats.video_source_frames, 1800);
                        assert_eq!(stats.audio_source_frames, 2_880_000);
                        assert!(
                            stats
                                .processes
                                .iter()
                                .all(|child| child.exited && child.exit_code == Some(0))
                        );
                        break;
                    }
                } else if video_count >= 3 && audio_count >= 960 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "Indexed frame/sample preparation exceeded decoder-only test deadline"
                );
                thread::sleep(Duration::from_millis(1));
            }
            let stopped = decoder.close().unwrap();
            assert!(stopped.joined && stopped.processes_reaped && stopped.error.is_none());
            assert!(stopped.stats.processes.iter().all(|child| child.exited));
            assert_eq!(
                stopped.stats.first_delivered_video_index,
                Some(first_index as u64)
            );
            assert_eq!(
                stopped.stats.first_delivered_audio_frame,
                Some(start_audio_frame)
            );
            assert!(stopped.stats.audio_frames_read <= audio_count + 48000 + 8 * 480);
            assert!(stopped.stats.video_frames_read <= video_count as u64 + 5);
            let mut source = WavStream::new(
                File::open(fixtures.join("audio-60s.wav")).unwrap(),
                48000,
                2,
            )
            .unwrap();
            let mut cursor = 0_u64;
            let mut reference_hash = Sha256::new();
            while cursor < start_audio_frame + audio_count {
                let samples = source
                    .next()
                    .unwrap()
                    .expect("Reference PCM unexpectedly ended");
                for frame in samples.chunks_exact(2) {
                    if cursor >= start_audio_frame && cursor < start_audio_frame + audio_count {
                        for sample in frame {
                            reference_hash.update((f32::from(*sample) / 32768.0).to_le_bytes());
                        }
                    }
                    cursor += 1;
                }
            }
            assert_eq!(
                format!("{:x}", audio_hash.finalize()),
                format!("{:x}", reference_hash.finalize())
            );
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires audited sidecars and KAICUT_STREAM_TEST_FIXTURES containing video-20s.mp4/audio-20s.wav; no GPU or audio device"]
    fn real_sidecars_stream_seek_cancel_and_reap_failed_inputs() {
        use std::time::Duration;
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures = PathBuf::from(
            std::env::var_os("KAICUT_STREAM_TEST_FIXTURES").expect("fixture directory required"),
        );
        let ffmpeg = manifest.join("binaries/ffmpeg-x86_64-pc-windows-msvc.exe");
        let ffprobe = manifest.join("binaries/ffprobe-x86_64-pc-windows-msvc.exe");
        audited_binary(
            &ffmpeg,
            "ffmpeg",
            "9752fd44c9e1867c8d26702ef81df1aad1442c1099888d02fed7c29de63aff39",
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("video.mp4");
        let audio = directory.path().join("audio.wav");
        for (source, output, is_video) in [
            (fixtures.join("video-20s.mp4"), video.clone(), true),
            (fixtures.join("audio-20s.wav"), audio.clone(), false),
        ] {
            let mut make = command(&ffmpeg);
            make.args([
                "-hide_banner",
                "-v",
                "error",
                "-nostdin",
                "-n",
                "-protocol_whitelist",
                "file",
                "-i",
            ])
            .arg(source)
            .args(["-t", "4"]);
            if is_video {
                make.args(["-map", "0:v:0", "-an", "-c:v", "copy"]);
            } else {
                make.args(["-map", "0:a:0", "-vn", "-c:a", "pcm_s16le"]);
            }
            make.arg(output);
            let (process, stdout) = PipeProcess::spawn(make, "test-fixture-copy").unwrap();
            drop(stdout);
            process.wait_success().unwrap();
            process.join_stderr().unwrap();
        }
        let base = StreamConfig {
            ffmpeg,
            ffprobe,
            video,
            audio: audio.clone(),
            sample_rate: 48000,
            channels: 2,
            video_width: 160,
            video_height: 90,
            seek_mode: SeekMode::Sequential,
            epoch: 1,
            start_audio_frame: 0,
            start_video_ns: 0,
        };
        let drain = |decoder: &mut StreamingDecoder, start: u64| {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut cursor = start;
            let mut first_video = None;
            let mut video_count = 0;
            let mut digest = Sha256::new();
            loop {
                if let Some(block) = decoder.poll_audio().unwrap() {
                    assert_eq!(block.source_start_frame, cursor);
                    assert!(block.samples.len() <= 960);
                    cursor += (block.samples.len() / 2) as u64;
                    for sample in block.samples {
                        digest.update(sample.to_le_bytes());
                    }
                }
                if let Some(frame) = decoder.poll_video().unwrap() {
                    first_video.get_or_insert((frame.index, frame.pts_ns));
                    assert_eq!(frame.rgb.len(), FRAME_BYTES);
                    video_count += 1;
                }
                let stats = decoder.stats().unwrap();
                assert!(stats.audio_pending <= 8 && stats.video_pending <= 4);
                if stats.audio_eof
                    && stats.video_eof
                    && stats.audio_pending == 0
                    && stats.video_pending == 0
                {
                    assert!(
                        stats
                            .processes
                            .iter()
                            .all(|p| p.exited && p.exit_code == Some(0))
                    );
                    break;
                }
                assert!(Instant::now() < deadline, "stream drain timeout");
                thread::sleep(Duration::from_millis(1));
            }
            (
                cursor,
                first_video.unwrap(),
                video_count,
                format!("{:x}", digest.finalize()),
            )
        };
        let reference_hash = |start: u64| {
            let mut wav = WavStream::new(File::open(&audio).unwrap(), 48000, 2).unwrap();
            let mut cursor = 0u64;
            let mut hash = Sha256::new();
            while let Some(samples) = wav.next().unwrap() {
                for frame in samples.chunks_exact(2) {
                    if cursor >= start {
                        for sample in frame {
                            hash.update((f32::from(*sample) / 32768.0).to_le_bytes())
                        }
                    }
                    cursor += 1;
                }
            }
            format!("{:x}", hash.finalize())
        };
        let mut decoder = StreamingDecoder::start(base.clone()).unwrap();
        let full = drain(&mut decoder, 0);
        assert_eq!(full.0, 192000);
        assert_eq!(full.1, (0, 0));
        assert_eq!(full.2, 120);
        assert_eq!(full.3, reference_hash(0));
        let closed = decoder.close().unwrap();
        assert!(closed.joined && closed.processes_reaped && closed.error.is_none());
        let mut seek = base.clone();
        seek.epoch = 2;
        seek.start_audio_frame = 72001;
        seek.start_video_ns = 1_500_020_833;
        let mut decoder = StreamingDecoder::start(seek).unwrap();
        let selected = drain(&mut decoder, 72001);
        assert_eq!(selected.0, 192000);
        assert_eq!(selected.1, (45, 1_500_000_000));
        assert_eq!(selected.2, 75);
        assert_eq!(selected.3, reference_hash(72001));
        assert!(decoder.close().unwrap().processes_reaped);
        let mut long = base.clone();
        long.video = fixtures.join("video-20s.mp4");
        long.audio = fixtures.join("audio-20s.wav");
        long.epoch = 3;
        let mut decoder = StreamingDecoder::start(long).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let stats = decoder.stats().unwrap();
            if stats.audio_pending == 8 && stats.video_pending == 4 {
                break;
            }
            assert!(Instant::now() < deadline, "backpressure fill timeout");
            thread::sleep(Duration::from_millis(5));
        }
        let shutdown = decoder.close().unwrap();
        assert!(shutdown.joined && shutdown.processes_reaped && shutdown.error.is_none());
        assert!(shutdown.elapsed_ms < 3000.0);
        assert!(!shutdown.stats.audio_eof && !shutdown.stats.video_eof);
        let bad = directory.path().join("bad.wav");
        fs::write(&bad, b"RIFFbroken").unwrap();
        let mut broken = base;
        broken.audio = bad;
        broken.epoch = 4;
        let mut decoder = StreamingDecoder::start(broken).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if decoder.stats().unwrap().error.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "failure propagation timeout");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(decoder.poll_audio().is_err());
        let shutdown = decoder.close().unwrap();
        assert!(shutdown.joined && shutdown.processes_reaped && shutdown.error.is_some());
        assert!(!shutdown.stats.audio_eof);
    }
}

#[cfg(test)]
mod packaged_source_tests {
    use super::*;
    #[test]
    fn source_binaries_are_fixed_siblings_not_arbitrary_same_named_files() {
        let exe = Path::new("C:/app/kaicut.exe");
        assert!(packaged_location(
            Path::new("C:/app/ffmpeg.exe"),
            exe,
            "ffmpeg"
        ));
        assert!(packaged_location(
            Path::new("C:/app/binaries/ffprobe.exe"),
            exe,
            "ffprobe"
        ));
        assert!(!packaged_location(
            Path::new("C:/downloads/ffmpeg.exe"),
            exe,
            "ffmpeg"
        ));
        assert!(!packaged_location(
            Path::new("C:/app/other.exe"),
            exe,
            "ffmpeg"
        ));
    }
}
