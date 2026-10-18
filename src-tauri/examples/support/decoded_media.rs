//! Bounded offline decoding for generated native-media probe fixtures only.
//! Actual video PTS come from decoded-frame metadata, never average frame rate.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const VIDEO_WIDTH: u32 = 160;
pub const VIDEO_HEIGHT: u32 = 90;
const VIDEO_FRAME_BYTES: usize = VIDEO_WIDTH as usize * VIDEO_HEIGHT as usize * 3;
const VIDEO_LIMIT: usize = 8 * 1024 * 1024;
const AUDIO_LIMIT: usize = 64 * 1024 * 1024;
const PROBE_LIMIT: usize = 4 * 1024 * 1024;
const LOG_LIMIT: usize = 1024 * 1024;
const MAX_DURATION_NS: u64 = 10_000_000_000;

#[derive(Debug)]
pub struct DecodedPcm {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub frame_count: u64,
    /// SHA-256 of the original decoded WAV bytes, before f32 conversion.
    pub sha256: String,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct DecodedVideoFrame {
    pub pts_ns: u64,
    /// SHA-256 of one complete 160x90 RGB24 decoded frame.
    pub sha256: String,
}

#[derive(Debug)]
pub struct DecodedMedia {
    pub audio: DecodedPcm,
    pub video: Vec<DecodedVideoFrame>,
    pub video_width: u32,
    pub video_height: u32,
    pub video_path: PathBuf,
    pub probe_path: PathBuf,
    pub audio_probe_path: PathBuf,
    pub source_time_base_numerator: u32,
    pub source_time_base_denominator: u32,
    pub source_first_pts: i64,
}

struct VideoProbe {
    numerator: u32,
    denominator: u32,
    pts_ns: Vec<u64>,
    first_pts: i64,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn u16_le(bytes: &[u8], offset: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or("Truncated WAV integer")?
            .try_into()
            .map_err(|_| "Invalid WAV integer")?,
    ))
}
fn u32_le(bytes: &[u8], offset: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or("Truncated WAV integer")?
            .try_into()
            .map_err(|_| "Invalid WAV integer")?,
    ))
}
fn device_format(sample_rate: u32, channels: u16) -> Result<(), String> {
    if !(8_000..=192_000).contains(&sample_rate) || !(1..=8).contains(&channels) {
        return Err("Probe PCM format must be 8–192 kHz with 1–8 channels".into());
    }
    Ok(())
}

fn parse_pcm16_wav(bytes: &[u8], sample_rate: u32, channels: u16) -> Result<DecodedPcm, String> {
    device_format(sample_rate, channels)?;
    if bytes.len() > AUDIO_LIMIT
        || bytes.get(..4) != Some(b"RIFF")
        || bytes.get(8..12) != Some(b"WAVE")
        || u64::from(u32_le(bytes, 4)?) + 8 != bytes.len() as u64
    {
        return Err("Invalid, truncated or oversized RIFF/WAVE file".into());
    }
    let mut cursor = 12_usize;
    let mut format = None;
    let mut data = None;
    while cursor < bytes.len() {
        let header = bytes
            .get(cursor..cursor + 8)
            .ok_or("Truncated WAV chunk header")?;
        let length = u32_le(header, 4)? as usize;
        let start = cursor.checked_add(8).ok_or("WAV size overflow")?;
        let end = start.checked_add(length).ok_or("WAV size overflow")?;
        let chunk = bytes.get(start..end).ok_or("Truncated WAV chunk body")?;
        match &header[..4] {
            b"fmt " => {
                if format.replace(chunk).is_some() {
                    return Err("Duplicate WAV fmt chunk".into());
                }
            }
            b"data" => {
                if data.replace(chunk).is_some() {
                    return Err("Duplicate WAV data chunk".into());
                }
            }
            _ => {}
        }
        cursor = end.checked_add(length & 1).ok_or("WAV padding overflow")?;
        if cursor > bytes.len() {
            return Err("Missing WAV padding byte".into());
        }
    }
    let format = format.ok_or("Missing WAV fmt chunk")?;
    let data = data.ok_or("Missing WAV data chunk")?;
    let tag = u16_le(format, 0)?;
    if format.len() != 16
        && (format.len() < 18 || usize::from(u16_le(format, 16)?) + 18 != format.len())
    {
        return Err("WAV fmt extension length is inconsistent".into());
    }
    if tag == 0xfffe {
        const PCM_GUID: [u8; 16] = [
            1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
        ];
        if format.len() < 40
            || u16_le(format, 16)? < 22
            || u16_le(format, 18)? != 16
            || format.get(24..40) != Some(PCM_GUID.as_slice())
        {
            return Err("WAVEFORMATEXTENSIBLE is not 16-bit PCM".into());
        }
    } else if tag != 1 || format.len() < 16 {
        return Err("Decoded WAV must contain integer PCM".into());
    }
    let stride = channels * 2;
    if u16_le(format, 2)? != channels
        || u32_le(format, 4)? != sample_rate
        || u16_le(format, 14)? != 16
        || u16_le(format, 12)? != stride
        || u32_le(format, 8)? != sample_rate * u32::from(stride)
        || data.is_empty()
        || data.len() % usize::from(stride) != 0
    {
        return Err("Decoded WAV format/stride does not match the requested device format".into());
    }
    let frame_count = (data.len() / usize::from(stride)) as u64;
    if frame_count > u64::from(sample_rate) * 10 {
        return Err("Decoded audio exceeds ten seconds".into());
    }
    let samples: Vec<f32> = data
        .chunks_exact(2)
        .map(|sample| f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0)
        .collect();
    if !samples.iter().all(|sample| sample.is_finite()) {
        return Err("Decoded PCM contains non-finite samples".into());
    }
    Ok(DecodedPcm {
        samples,
        sample_rate,
        channels,
        frame_count,
        sha256: hash(bytes),
        path: PathBuf::new(),
    })
}

