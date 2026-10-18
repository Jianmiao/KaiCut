//! Decoder compatibility validation. No playback clock, audio endpoint or GPU.
use super::{
    stream_reference::reference_pcm_hash,
    streaming_decoder::{SeekMode, StreamConfig, StreamingDecoder},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn uint(value: &Value, name: &str) -> Result<u64, String> {
    value[name]
        .as_u64()
        .ok_or_else(|| format!("Missing unsigned field {name}"))
}
fn string<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    value[name]
        .as_str()
        .ok_or_else(|| format!("Missing string field {name}"))
}
fn read_json(path: &Path, limit: u64) -> Result<Value, String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    require(
        bytes.len() as u64 <= limit,
        "Reference document exceeds its byte limit",
    )?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
fn fingerprint(path: &Path) -> Result<Value, String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let info = file.metadata().map_err(|e| e.to_string())?;
    require(info.is_file(), "Source is not a regular file")?;
    let mut h = Sha256::new();
    let mut block = [0; 65536];
    loop {
        let n = file.read(&mut block).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&block[..n]);
    }
    Ok(
        json!({"path":path,"size":info.len(),"modified":format!("{:?}",info.modified().map_err(|e|e.to_string())?),"sha256":format!("{:x}",h.finalize())}),
    )
}
fn fixture_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = Path::new(name);
    require(
        path.is_relative()
            && !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "Fixture path must be relative and stay in its directory",
    )?;
    let resolved = root.join(path).canonicalize().map_err(|e| e.to_string())?;
    require(resolved.starts_with(root), "Fixture escaped through a link")?;
    Ok(resolved)
}

struct VideoReference {
    width: u32,
    height: u32,
    frames: Vec<Value>,
    times: Vec<u64>,
}
impl VideoReference {
    fn parse(value: Value, source_hash: &str) -> Result<Self, String> {
        let hash_valid = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        require(
            hash_valid(source_hash) && string(&value, "sourceHash")? == source_hash,
            "Reference belongs to a different video source",
        )?;
        let width = u32::try_from(uint(&value, "width")?).map_err(|e| e.to_string())?;
        let height = u32::try_from(uint(&value, "height")?).map_err(|e| e.to_string())?;
        require(
            (1..=1920).contains(&width) && (1..=1080).contains(&height),
            "Invalid reference dimensions",
        )?;
        let numerator = string(&value, "timebaseN")?
            .parse::<u32>()
            .map_err(|e| e.to_string())?;
        let denominator = string(&value, "timebaseD")?
            .parse::<u32>()
            .map_err(|e| e.to_string())?;
        require(
            numerator > 0 && denominator > 0,
            "Invalid reference timebase",
        )?;
        let first = string(&value, "sourceFirstPts")?
            .parse::<i64>()
            .map_err(|e| e.to_string())?;
        let frames = value["frames"]
            .as_array()
            .ok_or("Missing reference frames")?
            .clone();
        require(
            !frames.is_empty() && frames.len() <= 100000,
            "Reference frame count outside bounds",
        )?;
        let mut times = Vec::with_capacity(frames.len());
        let mut last = None;
        for (index, frame) in frames.iter().enumerate() {
            require(
                uint(frame, "index")? == index as u64,
                "Reference source index is not contiguous",
            )?;
            let raw = string(frame, "rawPts")?
                .parse::<i64>()
                .map_err(|e| e.to_string())?;
            require(
                index != 0 || raw == first,
                "Reference first PTS differs from origin",
            )?;
            require(
                last.is_none_or(|previous| raw > previous),
                "Reference PTS repeats or reverses",
            )?;
            last = Some(raw);
            let delta =
                u128::try_from(i128::from(raw) - i128::from(first)).map_err(|e| e.to_string())?;
            let ns = delta
                .checked_mul(u128::from(numerator))
                .and_then(|n| n.checked_mul(1_000_000_000))
                .ok_or("Reference PTS overflow")?
                / u128::from(denominator);
            require(
                ns <= 24 * 60 * 60 * 1_000_000_000,
                "Reference duration exceeds bound",
            )?;
            require(
                string(frame, "ptsNs")? == ns.to_string(),
                "Reference normalized PTS does not match raw timestamp",
            )?;
            require(
                hash_valid(string(frame, "rgbSha256")?),
                "Invalid reference RGB hash",
            )?;
            times.push(ns as u64);
        }
        Ok(Self {
            width,
            height,
            frames,
            times,
        })
    }
    fn anchor(&self, ns: u64) -> Result<usize, String> {
        self.times
            .partition_point(|pts| *pts <= ns)
            .checked_sub(1)
            .ok_or_else(|| "No preceding reference frame".into())
    }
}

