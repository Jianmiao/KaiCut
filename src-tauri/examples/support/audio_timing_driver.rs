//! Offline timing/materialization validation; never opens an audio endpoint.
use super::{
    audio_timeline::{AudioTimeline, Timestamp},
    stream_reference::{reference_pcm_hash, reference_pcm_samples},
    streaming_decoder::{SeekMode, StreamConfig, StreamingDecoder},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn need(ok: bool, message: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(message.into()) }
}
pub(crate) fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key].as_str().ok_or_else(|| format!("Missing {key}"))
}
pub(crate) fn number(v: &Value, key: &str) -> Result<u64, String> {
    v[key].as_u64().ok_or_else(|| format!("Missing {key}"))
}
pub(crate) fn json_file(path: &Path) -> Result<Value, String> {
    let mut b = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut b)
        .map_err(|e| e.to_string())?;
    need(
        b.len() <= 2 * 1024 * 1024,
        "Timing fixture document exceeds 2 MiB",
    )?;
    serde_json::from_slice(&b).map_err(|e| e.to_string())
}
pub(crate) fn local(root: &Path, name: &str) -> Result<PathBuf, String> {
    let p = Path::new(name);
    need(
        p.is_relative()
            && !p
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "Fixture path escapes root",
    )?;
    let p = root.join(p).canonicalize().map_err(|e| e.to_string())?;
    need(p.starts_with(root), "Fixture path escaped through link")?;
    Ok(p)
}
pub(crate) fn identity(path: &Path) -> Result<Value, String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let m = f.metadata().map_err(|e| e.to_string())?;
    need(m.is_file(), "Expected regular file")?;
    let mut h = Sha256::new();
    let mut b = [0; 65536];
    loop {
        let n = f.read(&mut b).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(
        json!({"path":path,"size":m.len(),"modified":format!("{:?}",m.modified().map_err(|e|e.to_string())?),"sha256":format!("{:x}",h.finalize())}),
    )
}
pub(crate) fn origin(v: &Value, audio: bool) -> Result<Timestamp, String> {
    Ok(Timestamp {
        pts: text(v, "originPts")?
            .parse::<i64>()
            .map_err(|e| e.to_string())?,
        numerator: text(
            v,
            if audio {
                "timebaseN"
            } else {
                "originTimebaseN"
            },
        )?
        .parse::<u32>()
        .map_err(|e| e.to_string())?,
        denominator: text(
            v,
            if audio {
                "timebaseD"
            } else {
                "originTimebaseD"
            },
        )?
        .parse::<u32>()
        .map_err(|e| e.to_string())?,
    })
}
pub(crate) fn sample_hash(samples: &[f32]) -> String {
    let mut h = Sha256::new();
    for s in samples {
        h.update(s.to_le_bytes());
    }
    format!("{:x}", h.finalize())
}

pub(crate) fn validate_audio_metadata(v: &Value, ts: Timestamp, frames: u64) -> Result<(), String> {
    let records = v["packets_and_frames"]
        .as_array()
        .ok_or("Missing independent audio frame metadata")?;
    let mut count = 0u64;
    let mut first = true;
    for frame in records.iter().filter(|f| f["type"] == "frame") {
        let pts = frame["best_effort_timestamp"]
            .as_i64()
            .ok_or("Missing decoded audio PTS")?;
        if first {
            need(
                pts == ts.pts,
                "Audio origin differs from first decoded frame",
            )?;
            first = false;
        }
        let lhs = (i128::from(pts) - i128::from(ts.pts)) * i128::from(ts.numerator) * 48000;
        let rhs = i128::from(count) * i128::from(ts.denominator);
        need(
            lhs == rhs,
            "Decoded audio frame timestamps have a discontinuity",
        )?;
        count = count
            .checked_add(number(frame, "nb_samples")?)
            .ok_or("Audio frame count overflow")?;
    }
    need(
        !first && count == frames,
        "Decoded audio metadata sample count differs from reference",
    )
}

