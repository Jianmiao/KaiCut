use serde::Serialize;
use serde_json::Value;
use std::{
    io::Read,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaEngineStatus {
    available: bool,
    version: Option<String>,
    h264_encoder_available: bool,
    message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaProbeState {
    Ready,
    Missing,
    Unlinked,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProbeResult {
    pub media_id: String,
    pub state: MediaProbeState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps_numerator: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps_denominator: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_channels: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

const FFPROBE_TIMEOUT: Duration = Duration::from_secs(10);
const FFPROBE_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

#[cfg(target_os = "windows")]
pub(crate) fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn hide_console(_command: &mut Command) {}

fn read_capped<R: Read>(mut reader: R) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if output.len() < FFPROBE_OUTPUT_LIMIT {
            let remaining = FFPROBE_OUTPUT_LIMIT - output.len();
            output.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    output
}

pub(crate) fn collect_child_output(
    mut child: Child,
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), String> {
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("无法读取 ffprobe 标准输出".into());
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("无法读取 ffprobe 错误输出".into());
    };
    let stdout_reader = thread::spawn(move || read_capped(stdout));
    let stderr_reader = thread::spawn(move || read_capped(stderr));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err("ffprobe 探测失败或超时".into());
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "读取 ffprobe 标准输出失败".to_string())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "读取 ffprobe 错误输出失败".to_string())?;
    Ok((status, stdout, stderr))
}

fn unavailable(message: impl Into<String>) -> MediaEngineStatus {
    MediaEngineStatus {
        available: false,
        version: None,
        h264_encoder_available: false,
        message: message.into(),
    }
}

pub(crate) fn packaged_binary(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let filename = if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(&filename));
            candidates.push(directory.join("binaries").join(&filename));
        }
    }
    if let Ok(resources) = app.path().resource_dir() {
        candidates.push(resources.join(&filename));
        candidates.push(resources.join("binaries").join(&filename));
    }
    #[cfg(all(debug_assertions, target_os = "windows", target_arch = "x86_64"))]
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!("{name}-x86_64-pc-windows-msvc.exe")),
    );
    candidates.into_iter().find(|path| path.is_file())
}

fn packaged_ffmpeg(app: &AppHandle) -> Option<PathBuf> {
    packaged_binary(app, "ffmpeg")
}

fn probe(app: AppHandle) -> MediaEngineStatus {
    let Some(path) = packaged_ffmpeg(&app) else {
        return unavailable(
            "尚未随应用打包 FFmpeg；当前仅支持素材元数据导入，预览和导出仍为占位。",
        );
    };
    let mut command = Command::new(&path);
    command
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return unavailable(format!("已找到 FFmpeg，但无法启动：{error}")),
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return unavailable("无法读取 FFmpeg 版本输出");
    };
    let reader = thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let _ = stdout.take(64 * 1024).read_to_string(&mut text);
        text
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = reader.join().unwrap_or_default();
                let version = output.lines().next().unwrap_or("").trim();
                if status.success() && version.starts_with("ffmpeg version") {
                    let h264_encoder_available = probe_encoder(&path);
                    return MediaEngineStatus {
                        available: true,
                        version: Some(version.chars().take(300).collect()),
                        h264_encoder_available,
                        message: if h264_encoder_available {
                            "FFmpeg sidecar 已就绪；已编译 h264_mf 支持，当前设备运行能力待检测。"
                                .into()
                        } else {
                            "FFmpeg sidecar 已就绪；此构建未检测到 h264_mf 编译支持。".into()
                        },
                    };
                }
                return unavailable("打包的 FFmpeg 版本探测未返回有效结果");
            }
            Ok(None) if start.elapsed() < Duration::from_secs(3) => {
                thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return unavailable("FFmpeg 版本探测失败或超时");
            }
        }
    }
}