fn stream_timebase(document: &Value) -> Result<(u32, u32), String> {
    let streams = document["streams"]
        .as_array()
        .ok_or("Missing probe streams")?;
    if streams.len() != 1 {
        return Err("Probe requires exactly one selected stream".into());
    }
    let stream = &streams[0];
    let (numerator, denominator) = stream["time_base"]
        .as_str()
        .ok_or("Missing stream time_base")?
        .split_once('/')
        .ok_or("Invalid stream time_base")?;
    let numerator: u32 = numerator
        .parse()
        .map_err(|_| "Invalid timebase numerator")?;
    let denominator: u32 = denominator
        .parse()
        .map_err(|_| "Invalid timebase denominator")?;
    if numerator == 0 || denominator == 0 {
        return Err("Zero stream timebase".into());
    }
    let duration = stream["duration_ts"]
        .as_u64()
        .ok_or("Missing positive stream duration_ts")?;
    let duration_ns =
        u128::from(duration) * u128::from(numerator) * 1_000_000_000 / u128::from(denominator);
    if duration == 0 || duration_ns > u128::from(MAX_DURATION_NS) {
        return Err("Fixture stream must be nonempty and no longer than ten seconds".into());
    }
    Ok((numerator, denominator))
}

fn parse_video_probe(bytes: &[u8]) -> Result<VideoProbe, String> {
    if bytes.len() > PROBE_LIMIT {
        return Err("Video probe exceeds 4 MiB".into());
    }
    let document: Value = serde_json::from_slice(bytes).map_err(|_| "Invalid ffprobe JSON")?;
    let (numerator, denominator) = stream_timebase(&document)?;
    let frames = document["frames"]
        .as_array()
        .ok_or("Missing decoded frame timestamps")?;
    if frames.is_empty() || frames.len() > VIDEO_LIMIT / VIDEO_FRAME_BYTES {
        return Err("Video frame count exceeds bounded RGB fixture capacity".into());
    }
    let first = frames[0]["best_effort_timestamp"]
        .as_i64()
        .ok_or("Missing integer first video PTS")?;
    let mut previous = None;
    let mut pts_ns = Vec::with_capacity(frames.len());
    for frame in frames {
        let pts = frame["best_effort_timestamp"]
            .as_i64()
            .ok_or("Missing integer decoded video PTS")?;
        if previous.is_some_and(|old| pts <= old) {
            return Err("Duplicate or nonmonotonic decoded video PTS".into());
        }
        let delta = u128::try_from(i128::from(pts) - i128::from(first))
            .map_err(|_| "Video PTS precedes first frame")?;
        let nanos = delta
            .checked_mul(u128::from(numerator))
            .and_then(|value| value.checked_mul(1_000_000_000))
            .ok_or("Video PTS overflow")?
            / u128::from(denominator);
        let nanos = u64::try_from(nanos).map_err(|_| "Video PTS overflow")?;
        if nanos >= MAX_DURATION_NS || pts_ns.last().is_some_and(|old| nanos <= *old) {
            return Err("Video PTS exceeds ten seconds or loses nanosecond precision".into());
        }
        pts_ns.push(nanos);
        previous = Some(pts);
    }
    Ok(VideoProbe {
        numerator,
        denominator,
        pts_ns,
        first_pts: first,
    })
}

