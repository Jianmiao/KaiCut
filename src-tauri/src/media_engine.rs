use serde::Serialize;
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

#[derive(Serialize)]
pub struct MediaEngineStatus {
    available: bool,
    version: Option<String>,
    message: String,
}

fn unavailable(message: impl Into<String>) -> MediaEngineStatus {
    MediaEngineStatus {
        available: false,
        version: None,
        message: message.into(),
    }
}

fn packaged_ffmpeg(app: &AppHandle) -> Option<PathBuf> {
    let filename = if cfg!(target_os = "windows") {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join(filename));
            candidates.push(directory.join("binaries").join(filename));
        }
    }
    if let Ok(resources) = app.path().resource_dir() {
        candidates.push(resources.join(filename));
        candidates.push(resources.join("binaries").join(filename));
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn probe(app: AppHandle) -> MediaEngineStatus {
    let Some(path) = packaged_ffmpeg(&app) else {
        return unavailable(
            "尚未随应用打包 FFmpeg；当前仅支持素材元数据导入，预览和导出仍为占位。",
        );
    };
    let mut command = Command::new(path);
    command
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
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
                    return MediaEngineStatus {
                        available: true,
                        version: Some(version.chars().take(300).collect()),
                        message: "FFmpeg sidecar 已就绪；此阶段尚未实现解码、预览或导出。".into(),
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

#[tauri::command]
pub async fn media_engine_status(app: AppHandle) -> Result<MediaEngineStatus, String> {
    tauri::async_runtime::spawn_blocking(move || probe(app))
        .await
        .map_err(|error| format!("媒体引擎状态检查失败：{error}"))
}