fn probe_encoder(path: &PathBuf) -> bool {
    probe_encoders(path).h264_mf
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EncoderCapabilities {
    pub h264_mf: bool,
    pub aac: bool,
}

/// Check the encoder list exposed by the packaged FFmpeg binary. This is a
/// compile-time/sidecar capability check; it does not claim that a device MFT
/// can successfully encode a particular input until an actual export probe is
/// run.
pub(crate) fn probe_encoders(path: &PathBuf) -> EncoderCapabilities {
    let mut command = Command::new(path);
    command
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    let Ok(child) = command.spawn() else {
        return EncoderCapabilities::default();
    };
    let Ok((status, stdout, _)) = collect_child_output(child, Duration::from_secs(10)) else {
        return EncoderCapabilities::default();
    };
    if !status.success() {
        return EncoderCapabilities::default();
    }
    parse_encoder_capabilities(&stdout)
}

fn parse_encoder_capabilities(bytes: &[u8]) -> EncoderCapabilities {
    let mut capabilities = EncoderCapabilities::default();
    for line in String::from_utf8_lossy(bytes).lines() {
        match line.split_whitespace().nth(1) {
            Some("h264_mf") => capabilities.h264_mf = true,
            Some("aac") => capabilities.aac = true,
            _ => {}
        }
    }
    capabilities
}

fn parse_rational(value: Option<&Value>) -> Option<(u32, u32)> {
    let text = value?.as_str()?;
    let (numerator, denominator) = text.split_once('/')?;
    let numerator = numerator.parse().ok()?;
    let denominator = denominator.parse().ok()?;
    (denominator != 0).then_some((numerator, denominator))
}

fn parse_duration_ms(value: Option<&Value>) -> Option<u64> {
    let seconds = value?.as_str()?.parse::<f64>().ok()?;
    let milliseconds = seconds * 1000.0;
    (milliseconds.is_finite() && milliseconds >= 0.0 && milliseconds <= u64::MAX as f64)
        .then_some(milliseconds.round() as u64)
}

fn probe_error(
    media_id: &str,
    state: MediaProbeState,
    message: impl Into<String>,
) -> MediaProbeResult {
    MediaProbeResult {
        media_id: media_id.into(),
        state,
        duration_ms: None,
        width: None,
        height: None,
        fps_numerator: None,
        fps_denominator: None,
        audio_sample_rate: None,
        audio_channels: None,
        rotation: None,
        error: Some(message.into()),
    }
}

fn parse_probe_output(media: &crate::project::MediaAsset, bytes: &[u8]) -> MediaProbeResult {
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(error) => {
            return probe_error(
                &media.id,
                MediaProbeState::Failed,
                format!("ffprobe 返回了无效 JSON：{error}"),
            );
        }
    };
    let Some(streams) = value.get("streams").and_then(Value::as_array) else {
        return probe_error(
            &media.id,
            MediaProbeState::Failed,
            "ffprobe 没有返回有效的媒体流",
        );
    };
    let video = streams.iter().find(|stream| {
        stream.get("codec_type").and_then(Value::as_str) == Some("video")
            && stream
                .get("disposition")
                .and_then(|value| value.get("attached_pic"))
                .and_then(Value::as_u64)
                != Some(1)
    });
    let audio = streams
        .iter()
        .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("audio"));
    if video.is_none() && audio.is_none() {
        return probe_error(
            &media.id,
            MediaProbeState::Failed,
            "素材中没有可识别的视频、图像或音频流",
        );
    }
    let is_image = media.kind == crate::project::MediaKind::Image;
    let fps = if is_image {
        None
    } else {
        parse_rational(video.and_then(|stream| stream.get("avg_frame_rate")))
            .filter(|(numerator, _)| *numerator > 0)
            .or_else(|| {
                parse_rational(video.and_then(|stream| stream.get("r_frame_rate")))
                    .filter(|(numerator, _)| *numerator > 0)
            })
    };
    let (fps_numerator, fps_denominator) = fps.map_or((None, None), |(n, d)| (Some(n), Some(d)));
    let rotation = video
        .and_then(|stream| stream.get("side_data_list"))
        .and_then(Value::as_array)
        .and_then(|data| {
            data.iter()
                .find_map(|item| item.get("rotation").and_then(Value::as_i64))
        })
        .and_then(|value| i32::try_from(value).ok())
        .or_else(|| {
            video
                .and_then(|stream| stream.get("tags"))
                .and_then(|tags| tags.get("rotate"))
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok())
        });
    let duration_ms = if is_image {
        None
    } else {
        parse_duration_ms(
            value
                .get("format")
                .and_then(|format| format.get("duration")),
        )
        .or_else(|| {
            streams
                .iter()
                .filter_map(|stream| parse_duration_ms(stream.get("duration")))
                .max()
        })
    };
    MediaProbeResult {
        media_id: media.id.clone(),
        state: MediaProbeState::Ready,
        duration_ms,
        width: video
            .and_then(|stream| stream.get("width"))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        height: video
            .and_then(|stream| stream.get("height"))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        fps_numerator,
        fps_denominator,
        audio_sample_rate: audio
            .and_then(|stream| stream.get("sample_rate"))
            .and_then(Value::as_str)
            .and_then(|value| value.parse().ok()),
        audio_channels: audio
            .and_then(|stream| stream.get("channels"))
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok()),
        rotation,
        error: None,
    }
}