fn video_frames(bytes: &[u8], pts: &[u64]) -> Result<Vec<DecodedVideoFrame>, String> {
    if bytes.is_empty()
        || bytes.len() > VIDEO_LIMIT
        || bytes.len() % VIDEO_FRAME_BYTES != 0
        || bytes.len() / VIDEO_FRAME_BYTES != pts.len()
    {
        return Err(
            "Decoded RGB frame count/stride does not exactly match ffprobe frame PTS".into(),
        );
    }
    Ok(bytes
        .chunks_exact(VIDEO_FRAME_BYTES)
        .zip(pts)
        .map(|(frame, &pts_ns)| DecodedVideoFrame {
            pts_ns,
            sha256: hash(frame),
        })
        .collect())
}

fn configured_command(binary: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}
fn video_command(ffmpeg: &Path, input: &Path) -> Command {
    let mut command = configured_command(ffmpeg);
    command
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-nostdin",
            "-n",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-protocol_whitelist",
            "file",
            "-f",
            "mov",
            "-i",
        ])
        .arg(input)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-vf",
            "scale=160:90",
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
    command
}
fn probe_command(ffprobe: &Path, input: &Path, video: bool) -> Command {
    let mut command = configured_command(ffprobe);
    command
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-f",
            if video { "mov" } else { "wav" },
            "-select_streams",
            if video { "v:0" } else { "a:0" },
            "-show_entries",
        ])
        .arg(if video {
            "stream=time_base,duration_ts:frame=best_effort_timestamp"
        } else {
            "stream=time_base,duration_ts"
        });
    if video {
        command.arg("-show_frames");
    }
    command.args(["-of", "json"]).arg(input);
    command
}

struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: ExitStatus,
    failure: Option<String>,
}
fn drain(mut input: impl Read, limit: usize, overflow: Arc<AtomicBool>) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(length) => {
                let remaining = limit.saturating_sub(output.len());
                output.extend_from_slice(&buffer[..length.min(remaining)]);
                if length > remaining {
                    overflow.store(true, Ordering::Release);
                }
            }
            Err(_) => {
                overflow.store(true, Ordering::Release);
                break;
            }
        }
    }
    output
}
fn run_bounded(
    mut command: Command,
    stdout_limit: usize,
    monitor: Option<&Path>,
) -> Result<ProcessOutput, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot start audited media binary: {error}"))?;
    let stdout = child.stdout.take().ok_or("Missing decoder stdout")?;
    let stderr = child.stderr.take().ok_or("Missing decoder stderr")?;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout_flag = Arc::clone(&overflow);
    let stderr_flag = Arc::clone(&overflow);
    let stdout_reader = thread::spawn(move || drain(stdout, stdout_limit, stdout_flag));
    let stderr_reader = thread::spawn(move || drain(stderr, LOG_LIMIT, stderr_flag));
    let started = Instant::now();
    let mut failure = None;
    let status = loop {
        let oversized_file = monitor
            .and_then(|path| fs::metadata(path).ok())
            .is_some_and(|info| info.len() > AUDIO_LIMIT as u64);
        if started.elapsed() > Duration::from_secs(30)
            || overflow.load(Ordering::Acquire)
            || oversized_file
        {
            failure =
                Some("Media subprocess exceeded deadline or bounded output capacity".to_owned());
            let _ = child.kill();
            break child
                .wait()
                .map_err(|error| format!("Cannot reap decoder after cancellation: {error}"))?;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                failure = Some(format!("Cannot poll media subprocess: {error}"));
                let _ = child.kill();
                break child.wait().map_err(|error| error.to_string())?;
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "Decoder stdout reader panicked")?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "Decoder stderr reader panicked")?;
    if overflow.load(Ordering::Acquire) {
        failure =
            Some("Media subprocess output exceeded its capture limit or could not be read".into());
    }
    Ok(ProcessOutput {
        stdout,
        stderr,
        status,
        failure,
    })
}
fn create_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            format!(
                "Cannot create new probe artifact {}: {error}",
                path.display()
            )
        })?;
    output
        .write_all(bytes)
        .and_then(|_| output.sync_all())
        .map_err(|error| error.to_string())
}
fn save_process(
    result: ProcessOutput,
    stdout: Option<&Path>,
    log: &Path,
) -> Result<Vec<u8>, String> {
    if let Some(path) = stdout {
        create_new(path, &result.stdout)?;
    }
    create_new(log, &result.stderr)?;
    if let Some(error) = result.failure {
        return Err(error);
    }
    if !result.status.success() {
        return Err(format!(
            "Media decoding failed ({}); see {}",
            result.status,
            log.display()
        ));
    }
    Ok(result.stdout)
}
fn read_capped(path: &Path, cap: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > cap {
        return Err("Decoded artifact exceeds read limit".into());
    }
    Ok(bytes)
}
fn checked_binary(path: &Path, name: &str) -> Result<PathBuf, String> {
    let expected = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!("{name}-x86_64-pc-windows-msvc.exe"))
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let actual = path.canonicalize().map_err(|error| error.to_string())?;
    if actual != expected {
        return Err(
            "Only this checkout's fixed audited Windows FFmpeg binaries are permitted".into(),
        );
    }
    Ok(actual)
}
fn checked_input(path: &Path) -> Result<PathBuf, String> {
    let actual = path.canonicalize().map_err(|error| error.to_string())?;
    let info = fs::metadata(&actual).map_err(|error| error.to_string())?;
    if !info.is_file() || info.len() == 0 || info.len() > AUDIO_LIMIT as u64 {
        return Err("Fixture must be a nonempty regular file of at most 64 MiB".into());
    }
    Ok(actual)
}

