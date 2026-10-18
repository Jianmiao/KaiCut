//! Bounded, local source admission for the opt-in native source player.
//! Callers resolve MediaAsset from the authoritative Rust session first.
use super::{
    audio_timeline::{AudioTimeline, Timestamp},
    streaming_decoder::{SeekMode, StreamConfig},
};
use crate::project::{MediaAsset, MediaKind};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Instant, UNIX_EPOCH},
};
use tauri::AppHandle;

#[path = "../../examples/support/stream_process.rs"]
#[allow(dead_code)]
mod process;

const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_FRAMES: usize = 100_000;
const MAX_DURATION_NS: u64 = 60_000_000_000;
const SAMPLE_RATE: u32 = 48_000;
const FFMPEG_SHA: &str = "9752fd44c9e1867c8d26702ef81df1aad1442c1099888d02fed7c29de63aff39";
const FFPROBE_SHA: &str = "3e3dfe5c77e7f3b8f9de1c8ec2ccfea4ec1e9f34e930b6f918894c9d2838219a";

pub struct SourceProfile {
    pub config: StreamConfig,
    pub timeline: AudioTimeline,
    pub end_ns: u64,
    pub source_fingerprint: Value,
    pub metadata: Value,
}

struct Parsed {
    width: u32,
    height: u32,
    timeline: AudioTimeline,
    end_ns: u64,
    metadata: Value,
}

fn unsupported(message: impl AsRef<str>) -> String {
    format!("Native source preview unsupported: {}", message.as_ref())
}

fn integer(value: &Value, name: &str) -> Result<i64, String> {
    value[name]
        .as_i64()
        .ok_or_else(|| unsupported(format!("missing integer {name}")))
}

fn ratio(stream: &Value) -> Result<(u32, u32), String> {
    let (n, d) = stream["time_base"]
        .as_str()
        .and_then(|v| v.split_once('/'))
        .ok_or_else(|| unsupported("missing rational timebase"))?;
    let n = n.parse::<u32>().ok().filter(|v| *v > 0);
    let d = d.parse::<u32>().ok().filter(|v| *v > 0);
    match (n, d) {
        (Some(n), Some(d)) => Ok((n, d)),
        _ => Err(unsupported("invalid rational timebase")),
    }
}

fn timestamp(pts: i64, timebase: (u32, u32)) -> Timestamp {
    Timestamp {
        pts,
        numerator: timebase.0,
        denominator: timebase.1,
    }
}

fn nanos(ticks: i128, timebase: (u32, u32)) -> Result<u64, String> {
    let ns = ticks
        .checked_mul(i128::from(timebase.0))
        .and_then(|v| v.checked_mul(1_000_000_000))
        .ok_or_else(|| unsupported("timestamp overflow"))?
        / i128::from(timebase.1);
    u64::try_from(ns).map_err(|_| unsupported("negative or overflowing duration"))
}

// ffprobe writes decimal seconds. Parse exactly rather than admitting a NaN,
// an infinity, or a rounded value just past the development limit.
fn decimal_ns(value: &Value) -> Result<u64, String> {
    let text = value
        .as_str()
        .ok_or_else(|| unsupported("unknown source duration"))?;
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|v| v.is_ascii_digit())
        || fraction.len() > 9
        || !fraction.bytes().all(|v| v.is_ascii_digit())
    {
        return Err(unsupported("invalid source duration"));
    }
    let whole = whole
        .parse::<u64>()
        .map_err(|_| unsupported("source duration overflow"))?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<u64>()
            .map_err(|_| unsupported("source duration overflow"))?
            * 10u64.pow(9 - fraction.len() as u32)
    };
    whole
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(fraction))
        .ok_or_else(|| unsupported("source duration overflow"))
}

struct Metadata<'a> {
    video: &'a Value,
    audio: &'a Value,
    width: u32,
    height: u32,
    video_timebase: (u32, u32),
    audio_timebase: (u32, u32),
    video_end_pts: i128,
}

