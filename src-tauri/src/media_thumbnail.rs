use crate::{
    media_engine::{collect_child_output, hide_console, packaged_binary},
    project::{MediaAsset, MediaKind},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager};

const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 512;
const MAX_EDGE: u32 = 960;
const CONVERSION_VERSION: &str = "first-frame-rgba-960-v1";
// Serialize cache pruning and decoding; a large import must not launch a decoder per asset.
static THUMBNAIL_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThumbnailState {
    Ready,
    Failed,
    Missing,
    Changed,
    Unsupported,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailResult {
    media_id: String,
    state: ThumbnailState,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ThumbnailResult {
    fn error(media: &MediaAsset, state: ThumbnailState, error: impl Into<String>) -> Self {
        Self {
            media_id: media.id.clone(),
            state,
            data_url: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, PartialEq)]
struct SourceIdentity {
    path: PathBuf,
    size: u64,
    modified: Duration,
}

fn source_identity(media: &MediaAsset) -> Result<SourceIdentity, ThumbnailResult> {
    let missing = || ThumbnailResult::error(media, ThumbnailState::Missing, "找不到原始素材");
    let path = media.source_path.as_deref().ok_or_else(missing)?;
    let path = Path::new(path).canonicalize().map_err(|_| missing())?;
    let metadata = path.metadata().map_err(|_| missing())?;
    if !metadata.is_file() {
        return Err(missing());
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .ok_or_else(|| {
            ThumbnailResult::error(media, ThumbnailState::Failed, "无法读取素材修改时间")
        })?;
    if metadata.len() != media.size || modified.as_millis() != u128::from(media.last_modified) {
        return Err(ThumbnailResult::error(
            media,
            ThumbnailState::Changed,
            "原始素材已更改，请重新链接后生成缩略图",
        ));
    }
    Ok(SourceIdentity {
        path,
        size: metadata.len(),
        modified,
    })
}

fn cache_key(source: &SourceIdentity) -> String {
    let mut hash = Sha256::new();
    hash.update(CONVERSION_VERSION);
    hash.update([0]);
    hash.update(source.path.as_os_str().as_encoded_bytes());
    hash.update([0]);
    hash.update(source.size.to_le_bytes());
    hash.update(source.modified.as_nanos().to_le_bytes());
    format!("{:x}.png", hash.finalize())
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 33
        || bytes.get(..8)? != b"\x89PNG\r\n\x1a\n"
        || bytes.get(8..16)? != b"\0\0\0\rIHDR"
        || !bytes.ends_with(b"\0\0\0\0IEND\xaeB`\x82")
    {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    (width > 0 && height > 0 && width <= MAX_EDGE && height <= MAX_EDGE).then_some((width, height))
}

fn read_png(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "无法读取缩略图缓存")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_IMAGE_BYTES
    {
        return Err("缩略图缓存不是有效的限额内文件".into());
    }
    let file = fs::File::open(path).map_err(|_| "无法打开缩略图缓存")?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取缩略图缓存")?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES || png_dimensions(&bytes).is_none() {
        return Err("缩略图缓存格式或尺寸无效".into());
    }
    Ok(bytes)
}

fn square_pixel_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    // Scale already uses display aspect ratio. Omit physical pixel proportions so consumers
    // cannot apply the source SAR a second time. This leaves all pixel/alpha chunks unchanged.
    let mut result = bytes.get(..8).ok_or("PNG 数据不完整")?.to_vec();
    let mut offset = 8;
    while offset < bytes.len() {
        let length = bytes.get(offset..offset + 4).ok_or("PNG 数据不完整")?;
        let length = u32::from_be_bytes(length.try_into().map_err(|_| "PNG 数据不完整")?) as usize;
        let end = offset
            .checked_add(length)
            .and_then(|value| value.checked_add(12))
            .ok_or("PNG 数据过长")?;
        let chunk = bytes.get(offset..end).ok_or("PNG 数据不完整")?;
        if &chunk[4..8] != b"pHYs" {
            result.extend_from_slice(chunk);
        }
        offset = end;
    }
    Ok(result)
}

fn prepare_cache(directory: &Path) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|_| "无法创建缩略图缓存目录")?;
    if fs::symlink_metadata(directory)
        .map_err(|_| "无法读取缩略图缓存目录")?
        .file_type()
        .is_symlink()
    {
        return Err("缩略图缓存目录不能是符号链接".into());
    }
    Ok(())
}