/// Decode only short generated fixtures using fixed arguments and this
/// checkout's audited binaries. The caller independently checks source hashes.
pub fn decode_fixture(
    ffmpeg: &Path,
    ffprobe: &Path,
    video: &Path,
    audio: &Path,
    output_dir: &Path,
    sample_rate: u32,
    channels: u16,
) -> Result<DecodedMedia, String> {
    device_format(sample_rate, channels)?;
    let ffmpeg = checked_binary(ffmpeg, "ffmpeg")?;
    let ffprobe = checked_binary(ffprobe, "ffprobe")?;
    let video = checked_input(video)?;
    let audio = checked_input(audio)?;
    let output = output_dir
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !output.is_dir() || !output.starts_with(target) {
        return Err(
            "Probe output directory must exist inside this checkout's target directory".into(),
        );
    }
    for name in [
        "video-frames.json",
        "audio-stream.json",
        "decoded-video.rgb",
        "decoded-audio.wav",
        "video-probe.log",
        "audio-probe.log",
        "video-decode.log",
        "audio-decode.log",
    ] {
        if output.join(name).symlink_metadata().is_ok() {
            return Err(format!("Probe artifact already exists: {name}"));
        }
    }
    let probe_path = output.join("video-frames.json");
    let audio_probe_path = output.join("audio-stream.json");
    let video_probe_bytes = save_process(
        run_bounded(probe_command(&ffprobe, &video, true), PROBE_LIMIT, None)?,
        Some(&probe_path),
        &output.join("video-probe.log"),
    )?;
    let video_probe = parse_video_probe(&video_probe_bytes)?;
    let audio_probe_bytes = save_process(
        run_bounded(probe_command(&ffprobe, &audio, false), PROBE_LIMIT, None)?,
        Some(&audio_probe_path),
        &output.join("audio-probe.log"),
    )?;
    let audio_probe: Value =
        serde_json::from_slice(&audio_probe_bytes).map_err(|_| "Invalid audio probe JSON")?;
    stream_timebase(&audio_probe)?;

    let video_path = output.join("decoded-video.rgb");
    let rgb = save_process(
        run_bounded(video_command(&ffmpeg, &video), VIDEO_LIMIT, None)?,
        Some(&video_path),
        &output.join("video-decode.log"),
    )?;
    let video = video_frames(&rgb, &video_probe.pts_ns)?;
    let audio_path = output.join("decoded-audio.wav");
    let mut command = configured_command(&ffmpeg);
    command
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-nostdin",
            "-n",
            "-threads",
            "1",
            "-protocol_whitelist",
            "file",
            "-f",
            "wav",
            "-i",
        ])
        .arg(&audio)
        .args(["-map", "0:a:0", "-vn", "-c:a", "pcm_s16le", "-ar"])
        .arg(sample_rate.to_string())
        .arg("-ac")
        .arg(channels.to_string())
        .arg("-fs")
        .arg(AUDIO_LIMIT.to_string())
        .args(["-f", "wav"])
        .arg(&audio_path);
    save_process(
        run_bounded(command, LOG_LIMIT, Some(&audio_path))?,
        None,
        &output.join("audio-decode.log"),
    )?;
    let mut audio = parse_pcm16_wav(
        &read_capped(&audio_path, AUDIO_LIMIT)?,
        sample_rate,
        channels,
    )?;
    if audio
        .samples
        .len()
        .checked_mul(std::mem::size_of::<f32>())
        .and_then(|bytes| bytes.checked_add(rgb.len()))
        .is_none_or(|bytes| bytes > AUDIO_LIMIT)
    {
        return Err("Combined decoded RGB and float PCM exceeds 64 MiB".into());
    }
    audio.path = audio_path;
    Ok(DecodedMedia {
        audio,
        video,
        video_width: VIDEO_WIDTH,
        video_height: VIDEO_HEIGHT,
        video_path,
        probe_path,
        audio_probe_path,
        source_time_base_numerator: video_probe.numerator,
        source_time_base_denominator: video_probe.denominator,
        source_first_pts: video_probe.first_pts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
        let length = samples.len() * 2;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + length as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(length as u32).to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn parses_real_pcm16_interleaving_without_inventing_device_rate() {
        let pcm =
            parse_pcm16_wav(&wave(&[i16::MIN, 0, i16::MAX, 16384], 48_000, 2), 48_000, 2).unwrap();
        assert_eq!(pcm.frame_count, 2);
        assert_eq!(pcm.samples, vec![-1.0, 0.0, 32767.0 / 32768.0, 0.5]);
        assert_eq!(pcm.sample_rate, 48_000);
        assert_eq!(pcm.channels, 2);
    }

    #[test]
    fn rejects_truncated_misaligned_mislabeled_and_unbounded_wav() {
        let original = wave(&[1, 2, 3, 4], 48_000, 2);
        for index in [0, 4, 20, 22, 24, 28, 32, 34, 40] {
            let mut broken = original.clone();
            broken[index] ^= 0xff;
            assert!(
                parse_pcm16_wav(&broken, 48_000, 2).is_err(),
                "mutated WAV offset {index}"
            );
        }
        assert!(parse_pcm16_wav(&original[..original.len() - 1], 48_000, 2).is_err());
        assert!(parse_pcm16_wav(&original, 44_100, 2).is_err());
        assert!(parse_pcm16_wav(&wave(&[1, 2, 3], 48_000, 2), 48_000, 2).is_err());
        let mut huge = original.clone();
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_pcm16_wav(&huge, 48_000, 2).is_err());
    }

    #[test]
    fn maps_integer_presentation_timestamps_with_negative_origin_and_rational_timebase() {
        let probe = br#"{"streams":[{"time_base":"1/30000","duration_ts":3003}],"frames":[{"best_effort_timestamp":-1001},{"best_effort_timestamp":0},{"best_effort_timestamp":1001}]}"#;
        let parsed = parse_video_probe(probe).unwrap();
        assert_eq!(parsed.pts_ns, vec![0, 33_366_666, 66_733_333]);
        assert_eq!((parsed.numerator, parsed.denominator), (1, 30000));
    }

    #[test]
    fn rejects_duplicate_backwards_missing_or_unbounded_pts() {
        for frames in [
            r#"[{"best_effort_timestamp":0},{"best_effort_timestamp":0}]"#,
            r#"[{"best_effort_timestamp":1},{"best_effort_timestamp":0}]"#,
            r#"[{}]"#,
            r#"[{"best_effort_timestamp":0},{"best_effort_timestamp":300001}]"#,
        ] {
            let probe = format!(
                r#"{{"streams":[{{"time_base":"1/30000","duration_ts":3000}}],"frames":{frames}}}"#
            );
            assert!(parse_video_probe(probe.as_bytes()).is_err());
        }
        assert!(parse_video_probe(br#"{"streams":[{"time_base":"0/30"}],"frames":[]}"#).is_err());
    }

    #[test]
    fn raw_video_frames_must_match_real_pts_exactly() {
        let pts = vec![0, 33_333_333];
        let mut pixels = vec![0_u8; VIDEO_FRAME_BYTES * 2];
        pixels[VIDEO_FRAME_BYTES] = 255;
        let decoded = video_frames(&pixels, &pts).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[1].pts_ns, 33_333_333);
        assert_ne!(decoded[0].sha256, decoded[1].sha256);
        assert!(video_frames(&pixels[..pixels.len() - 1], &pts).is_err());
        assert!(video_frames(&pixels, &[0]).is_err());
    }

    #[test]
    fn command_paths_remain_one_literal_argument_with_unicode_quotes_and_metacharacters() {
        let path = Path::new("E:/验收 'quote' [raw]; $literal/video.mp4");
        let command = video_command(Path::new("E:/bin/ffmpeg.exe"), path);
        let args: Vec<_> = command.get_args().collect();
        let index = args.iter().position(|arg| *arg == "-i").unwrap();
        assert_eq!(args[index + 1], path.as_os_str());
        assert!(args.iter().any(|arg| *arg == "passthrough"));
        assert!(args.iter().any(|arg| *arg == "pipe:1"));
    }
}
