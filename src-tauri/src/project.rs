use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tempfile::NamedTempFile;
use uuid::Uuid;

pub const MAX_PROJECT_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_CLIPS: usize = 10_000;
/// The fixed primary video lane used by runtime magnet behavior. It is not persisted or configurable.
pub const PRIMARY_VIDEO_TRACK_ID: &str = "video-1";
const MAX_MEDIA_REFS: usize = 10_000;
pub const MEDIA_EXTENSIONS: &[&str] = &["mp4", "mov", "mp3", "wav", "png", "jpg", "jpeg"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaAsset {
    pub id: String,
    pub name: String,
    pub kind: MediaKind,
    pub extension: String,
    pub size: u64,
    pub last_modified: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimelineClip {
    pub id: String,
    pub media_id: String,
    pub track_id: String,
    pub start_frame: u64,
    pub duration_frames: u64,
    pub source_in_frame: u64,
    pub source_duration_frames: u64,
    /// Audio-only clip fade durations, expressed in the project timebase.
    #[serde(default)]
    pub fade_in_frames: u64,
    #[serde(default)]
    pub fade_out_frames: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineTrack {
    pub id: String,
    pub name: String,
    pub kind: TrackKind,
    pub locked: bool,
    pub muted: bool,
    /// Linear track gain. One is unity gain; the current editor clamps values to 0–1.
    #[serde(default = "default_track_volume")]
    pub volume: f32,
    pub clips: Vec<TimelineClip>,
}

pub fn default_track_volume() -> f32 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Timebase {
    pub fps_numerator: u32,
    pub fps_denominator: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KaicutProject {
    pub schema_version: u32,
    pub app_version: String,
    pub project_id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub timebase: Timebase,
    pub media_refs: Vec<MediaAsset>,
    pub tracks: Vec<TimelineTrack>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSession {
    pub project: KaicutProject,
    pub path: Option<String>,
    pub dirty: bool,
    pub status: String,
    pub revision: u64,
    pub can_undo: bool,
    pub can_redo: bool,
    #[serde(skip)]
    pub(crate) history: crate::timeline::TimelineHistory,
}

pub fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl Default for ProjectSession {
    fn default() -> Self {
        Self {
            project: KaicutProject::new(),
            path: None,
            dirty: false,
            status: "就绪 · 新建项目尚未保存".into(),
            revision: 0,
            can_undo: false,
            can_redo: false,
            history: Default::default(),
        }
    }
}

impl KaicutProject {
    pub fn new() -> Self {
        let now = timestamp();
        Self {
            schema_version: 2,
            app_version: env!("CARGO_PKG_VERSION").into(),
            project_id: format!("project-{}", Uuid::new_v4()),
            name: "未命名项目".into(),
            created_at: now.clone(),
            updated_at: now,
            timebase: Timebase {
                fps_numerator: 30,
                fps_denominator: 1,
            },
            media_refs: Vec::new(),
            tracks: [
                (PRIMARY_VIDEO_TRACK_ID, "视频 1", TrackKind::Video),
                ("video-2", "视频 2", TrackKind::Video),
                ("audio-1", "音频 1", TrackKind::Audio),
                ("audio-2", "音频 2", TrackKind::Audio),
            ]
            .into_iter()
            .map(|(id, name, kind)| TimelineTrack {
                id: id.into(),
                name: name.into(),
                kind,
                locked: false,
                muted: false,
                volume: default_track_volume(),
                clips: Vec::new(),
            })
            .collect(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 2 {
            return Err(format!(
                "不支持的项目版本：{}（当前支持版本 2）",
                self.schema_version
            ));
        }
        validate_text(&self.app_version, "应用版本", 64)?;
        validate_text(&self.project_id, "项目 ID", 128)?;
        validate_text(&self.name, "项目名称", 120)?;
        for value in [&self.created_at, &self.updated_at] {
            DateTime::parse_from_rfc3339(value).map_err(|_| "项目时间戳无效".to_string())?;
        }
        if self.timebase.fps_numerator == 0
            || self.timebase.fps_denominator == 0
            || self.timebase.fps_numerator > 1_000_000
            || self.timebase.fps_denominator > 1_000_000
            || f64::from(self.timebase.fps_numerator) / f64::from(self.timebase.fps_denominator)
                > 240.0
        {
            return Err("项目时间基无效：帧率必须大于 0 且不超过 240".into());
        }
        if self.max_timeline_frames() < 1 {
            return Err("项目帧率过低，24 小时内不足一帧".into());
        }
        if self.media_refs.len() > MAX_MEDIA_REFS {
            return Err("项目素材数量超过首版限制（10000 项）".into());
        }
        let mut ids = HashSet::new();
        for media in &self.media_refs {
            validate_text(&media.id, "素材 ID", 128)?;
            validate_text(&media.name, "素材名称", 1024)?;
            if !ids.insert(&media.id) {
                return Err("项目中有重复的素材 ID".into());
            }
            if media_kind(&media.extension) != Some(media.kind.clone()) {
                return Err(format!("素材类型或扩展名不受支持：{}", media.name));
            }
            if media.size > MAX_SAFE_INTEGER || media.last_modified > MAX_SAFE_INTEGER {
                return Err("素材元数据超出安全整数范围".into());
            }
            if let Some(path) = &media.source_path {
                validate_text(path, "素材路径", 32768)?;
            }
        }
        let media_by_id: HashMap<_, _> = self
            .media_refs
            .iter()
            .map(|media| (&media.id, media))
            .collect();
        let mut clip_ids = HashSet::new();
        let mut track_ids = HashSet::new();
        let mut video_count = 0;
        let mut audio_count = 0;
        for track in &self.tracks {
            validate_text(&track.id, "轨道 ID", 128)?;
            validate_text(&track.name, "轨道名称", 200)?;
            if !track_ids.insert(&track.id) {
                return Err("项目中有重复的轨道 ID".into());
            }
            if !track.volume.is_finite() || !(0.0..=1.0).contains(&track.volume) {
                return Err("音频轨音量必须在 0 到 1 之间".into());
            }
            if track.kind != TrackKind::Audio && track.volume != default_track_volume() {
                return Err("视频轨不能设置音频音量".into());
            }
            let mut ranges = Vec::with_capacity(track.clips.len());
            for clip in &track.clips {
                validate_text(&clip.id, "片段 ID", 128)?;
                if !clip_ids.insert(&clip.id) || clip_ids.len() > MAX_CLIPS {
                    return Err("片段 ID 重复或数量超过 10000 项".into());
                }
                let media = media_by_id
                    .get(&clip.media_id)
                    .ok_or("片段引用的素材不存在")?;
                if clip.track_id != track.id || !track_accepts(&track.kind, &media.kind) {
                    return Err("片段与轨道 ID 或类型不匹配".into());
                }
                if [
                    clip.start_frame,
                    clip.duration_frames,
                    clip.source_in_frame,
                    clip.source_duration_frames,
                ]
                .into_iter()
                .any(|value| value > MAX_SAFE_INTEGER)
                    || clip.duration_frames == 0
                    || clip.source_duration_frames == 0
                    || clip.source_duration_frames > self.max_timeline_frames()
                    || clip.fade_in_frames > clip.duration_frames
                    || clip.fade_out_frames > clip.duration_frames
                    || clip
                        .fade_in_frames
                        .checked_add(clip.fade_out_frames)
                        .is_none_or(|sum| sum > clip.duration_frames)
                {
                    return Err("片段时长无效或超过 24 小时限制".into());
                }
                let end = clip
                    .start_frame
                    .checked_add(clip.duration_frames)
                    .ok_or("片段时间溢出")?;
                let source_end = clip
                    .source_in_frame
                    .checked_add(clip.duration_frames)
                    .ok_or("片段源时间溢出")?;
                if end > self.max_timeline_frames() || source_end > clip.source_duration_frames {
                    return Err("片段超过源素材时长或 24 小时时间线限制".into());
                }
                ranges.push((clip.start_frame, end));
            }
            ranges.sort_unstable();
            if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
                return Err("同一轨道的片段不能重叠".into());
            }
            match track.kind {
                TrackKind::Video => video_count += 1,
                TrackKind::Audio => audio_count += 1,
            }
        }
        if video_count > 2 || audio_count > 2 || self.tracks.is_empty() {
            return Err("当前版本仅支持 2 条视频轨道和 2 条音频轨道".into());
        }
        Ok(())
    }

    pub fn max_timeline_frames(&self) -> u64 {
        86_400 * u64::from(self.timebase.fps_numerator)
            / u64::from(self.timebase.fps_denominator.max(1))
    }
}

pub fn track_accepts(track: &TrackKind, media: &MediaKind) -> bool {
    matches!(
        (track, media),
        (TrackKind::Video, MediaKind::Video | MediaKind::Image)
            | (TrackKind::Audio, MediaKind::Audio)
    )
}

impl ProjectSession {
    pub fn replace_project(&mut self, project: KaicutProject, path: Option<String>, dirty: bool) {
        self.project = project;
        self.path = path;
        self.dirty = dirty;
        self.history = Default::default();
        self.can_undo = false;
        self.can_redo = false;
        self.revision += 1;
    }
}

pub fn validate_text(value: &str, label: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().count() > max || value.chars().any(char::is_control)
    {
        return Err(format!(
            "{label}无效：不能为空、包含控制字符或超过 {max} 个字符"
        ));
    }
    Ok(())
}

pub fn parse_project(bytes: &[u8]) -> Result<KaicutProject, String> {
    if bytes.len() as u64 > MAX_PROJECT_BYTES {
        return Err("项目文件超过 10 MiB 限制".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| format!("项目文件不是有效 JSON：{error}"))?;
    parse_project_value(value)
}

pub(crate) fn parse_project_value(mut value: serde_json::Value) -> Result<KaicutProject, String> {
    match value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
    {
        Some(1) => {
            let tracks = value
                .get("tracks")
                .and_then(serde_json::Value::as_array)
                .ok_or("项目轨道结构无效")?;
            if tracks.iter().any(|track| {
                track
                    .get("clips")
                    .and_then(serde_json::Value::as_array)
                    .is_none_or(|clips| !clips.is_empty())
            }) {
                return Err("版本 1 仅允许升级空轨道，不能迁移已有剪辑片段".into());
            }
            value["schemaVersion"] = 2.into();
        }
        Some(2) => {}
        _ => return Err("不支持的项目版本；当前支持 schemaVersion 1（空轨迁移）和 2".into()),
    }
    let project: KaicutProject = serde_json::from_value(value)
        .map_err(|error| format!("项目结构无效或含当前版本不支持的字段：{error}"))?;
    project.validate()?;
    Ok(project)
}

pub fn read_project(path: &Path) -> Result<KaicutProject, String> {
    require_project_extension(path)?;
    let file = File::open(path).map_err(|error| format!("无法打开项目：{error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("无法读取项目属性：{error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_PROJECT_BYTES {
        return Err("项目必须是小于等于 10 MiB 的普通文件".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROJECT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取项目：{error}"))?;
    parse_project(&bytes)
}

pub fn save_project(path: &Path, project: &KaicutProject) -> Result<(), String> {
    require_project_extension(path)?;
    project.validate()?;
    let bytes =
        serde_json::to_vec_pretty(project).map_err(|error| format!("无法序列化项目：{error}"))?;
    if bytes.len() as u64 > MAX_PROJECT_BYTES {
        return Err("项目文件超过 10 MiB 限制".into());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| format!("无法在目标目录创建临时文件：{error}"))?;
    temporary
        .write_all(&bytes)
        .map_err(|error| format!("写入项目失败：{error}"))?;
    temporary
        .flush()
        .map_err(|error| format!("刷新项目文件失败：{error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("同步项目文件失败：{error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("替换项目文件失败，原文件已保留：{}", error.error))?;
    Ok(())
}

pub fn require_project_extension(path: &Path) -> Result<(), String> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("kaicut"))
    {
        Ok(())
    } else {
        Err("项目文件必须使用 .kaicut 扩展名".into())
    }
}

pub fn media_kind(extension: &str) -> Option<MediaKind> {
    match extension.to_ascii_lowercase().as_str() {
        "mp4" | "mov" => Some(MediaKind::Video),
        "mp3" | "wav" => Some(MediaKind::Audio),
        "png" | "jpg" | "jpeg" => Some(MediaKind::Image),
        _ => None,
    }
}

pub fn import_media(
    project: &mut KaicutProject,
    paths: Vec<PathBuf>,
) -> Result<(usize, usize), String> {
    let mut candidate = project.clone();
    let mut added = 0;
    let mut skipped = 0;
    let mut sources: HashSet<PathBuf> = project
        .media_refs
        .iter()
        .filter_map(|media| media.source_path.as_deref().map(PathBuf::from))
        .collect();
    for path in paths {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(kind) = media_kind(&extension) else {
            skipped += 1;
            continue;
        };
        let source = path
            .canonicalize()
            .map_err(|error| format!("无法访问素材 {}：{error}", path.display()))?;
        if !sources.insert(source.clone()) {
            skipped += 1;
            continue;
        }
        let metadata = source
            .metadata()
            .map_err(|error| format!("无法读取素材属性：{error}"))?;
        if !metadata.is_file() {
            skipped += 1;
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("素材文件名必须是有效 Unicode")?
            .to_string();
        let source_path = source
            .to_str()
            .ok_or("素材路径必须是有效 Unicode")?
            .to_string();
        let modified = metadata
            .modified()
            .map_err(|error| format!("无法读取素材修改时间：{error}"))?;
        let modified_millis = modified
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "素材修改时间早于 1970 年，当前不支持")?
            .as_millis();
        let last_modified =
            u64::try_from(modified_millis).map_err(|_| "素材修改时间超出支持范围")?;
        candidate.media_refs.push(MediaAsset {
            id: format!("media-{}", Uuid::new_v4()),
            name,
            kind,
            extension,
            size: metadata.len(),
            last_modified,
            source_path: Some(source_path),
        });
        added += 1;
    }
    if added > 0 {
        candidate.updated_at = timestamp();
        candidate.validate()?;
        *project = candidate;
    }
    Ok((added, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn roundtrip_preserves_unicode_metadata_and_source_paths() {
        let directory = tempfile::tempdir().unwrap();
        let media_path = directory.path().join("假期 片段.mp4");
        fs::write(&media_path, b"placeholder media bytes").unwrap();
        let mut project = KaicutProject::new();
        import_media(&mut project, vec![media_path]).unwrap();
        let path = directory.path().join("我的项目.kaicut");
        save_project(&path, &project).unwrap();
        assert_eq!(read_project(&path).unwrap(), project);
        assert!(project.media_refs[0].source_path.is_some());
    }

    #[test]
    fn rejects_corrupt_future_unknown_and_nonempty_clip_projects() {
        assert!(parse_project(b"{broken").is_err());
        let original = serde_json::to_value(KaicutProject::new()).unwrap();
        let mut future = original.clone();
        future["schemaVersion"] = 3.into();
        assert!(
            parse_project(&serde_json::to_vec(&future).unwrap())
                .unwrap_err()
                .contains("版本")
        );
        let mut unknown = original.clone();
        unknown["futureEditingData"] = true.into();
        assert!(parse_project(&serde_json::to_vec(&unknown).unwrap()).is_err());
        let mut unknown_nested = original.clone();
        unknown_nested["timebase"]["dropFrame"] = true.into();
        assert!(parse_project(&serde_json::to_vec(&unknown_nested).unwrap()).is_err());
        let mut clips = original;
        clips["schemaVersion"] = 1.into();
        clips["tracks"][0]["clips"] = serde_json::json!([{"id":"clip-1","mediaId":"media-1","trackId":"video-1","startFrame":0,"durationFrames":30}]);
        assert!(
            parse_project(&serde_json::to_vec(&clips).unwrap())
                .unwrap_err()
                .contains("剪辑片段")
        );
    }

    #[test]
    fn rejects_invalid_timebase_duplicate_ids_and_oversized_files() {
        let mut project = KaicutProject::new();
        project.timebase.fps_denominator = 0;
        assert!(project.validate().is_err());
        project.timebase.fps_denominator = 1;
        project.tracks[1].id = project.tracks[0].id.clone();
        assert!(project.validate().is_err());
        assert!(parse_project(&vec![b' '; MAX_PROJECT_BYTES as usize + 1]).is_err());
    }

    #[test]
    fn primary_video_track_is_a_fixed_runtime_convention() {
        let project = KaicutProject::new();
        let primary = project
            .tracks
            .iter()
            .find(|track| track.id == PRIMARY_VIDEO_TRACK_ID)
            .expect("new projects must contain the primary video track");
        assert_eq!(primary.kind, TrackKind::Video);
        assert_eq!(PRIMARY_VIDEO_TRACK_ID, "video-1");
        assert_eq!(project.schema_version, 2);
    }

    #[test]
    fn failed_save_preserves_existing_file_and_successful_save_replaces_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.kaicut");
        let mut project = KaicutProject::new();
        save_project(&path, &project).unwrap();
        let original = fs::read(&path).unwrap();
        project.name.clear();
        assert!(save_project(&path, &project).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        project.name = "更新后的项目".into();
        save_project(&path, &project).unwrap();
        assert_eq!(read_project(&path).unwrap().name, "更新后的项目");
        assert!(
            save_project(
                &directory.path().join("missing").join("file.kaicut"),
                &project
            )
            .is_err()
        );
        let blocked = directory.path().join("folder.kaicut");
        fs::create_dir(&blocked).unwrap();
        assert!(save_project(&blocked, &project).is_err());
        assert!(blocked.is_dir());
    }

    #[test]
    fn import_supports_requested_types_skips_duplicates_and_is_transactional() {
        let directory = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for extension in ["mp4", "MOV", "mp3", "wav", "png", "jpg", "jpeg", "txt"] {
            let path = directory.path().join(format!("file.{extension}"));
            fs::write(&path, b"test").unwrap();
            paths.push(path);
        }
        paths.push(paths[0].clone());
        let mut project = KaicutProject::new();
        assert_eq!(import_media(&mut project, paths).unwrap(), (7, 2));
        assert_eq!(
            project
                .media_refs
                .iter()
                .filter(|asset| asset.kind == MediaKind::Video)
                .count(),
            2
        );
        let previous = project.clone();
        let valid = directory.path().join("another.mp4");
        fs::write(&valid, b"test").unwrap();
        assert!(
            import_media(
                &mut project,
                vec![valid, directory.path().join("missing.mov")]
            )
            .is_err()
        );
        assert_eq!(project, previous);
    }
}