fn prune_cache(directory: &Path, reserve: u64) -> Result<(), String> {
    let mut files = Vec::new();
    let mut total = 0_u64;
    for entry in fs::read_dir(directory).map_err(|_| "无法枚举缩略图缓存")? {
        let entry = entry.map_err(|_| "无法读取缩略图缓存项")?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| "无法读取缩略图缓存属性")?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        if name.starts_with(".thumbnail-") {
            if SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default()
                > Duration::from_secs(3600)
            {
                let _ = fs::remove_file(entry.path());
            }
            continue;
        }
        if name.len() == 68
            && name.ends_with(".png")
            && name.as_bytes()[..64].iter().all(u8::is_ascii_hexdigit)
        {
            total = total.saturating_add(metadata.len());
            files.push((modified, entry.path(), metadata.len()));
        }
    }
    files.sort_by_key(|file| file.0);
    let mut count = files.len();
    for (_, path, size) in files {
        if total.saturating_add(reserve) <= MAX_CACHE_BYTES && count < MAX_CACHE_FILES {
            break;
        }
        fs::remove_file(path).map_err(|_| "无法清理缩略图缓存")?;
        total = total.saturating_sub(size);
        count -= 1;
    }
    Ok(())
}

fn decode(ffmpeg: &Path, source: &Path, image: bool, output: &Path) -> Result<(), String> {
    let mut command = Command::new(ffmpeg);
    command.args([
        "-v",
        "error",
        "-nostdin",
        "-y",
        "-max_alloc",
        "67108864",
        "-protocol_whitelist",
        "file",
        "-format_whitelist",
        "mov,mp4,m4a,3gp,3g2,mj2,mp3,wav,image2,image2pipe",
        "-threads",
        "1",
        "-filter_threads",
        "1",
    ]);
    if image {
        // Literal image paths may contain '%' or consecutive numbers; never expand a sequence.
        command.args(["-f", "image2", "-pattern_type", "none"]);
    }
    command
        .arg("-i")
        .arg(source)
        .args([
            "-map",
            "0:V:0",
            "-frames:v",
            "1",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            "scale=w='max(1,trunc(min(960,min(ih*dar,960*dar))))':h='max(1,trunc(min(960,min(ih,960/dar))))'",
            "-c:v",
            "png",
            "-pix_fmt",
            "rgba",
            "-threads",
            "1",
            "-fs",
            "4194304",
            "-f",
            "image2",
            "-update",
            "1",
        ])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    let child = command
        .spawn()
        .map_err(|error| format!("无法启动 FFmpeg 缩略图生成：{error}"))?;
    let (status, _, stderr) = collect_child_output(child, Duration::from_secs(10))
        .map_err(|_| "缩略图生成失败或超过 10 秒，请检查素材是否损坏".to_string())?;
    if !status.success() {
        log::warn!(
            "Thumbnail decode failed: {}",
            String::from_utf8_lossy(&stderr)
                .chars()
                .take(500)
                .collect::<String>()
        );
        return Err("无法解码此素材的首帧，文件可能损坏或编码暂不支持".into());
    }
    Ok(())
}

