use crate::{
    media_engine::{collect_child_output, hide_console, packaged_binary},
    project::{MediaAsset, MediaKind},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};
use tauri::AppHandle;

const MAX_WAVEFORM_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WaveformState {
    Ready,
    Failed,
    Missing,
    Changed,
    Unsupported,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveformResult {
    media_id: String,
    state: WaveformState,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn result(
    media: &MediaAsset,
    state: WaveformState,
    error: Option<String>,
    data_url: Option<String>,
) -> WaveformResult {
    WaveformResult {
        media_id: media.id.clone(),
        state,
        data_url,
        error,
    }
}

fn source_path(media: &MediaAsset) -> Result<std::path::PathBuf, (WaveformState, String)> {
    if media.kind != MediaKind::Audio {
        return Err((WaveformState::Unsupported, "只有音频素材生成波形".into()));
    }
    let Some(path) = media.source_path.as_deref() else {
        return Err((WaveformState::Missing, "素材没有可用路径".into()));
    };
    let path = Path::new(path)
        .canonicalize()
        .map_err(|_| (WaveformState::Missing, "找不到原始素材".into()))?;
    let metadata = path
        .metadata()
        .map_err(|_| (WaveformState::Missing, "无法读取素材属性".into()))?;
    if !metadata.is_file() {
        return Err((WaveformState::Missing, "素材路径不是文件".into()));
    }
    if metadata.len() != media.size {
        return Err((WaveformState::Changed, "原始素材已更改，请重新链接".into()));
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as u64);
    if modified != Some(media.last_modified) {
        return Err((WaveformState::Changed, "原始素材已更改，请重新链接".into()));
    }
    Ok(path)
}

fn generate(ffmpeg: &Path, source: &Path, output: &Path) -> Result<Vec<u8>, String> {
    let mut command = Command::new(ffmpeg);
    command.args([
        "-v", "error", "-nostdin", "-y", "-max_alloc", "67108864",
        "-protocol_whitelist", "file",
        "-format_whitelist", "mp3,wav",
        "-i", source.to_string_lossy().as_ref(),
        "-filter_complex", "[0:a]aformat=channel_layouts=mono,showwaves=s=1200x96:mode=cline:colors=0x70A7FF[wave]",
        "-map", "[wave]", "-frames:v", "1", "-an", "-c:v", "png", "-pix_fmt", "rgba", "-threads", "1", "-fs", "2097152", "-f", "image2", "-update", "1",
        output.to_string_lossy().as_ref(),
    ]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_console(&mut command);
    let child = command
        .spawn()
        .map_err(|error| format!("无法启动 FFmpeg 波形生成：{error}"))?;
    let (status, _, stderr) = collect_child_output(child, Duration::from_secs(10))
        .map_err(|_| "波形生成失败或超过 10 秒".to_string())?;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr)
            .trim()
            .chars()
            .take(240)
            .collect::<String>();
        return Err(if detail.is_empty() {
            "无法读取此音频的波形".into()
        } else {
            detail
        });
    }
    let metadata = fs::metadata(output).map_err(|_| "FFmpeg 未生成波形文件".to_string())?;
    if metadata.len() == 0 || metadata.len() > MAX_WAVEFORM_BYTES {
        return Err("波形文件超过大小限制".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(output)
        .map_err(|_| "无法打开波形文件".to_string())?
        .take(MAX_WAVEFORM_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取波形文件".to_string())?;
    if bytes.len() as u64 > MAX_WAVEFORM_BYTES || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("FFmpeg 返回的波形不是有效 PNG".into());
    }
    Ok(bytes)
}

fn waveform_one(ffmpeg: &Path, media: &MediaAsset) -> WaveformResult {
    let source = match source_path(media) {
        Ok(path) => path,
        Err((state, error)) => return result(media, state, Some(error), None),
    };
    let directory = std::env::temp_dir().join("kaicut-waveforms");
    if fs::create_dir_all(&directory).is_err() {
        return result(
            media,
            WaveformState::Failed,
            Some("无法创建波形临时目录".into()),
            None,
        );
    }
    let output = directory.join(format!("{}-waveform.png", media.id));
    let data = match generate(ffmpeg, &source, &output) {
        Ok(data) => data,
        Err(error) => return result(media, WaveformState::Failed, Some(error), None),
    };
    let _ = fs::remove_file(&output);
    result(
        media,
        WaveformState::Ready,
        None,
        Some(format!("data:image/png;base64,{}", STANDARD.encode(data))),
    )
}

#[tauri::command]
pub async fn media_waveform(app: AppHandle, media_id: String) -> Result<WaveformResult, String> {
    let session = crate::commands::with_session(app.clone(), |_, _| Ok(())).await?;
    let media = session
        .project
        .media_refs
        .into_iter()
        .find(|media| media.id == media_id)
        .ok_or("当前项目中找不到该素材")?;
    let Some(ffmpeg) = packaged_binary(&app, "ffmpeg") else {
        return Ok(result(
            &media,
            WaveformState::Failed,
            Some("尚未随应用打包 FFmpeg".into()),
            None,
        ));
    };
    tauri::async_runtime::spawn_blocking(move || Ok(waveform_one(&ffmpeg, &media)))
        .await
        .map_err(|error| format!("波形任务未能完成：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_without_path_is_missing_and_video_is_unsupported() {
        let audio = MediaAsset {
            id: "a".into(),
            name: "a.wav".into(),
            kind: MediaKind::Audio,
            extension: "wav".into(),
            size: 1,
            last_modified: 1,
            source_path: None,
        };
        assert_eq!(source_path(&audio).unwrap_err().0, WaveformState::Missing);
        let video = MediaAsset {
            id: "v".into(),
            name: "v.mp4".into(),
            kind: MediaKind::Video,
            extension: "mp4".into(),
            size: 1,
            last_modified: 1,
            source_path: None,
        };
        assert_eq!(
            source_path(&video).unwrap_err().0,
            WaveformState::Unsupported
        );
    }
}