fn validate_metadata(data: &Value) -> Result<Metadata<'_>, String> {
    let duration = decimal_ns(&data["format"]["duration"])?;
    if duration == 0 || duration > MAX_DURATION_NS {
        return Err(unsupported(
            "source duration must be known and at most 60 seconds",
        ));
    }
    let streams = data["streams"]
        .as_array()
        .filter(|v| v.len() <= 16)
        .ok_or_else(|| unsupported("missing or excessive source streams"))?;
    let selected = |kind: &str| -> Result<&Value, String> {
        let mut selected = streams.iter().filter(|v| v["codec_type"] == kind);
        let first = selected
            .next()
            .ok_or_else(|| unsupported(format!("missing {kind} stream")))?;
        if selected.next().is_some() {
            return Err(unsupported(format!("multiple {kind} streams")));
        }
        Ok(first)
    };
    let video = selected("video")?;
    let audio = selected("audio")?;
    let width = video["width"]
        .as_u64()
        .filter(|v| (1..=1920).contains(v))
        .ok_or_else(|| unsupported("video width exceeds 1920 or is invalid"))?
        as u32;
    let height = video["height"]
        .as_u64()
        .filter(|v| (1..=1080).contains(v))
        .ok_or_else(|| unsupported("video height exceeds 1080 or is invalid"))?
        as u32;
    if video["sample_aspect_ratio"] != "1:1" {
        return Err(unsupported(
            "only explicitly square-pixel video is supported",
        ));
    }
    if let Some(rotation) = video["tags"].get("rotate") {
        if rotation.as_str() != Some("0") && rotation.as_i64() != Some(0) {
            return Err(unsupported("rotated video"));
        }
    }
    if let Some(side_data) = video.get("side_data_list") {
        for side in side_data
            .as_array()
            .ok_or_else(|| unsupported("malformed video side data"))?
        {
            if side.get("displaymatrix").is_some()
                || side["side_data_type"] == "Display Matrix"
                || side.get("rotation").is_some_and(|v| v.as_i64() != Some(0))
            {
                return Err(unsupported("video display transforms are not supported"));
            }
        }
    }
    if audio["sample_rate"] != "48000" || audio["channels"].as_u64() != Some(2) {
        return Err(unsupported(
            "audio must already be 48 kHz stereo; resampling is not enabled",
        ));
    }
    if audio["codec_name"] != "aac" {
        return Err(unsupported(
            "the first native source path requires AAC audio",
        ));
    }
    let video_timebase = ratio(video)?;
    let audio_timebase = ratio(audio)?;
    let start = integer(video, "start_pts")?;
    let duration_ticks = integer(video, "duration_ts")?;
    if start < 0
        || duration_ticks <= 0
        || nanos(i128::from(duration_ticks), video_timebase)? > MAX_DURATION_NS
    {
        return Err(unsupported(
            "video needs nonnegative start PTS and known duration within 60 seconds",
        ));
    }
    Ok(Metadata {
        video,
        audio,
        width,
        height,
        video_timebase,
        audio_timebase,
        video_end_pts: i128::from(start) + i128::from(duration_ticks),
    })
}

fn frames(data: &Value) -> Result<&Vec<Value>, String> {
    data["frames"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= MAX_FRAMES)
        .ok_or_else(|| unsupported("decoded frame count must be within 1..=100000"))
}