pub(crate) fn probe_one(ffprobe: &PathBuf, media: &crate::project::MediaAsset) -> MediaProbeResult {
    let Some(path) = media.source_path.as_deref() else {
        return probe_error(&media.id, MediaProbeState::Unlinked, "素材没有可用路径");
    };
    if !std::path::Path::new(path).is_file() {
        return probe_error(&media.id, MediaProbeState::Missing, "找不到原始素材");
    }
    let mut command = Command::new(ffprobe);
    command.args([
        "-v",
        "error",
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        "mov,mp4,m4a,3gp,3g2,mj2,mp3,wav,image2,image2pipe",
        "-show_streams",
        "-show_format",
        "-print_format",
        "json",
    ]);
    if media.kind == crate::project::MediaKind::Image {
        command.args(["-f", "image2", "-pattern_type", "none"]);
    }
    command
        .args(["-i", path])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return probe_error(
                &media.id,
                MediaProbeState::Failed,
                format!("无法启动 ffprobe：{error}"),
            );
        }
    };
    let (status, stdout, stderr) = match collect_child_output(child, FFPROBE_TIMEOUT) {
        Ok(output) => output,
        Err(error) => return probe_error(&media.id, MediaProbeState::Failed, error),
    };
    if !status.success() {
        let error: String = String::from_utf8_lossy(&stderr)
            .trim()
            .chars()
            .take(300)
            .collect();
        return probe_error(
            &media.id,
            MediaProbeState::Failed,
            if error.is_empty() {
                "ffprobe 未能识别该素材".into()
            } else {
                error
            },
        );
    }
    parse_probe_output(media, &stdout)
}

#[tauri::command]
pub async fn media_probe(app: AppHandle) -> Result<Vec<MediaProbeResult>, String> {
    let session = crate::commands::with_session(app.clone(), |_, _| Ok(())).await?;
    let ffprobe = packaged_binary(&app, "ffprobe").ok_or("尚未随应用打包 ffprobe")?;
    tauri::async_runtime::spawn_blocking(move || {
        Ok(session
            .project
            .media_refs
            .iter()
            .map(|media| probe_one(&ffprobe, media))
            .collect())
    })
    .await
    .map_err(|error| format!("媒体探测未能完成：{error}"))?
}