fn thumbnail_one(ffmpeg: &Path, directory: &Path, media: &MediaAsset) -> ThumbnailResult {
    let failure = |message| ThumbnailResult::error(media, ThumbnailState::Failed, message);
    let source = match source_identity(media) {
        Ok(source) => source,
        Err(error) => return error,
    };
    if media.kind == MediaKind::Audio {
        return ThumbnailResult::error(
            media,
            ThumbnailState::Unsupported,
            "音频素材不生成画面缩略图",
        );
    }
    let Ok(_guard) = THUMBNAIL_LOCK.lock() else {
        return failure("缩略图缓存暂不可用".into());
    };
    let result = (|| -> Result<Vec<u8>, String> {
        prepare_cache(directory)?;
        let path = directory.join(cache_key(&source));
        if let Ok(bytes) = read_png(&path) {
            return Ok(bytes);
        }
        prune_cache(directory, MAX_IMAGE_BYTES)?;
        let temporary = tempfile::Builder::new()
            .prefix(".thumbnail-")
            .tempfile_in(directory)
            .map_err(|_| "无法创建缩略图临时文件")?
            .into_temp_path();
        decode(
            ffmpeg,
            &source.path,
            media.kind == MediaKind::Image,
            &temporary,
        )?;
        let bytes = square_pixel_png(&read_png(&temporary)?)?;
        if source_identity(media).ok().as_ref() != Some(&source) {
            return Err("生成期间素材已更改，未写入缓存".into());
        }
        fs::write(&temporary, &bytes).map_err(|_| "无法写入缩略图缓存")?;
        fs::OpenOptions::new()
            .write(true)
            .open(&temporary)
            .and_then(|file| file.sync_all())
            .map_err(|_| "无法写入缩略图缓存")?;
        temporary.persist(&path).map_err(|_| "无法保存缩略图缓存")?;
        Ok(bytes)
    })();
    // Check after cache access/decoding as well: do not publish frames from a replaced source.
    match source_identity(media) {
        Ok(current) if current == source => {}
        Ok(_) => {
            return ThumbnailResult::error(
                media,
                ThumbnailState::Changed,
                "生成期间素材已更改，请重新链接",
            );
        }
        Err(error) => return error,
    }
    match result {
        Ok(bytes) => ThumbnailResult {
            media_id: media.id.clone(),
            state: ThumbnailState::Ready,
            data_url: Some(format!("data:image/png;base64,{}", STANDARD.encode(bytes))),
            error: None,
        },
        Err(error) => failure(error),
    }
}