fn parse_profile(metadata: &Value, audio: &Value, video: &Value) -> Result<Parsed, String> {
    let source = validate_metadata(metadata)?;
    let video_frames = frames(video)?;
    let audio_frames = frames(audio)?;
    let first_video = integer(&video_frames[0], "best_effort_timestamp")?;
    if first_video < 0 {
        return Err(unsupported("negative first decoded video PTS"));
    }
    let mut previous_video = None;
    let mut last_frame_end = None;
    for frame in video_frames {
        let pts = integer(frame, "best_effort_timestamp")?;
        if previous_video.is_some_and(|last| pts <= last) || i128::from(pts) >= source.video_end_pts
        {
            return Err(unsupported(
                "video PTS must increase inside its declared stream extent",
            ));
        }
        previous_video = Some(pts);
        let duration = frame.get("duration").or_else(|| frame.get("pkt_duration"));
        last_frame_end = match duration {
            Some(duration) => {
                let ticks = duration
                    .as_i64()
                    .filter(|v| *v >= 0)
                    .ok_or_else(|| unsupported("invalid decoded video frame duration"))?;
                if ticks == 0 {
                    None
                } else {
                    Some(i128::from(pts) + i128::from(ticks))
                }
            }
            None => None,
        };
        if last_frame_end.is_some_and(|end| end > source.video_end_pts) {
            return Err(unsupported("decoded frame extends past declared video EOF"));
        }
    }
    let video_end = nanos(
        last_frame_end.unwrap_or(source.video_end_pts) - i128::from(first_video),
        source.video_timebase,
    )?;
    let first_audio = integer(&audio_frames[0], "best_effort_timestamp")?;
    let mut total_samples = 0u64;
    for frame in audio_frames {
        let pts = integer(frame, "best_effort_timestamp")?;
        let delta = i128::from(pts) - i128::from(first_audio);
        let timestamp_samples = delta
            .checked_mul(i128::from(source.audio_timebase.0))
            .and_then(|v| v.checked_mul(i128::from(SAMPLE_RATE)))
            .ok_or_else(|| unsupported("audio timestamp arithmetic overflow"))?;
        if timestamp_samples != i128::from(total_samples) * i128::from(source.audio_timebase.1) {
            return Err(unsupported(
                "decoded audio has a gap, overlap or non-sample-aligned PTS",
            ));
        }
        let samples = frame["nb_samples"]
            .as_u64()
            .filter(|v| (1..=SAMPLE_RATE as u64).contains(v))
            .ok_or_else(|| unsupported("decoded audio has invalid sample count"))?;
        total_samples = total_samples
            .checked_add(samples)
            .filter(|v| *v <= u64::from(SAMPLE_RATE) * 60)
            .ok_or_else(|| unsupported("decoded audio exceeds 60 seconds"))?;
    }
    let timeline = AudioTimeline::new(
        timestamp(first_audio, source.audio_timebase),
        timestamp(first_video, source.video_timebase),
        SAMPLE_RATE,
        total_samples,
    )?;
    let diagnostics = timeline.diagnostics();
    let audio_end = diagnostics
        .source_end_timeline_ns
        .parse::<i128>()
        .map_err(|_| unsupported("audio end cannot be represented"))?;
    let end_ns = video_end.max(
        u64::try_from(audio_end.max(0))
            .map_err(|_| unsupported("audio end exceeds supported extent"))?,
    );
    if end_ns == 0 || end_ns > MAX_DURATION_NS {
        return Err(unsupported("normalized source duration exceeds 60 seconds"));
    }
    Ok(Parsed {
        width: source.width,
        height: source.height,
        timeline,
        end_ns,
        metadata: json!({
            "videoCodec": source.video["codec_name"], "audioCodec": source.audio["codec_name"],
            "width":source.width,"height":source.height,"sampleRate":SAMPLE_RATE,"channels":2,
            "decodedVideoFrames":video_frames.len(),"decodedAudioFrames":audio_frames.len(),
            "decodedAudioSamples":total_samples,"videoEndNs":video_end.to_string(),
            "endNs":end_ns.to_string(),"timeline":diagnostics,
            "durationSource":"decoded frame PTS and sample counts; final video frame duration or stream extent",
            "limits":{"maxDurationSeconds":60,"maxSourceBytes":MAX_SOURCE_BYTES,
                "maxFramesPerStream":MAX_FRAMES,"maxJsonBytesPerProbe":MAX_JSON_BYTES,
                "maxSecondsPerProbe":10,"probeCount":3,"maxAllocationBytes":33554432},
            "resampled":false,"primingSubtractedAgain":false,"audioContiguous":true
        }),
    })
}

fn file_hash(file: &mut File, limit: u64) -> Result<String, String> {
    file_hash_controlled(file, limit, "file-hash", &|_| Ok(()))
}