#[tauri::command]
pub async fn media_engine_status(app: AppHandle) -> Result<MediaEngineStatus, String> {
    tauri::async_runtime::spawn_blocking(move || probe(app))
        .await
        .map_err(|error| format!("媒体引擎状态检查失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn media(kind: crate::project::MediaKind) -> crate::project::MediaAsset {
        crate::project::MediaAsset {
            id: "media-test".into(),
            name: "test".into(),
            kind,
            extension: "mp4".into(),
            size: 0,
            last_modified: 0,
            source_path: None,
        }
    }

    #[test]
    fn parses_valid_rational_and_rejects_malformed_values() {
        assert_eq!(
            parse_rational(Some(&json!("30000/1001"))),
            Some((30_000, 1_001))
        );
        assert_eq!(parse_rational(Some(&json!("25/0"))), None);
        assert_eq!(parse_rational(Some(&json!("0/25"))), Some((0, 25)));
        assert_eq!(parse_rational(Some(&json!("25"))), None);
        assert_eq!(parse_rational(Some(&json!("-1/25"))), None);
        assert_eq!(parse_rational(Some(&json!(25))), None);
        assert_eq!(parse_rational(None), None);
    }

    #[test]
    fn parses_duration_in_milliseconds_with_bounds() {
        assert_eq!(parse_duration_ms(Some(&json!("2.001"))), Some(2_001));
        assert_eq!(parse_duration_ms(Some(&json!("0"))), Some(0));
        assert_eq!(parse_duration_ms(Some(&json!("-1"))), None);
        assert_eq!(parse_duration_ms(Some(&json!("N/A"))), None);
        assert_eq!(parse_duration_ms(Some(&json!("1e30"))), None);
        assert_eq!(parse_duration_ms(Some(&json!(2.0))), None);
    }

    #[test]
    fn parses_probe_metadata_and_falls_back_from_invalid_duration() {
        let asset = media(crate::project::MediaKind::Video);
        let result = parse_probe_output(
            &asset,
            br#"{
                "streams": [
                    {"codec_type":"video","width":1920,"height":1080,"avg_frame_rate":"0/0","r_frame_rate":"30000/1001","duration":"N/A","side_data_list":[{"rotation":90}]},
                    {"codec_type":"audio","sample_rate":"48000","channels":2,"duration":"2.25"}
                ],
                "format": {"duration":"N/A"}
            }"#,
        );
        assert_eq!(result.state, MediaProbeState::Ready);
        assert_eq!(result.duration_ms, Some(2_250));
        assert_eq!(result.width, Some(1_920));
        assert_eq!(result.height, Some(1_080));
        assert_eq!(
            (result.fps_numerator, result.fps_denominator),
            (Some(30_000), Some(1_001))
        );
        assert_eq!(result.audio_sample_rate, Some(48_000));
        assert_eq!(result.audio_channels, Some(2));
        assert_eq!(result.rotation, Some(90));
    }

    #[test]
    fn image_probe_does_not_report_video_timing() {
        let mut asset = media(crate::project::MediaKind::Image);
        asset.extension = "png".into();
        let result = parse_probe_output(
            &asset,
            br#"{"streams":[{"codec_type":"video","width":100,"height":80,"r_frame_rate":"25/1","duration":"1"}],"format":{"duration":"1"}}"#,
        );
        assert_eq!(result.state, MediaProbeState::Ready);
        assert_eq!(result.width, Some(100));
        assert_eq!(result.height, Some(80));
        assert_eq!(result.duration_ms, None);
        assert_eq!(result.fps_numerator, None);
        assert_eq!(result.fps_denominator, None);
    }

    #[test]
    fn attached_picture_is_not_treated_as_video() {
        let asset = media(crate::project::MediaKind::Audio);
        let result = parse_probe_output(
            &asset,
            br#"{"streams":[{"codec_type":"video","width":100,"height":80,"disposition":{"attached_pic":1}},{"codec_type":"audio","sample_rate":"44100","channels":1,"duration":"3"}],"format":{"duration":"3"}}"#,
        );
        assert_eq!(result.state, MediaProbeState::Ready);
        assert_eq!(result.width, None);
        assert_eq!(result.height, None);
        assert_eq!(result.audio_sample_rate, Some(44_100));
        assert_eq!(result.duration_ms, Some(3_000));
    }

    #[test]
    fn invalid_probe_output_is_failed() {
        let result = parse_probe_output(&media(crate::project::MediaKind::Video), b"not-json");
        assert_eq!(result.state, MediaProbeState::Failed);
        assert!(result.error.is_some());
        assert!(
            parse_probe_output(
                &media(crate::project::MediaKind::Video),
                br#"{"streams":[]}"#
            )
            .error
            .is_some()
        );
    }

    #[test]
    fn probe_result_serializes_state_and_omits_unknown_metadata() {
        let value = serde_json::to_value(probe_error(
            "media-1",
            MediaProbeState::Missing,
            "找不到原始素材",
        ))
        .unwrap();
        assert_eq!(value.get("state"), Some(&json!("missing")));
        assert_eq!(value.get("mediaId"), Some(&json!("media-1")));
        assert_eq!(value.get("error"), Some(&json!("找不到原始素材")));
        assert!(value.get("durationMs").is_none());
        assert!(value.get("width").is_none());
    }

    #[test]
    fn parses_export_encoder_capabilities_without_confusing_decoder_rows() {
        let capabilities = parse_encoder_capabilities(
            b" V....D h264\n A..... aac\n V....D h264_mf\n V..... libx264\n",
        );
        assert!(capabilities.h264_mf);
        assert!(capabilities.aac);
    }
}