fn decode_audio(
    video: &Path,
    audio: &Path,
    start: u64,
    count: u64,
    full: bool,
    epoch: u64,
    report: &mut Value,
) -> Result<Vec<f32>, String> {
    let before = Instant::now();
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
    let mut decoder = StreamingDecoder::start(StreamConfig {
        ffmpeg: bin.join("ffmpeg-x86_64-pc-windows-msvc.exe"),
        ffprobe: bin.join("ffprobe-x86_64-pc-windows-msvc.exe"),
        video: video.into(),
        audio: audio.into(),
        sample_rate: 48000,
        channels: 2,
        video_width: 320,
        video_height: 180,
        seek_mode: SeekMode::IndexedPts,
        epoch,
        start_audio_frame: start,
        start_video_ns: 0,
    })?;
    let mut h = Sha256::new();
    let mut samples = Vec::new();
    let mut consumed = 0u64;
    let mut first = None;
    let result = (|| {
        loop {
            need(
                before.elapsed() < Duration::from_secs(20),
                "Audio range decode timed out",
            )?;
            // Drain video descriptors so an audio-only check cannot strand its sibling pipe.
            let _ = decoder.poll_video()?;
            if let Some(block) = decoder.poll_audio()? {
                first.get_or_insert(block.source_start_frame);
                need(
                    block.epoch == epoch && block.source_start_frame == start + consumed,
                    "Audio range offset or epoch mismatch",
                )?;
                need(
                    block.samples.len() % 2 == 0 && block.samples.len() <= 960,
                    "Invalid audio packet length",
                )?;
                let n = ((block.samples.len() / 2) as u64).min(count - consumed);
                for sample in &block.samples[..n as usize * 2] {
                    h.update(sample.to_le_bytes());
                }
                if !full {
                    samples.extend_from_slice(&block.samples[..n as usize * 2]);
                }
                if full {
                    need(
                        n as usize * 2 == block.samples.len(),
                        "Full audio decode exceeded reference length",
                    )?;
                }
                consumed += n;
            }
            let stats = decoder.stats()?;
            need(
                stats.error.is_none() && stats.audio_high_water <= 8 && stats.video_high_water <= 4,
                "Decode failure or queue overflow",
            )?;
            if full {
                if stats.audio_eof && stats.audio_pending == 0 {
                    need(consumed == count, "Audio EOF before full reference")?;
                    need(
                        stats
                            .processes
                            .iter()
                            .filter(|p| p.label == "stream-pcm")
                            .all(|p| p.exited && p.exit_code == Some(0)),
                        "Audio EOF without successful decoder exit",
                    )?;
                    break;
                }
            } else if consumed == count {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok::<(), String>(())
    })();
    let closed = decoder.close()?;
    *report = json!({"sourceStartFrame":start,"frames":consumed,"firstDeliveredFrame":first,"pcmSha256":format!("{:x}",h.finalize()),
        "elapsedMs":before.elapsed().as_secs_f64()*1000.0,"fullSource":full,"shutdown":closed,"decodeError":result.as_ref().err()});
    result?;
    need(
        closed.joined && closed.processes_reaped && closed.error.is_none(),
        "Audio range cleanup failed",
    )?;
    Ok(samples)
}

fn materialize(source: &[f32], leading: u32, trailing: u32) -> Vec<f32> {
    let mut out = vec![0.0; leading as usize * 2];
    out.extend_from_slice(source);
    out.resize(out.len() + trailing as usize * 2, 0.0);
    out
}

pub fn run() -> Result<(), String> {
    let mut fixture = None;
    let mut output = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(k) = args.next() {
        let p = PathBuf::from(args.next().ok_or("Missing option value")?);
        match k.to_str() {
            Some("--fixture-dir") => fixture = Some(p),
            Some("--output-dir") => output = Some(p),
            _ => return Err("Expected --fixture-dir and --output-dir".into()),
        }
    }
    let root = fixture
        .ok_or("Missing fixture directory")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let output = output.ok_or("Missing output directory")?;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lexical = manifest.join("target");
    let target = lexical.canonicalize().map_err(|e| e.to_string())?;
    need(
        output.is_absolute()
            && output.starts_with(&lexical)
            && !output
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "Output must stay in checkout target",
    )?;
    let ancestor = output
        .ancestors()
        .find(|p| p.exists())
        .ok_or("Missing output ancestor")?;
    need(
        ancestor
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&target),
        "Output escaped through a link",
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let output = output.canonicalize().map_err(|e| e.to_string())?;
    need(output.starts_with(&target), "Output escaped target")?;
    let run = output.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run).map_err(|e| e.to_string())?;
    let fixture_path = root.join("audio-timing-fixtures.json");
    let doc = json_file(&fixture_path)?;
    let mut report = json!({"schemaVersion":1,"ok":false,"createdAt":chrono::Utc::now().to_rfc3339(),"buildMode":if cfg!(debug_assertions){"debug"}else{"release"},
        "scope":{"decoderOnly":true,"gpu":false,"audioDevice":false,"visibleWindow":false,"sampleRate":48000,"channels":2,"packetFrames":4800,
        "semantics":"Snap block start to discrete source sample timestamp; then read consecutive samples. Silence retains requested output duration."},
        "fixtureManifest":identity(&fixture_path)?,"fullAudioRuns":[],"cases":[]});
    let work = (|| {
        let videos = doc["videoCases"].as_array().ok_or("Missing video cases")?;
        let audios = doc["audioCases"].as_array().ok_or("Missing audio cases")?;
        need(
            videos.len() == 2 && audios.len() == 3,
            "Expected bounded 2-video 3-audio timing fixture matrix",
        )?;
        let mut epoch = 1u64;
        for audio in audios {
            let audio_path = local(&root, text(audio, "source")?)?;
            let reference = local(&root, text(audio, "referenceWav")?)?;
            let aidentity = identity(&audio_path)?;
            let ridentity = identity(&reference)?;
            need(
                aidentity["sha256"] == text(audio, "sourceSha256")?
                    && ridentity["sha256"] == text(audio, "referenceSha256")?,
                "Audio/reference fixture identity mismatch",
            )?;
            let aorigin = origin(audio, true)?;
            let total = number(audio, "decodedFrames")?;
            let metadata_path = local(&root, text(audio, "metadata")?)?;
            let metadata = json_file(&metadata_path)?;
            validate_audio_metadata(&metadata, aorigin, total)?;
            report["activeDecode"] = json!({"audioLabel":audio["label"],"fullSource":true});
            let _ = decode_audio(
                &local(&root, text(&videos[0], "video")?)?,
                &audio_path,
                0,
                total,
                true,
                epoch,
                &mut report["activeDecode"],
            )?;
            epoch += 1;
            let mut full = report["activeDecode"].take();
            let expected = reference_pcm_hash(&reference, 0, total, 48000, 2)?;
            need(
                full["pcmSha256"] == expected,
                "Full nonperiodic audio differs from reference",
            )?;
            full["audioLabel"] = audio["label"].clone();
            full["audioSource"] = aidentity.clone();
            full["pcmReference"] = ridentity.clone();
            full["metadata"] = identity(&metadata_path)?;
            full["audioOrigin"] = json!(aorigin);
            full["ok"] = true.into();
            report["fullAudioRuns"].as_array_mut().unwrap().push(full);
            for video in videos {
                let video_path = local(&root, text(video, "video")?)?;
                let vid = identity(&video_path)?;
                need(
                    vid["sha256"] == text(video, "sourceSha256")?,
                    "Video origin source identity mismatch",
                )?;
                let vorig = origin(video, false)?;
                let vref = json_file(&local(&root, text(video, "reference")?)?)?;
                need(
                    vref["sourceHash"] == vid["sha256"]
                        && vref["sourceFirstPts"] == video["originPts"]
                        && vref["timebaseN"] == video["originTimebaseN"]
                        && vref["timebaseD"] == video["originTimebaseD"],
                    "Video origin does not match decoded reference",
                )?;
                let timeline = AudioTimeline::new(aorigin, vorig, 48000, total)?;
                let diagnostics = timeline.diagnostics();
                let mut targets = BTreeSet::new();
                for t in doc["targetsNs"]
                    .as_array()
                    .ok_or("Missing timing targets")?
                {
                    targets.insert(
                        t.as_str()
                            .ok_or("Target must be decimal")?
                            .parse::<u64>()
                            .map_err(|e| e.to_string())?,
                    );
                }
                // Add exact observed start/end and one ns/sample to either side.
                let diag = json!(diagnostics);
                for key in ["firstSourceTimelineNs", "sourceEndTimelineNs"] {
                    let value = text(&diag, key)?
                        .parse::<i128>()
                        .map_err(|e| e.to_string())?;
                    for delta in [-20834i128, -1, 0, 1, 20834] {
                        let time = value + delta;
                        if time >= 0 {
                            targets.insert(u64::try_from(time).map_err(|e| e.to_string())?);
                        }
                    }
                }
                need(
                    targets.len() <= 24,
                    "Timing target matrix grew beyond limit",
                )?;
                for ns in targets {
                    let span = timeline.span(ns, 4800)?;
                    report["activeCase"] = json!({"audioLabel":audio["label"],"videoLabel":video["label"],"targetNs":ns.to_string(),"span":span});
                    let (samples, source_decode) = if span.source_frames > 0 {
                        report["activeDecode"] = Value::Null;
                        let samples = decode_audio(
                            &video_path,
                            &audio_path,
                            span.source_start_frame,
                            u64::from(span.source_frames),
                            false,
                            epoch,
                            &mut report["activeDecode"],
                        )?;
                        epoch += 1;
                        let decoded = report["activeDecode"].take();
                        (samples, decoded)
                    } else {
                        (Vec::new(), Value::Null)
                    };
                    let expected_source = reference_pcm_samples(
                        &reference,
                        span.source_start_frame,
                        span.source_frames,
                        48000,
                        2,
                    )?;
                    let actual = materialize(
                        &samples,
                        span.leading_silence_frames,
                        span.trailing_silence_frames,
                    );
                    let expected = materialize(
                        &expected_source,
                        span.leading_silence_frames,
                        span.trailing_silence_frames,
                    );
                    need(
                        actual.len() == 9600 && actual == expected,
                        "Timeline audio packet differs from exact source/silence placement",
                    )?;
                    let left: Vec<f32> = actual.chunks_exact(2).map(|f| f[0]).collect();
                    let right: Vec<f32> = actual.chunks_exact(2).map(|f| f[1]).collect();
                    report["cases"].as_array_mut().unwrap().push(json!({"ok":true,"audioLabel":audio["label"],"videoLabel":video["label"],"targetNs":ns.to_string(),"requestedFrames":4800,
                        "audioOrigin":aorigin,"videoOrigin":vorig,"totalSourceFrames":total,"timeline":diag,"span":span,"sourceDecode":source_decode,
                        "audioSource":aidentity,"pcmReference":ridentity,"videoSource":vid,
                        "actualOutputHash":sample_hash(&actual),"expectedOutputHash":sample_hash(&expected),"actualLeftHash":sample_hash(&left),"actualRightHash":sample_hash(&right)}));
                }
                need(
                    identity(&video_path)? == vid,
                    "Video source changed during timing test",
                )?;
            }
            need(
                identity(&audio_path)? == aidentity && identity(&reference)? == ridentity,
                "Audio source/reference changed during timing test",
            )?;
        }
        Ok::<(), String>(())
    })();
    if work.is_ok() {
        report.as_object_mut().unwrap().remove("activeCase");
        report.as_object_mut().unwrap().remove("activeDecode");
    }
    report["ok"] = work.is_ok().into();
    report["functionalPassed"] = work.is_ok().into();
    report["realtimePerformanceQualified"] = false.into();
    if let Err(e) = &work {
        report["error"] = e.clone().into();
    }
    let path = run.join("native-audio-timing-validation.json");
    let mut f = tempfile::NamedTempFile::new_in(&run).map_err(|e| e.to_string())?;
    let saved = serde_json::to_writer_pretty(&mut f, &report)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            f.write_all(b"\n")
                .and_then(|_| f.as_file().sync_all())
                .map_err(|e| e.to_string())
        });
    if let Err(error) = saved {
        let retained = f.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{error}; retained={retained:?}"));
    }
    if let Err(error) = f.persist_noclobber(&path) {
        let why = error.error.to_string();
        let retained = error.file.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{why}; retained={retained:?}"));
    }
    println!(
        "{}",
        json!({"ok":report["ok"],"report":path,"cases":report["cases"].as_array().unwrap().len(),"error":report.get("error")})
    );
    work
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn materialization_preserves_interleaved_samples_and_exact_silence() {
        assert_eq!(
            materialize(&[0.25, -0.75, 0.5, -0.125], 1, 2),
            [0.0, 0.0, 0.25, -0.75, 0.5, -0.125, 0.0, 0.0, 0.0, 0.0]
        );
    }
    #[test]
    fn decoded_origin_check_rejects_pts_gaps_and_metadata_only_priming() {
        let origin = Timestamp {
            pts: 94944,
            numerator: 1,
            denominator: 48000,
        };
        let mut metadata = json!({"packets_and_frames":[{"type":"packet","pts":93920},{"type":"frame","best_effort_timestamp":94944,"nb_samples":1024},{"type":"frame","best_effort_timestamp":95968,"nb_samples":768}]});
        assert!(validate_audio_metadata(&metadata, origin, 1792).is_ok());
        metadata["packets_and_frames"][2]["best_effort_timestamp"] = 95969.into();
        assert!(validate_audio_metadata(&metadata, origin, 1792).is_err());
    }
}