#[tauri::command]
pub async fn media_thumbnail(app: AppHandle, media_id: String) -> Result<ThumbnailResult, String> {
    let session = crate::commands::with_session(app.clone(), |_, _| Ok(())).await?;
    let media = session
        .project
        .media_refs
        .into_iter()
        .find(|media| media.id == media_id)
        .ok_or("当前项目中找不到该素材")?;
    let Some(ffmpeg) = packaged_binary(&app, "ffmpeg") else {
        return Ok(ThumbnailResult::error(
            &media,
            ThumbnailState::Failed,
            "尚未随应用打包 FFmpeg，无法生成缩略图",
        ));
    };
    let directory = app
        .path()
        .app_cache_dir()
        .map_err(|_| "无法定位应用缓存目录")?
        .join("thumbnails");
    tauri::async_runtime::spawn_blocking(move || thumbnail_one(&ffmpeg, &directory, &media))
        .await
        .map_err(|error| format!("缩略图任务未能完成：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSPARENT_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DAAAAEAQEARwbK3gAAAABJRU5ErkJggg==";

    fn asset(path: &Path, kind: MediaKind) -> MediaAsset {
        let metadata = path.metadata().unwrap();
        MediaAsset {
            id: "media-thumbnail-test".into(),
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            kind,
            extension: path.extension().unwrap().to_string_lossy().into_owned(),
            size: metadata.len(),
            last_modified: metadata
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            source_path: Some(path.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn thumbnail_handles_missing_changed_and_audio_without_decoding() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("音频 test.wav");
        fs::write(&path, b"placeholder").unwrap();
        let media = asset(&path, MediaKind::Audio);
        let cache = directory.path().join("cache");
        let ffmpeg = Path::new("intentionally-absent");
        assert_eq!(
            thumbnail_one(ffmpeg, &cache, &media).state,
            ThumbnailState::Unsupported
        );
        fs::write(&path, b"changed").unwrap();
        assert_eq!(
            thumbnail_one(ffmpeg, &cache, &media).state,
            ThumbnailState::Changed
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(
            thumbnail_one(ffmpeg, &cache, &media).state,
            ThumbnailState::Missing
        );
        assert!(!cache.exists());
    }

    #[test]
    fn cache_key_changes_with_path_size_and_submillisecond_time() {
        let mut source = SourceIdentity {
            path: PathBuf::from("C:/素材 test.mp4"),
            size: 100,
            modified: Duration::from_millis(1234),
        };
        let first = cache_key(&source);
        source.size += 1;
        assert_ne!(first, cache_key(&source));
        source.size -= 1;
        source.modified += Duration::from_nanos(1);
        assert_ne!(first, cache_key(&source));
        source.modified -= Duration::from_nanos(1);
        source.path = PathBuf::from("C:/other.mp4");
        assert_ne!(first, cache_key(&source));
        assert_eq!(first.len(), 68);
    }

    #[test]
    fn cache_reads_only_valid_bounded_pngs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.png");
        let bytes = STANDARD.decode(TRANSPARENT_PNG).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(read_png(&path).unwrap(), bytes);
        let mut oversized = bytes;
        oversized[16..20].copy_from_slice(&961_u32.to_be_bytes());
        fs::write(&path, oversized).unwrap();
        assert!(read_png(&path).is_err());
        fs::write(&path, b"not a PNG").unwrap();
        assert!(read_png(&path).is_err());
        fs::File::create(&path)
            .unwrap()
            .set_len(MAX_IMAGE_BYTES + 1)
            .unwrap();
        assert!(read_png(&path).is_err());
    }

    #[test]
    fn normalizes_png_pixels_without_changing_color_or_alpha_chunks() {
        let original = STANDARD.decode(TRANSPARENT_PNG).unwrap();
        let mut with_density = original[..33].to_vec();
        with_density.extend_from_slice(b"\0\0\0\x09pHYs\0\0\0\x04\0\0\0\x03\0\0\0\0\0");
        with_density.extend_from_slice(&original[33..]);
        assert_eq!(square_pixel_png(&with_density).unwrap(), original);
        assert!(square_pixel_png(&with_density[..40]).is_err());
    }

    #[test]
    fn prunes_owned_cache_files_with_room_for_next_image() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..33 {
            fs::File::create(directory.path().join(format!("{index:064x}.png")))
                .unwrap()
                .set_len(MAX_IMAGE_BYTES)
                .unwrap();
        }
        let unrelated = directory.path().join("do-not-remove.txt");
        fs::write(&unrelated, b"unrelated").unwrap();
        // A foreign non-ASCII name must not panic when the owned-name predicate is applied.
        fs::write(
            directory.path().join(format!("{}a.png", "中".repeat(21))),
            b"foreign",
        )
        .unwrap();
        prune_cache(directory.path(), MAX_IMAGE_BYTES).unwrap();
        let total: u64 = fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_string_lossy().len() == 68
                    && entry.file_name().to_string_lossy().starts_with('0')
            })
            .map(|entry| entry.metadata().unwrap().len())
            .sum();
        assert!(total + MAX_IMAGE_BYTES <= MAX_CACHE_BYTES);
        assert!(unrelated.exists());
    }

    #[test]
    #[ignore = "Requires the audited Windows x64 sidecar and KAICUT_THUMBNAIL_TEST_VIDEO pointing to a local MP4 fixture"]
    fn real_sidecar_thumbnail_matrix_and_cache_invalidation() {
        let ffmpeg = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/ffmpeg-x86_64-pc-windows-msvc.exe");
        assert!(ffmpeg.is_file(), "Audited FFmpeg sidecar is required");
        let fixture = std::env::var_os("KAICUT_THUMBNAIL_TEST_VIDEO")
            .expect("Set KAICUT_THUMBNAIL_TEST_VIDEO");
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("cache");
        let video_path = directory.path().join("中文 视频 01.mp4");
        fs::copy(fixture, &video_path).unwrap();
        let video = asset(&video_path, MediaKind::Video);
        let result = thumbnail_one(&ffmpeg, &cache, &video);
        assert_eq!(result.state, ThumbnailState::Ready, "{:?}", result.error);
        let data = STANDARD
            .decode(
                result
                    .data_url
                    .as_ref()
                    .unwrap()
                    .strip_prefix("data:image/png;base64,")
                    .unwrap(),
            )
            .unwrap();
        let (width, height) = png_dimensions(&data).unwrap();
        assert!(width <= MAX_EDGE && height <= MAX_EDGE);
        let cache_path = cache.join(cache_key(&source_identity(&video).unwrap()));
        let modified = cache_path.metadata().unwrap().modified().unwrap();
        // A cache hit must work even with no executable available and must not rewrite the file.
        assert_eq!(
            thumbnail_one(Path::new("intentionally-absent"), &cache, &video).data_url,
            result.data_url
        );
        assert_eq!(cache_path.metadata().unwrap().modified().unwrap(), modified);

        let rotated_path = directory.path().join("旋转 视频.mov");
        let mut remux = Command::new(&ffmpeg);
        remux
            .args([
                "-v",
                "error",
                "-nostdin",
                "-y",
                "-display_rotation",
                "90",
                "-i",
            ])
            .arg(&video_path)
            .args(["-map", "0:v:0", "-c", "copy"])
            .arg(&rotated_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console(&mut remux);
        let (status, _, error) =
            collect_child_output(remux.spawn().unwrap(), Duration::from_secs(10)).unwrap();
        assert!(status.success(), "{}", String::from_utf8_lossy(&error));
        let rotated = thumbnail_one(&ffmpeg, &cache, &asset(&rotated_path, MediaKind::Video));
        assert_eq!(rotated.state, ThumbnailState::Ready, "{:?}", rotated.error);
        let rotated_bytes = STANDARD
            .decode(
                rotated
                    .data_url
                    .unwrap()
                    .strip_prefix("data:image/png;base64,")
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            png_dimensions(&rotated_bytes),
            Some((height, width)),
            "Display rotation must be applied before publishing the first frame"
        );

        let anamorphic_path = directory.path().join("非方形像素 视频.mov");
        let mut remux = Command::new(&ffmpeg);
        remux
            .args(["-v", "error", "-nostdin", "-y", "-i"])
            .arg(&video_path)
            .args(["-map", "0:v:0", "-c", "copy", "-aspect", "16:9"])
            .arg(&anamorphic_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console(&mut remux);
        let (status, _, error) =
            collect_child_output(remux.spawn().unwrap(), Duration::from_secs(10)).unwrap();
        assert!(status.success(), "{}", String::from_utf8_lossy(&error));
        let anamorphic = thumbnail_one(&ffmpeg, &cache, &asset(&anamorphic_path, MediaKind::Video));
        assert_eq!(
            anamorphic.state,
            ThumbnailState::Ready,
            "{:?}",
            anamorphic.error
        );
        let anamorphic_bytes = STANDARD
            .decode(
                anamorphic
                    .data_url
                    .unwrap()
                    .strip_prefix("data:image/png;base64,")
                    .unwrap(),
            )
            .unwrap();
        let (width, height) = png_dimensions(&anamorphic_bytes).unwrap();
        assert!(
            (f64::from(width) / f64::from(height) - 16.0 / 9.0).abs() <= 1.0 / f64::from(height)
        );
        assert_eq!(
            square_pixel_png(&anamorphic_bytes).unwrap(),
            anamorphic_bytes,
            "Cached PNG must omit source pixel proportions"
        );

        let image_path = directory.path().join("透明 图片 %03d.png");
        fs::write(&image_path, STANDARD.decode(TRANSPARENT_PNG).unwrap()).unwrap();
        let image = asset(&image_path, MediaKind::Image);
        let image_result = thumbnail_one(&ffmpeg, &cache, &image);
        assert_eq!(
            image_result.state,
            ThumbnailState::Ready,
            "{:?}",
            image_result.error
        );
        let image_bytes = STANDARD
            .decode(
                image_result
                    .data_url
                    .unwrap()
                    .strip_prefix("data:image/png;base64,")
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(png_dimensions(&image_bytes), Some((1, 1)));
        assert_eq!(
            image_bytes[25], 6,
            "PNG output must retain an RGBA alpha channel"
        );

        fs::write(&video_path, b"changed and damaged file").unwrap();
        assert_eq!(
            thumbnail_one(&ffmpeg, &cache, &video).state,
            ThumbnailState::Changed
        );
        let relinked = asset(&video_path, MediaKind::Video);
        assert_ne!(
            cache_key(&source_identity(&relinked).unwrap()),
            cache_path.file_name().unwrap().to_str().unwrap()
        );
        let failed = thumbnail_one(&ffmpeg, &cache, &relinked);
        assert_eq!(failed.state, ThumbnailState::Failed);
        assert!(failed.data_url.is_none());
        fs::remove_file(&video_path).unwrap();
        assert_eq!(
            thumbnail_one(&ffmpeg, &cache, &relinked).state,
            ThumbnailState::Missing
        );
        let audio_path = directory.path().join("中文 音频.wav");
        fs::write(&audio_path, b"audio placeholder").unwrap();
        assert_eq!(
            thumbnail_one(&ffmpeg, &cache, &asset(&audio_path, MediaKind::Audio)).state,
            ThumbnailState::Unsupported
        );
        assert!(
            !fs::read_dir(cache)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".thumbnail-"))
        );
    }
}