fn file_hash_controlled(
    file: &mut File,
    limit: u64,
    stage: &str,
    checkpoint: &dyn Fn(&str) -> Result<(), String>,
) -> Result<String, String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut total = 0u64;
    loop {
        checkpoint(stage)?;
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > limit {
            return Err(unsupported("file exceeded admitted byte limit"));
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn source_identity(file: &mut File, media: &MediaAsset, path: &Path) -> Result<Value, String> {
    source_identity_controlled(file, media, path, "source-hash", &|_| Ok(()))
}

fn source_identity_controlled(
    file: &mut File,
    media: &MediaAsset,
    path: &Path,
    stage: &str,
    checkpoint: &dyn Fn(&str) -> Result<(), String>,
) -> Result<Value, String> {
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    let modified = metadata
        .modified()
        .map_err(|e| e.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_SOURCE_BYTES
        || metadata.len() != media.size
        || modified.as_millis() != u128::from(media.last_modified)
    {
        return Err(unsupported(
            "source identity changed, is empty, or exceeds 512 MiB",
        ));
    }
    Ok(
        json!({"path":path,"size":metadata.len(),"modifiedMs":media.last_modified,
        "modifiedNs":modified.as_nanos().to_string(),"sha256":file_hash_controlled(file,metadata.len(),stage,checkpoint)?}),
    )
}

fn audited_packaged_binary(
    app: &AppHandle,
    name: &str,
    sha: &str,
    stage: &str,
    checkpoint: &dyn Fn(&str) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let path = crate::media_engine::packaged_binary(app, name)
        .ok_or_else(|| unsupported(format!("packaged {name} is unavailable")))?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let mut file = File::open(&path).map_err(|e| e.to_string())?;
    if file_hash_controlled(&mut file, MAX_SOURCE_BYTES, stage, checkpoint)? != sha {
        return Err(unsupported(format!(
            "packaged {name} does not match the audited SHA-256"
        )));
    }
    Ok(path)
}

fn probe(
    binary: &Path,
    source: &Path,
    selection: Option<&str>,
    entries: &str,
    stage: &str,
    checkpoint: &dyn Fn(&str) -> Result<(), String>,
    observe: &dyn Fn(Value),
) -> Result<Value, String> {
    let running = std::cell::Cell::new(false);
    let running_stage = format!("{stage}:running");
    let mut command = process::command(binary);
    command.args([
        "-v",
        "error",
        "-max_alloc",
        "33554432",
        "-threads",
        "1",
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        "mov",
        "-f",
        "mov",
    ]);
    if let Some(selection) = selection {
        command.args(["-select_streams", selection, "-show_frames"]);
    }
    command
        .args(["-show_entries", entries, "-of", "json"])
        .arg(source);
    let bytes = process::bounded_output_controlled(
        command,
        MAX_JSON_BYTES,
        &|| checkpoint(if running.get() { &running_stage } else { stage }),
        &|mut event| {
            if event["event"] == "probe-start" {
                running.set(true);
            }
            event["stage"] = json!(stage);
            observe(event);
        },
    )?;
    checkpoint(stage)?;
    serde_json::from_slice(&bytes).map_err(|_| unsupported("invalid ffprobe JSON"))
}

/// Run on a background worker. The MediaAsset must already be authorized by
/// current project ID/revision and media ID; this API never accepts a JS path.
pub fn prepare(app: &AppHandle, media: &MediaAsset) -> Result<SourceProfile, String> {
    prepare_controlled(app, media, &|_| Ok(()), &|_| {})
}

/// The checkpoint runs on this background worker between phases, before each
/// 64 KiB hash read, and every 5 ms while an owned ffprobe is still running.
/// Returning an error cancels preparation only after process/readers cleanup.
pub fn prepare_controlled(
    app: &AppHandle,
    media: &MediaAsset,
    checkpoint: &dyn Fn(&str) -> Result<(), String>,
    observe: &dyn Fn(Value),
) -> Result<SourceProfile, String> {
    let started = Instant::now();
    let stage = |name: &str| -> Result<(), String> {
        checkpoint(name)?;
        observe(json!({"event":"prepare-stage","stage":name}));
        Ok(())
    };
    stage("source-resolve")?;
    if media.kind != MediaKind::Video
        || !matches!(media.extension.to_ascii_lowercase().as_str(), "mp4" | "mov")
    {
        return Err(unsupported("only imported MP4/MOV video is admitted"));
    }
    let original = PathBuf::from(
        media
            .source_path
            .as_deref()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| unsupported("source is unlinked"))?,
    );
    if !original.is_absolute() {
        return Err(unsupported("source path must be absolute"));
    }
    let source = original
        .canonicalize()
        .map_err(|e| format!("{}: {e}", unsupported("source is missing")))?;
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if !matches!(source.components().next(),Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        {
            return Err(unsupported(
                "network and device source paths are not admitted",
            ));
        }
    }
    if !source
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.eq_ignore_ascii_case(&media.extension))
    {
        return Err(unsupported(
            "source extension does not match the imported asset",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ: deny write/delete during probe.
    }
    let mut file = options.open(&source).map_err(|e| e.to_string())?;
    stage("source-before-hash")?;
    let before =
        source_identity_controlled(&mut file, media, &source, "source-before-hash", checkpoint)?;
    stage("ffmpeg-hash")?;
    let ffmpeg = audited_packaged_binary(app, "ffmpeg", FFMPEG_SHA, "ffmpeg-hash", checkpoint)?;
    stage("ffprobe-hash")?;
    let ffprobe = audited_packaged_binary(app, "ffprobe", FFPROBE_SHA, "ffprobe-hash", checkpoint)?;
    stage("metadata-probe")?;
    let metadata = probe(
        &ffprobe,
        &source,
        None,
        "format=duration,format_name:stream=codec_type,codec_name,width,height,sample_aspect_ratio,time_base,start_pts,duration_ts,sample_rate,channels:stream_tags=rotate:stream_side_data=side_data_type,rotation,displaymatrix",
        "metadata-probe",
        checkpoint,
        observe,
    )?;
    checkpoint("metadata-validate")?;
    validate_metadata(&metadata)?; // Reject long inputs before any frame decoding.
    stage("audio-probe")?;
    let audio = probe(
        &ffprobe,
        &source,
        Some("a:0"),
        "frame=best_effort_timestamp,nb_samples",
        "audio-probe",
        checkpoint,
        observe,
    )?;
    stage("video-probe")?;
    let video = probe(
        &ffprobe,
        &source,
        Some("v:0"),
        "frame=best_effort_timestamp,duration,pkt_duration",
        "video-probe",
        checkpoint,
        observe,
    )?;
    stage("parse-profile")?;
    let mut parsed = parse_profile(&metadata, &audio, &video)?;
    stage("source-after-hash")?;
    let after =
        source_identity_controlled(&mut file, media, &source, "source-after-hash", checkpoint)?;
    if before != after || original.canonicalize().map_err(|e| e.to_string())? != source {
        return Err(unsupported("source changed during profile preparation"));
    }
    parsed.metadata["prepareMs"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    parsed.metadata["sidecars"] = json!({"ffmpegSha256":FFMPEG_SHA,"ffprobeSha256":FFPROBE_SHA});
    stage("prepared")?;
    Ok(SourceProfile {
        config: StreamConfig {
            ffmpeg,
            ffprobe,
            video: source.clone(),
            audio: source,
            sample_rate: SAMPLE_RATE,
            channels: 2,
            video_width: parsed.width,
            video_height: parsed.height,
            seek_mode: SeekMode::IndexedPts,
            epoch: 1,
            start_audio_frame: 0,
            start_video_ns: 0,
        },
        timeline: parsed.timeline,
        end_ns: parsed.end_ns,
        source_fingerprint: before,
        metadata: parsed.metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_stops_hash_before_reading_the_next_64k_chunk() {
        use std::{cell::Cell, io::Write};
        let mut source = tempfile::NamedTempFile::new().unwrap();
        source.write_all(&vec![42; 131_073]).unwrap();
        let calls = Cell::new(0);
        let result =
            file_hash_controlled(source.as_file_mut(), 131_073, "source-before-hash", &|_| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    Err("superseded during hash".into())
                } else {
                    Ok(())
                }
            });
        assert_eq!(result.unwrap_err(), "superseded during hash");
        assert_eq!(source.as_file_mut().stream_position().unwrap(), 65_536);
    }

    fn fixture() -> (Value, Value, Value) {
        (
            json!({"format":{"duration":"0.080000"},"streams":[
                {"codec_type":"video","codec_name":"h264","width":160,"height":90,"sample_aspect_ratio":"1:1","time_base":"1/1000","start_pts":0,"duration_ts":80},
                {"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"time_base":"1/48000","start_pts":0,"duration_ts":3840}
            ]}),
            json!({"frames":[{"best_effort_timestamp":0,"nb_samples":1920},{"best_effort_timestamp":1920,"nb_samples":1920}]}),
            json!({"frames":[{"best_effort_timestamp":0,"duration":40},{"best_effort_timestamp":40,"duration":40}]}),
        )
    }

    #[test]
    fn actual_decoded_origins_and_sample_counts_define_eof() {
        let (mut metadata, mut audio, mut video) = fixture();
        metadata["format"]["duration"] = "0.180000".into();
        metadata["streams"][0]["start_pts"] = 100.into();
        metadata["streams"][1]["start_pts"] = 5760.into();
        audio["frames"][0]["best_effort_timestamp"] = 5760.into();
        audio["frames"][1]["best_effort_timestamp"] = 7680.into();
        video["frames"][0]["best_effort_timestamp"] = 100.into();
        video["frames"][1]["best_effort_timestamp"] = 140.into();
        let parsed = parse_profile(&metadata, &audio, &video).unwrap();
        let diagnostics = parsed.timeline.diagnostics();
        assert_eq!(diagnostics.total_frames, 3840);
        assert_eq!(diagnostics.first_source_timeline_ns, "20000000");
        assert_eq!(parsed.end_ns, 100_000_000);
        assert_eq!((parsed.width, parsed.height), (160, 90));
    }

    #[test]
    fn decoded_audio_gaps_and_overlap_are_unsupported() {
        for timestamp in [1919, 1921] {
            let (metadata, mut audio, video) = fixture();
            audio["frames"][1]["best_effort_timestamp"] = timestamp.into();
            assert!(parse_profile(&metadata, &audio, &video).is_err());
        }
    }

    #[test]
    fn vfr_uses_actual_final_frame_duration_without_average_rate() {
        let (mut metadata, audio, mut video) = fixture();
        metadata["streams"][0]["duration_ts"] = 100.into();
        metadata["format"]["duration"] = "0.100000".into();
        video["frames"] = json!([
            {"best_effort_timestamp":0,"duration":20},
            {"best_effort_timestamp":20,"duration":50},
            {"best_effort_timestamp":70,"duration":30}
        ]);
        assert_eq!(
            parse_profile(&metadata, &audio, &video).unwrap().end_ns,
            100_000_000
        );
    }

    #[test]
    fn rejects_transform_resample_missing_frames_and_noninteger_pts() {
        let (metadata, audio, video) = fixture();
        let mut altered = metadata.clone();
        altered["streams"][0]["side_data_list"] = json!([{"rotation":90}]);
        assert!(parse_profile(&altered, &audio, &video).is_err());
        altered = metadata.clone();
        altered["streams"][0]["sample_aspect_ratio"] = "4:3".into();
        assert!(parse_profile(&altered, &audio, &video).is_err());
        altered = metadata.clone();
        altered["streams"][1]["sample_rate"] = "44100".into();
        assert!(parse_profile(&altered, &audio, &video).is_err());
        altered = metadata.clone();
        altered["streams"][1]["codec_name"] = "pcm_s16le".into();
        assert!(parse_profile(&altered, &audio, &video).is_err());
        assert!(parse_profile(&metadata, &json!({"frames":[]}), &video).is_err());
        let mut malformed = audio.clone();
        malformed["frames"][0]["best_effort_timestamp"] = "N/A".into();
        assert!(parse_profile(&metadata, &malformed, &video).is_err());
    }

    #[test]
    fn limits_reject_large_dimensions_duration_and_frame_count() {
        let (metadata, audio, video) = fixture();
        let mut altered = metadata.clone();
        altered["streams"][0]["width"] = 1921.into();
        assert!(parse_profile(&altered, &audio, &video).is_err());
        altered = metadata.clone();
        altered["format"]["duration"] = "60.000001".into();
        assert!(parse_profile(&altered, &audio, &video).is_err());
        let too_many =
            json!({"frames":vec![json!({"best_effort_timestamp":0,"nb_samples":1});100001]});
        assert!(parse_profile(&metadata, &too_many, &video).is_err());
    }

    #[test]
    fn rejects_duplicate_video_pts_and_decoded_tail_beyond_stream_end() {
        let (metadata, audio, mut video) = fixture();
        video["frames"][1]["best_effort_timestamp"] = 0.into();
        assert!(parse_profile(&metadata, &audio, &video).is_err());
        video["frames"][1]["best_effort_timestamp"] = 70.into();
        assert!(parse_profile(&metadata, &audio, &video).is_err());
    }

    #[test]
    fn missing_final_duration_uses_declared_extent_but_not_average_fps() {
        let (metadata, audio, mut video) = fixture();
        video["frames"][1]
            .as_object_mut()
            .unwrap()
            .remove("duration");
        let parsed = parse_profile(&metadata, &audio, &video).unwrap();
        assert_eq!(parsed.end_ns, 80_000_000);
        assert_eq!(parsed.metadata["decodedVideoFrames"], 2);
        assert_eq!(parsed.metadata["decodedAudioSamples"], 3840);
    }

    #[test]
    fn negative_audio_origin_keeps_already_applied_decoder_priming() {
        let (metadata, mut audio, video) = fixture();
        audio["frames"][0]["best_effort_timestamp"] = (-480).into();
        audio["frames"][1]["best_effort_timestamp"] = 1440.into();
        let parsed = parse_profile(&metadata, &audio, &video).unwrap();
        assert_eq!(
            parsed.timeline.diagnostics().first_source_timeline_ns,
            "-10000000"
        );
        assert_eq!(
            parsed.timeline.span(0, 480).unwrap().source_start_frame,
            480
        );
        assert_eq!(parsed.end_ns, 80_000_000);
    }

    #[test]
    fn durations_are_exact_and_malformed_timebases_do_not_panic() {
        for malformed in ["NaN", "inf", "-1", "60.000000001", "99999999999999999999"] {
            let (mut metadata, audio, video) = fixture();
            metadata["format"]["duration"] = malformed.into();
            assert!(parse_profile(&metadata, &audio, &video).is_err());
        }
        for malformed in ["1/0", "0/1000", "1/", "N/A"] {
            let (mut metadata, audio, video) = fixture();
            metadata["streams"][0]["time_base"] = malformed.into();
            assert!(parse_profile(&metadata, &audio, &video).is_err());
        }
    }

    #[test]
    fn source_identity_checks_import_metadata_and_bounded_bytes() {
        use std::io::Write;
        let mut source = tempfile::NamedTempFile::new().unwrap();
        source.write_all(b"owned test source").unwrap();
        source.flush().unwrap();
        let stat = source.as_file().metadata().unwrap();
        let mut media = MediaAsset {
            id: "test".into(),
            name: "test.mp4".into(),
            kind: MediaKind::Video,
            extension: "mp4".into(),
            size: stat.len(),
            last_modified: stat
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            source_path: Some(source.path().to_string_lossy().into_owned()),
        };
        let path = source.path().to_owned();
        let identity = source_identity(source.as_file_mut(), &media, &path).unwrap();
        assert_eq!(
            identity["sha256"],
            format!("{:x}", Sha256::digest(b"owned test source"))
        );
        assert!(file_hash(source.as_file_mut(), 1).is_err());
        media.size += 1;
        assert!(source_identity(source.as_file_mut(), &media, &path).is_err());
        media.size = stat.len();
        media.last_modified += 1;
        assert!(source_identity(source.as_file_mut(), &media, &path).is_err());
    }
}