fn run_one(
    video: &Path,
    audio: &Path,
    audio_reference: &Path,
    reference: &VideoReference,
    total_audio: u64,
    target_ns: u64,
    epoch: u64,
    full: bool,
    report: &mut Value,
) -> Result<(), String> {
    let start = Instant::now();
    require(
        target_ns <= 24 * 60 * 60 * 1_000_000_000,
        "Compatibility target exceeds 24-hour bound",
    )?;
    let source_frame =
        u64::try_from(u128::from(target_ns) * 48000 / 1_000_000_000).map_err(|e| e.to_string())?;
    require(
        source_frame < total_audio,
        "Compatibility seek target exceeds decoded audio reference",
    )?;
    let expected_video = reference.anchor(target_ns)?;
    let binaries = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
    let mut decoder = StreamingDecoder::start(StreamConfig {
        ffmpeg: binaries.join("ffmpeg-x86_64-pc-windows-msvc.exe"),
        ffprobe: binaries.join("ffprobe-x86_64-pc-windows-msvc.exe"),
        video: video.into(),
        audio: audio.into(),
        sample_rate: 48000,
        channels: 2,
        video_width: reference.width,
        video_height: reference.height,
        seek_mode: SeekMode::IndexedPts,
        epoch,
        start_audio_frame: source_frame,
        start_video_ns: target_ns,
    })?;
    report["decoderStartMs"] = (start.elapsed().as_secs_f64() * 1000.0).into();
    let mut frames = Vec::new();
    let mut audio_frames = 0u64;
    let mut audio_hash = Sha256::new();
    let mut first_audio_ms = None;
    let mut first_video_ms = None;
    let result = (|| {
        loop {
            require(
                start.elapsed() < Duration::from_secs(30),
                "Compatibility decode exceeded 30-second budget",
            )?;
            if full || audio_frames < 4800 {
                if let Some(block) = decoder.poll_audio()? {
                    first_audio_ms.get_or_insert(start.elapsed().as_secs_f64() * 1000.0);
                    require(
                        block.epoch == epoch
                            && block.source_start_frame == source_frame + audio_frames,
                        "PCM gap, duplicate or stale epoch",
                    )?;
                    require(
                        !block.samples.is_empty()
                            && block.samples.len() % 2 == 0
                            && block.samples.len() <= 960,
                        "Invalid bounded PCM block",
                    )?;
                    for sample in &block.samples {
                        audio_hash.update(sample.to_le_bytes());
                    }
                    audio_frames += (block.samples.len() / 2) as u64;
                    require(
                        source_frame + audio_frames <= total_audio,
                        "Decoded PCM exceeded complete reference",
                    )?;
                }
            }
            if full || frames.len() < 6 {
                if let Some(frame) = decoder.poll_video()? {
                    first_video_ms.get_or_insert(start.elapsed().as_secs_f64() * 1000.0);
                    let index = expected_video + frames.len();
                    require(
                        frame.epoch == epoch && frame.index == index,
                        "Wrong preceding video frame or stale epoch",
                    )?;
                    let expected = reference
                        .frames
                        .get(index)
                        .ok_or("Video decoded beyond reference")?;
                    require(
                        frame.pts_ns == reference.times[index],
                        "Decoded PTS differs from independent reference",
                    )?;
                    let hash = format!("{:x}", Sha256::digest(&frame.rgb));
                    require(
                        hash == string(expected, "rgbSha256")?,
                        "Decoded RGB content differs from independent reference",
                    )?;
                    frames.push(
                        json!({"index":index,"ptsNs":frame.pts_ns.to_string(),"rgbSha256":hash}),
                    );
                }
            }
            let stats = decoder.stats()?;
            require(
                stats.error.is_none() && stats.audio_high_water <= 8 && stats.video_high_water <= 4,
                "Decoder error or queue bound violation",
            )?;
            if full {
                if stats.audio_eof
                    && stats.video_eof
                    && stats.audio_pending == 0
                    && stats.video_pending == 0
                {
                    require(
                        audio_frames == total_audio - source_frame
                            && frames.len() == reference.frames.len() - expected_video,
                        "Complete decode EOF differs from reference counts",
                    )?;
                    require(
                        stats
                            .processes
                            .iter()
                            .all(|p| p.exited && p.exit_code == Some(0)),
                        "EOF without successful child exits",
                    )?;
                    break;
                }
            } else if frames.len() == 6 && audio_frames >= 4800 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok::<(), String>(())
    })();
    report["elapsedBeforeCleanupMs"] = (start.elapsed().as_secs_f64() * 1000.0).into();
    report["firstAudioMs"] = json!(first_audio_ms);
    report["firstVideoMs"] = json!(first_video_ms);
    report["targetNs"] = target_ns.to_string().into();
    report["expectedFirstVideoIndex"] = expected_video.into();
    report["sourceAudioStartFrame"] = source_frame.into();
    report["audioFrames"] = audio_frames.into();
    report["videoFrames"] = json!(frames);
    report["full"] = full.into();
    report["epoch"] = epoch.into();
    let actual = format!("{:x}", audio_hash.finalize());
    report["pcmSha256"] = actual.clone().into();
    let closed = decoder.close()?;
    report["shutdown"] = json!(closed);
    result?;
    require(
        closed.joined && closed.processes_reaped && closed.error.is_none(),
        "Decoder cleanup did not complete",
    )?;
    let expected = reference_pcm_hash(audio_reference, source_frame, audio_frames, 48000, 2)?;
    report["referencePcmSha256"] = expected.clone().into();
    require(
        actual == expected,
        "PCM samples differ from full sequential decode reference",
    )?;
    report["ok"] = true.into();
    Ok(())
}

pub fn run() -> Result<(), String> {
    let mut fixture = None;
    let mut output = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(key) = args.next() {
        let value = PathBuf::from(args.next().ok_or("Missing option value")?);
        match key.to_str() {
            Some("--fixture-dir") => fixture = Some(value),
            Some("--output-dir") => output = Some(value),
            _ => return Err("Expected --fixture-dir and --output-dir".into()),
        }
    }
    let root = fixture
        .ok_or("Missing --fixture-dir")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let output = output.ok_or("Missing --output-dir")?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lexical = manifest.join("target");
    let target = lexical.canonicalize().map_err(|e| e.to_string())?;
    require(
        output.is_absolute()
            && output.starts_with(&lexical)
            && !output
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "Output must stay inside checkout target",
    )?;
    let ancestor = output
        .ancestors()
        .find(|p| p.exists())
        .ok_or("Missing output ancestor")?;
    require(
        ancestor
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&target),
        "Output escaped through a link",
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let output = output.canonicalize().map_err(|e| e.to_string())?;
    require(output.starts_with(&target), "Output escaped target")?;
    let run = output.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run).map_err(|e| e.to_string())?;
    let fixture_manifest = root.join("compat-fixtures.json");
    let fixture_doc = read_json(&fixture_manifest, 1024 * 1024)?;
    let mut report = json!({"schemaVersion":1,"ok":false,"createdAt":chrono::Utc::now().to_rfc3339(),"buildMode":if cfg!(debug_assertions){"debug"}else{"release"},
        "scope":{"decoderOnly":true,"gpu":false,"audioDevice":false,"visibleWindow":false,"applicationBackendChanged":false},"fixtureManifest":fingerprint(&fixture_manifest)?,"cases":[]});
    let result = (|| {
        let videos = fixture_doc["videoCases"]
            .as_array()
            .ok_or("Missing video cases")?;
        let audios = fixture_doc["audioCases"]
            .as_array()
            .ok_or("Missing audio cases")?;
        require(
            !videos.is_empty() && videos.len() <= 8 && !audios.is_empty() && audios.len() <= 4,
            "Fixture matrix is empty or too large",
        )?;
        let mut epoch = 1u64;
        for v in videos {
            let video = fixture_path(&root, string(v, "video")?)?;
            let video_before = fingerprint(&video)?;
            let reference_path = fixture_path(&root, string(v, "reference")?)?;
            let reference_before = fingerprint(&reference_path)?;
            let reference = VideoReference::parse(
                read_json(&reference_path, 8 * 1024 * 1024)?,
                string(&video_before, "sha256")?,
            )?;
            let target_values = v["targetsNs"].as_array().ok_or("Missing video targets")?;
            require(target_values.len() <= 4, "Too many seek targets")?;
            let mut targets = vec![(0, true)];
            for time in target_values {
                targets.push((
                    time.as_str()
                        .ok_or("Target must be decimal string")?
                        .parse::<u64>()
                        .map_err(|e| e.to_string())?,
                    false,
                ));
            }
            for a in audios {
                let audio = fixture_path(&root, string(a, "source")?)?;
                let audio_before = fingerprint(&audio)?;
                let pcm = fixture_path(&root, string(a, "referenceWav")?)?;
                let pcm_before = fingerprint(&pcm)?;
                require(
                    audio_before["sha256"] == string(a, "sourceSha256")?
                        && pcm_before["sha256"] == string(a, "referenceSha256")?,
                    "Audio fixture or PCM oracle differs from its recorded manifest identity",
                )?;
                let total_audio = uint(a, "decodedFrames")?;
                require(
                    total_audio > 0 && total_audio <= 48000 * 24 * 60 * 60,
                    "Invalid PCM reference frame count",
                )?;
                for (ns, full) in &targets {
                    let mut c = json!({"ok":false,"videoLabel":string(v,"label")?,"audioLabel":string(a,"label")?,"videoSource":video_before,"audioSource":audio_before,
                        "videoReference":fingerprint(&reference_path)?,"pcmReference":fingerprint(&pcm)?});
                    let result = run_one(
                        &video,
                        &audio,
                        &pcm,
                        &reference,
                        total_audio,
                        *ns,
                        epoch,
                        *full,
                        &mut c,
                    );
                    epoch += 1;
                    if let Err(error) = &result {
                        c["error"] = error.clone().into();
                    }
                    report["cases"].as_array_mut().unwrap().push(c);
                    result?;
                }
                require(
                    fingerprint(&audio)? == audio_before && fingerprint(&pcm)? == pcm_before,
                    "Audio source or PCM reference changed during matrix validation",
                )?;
            }
            require(
                fingerprint(&video)? == video_before
                    && fingerprint(&reference_path)? == reference_before,
                "Video source or frame reference changed during matrix validation",
            )?;
        }
        Ok::<(), String>(())
    })();
    report["ok"] = result.is_ok().into();
    report["functionalPassed"] = result.is_ok().into();
    report["realtimePerformanceQualified"] = false.into();
    if let Err(error) = &result {
        report["error"] = error.clone().into();
    }
    let path = run.join("native-compat-validation.json");
    let mut temp = tempfile::NamedTempFile::new_in(&run).map_err(|e| e.to_string())?;
    let saved = serde_json::to_writer_pretty(&mut temp, &report)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            temp.write_all(b"\n")
                .and_then(|_| temp.as_file().sync_all())
                .map_err(|e| e.to_string())
        });
    if let Err(error) = saved {
        let retained = temp.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{error}; retained {retained:?}"));
    }
    if let Err(error) = temp.persist_noclobber(&path) {
        let reason = error.error.to_string();
        let retained = error.file.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{reason}; retained {retained:?}"));
    }
    println!(
        "{}",
        json!({"ok":report["ok"],"report":path,"cases":report["cases"].as_array().unwrap().len(),"error":report.get("error")})
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn example() -> Value {
        json!({"width":320,"height":180,"sourceHash":"a".repeat(64),"timebaseN":"1","timebaseD":"1000","sourceFirstPts":"2000","durationNs":"180000000",
        "frames":[{"index":0,"rawPts":"2000","ptsNs":"0","rgbSha256":"b".repeat(64)},{"index":1,"rawPts":"2040","ptsNs":"40000000","rgbSha256":"c".repeat(64)},
        {"index":2,"rawPts":"2140","ptsNs":"140000000","rgbSha256":"d".repeat(64)}]})
    }
    #[test]
    fn vfr_reference_uses_real_pts_and_normalizes_nonzero_origin() {
        let r = VideoReference::parse(example(), &"a".repeat(64)).unwrap();
        assert_eq!(r.anchor(139999999).unwrap(), 1);
        assert_eq!(r.anchor(140000000).unwrap(), 2);
    }
    #[test]
    fn reference_rejects_mismatched_source_and_fabricated_normalized_pts() {
        let mut v = example();
        v["frames"][1]["ptsNs"] = "50000000".into();
        assert!(VideoReference::parse(v, &"a".repeat(64)).is_err());
        assert!(VideoReference::parse(example(), &"e".repeat(64)).is_err());
    }
}
