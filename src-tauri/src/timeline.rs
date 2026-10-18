use crate::{
    commands::with_session,
    media_engine::{MediaProbeState, packaged_binary, probe_one},
    project::{
        KaicutProject, MAX_SAFE_INTEGER, MediaAsset, MediaKind, ProjectSession, TimelineClip,
        TimelineTrack, timestamp, track_accepts,
    },
};
use serde::Deserialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};
use tauri::AppHandle;
use uuid::Uuid;

const MAX_HISTORY_STEPS: usize = 100;
const MAX_HISTORY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineHistory {
    undo: Vec<Vec<TimelineTrack>>,
    redo: Vec<Vec<TimelineTrack>>,
}

fn snapshot_bytes(tracks: &[TimelineTrack]) -> usize {
    tracks
        .iter()
        .map(|track| {
            std::mem::size_of::<TimelineTrack>()
                + track.id.capacity()
                + track.name.capacity()
                + track.clips.capacity() * std::mem::size_of::<TimelineClip>()
                + track
                    .clips
                    .iter()
                    .map(|clip| {
                        clip.id.capacity() + clip.track_id.capacity() + clip.media_id.capacity()
                    })
                    .sum::<usize>()
        })
        .sum()
}

impl TimelineHistory {
    fn bound(&mut self) {
        while self.undo.len() + self.redo.len() > MAX_HISTORY_STEPS
            || self
                .undo
                .iter()
                .chain(&self.redo)
                .map(|tracks| snapshot_bytes(tracks))
                .sum::<usize>()
                > MAX_HISTORY_BYTES
        {
            if !self.undo.is_empty() {
                self.undo.remove(0);
            } else if !self.redo.is_empty() {
                self.redo.remove(0);
            } else {
                break;
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TimelineEdit {
    Add {
        media_id: String,
        track_id: String,
    },
    Split {
        clip_id: String,
        frame: u64,
    },
    Delete {
        clip_ids: Vec<String>,
    },
    RippleDelete {
        clip_ids: Vec<String>,
    },
    Duplicate {
        clip_id: String,
    },
    Move {
        clip_id: String,
        track_id: String,
        start_frame: u64,
    },
    MoveMany {
        moves: Vec<ClipMove>,
    },
    Trim {
        clip_id: String,
        source_in_frame: u64,
        start_frame: u64,
        duration_frames: u64,
    },
    TrackVolume {
        track_id: String,
        volume: f32,
    },
    ClipFade {
        clip_id: String,
        fade_in_frames: u64,
        fade_out_frames: u64,
    },
    TrackState {
        track_id: String,
        locked: bool,
        muted: bool,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipMove {
    pub clip_id: String,
    pub track_id: String,
    pub start_frame: u64,
}

fn check_revision(session: &ProjectSession, project_id: &str, revision: u64) -> Result<(), String> {
    if session.project.project_id != project_id || session.revision != revision {
        return Err("项目状态已更新，请重试当前操作".into());
    }
    if revision >= MAX_SAFE_INTEGER {
        return Err("项目修订号已超过支持范围，请重新打开项目".into());
    }
    Ok(())
}

fn track_index(project: &KaicutProject, id: &str) -> Result<usize, String> {
    project
        .tracks
        .iter()
        .position(|track| track.id == id)
        .ok_or_else(|| "找不到轨道".into())
}

fn clip_index(project: &KaicutProject, id: &str) -> Result<(usize, usize), String> {
    project
        .tracks
        .iter()
        .enumerate()
        .find_map(|(t, track)| {
            track
                .clips
                .iter()
                .position(|clip| clip.id == id)
                .map(|c| (t, c))
        })
        .ok_or_else(|| "找不到片段".into())
}

fn require_unlocked(track: &TimelineTrack) -> Result<(), String> {
    if track.locked {
        Err("轨道已锁定，请先解锁".into())
    } else {
        Ok(())
    }
}

fn track_end(track: &TimelineTrack) -> u64 {
    track
        .clips
        .iter()
        .map(|clip| clip.start_frame + clip.duration_frames)
        .max()
        .unwrap_or(0)
}

fn validate_clip_ids(
    project: &KaicutProject,
    clip_ids: &[String],
) -> Result<HashSet<String>, String> {
    if clip_ids.is_empty() || clip_ids.len() > crate::project::MAX_CLIPS {
        return Err("请选择一个或多个片段".into());
    }
    let ids: HashSet<_> = clip_ids.iter().cloned().collect();
    if ids.len() != clip_ids.len() {
        return Err("片段列表含重复 ID".into());
    }
    for id in clip_ids {
        let (track, _) = clip_index(project, id)?;
        require_unlocked(&project.tracks[track])?;
    }
    Ok(ids)
}

#[derive(PartialEq)]
struct SourceIdentity {
    path: PathBuf,
    size: u64,
    modified: Duration,
}

fn source_identity(media: &MediaAsset) -> Result<SourceIdentity, String> {
    let path = Path::new(media.source_path.as_deref().ok_or("素材尚未链接")?)
        .canonicalize()
        .map_err(|_| "找不到原始素材")?;
    let metadata = path.metadata().map_err(|_| "无法读取素材属性")?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .ok_or("无法读取素材修改时间")?;
    if !metadata.is_file()
        || metadata.len() != media.size
        || modified.as_millis() != u128::from(media.last_modified)
    {
        return Err("素材文件已更改，请重新链接后再添加".into());
    }
    Ok(SourceIdentity {
        path,
        size: metadata.len(),
        modified,
    })
}

pub(crate) fn source_frames(
    ffprobe: &PathBuf,
    media: &MediaAsset,
    project: &KaicutProject,
) -> Result<u64, String> {
    let identity = source_identity(media)?;
    let probe = probe_one(ffprobe, media);
    if probe.state != MediaProbeState::Ready {
        return Err(probe.error.unwrap_or_else(|| "无法识别素材时长".into()));
    }
    if source_identity(media)? != identity {
        return Err("探测期间素材发生变化，请重试".into());
    }
    let compatible = match media.kind {
        MediaKind::Video | MediaKind::Image => {
            probe.width.is_some_and(|width| width > 0)
                && probe.height.is_some_and(|height| height > 0)
        }
        MediaKind::Audio => probe.audio_channels.is_some_and(|channels| channels > 0),
    };
    if !compatible {
        return Err("文件实际媒体流与导入类型不匹配".into());
    }
    let milliseconds = if media.kind == MediaKind::Image {
        5_000
    } else {
        probe
            .duration_ms
            .ok_or("无法确定素材时长，暂不能添加到时间线")?
    };
    let frames = u128::from(milliseconds) * u128::from(project.timebase.fps_numerator)
        / (1_000 * u128::from(project.timebase.fps_denominator));
    if frames == 0 || frames > u128::from(project.max_timeline_frames()) {
        return Err("素材时长必须至少一帧且不超过 24 小时".into());
    }
    Ok(frames as u64)
}

fn apply_edit(
    project: &mut KaicutProject,
    edit: TimelineEdit,
    add_duration: Option<u64>,
) -> Result<(), String> {
    match edit {
        TimelineEdit::Add { media_id, track_id } => {
            let t = track_index(project, &track_id)?;
            let media = project
                .media_refs
                .iter()
                .find(|media| media.id == media_id)
                .ok_or("找不到素材")?;
            let track = &mut project.tracks[t];
            require_unlocked(track)?;
            if !track_accepts(&track.kind, &media.kind) {
                return Err("素材类型与轨道不匹配".into());
            }
            let duration_frames = add_duration.ok_or("素材时长尚未确认")?;
            let source_duration_frames = if media.kind == MediaKind::Image {
                86_400 * u64::from(project.timebase.fps_numerator)
                    / u64::from(project.timebase.fps_denominator)
            } else {
                duration_frames
            };
            track.clips.push(TimelineClip {
                id: format!("clip-{}", Uuid::new_v4()),
                media_id,
                track_id,
                start_frame: track_end(track),
                duration_frames,
                source_in_frame: 0,
                source_duration_frames,
                fade_in_frames: 0,
                fade_out_frames: 0,
            });
        }
        TimelineEdit::Split { clip_id, frame } => {
            let (t, c) = clip_index(project, &clip_id)?;
            let track = &mut project.tracks[t];
            require_unlocked(track)?;
            let clip = &mut track.clips[c];
            let end = clip.start_frame + clip.duration_frames;
            if frame <= clip.start_frame || frame >= end {
                return Err("播放头必须位于片段内部才能分割".into());
            }
            let offset = frame - clip.start_frame;
            let mut right = clip.clone();
            right.id = format!("clip-{}", Uuid::new_v4());
            right.start_frame = frame;
            right.duration_frames = end - frame;
            right.source_in_frame += offset;
            clip.duration_frames = offset;
            // Fades belong to the original outer edges. Re-anchor and clamp
            // those edges when cutting inside a fade; never create a new fade
            // at either side of the internal cut.
            clip.fade_in_frames = clip.fade_in_frames.min(clip.duration_frames);
            clip.fade_out_frames = 0;
            right.fade_in_frames = 0;
            right.fade_out_frames = right.fade_out_frames.min(right.duration_frames);
            track.clips.insert(c + 1, right);
        }
        TimelineEdit::Delete { clip_ids } => {
            let ids = validate_clip_ids(project, &clip_ids)?;
            for track in &mut project.tracks {
                track.clips.retain(|clip| !ids.contains(&clip.id));
            }
        }
        TimelineEdit::RippleDelete { clip_ids } => {
            let ids = validate_clip_ids(project, &clip_ids)?;
            for track in &mut project.tracks {
                let removed: Vec<(u64, u64)> = track
                    .clips
                    .iter()
                    .filter(|clip| ids.contains(&clip.id))
                    .map(|clip| (clip.start_frame, clip.duration_frames))
                    .collect();
                if removed.is_empty() {
                    continue;
                }
                for clip in &mut track.clips {
                    if ids.contains(&clip.id) {
                        continue;
                    }
                    let shift: u64 = removed
                        .iter()
                        .filter(|(start, _)| *start < clip.start_frame)
                        .map(|(_, duration)| *duration)
                        .try_fold(0_u64, |total, duration| total.checked_add(duration))
                        .ok_or("波纹删除导致时间溢出")?;
                    clip.start_frame = clip
                        .start_frame
                        .checked_sub(shift)
                        .ok_or("波纹删除导致时间下溢")?;
                }
                track.clips.retain(|clip| !ids.contains(&clip.id));
            }
        }
        TimelineEdit::Duplicate { clip_id } => {
            let (t, c) = clip_index(project, &clip_id)?;
            let track = &mut project.tracks[t];
            require_unlocked(track)?;
            let mut clip = track.clips[c].clone();
            clip.id = format!("clip-{}", Uuid::new_v4());
            clip.start_frame = track_end(track);
            track.clips.push(clip);
        }
        TimelineEdit::Move {
            clip_id,
            track_id,
            start_frame,
        } => {
            let (t, c) = clip_index(project, &clip_id)?;
            let destination = track_index(project, &track_id)?;
            require_unlocked(&project.tracks[t])?;
            require_unlocked(&project.tracks[destination])?;
            let mut clip = project.tracks[t].clips.remove(c);
            clip.track_id = track_id;
            clip.start_frame = start_frame;
            project.tracks[destination].clips.push(clip);
        }
        TimelineEdit::MoveMany { moves } => {
            if moves.is_empty() || moves.len() > crate::project::MAX_CLIPS {
                return Err("请选择一个或多个要移动的片段".into());
            }
            let ids: HashSet<_> = moves.iter().map(|item| item.clip_id.as_str()).collect();
            if ids.len() != moves.len() {
                return Err("批量移动列表含重复片段".into());
            }
            let mut moving = Vec::with_capacity(moves.len());
            for item in &moves {
                let (source, index) = clip_index(project, &item.clip_id)?;
                let destination = track_index(project, &item.track_id)?;
                require_unlocked(&project.tracks[source])?;
                require_unlocked(&project.tracks[destination])?;
                let clip = project.tracks[source].clips[index].clone();
                let media = project
                    .media_refs
                    .iter()
                    .find(|media| media.id == clip.media_id)
                    .ok_or("片段引用的素材不存在")?;
                if !track_accepts(&project.tracks[destination].kind, &media.kind) {
                    return Err("批量移动包含不兼容的目标轨道".into());
                }
                let mut moved = clip;
                moved.track_id = item.track_id.clone();
                moved.start_frame = item.start_frame;
                moving.push((item.clip_id.clone(), source, moved));
            }
            for (_, _, clip) in &moving {
                for track in &mut project.tracks {
                    track.clips.retain(|candidate| candidate.id != clip.id);
                }
            }
            for (_, _, clip) in moving {
                let destination = track_index(project, &clip.track_id)?;
                project.tracks[destination].clips.push(clip);
            }
        }
        TimelineEdit::Trim {
            clip_id,
            source_in_frame,
            start_frame,
            duration_frames,
        } => {
            let (t, c) = clip_index(project, &clip_id)?;
            require_unlocked(&project.tracks[t])?;
            let clip = &mut project.tracks[t].clips[c];
            clip.source_in_frame = source_in_frame;
            clip.start_frame = start_frame;
            clip.duration_frames = duration_frames;
            let fades = u128::from(clip.fade_in_frames) + u128::from(clip.fade_out_frames);
            if fades > u128::from(duration_frames) {
                // Preserve the relative edge lengths when a shorter trim no
                // longer fits both fades. Widen before multiplying and give
                // the integer rounding remainder to the outgoing fade.
                clip.fade_in_frames =
                    (u128::from(duration_frames) * u128::from(clip.fade_in_frames) / fades) as u64;
                clip.fade_out_frames = duration_frames - clip.fade_in_frames;
            }
        }
        TimelineEdit::TrackState {
            track_id,
            locked,
            muted,
        } => {
            let t = track_index(project, &track_id)?;
            project.tracks[t].locked = locked;
            project.tracks[t].muted = muted;
        }
        TimelineEdit::TrackVolume { track_id, volume } => {
            let t = track_index(project, &track_id)?;
            let track = &mut project.tracks[t];
            require_unlocked(track)?;
            if track.kind != crate::project::TrackKind::Audio {
                return Err("只有音频轨可以设置音量".into());
            }
            if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
                return Err("音频轨音量必须在 0 到 1 之间".into());
            }
            track.volume = volume;
        }
        TimelineEdit::ClipFade {
            clip_id,
            fade_in_frames,
            fade_out_frames,
        } => {
            let (t, c) = clip_index(project, &clip_id)?;
            let track = &mut project.tracks[t];
            require_unlocked(track)?;
            if track.kind != crate::project::TrackKind::Audio {
                return Err("只有音频片段可以设置淡入淡出".into());
            }
            let clip = &mut track.clips[c];
            if fade_in_frames > clip.duration_frames
                || fade_out_frames > clip.duration_frames
                || fade_in_frames
                    .checked_add(fade_out_frames)
                    .is_none_or(|sum| sum > clip.duration_frames)
            {
                return Err("淡入淡出总时长不能超过片段时长".into());
            }
            clip.fade_in_frames = fade_in_frames;
            clip.fade_out_frames = fade_out_frames;
        }
    }
    for track in &mut project.tracks {
        track.clips.sort_by_key(|clip| clip.start_frame);
    }
    project.validate()
}

/// Compute the linear playback gain at a project frame for one audio clip.
/// A muted track or a frame outside the clip is silent. Clip fades multiply
/// the independent track volume and are evaluated in project frames.
pub fn audio_gain(track: &TimelineTrack, clip: &TimelineClip, frame: u64) -> f32 {
    if track.kind != crate::project::TrackKind::Audio
        || track.muted
        || frame < clip.start_frame
        || frame >= clip.start_frame.saturating_add(clip.duration_frames)
    {
        return 0.0;
    }
    let local = frame - clip.start_frame;
    let mut gain = track.volume;
    if clip.fade_in_frames > 0 {
        gain *= (local as f32 / clip.fade_in_frames as f32).clamp(0.0, 1.0);
    }
    if clip.fade_out_frames > 0 {
        let remaining = clip.duration_frames.saturating_sub(local + 1);
        gain *= (remaining as f32 / clip.fade_out_frames as f32).clamp(0.0, 1.0);
    }
    gain.clamp(0.0, 1.0)
}

fn edit_session(
    session: &mut ProjectSession,
    edit: TimelineEdit,
    add_duration: Option<u64>,
) -> Result<(), String> {
    let mut candidate = session.project.clone();
    apply_edit(&mut candidate, edit, add_duration)?;
    if serde_json::to_vec_pretty(&candidate)
        .map_err(|error| error.to_string())?
        .len() as u64
        > crate::project::MAX_PROJECT_BYTES
    {
        return Err("编辑后项目将超过 10 MiB，操作未执行".into());
    }
    if candidate.tracks == session.project.tracks {
        return Ok(());
    }
    session.history.undo.push(session.project.tracks.clone());
    session.history.redo.clear();
    session.history.bound();
    candidate.updated_at = timestamp();
    session.project = candidate;
    session.dirty = true;
    session.status = "时间线已更新，尚未保存".into();
    session.sync_history_flags();
    Ok(())
}

impl ProjectSession {
    pub(crate) fn timeline_references_media(&self, media_id: &str) -> bool {
        self.project
            .tracks
            .iter()
            .chain(self.history.undo.iter().flatten())
            .chain(self.history.redo.iter().flatten())
            .any(|track| track.clips.iter().any(|clip| clip.media_id == media_id))
    }
    /// Relink updates saved source bounds and invalidates history that contains old bounds.
    pub(crate) fn reconcile_relinked_media(
        &mut self,
        media_id: &str,
        frames: u64,
    ) -> Result<(), String> {
        let mut tracks = self.project.tracks.clone();
        for clip in tracks
            .iter_mut()
            .flat_map(|track| &mut track.clips)
            .filter(|clip| clip.media_id == media_id)
        {
            if clip
                .source_in_frame
                .checked_add(clip.duration_frames)
                .is_none_or(|end| end > frames)
            {
                return Err(
                    "新素材比已有片段范围更短，重新链接已取消；请先裁短或删除相关片段".into(),
                );
            }
            clip.source_duration_frames = frames;
        }
        self.project.tracks = tracks;
        self.history = Default::default();
        self.sync_history_flags();
        Ok(())
    }

    pub(crate) fn sync_history_flags(&mut self) {
        self.can_undo = !self.history.undo.is_empty();
        self.can_redo = !self.history.redo.is_empty();
    }
}

fn step_history(session: &mut ProjectSession, redo: bool) -> Result<(), String> {
    let stack = if redo {
        &session.history.redo
    } else {
        &session.history.undo
    };
    let tracks = stack
        .last()
        .ok_or(if redo {
            "没有可重做的操作"
        } else {
            "没有可撤销的操作"
        })?
        .clone();
    let mut candidate = session.project.clone();
    candidate.tracks = tracks;
    candidate.validate()?;
    if redo {
        session.history.redo.pop();
        session.history.undo.push(session.project.tracks.clone());
    } else {
        session.history.undo.pop();
        session.history.redo.push(session.project.tracks.clone());
    }
    session.history.bound();
    candidate.updated_at = timestamp();
    session.project = candidate;
    session.dirty = true;
    session.status = if redo {
        "已重做时间线操作，尚未保存"
    } else {
        "已撤销时间线操作，尚未保存"
    }
    .into();
    session.sync_history_flags();
    Ok(())
}

#[tauri::command]
pub async fn timeline_edit(
    app: AppHandle,
    project_id: String,
    revision: u64,
    edit: TimelineEdit,
) -> Result<ProjectSession, String> {
    with_session(app, move |app, session| {
        check_revision(session, &project_id, revision)?;
        let duration = if let TimelineEdit::Add { media_id, track_id } = &edit {
            let track = &session.project.tracks[track_index(&session.project, track_id)?];
            require_unlocked(track)?;
            let media = session
                .project
                .media_refs
                .iter()
                .find(|media| &media.id == media_id)
                .ok_or("找不到素材")?;
            if !track_accepts(&track.kind, &media.kind) {
                return Err("素材类型与轨道不匹配".into());
            }
            let binary = packaged_binary(app, "ffprobe")
                .ok_or("尚未随应用打包 ffprobe，不能确定素材时长")?;
            Some(source_frames(&binary, media, &session.project)?)
        } else {
            None
        };
        edit_session(session, edit, duration)
    })
    .await
}

#[tauri::command]
pub async fn timeline_undo(
    app: AppHandle,
    project_id: String,
    revision: u64,
) -> Result<ProjectSession, String> {
    with_session(app, move |_, session| {
        check_revision(session, &project_id, revision)?;
        step_history(session, false)
    })
    .await
}

#[tauri::command]
pub async fn timeline_redo(
    app: AppHandle,
    project_id: String,
    revision: u64,
) -> Result<ProjectSession, String> {
    with_session(app, move |_, session| {
        check_revision(session, &project_id, revision)?;
        step_history(session, true)
    })
    .await
}

#[tauri::command]
pub async fn timeline_audio_gain(
    app: AppHandle,
    project_id: String,
    revision: u64,
    track_id: String,
    clip_id: String,
    frame: u64,
) -> Result<f32, String> {
    let check_track = track_id.clone();
    let check_clip = clip_id.clone();
    let session = with_session(app, move |_, session| {
        check_revision(session, &project_id, revision)?;
        let track = session
            .project
            .tracks
            .iter()
            .find(|track| track.id == check_track)
            .ok_or("找不到音频轨")?;
        let clip = track
            .clips
            .iter()
            .find(|clip| clip.id == check_clip)
            .ok_or("找不到音频片段")?;
        if track.kind != crate::project::TrackKind::Audio {
            return Err("指定轨道不是音频轨".into());
        }
        if clip.track_id != track.id {
            return Err("片段与轨道不匹配".into());
        }
        Ok(())
    })
    .await?;
    {
        let track = session
            .project
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .ok_or("找不到音频轨")?;
        let clip = track
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .ok_or("找不到音频片段")?;
        Ok(audio_gain(track, clip, frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{import_media, parse_project, read_project, save_project};
    use std::fs;

    fn session_with_clip() -> ProjectSession {
        let mut session = ProjectSession::default();
        session.project.media_refs.push(MediaAsset {
            id: "media-1".into(),
            name: "clip.mp4".into(),
            kind: MediaKind::Video,
            extension: "mp4".into(),
            size: 10,
            last_modified: 0,
            source_path: None,
        });
        edit_session(
            &mut session,
            TimelineEdit::Add {
                media_id: "media-1".into(),
                track_id: "video-1".into(),
            },
            Some(300),
        )
        .unwrap();
        session
    }

    fn first_id(session: &ProjectSession) -> String {
        session.project.tracks[0].clips[0].id.clone()
    }

    #[test]
    fn split_duplicate_move_trim_delete_undo_and_redo_preserve_source_ranges() {
        let mut session = session_with_clip();
        let id = first_id(&session);
        edit_session(
            &mut session,
            TimelineEdit::Split {
                clip_id: id.clone(),
                frame: 120,
            },
            None,
        )
        .unwrap();
        let right = session.project.tracks[0].clips[1].clone();
        assert_eq!((right.source_in_frame, right.duration_frames), (120, 180));
        edit_session(
            &mut session,
            TimelineEdit::Duplicate { clip_id: right.id },
            None,
        )
        .unwrap();
        let copy = session.project.tracks[0].clips[2].clone();
        assert_eq!((copy.start_frame, copy.source_in_frame), (300, 120));
        edit_session(
            &mut session,
            TimelineEdit::Move {
                clip_id: copy.id.clone(),
                track_id: "video-2".into(),
                start_frame: 15,
            },
            None,
        )
        .unwrap();
        edit_session(
            &mut session,
            TimelineEdit::Trim {
                clip_id: copy.id.clone(),
                source_in_frame: 150,
                start_frame: 45,
                duration_frames: 120,
            },
            None,
        )
        .unwrap();
        let trimmed = session.project.tracks.clone();
        edit_session(
            &mut session,
            TimelineEdit::Delete {
                clip_ids: vec![id, copy.id],
            },
            None,
        )
        .unwrap();
        assert_eq!(session.project.tracks[0].clips.len(), 1);
        assert!(session.project.tracks[1].clips.is_empty());
        step_history(&mut session, false).unwrap();
        assert_eq!(session.project.tracks, trimmed);
        step_history(&mut session, true).unwrap();
        assert!(session.project.tracks[1].clips.is_empty());
        assert!(session.dirty && session.can_undo && !session.can_redo);
    }

    #[test]
    fn invalid_commands_are_atomic_and_locked_tracks_are_protected() {
        let mut session = session_with_clip();
        let id = first_id(&session);
        for edit in [
            TimelineEdit::Split {
                clip_id: id.clone(),
                frame: 0,
            },
            TimelineEdit::Split {
                clip_id: id.clone(),
                frame: 300,
            },
            TimelineEdit::Move {
                clip_id: id.clone(),
                track_id: "audio-1".into(),
                start_frame: 0,
            },
            TimelineEdit::Move {
                clip_id: id.clone(),
                track_id: "video-1".into(),
                start_frame: u64::MAX,
            },
            TimelineEdit::Trim {
                clip_id: id.clone(),
                source_in_frame: 299,
                start_frame: 0,
                duration_frames: 2,
            },
            TimelineEdit::Trim {
                clip_id: id.clone(),
                source_in_frame: 0,
                start_frame: 0,
                duration_frames: 0,
            },
            TimelineEdit::Delete {
                clip_ids: vec![id.clone(), "unknown".into()],
            },
        ] {
            let before = session.clone();
            assert!(edit_session(&mut session, edit, None).is_err());
            assert_eq!(session, before);
        }
        edit_session(
            &mut session,
            TimelineEdit::TrackState {
                track_id: "video-1".into(),
                locked: true,
                muted: true,
            },
            None,
        )
        .unwrap();
        assert!(
            edit_session(
                &mut session,
                TimelineEdit::Delete { clip_ids: vec![id] },
                None
            )
            .is_err()
        );
        step_history(&mut session, false).unwrap();
        assert!(!session.project.tracks[0].locked);
        let clip_id = first_id(&session);
        edit_session(&mut session, TimelineEdit::Duplicate { clip_id }, None).unwrap();
        assert!(!session.can_redo);
    }

    #[test]
    fn ripple_delete_shifts_only_later_clips_on_each_affected_track() {
        let mut session = session_with_clip();
        let initial = first_id(&session);
        edit_session(
            &mut session,
            TimelineEdit::Duplicate { clip_id: initial },
            None,
        )
        .unwrap();
        let second = session.project.tracks[0].clips[1].id.clone();
        edit_session(
            &mut session,
            TimelineEdit::Duplicate {
                clip_id: second.clone(),
            },
            None,
        )
        .unwrap();
        let third = session.project.tracks[0].clips[2].id.clone();
        edit_session(
            &mut session,
            TimelineEdit::RippleDelete {
                clip_ids: vec![second],
            },
            None,
        )
        .unwrap();
        assert_eq!(session.project.tracks[0].clips.len(), 2);
        assert_eq!(session.project.tracks[0].clips[1].id, third);
        assert_eq!(session.project.tracks[0].clips[1].start_frame, 300);
        assert!(session.project.tracks[0].clips[0].start_frame == 0);
    }

    #[test]
    fn move_many_is_atomic_and_rejects_locked_or_incompatible_targets() {
        let mut session = session_with_clip();
        let first = first_id(&session);
        edit_session(
            &mut session,
            TimelineEdit::Duplicate {
                clip_id: first.clone(),
            },
            None,
        )
        .unwrap();
        let second = session.project.tracks[0].clips[1].id.clone();
        edit_session(
            &mut session,
            TimelineEdit::MoveMany {
                moves: vec![
                    ClipMove {
                        clip_id: first.clone(),
                        track_id: "video-2".into(),
                        start_frame: 0,
                    },
                    ClipMove {
                        clip_id: second.clone(),
                        track_id: "video-2".into(),
                        start_frame: 300,
                    },
                ],
            },
            None,
        )
        .unwrap();
        assert!(session.project.tracks[0].clips.is_empty());
        assert_eq!(session.project.tracks[1].clips.len(), 2);
        let before = session.clone();
        assert!(
            edit_session(
                &mut session,
                TimelineEdit::MoveMany {
                    moves: vec![ClipMove {
                        clip_id: first,
                        track_id: "audio-1".into(),
                        start_frame: 0,
                    }],
                },
                None,
            )
            .is_err()
        );
        assert_eq!(session, before);
        edit_session(
            &mut session,
            TimelineEdit::TrackState {
                track_id: "video-2".into(),
                locked: true,
                muted: false,
            },
            None,
        )
        .unwrap();
        let before = session.clone();
        assert!(
            edit_session(
                &mut session,
                TimelineEdit::MoveMany {
                    moves: vec![ClipMove {
                        clip_id: second,
                        track_id: "video-1".into(),
                        start_frame: 0,
                    }],
                },
                None,
            )
            .is_err()
        );
        assert_eq!(session, before);
    }

    #[test]
    fn move_many_rejects_target_overlap_and_frame_overflow_without_partial_moves() {
        let mut session = session_with_clip();
        let first = first_id(&session);
        edit_session(
            &mut session,
            TimelineEdit::Duplicate {
                clip_id: first.clone(),
            },
            None,
        )
        .unwrap();
        let second = session.project.tracks[0].clips[1].id.clone();
        edit_session(
            &mut session,
            TimelineEdit::Move {
                clip_id: first.clone(),
                track_id: "video-2".into(),
                start_frame: 0,
            },
            None,
        )
        .unwrap();
        let before = session.clone();
        assert!(
            edit_session(
                &mut session,
                TimelineEdit::MoveMany {
                    moves: vec![ClipMove {
                        clip_id: second.clone(),
                        track_id: "video-2".into(),
                        start_frame: 100,
                    }],
                },
                None,
            )
            .is_err()
        );
        assert_eq!(session, before);
        assert!(
            edit_session(
                &mut session,
                TimelineEdit::MoveMany {
                    moves: vec![ClipMove {
                        clip_id: second,
                        track_id: "video-2".into(),
                        start_frame: u64::MAX,
                    }],
                },
                None,
            )
            .is_err()
        );
        assert_eq!(session, before);
    }

    #[test]
    fn move_many_rejects_a_locked_source_track_before_removing_any_clip() {
        let mut session = session_with_clip();
        let clip_id = first_id(&session);
        edit_session(
            &mut session,
            TimelineEdit::TrackState {
                track_id: "video-1".into(),
                locked: true,
                muted: false,
            },
            None,
        )
        .unwrap();
        let before = session.clone();
        assert!(
            edit_session(
                &mut session,
                TimelineEdit::MoveMany {
                    moves: vec![ClipMove {
                        clip_id,
                        track_id: "video-2".into(),
                        start_frame: 0,
                    }],
                },
                None,
            )
            .is_err()
        );
        assert_eq!(session, before);
    }

    #[test]
    fn move_many_rejects_overlapping_destinations_without_partial_move_or_history() {
        let mut session = session_with_clip();
        let first = first_id(&session);
        edit_session(
            &mut session,
            TimelineEdit::Duplicate {
                clip_id: first.clone(),
            },
            None,
        )
        .unwrap();
        let second = session.project.tracks[0].clips[1].id.clone();
        let before = session.clone();
        let result = edit_session(
            &mut session,
            TimelineEdit::MoveMany {
                moves: vec![
                    ClipMove {
                        clip_id: first,
                        track_id: "video-2".into(),
                        start_frame: 0,
                    },
                    ClipMove {
                        clip_id: second,
                        track_id: "video-2".into(),
                        start_frame: 150,
                    },
                ],
            },
            None,
        );
        assert!(result.is_err());
        assert_eq!(session.project.tracks, before.project.tracks);
        assert_eq!(session.history, before.history);
        assert_eq!(session.can_undo, before.can_undo);
        assert_eq!(session.can_redo, before.can_redo);
    }

    #[test]
    fn batch_delete_uses_one_history_step_and_supports_undo_redo() {
        let mut session = session_with_clip();
        let first = first_id(&session);
        edit_session(
            &mut session,
            TimelineEdit::Duplicate {
                clip_id: first.clone(),
            },
            None,
        )
        .unwrap();
        let second = session.project.tracks[0].clips[1].id.clone();
        let history_before = session.history.undo.len();
        edit_session(
            &mut session,
            TimelineEdit::Delete {
                clip_ids: vec![first.clone(), second.clone()],
            },
            None,
        )
        .unwrap();
        assert_eq!(session.project.tracks[0].clips.len(), 0);
        assert_eq!(session.history.undo.len(), history_before + 1);
        step_history(&mut session, false).unwrap();
        assert_eq!(session.project.tracks[0].clips.len(), 2);
        step_history(&mut session, true).unwrap();
        assert!(session.project.tracks[0].clips.is_empty());
    }

    #[test]
    fn batch_delete_across_tracks_is_atomic_when_any_selected_track_is_locked() {
        let mut session = session_with_clip();
        session.project.media_refs.push(MediaAsset {
            id: "audio-1".into(),
            name: "music.wav".into(),
            kind: MediaKind::Audio,
            extension: "wav".into(),
            size: 10,
            last_modified: 0,
            source_path: None,
        });
        edit_session(
            &mut session,
            TimelineEdit::Add {
                media_id: "audio-1".into(),
                track_id: "audio-1".into(),
            },
            Some(150),
        )
        .unwrap();
        let video_id = session.project.tracks[0].clips[0].id.clone();
        let audio_id = session.project.tracks[2].clips[0].id.clone();
        edit_session(
            &mut session,
            TimelineEdit::TrackState {
                track_id: "audio-1".into(),
                locked: true,
                muted: false,
            },
            None,
        )
        .unwrap();
        let before = session.clone();
        let result = edit_session(
            &mut session,
            TimelineEdit::Delete {
                clip_ids: vec![video_id, audio_id],
            },
            None,
        );
        assert!(result.is_err());
        assert_eq!(session.project.tracks, before.project.tracks);
        assert_eq!(session.history, before.history);
        assert_eq!(session.can_undo, before.can_undo);
        assert_eq!(session.can_redo, before.can_redo);
    }

    #[test]
    fn schema_two_rejects_overlap_references_duplicate_ids_and_source_overflow() {
        let session = session_with_clip();
        for variant in 0..7 {
            let mut project = session.project.clone();
            let clip = &mut project.tracks[0].clips[0];
            match variant {
                0 => clip.media_id = "missing".into(),
                1 => clip.track_id = "video-2".into(),
                2 => clip.duration_frames = 301,
                3 => clip.source_duration_frames = MAX_SAFE_INTEGER + 1,
                4 => clip.source_in_frame = u64::MAX,
                5 => {
                    let copy = clip.clone();
                    project.tracks[0].clips.push(copy);
                }
                _ => {
                    let mut copy = clip.clone();
                    copy.id = "unique-id".into();
                    copy.start_frame = 299;
                    project.tracks[0].clips.push(copy);
                }
            }
            assert!(parse_project(&serde_json::to_vec(&project).unwrap()).is_err());
        }
        let mut legacy = serde_json::to_value(KaicutProject::new()).unwrap();
        legacy["schemaVersion"] = 1.into();
        assert_eq!(
            parse_project(&serde_json::to_vec(&legacy).unwrap())
                .unwrap()
                .schema_version,
            2
        );
    }

    #[test]
    fn undo_does_not_revert_rename_import_or_relink_and_save_keeps_history() {
        let mut session = session_with_clip();
        session.project.name = "新名称".into();
        session.project.media_refs[0].source_path = Some("E:/new.mp4".into());
        let mut imported = session.project.media_refs[0].clone();
        imported.id = "media-2".into();
        session.project.media_refs.push(imported);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("编辑.kaicut");
        save_project(&path, &session.project).unwrap();
        assert_eq!(read_project(&path).unwrap(), session.project);
        session.dirty = false;
        step_history(&mut session, false).unwrap();
        assert_eq!(session.project.name, "新名称");
        assert_eq!(session.project.media_refs.len(), 2);
        assert_eq!(
            session.project.media_refs[0].source_path.as_deref(),
            Some("E:/new.mp4")
        );
        assert!(session.project.tracks[0].clips.is_empty());
        assert!(session.dirty);
        let saved = read_project(&path).unwrap();
        session.revision = 100;
        session.replace_project(
            saved.clone(),
            Some(path.to_string_lossy().into_owned()),
            false,
        );
        assert_eq!(session.revision, 101);
        assert!(!session.can_undo && !session.can_redo);
        assert!(check_revision(&session, &saved.project_id, 100).is_err());
        assert!(check_revision(&session, "other-project", 101).is_err());
        assert!(check_revision(&session, &saved.project_id, 101).is_ok());
        session.replace_project(saved, session.path.clone(), false);
        assert_eq!(session.revision, 102);
    }

    #[test]
    fn history_has_step_and_memory_budgets_and_stays_out_of_session_json() {
        let mut session = session_with_clip();
        for n in 0..150 {
            edit_session(
                &mut session,
                TimelineEdit::TrackState {
                    track_id: "video-1".into(),
                    locked: n % 2 == 0,
                    muted: false,
                },
                None,
            )
            .unwrap();
        }
        assert_eq!(session.history.undo.len(), MAX_HISTORY_STEPS);
        let mut enormous = session.project.tracks.clone();
        enormous[0].name = "x".repeat(MAX_HISTORY_BYTES + 1);
        session.history.undo.push(enormous);
        session.history.bound();
        assert!(session.history.undo.is_empty());
        let json = serde_json::to_value(session).unwrap();
        assert!(json.get("canUndo").is_some() && json.get("canRedo").is_some());
        assert!(json.get("history").is_none());
    }

    #[test]
    fn audio_track_volume_mute_and_clip_fades_produce_bounded_playback_gain() {
        let mut session = ProjectSession::default();
        session.project.media_refs.push(MediaAsset {
            id: "audio-asset".into(),
            name: "voice.wav".into(),
            kind: MediaKind::Audio,
            extension: "wav".into(),
            size: 1,
            last_modified: 0,
            source_path: None,
        });
        edit_session(
            &mut session,
            TimelineEdit::Add {
                media_id: "audio-asset".into(),
                track_id: "audio-1".into(),
            },
            Some(30),
        )
        .unwrap();
        let clip_id = session.project.tracks[2].clips[0].id.clone();
        edit_session(
            &mut session,
            TimelineEdit::TrackVolume {
                track_id: "audio-1".into(),
                volume: 0.5,
            },
            None,
        )
        .unwrap();
        edit_session(
            &mut session,
            TimelineEdit::ClipFade {
                clip_id: clip_id.clone(),
                fade_in_frames: 10,
                fade_out_frames: 10,
            },
            None,
        )
        .unwrap();
        let track = &session.project.tracks[2];
        let clip = &track.clips[0];
        assert_eq!(audio_gain(track, clip, 0), 0.0);
        assert!((audio_gain(track, clip, 5) - 0.25).abs() < 0.001);
        assert!((audio_gain(track, clip, 15) - 0.5).abs() < 0.001);
        assert_eq!(audio_gain(track, clip, 29), 0.0);
        edit_session(
            &mut session,
            TimelineEdit::TrackState {
                track_id: "audio-1".into(),
                locked: false,
                muted: true,
            },
            None,
        )
        .unwrap();
        let track = &session.project.tracks[2];
        assert_eq!(audio_gain(track, &track.clips[0], 15), 0.0);
        let before = session.clone();
        assert!(
            edit_session(
                &mut session,
                TimelineEdit::ClipFade {
                    clip_id,
                    fade_in_frames: 20,
                    fade_out_frames: 20,
                },
                None,
            )
            .is_err()
        );
        assert_eq!(session, before);
        edit_session(
            &mut session,
            TimelineEdit::TrackState {
                track_id: "audio-1".into(),
                locked: true,
                muted: true,
            },
            None,
        )
        .unwrap();
        let before = session.clone();
        assert!(
            edit_session(
                &mut session,
                TimelineEdit::TrackVolume {
                    track_id: "audio-1".into(),
                    volume: 0.25,
                },
                None,
            )
            .is_err()
        );
        assert_eq!(session, before);
    }

    #[test]
    fn old_v2_tracks_default_audio_mixer_fields_and_persist_new_fields() {
        let mut value = serde_json::to_value(KaicutProject::new()).unwrap();
        for track in value["tracks"].as_array_mut().unwrap() {
            track.as_object_mut().unwrap().remove("volume");
        }
        let parsed = parse_project(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(parsed.tracks.iter().all(|track| track.volume == 1.0));
        let serialized = serde_json::to_value(parsed).unwrap();
        assert_eq!(serialized["tracks"][2]["volume"], serde_json::json!(1.0));
    }

    fn audio_session_with_fades(fade_in_frames: u64, fade_out_frames: u64) -> ProjectSession {
        let mut session = ProjectSession::default();
        session.project.media_refs.push(MediaAsset {
            id: "audio-fade-test".into(),
            name: "voice.wav".into(),
            kind: MediaKind::Audio,
            extension: "wav".into(),
            size: 10,
            last_modified: 0,
            source_path: None,
        });
        edit_session(
            &mut session,
            TimelineEdit::Add {
                media_id: "audio-fade-test".into(),
                track_id: "audio-1".into(),
            },
            Some(120),
        )
        .unwrap();
        let clip_id = session.project.tracks[2].clips[0].id.clone();
        edit_session(
            &mut session,
            TimelineEdit::ClipFade {
                clip_id,
                fade_in_frames,
                fade_out_frames,
            },
            None,
        )
        .unwrap();
        session
    }

    #[test]
    fn split_keeps_only_outer_audio_fades_with_history_and_saved_source_ranges() {
        for cut in [15, 60, 105] {
            let mut session = audio_session_with_fades(30, 30);
            let before = session.project.tracks.clone();
            let id = before[2].clips[0].id.clone();
            edit_session(
                &mut session,
                TimelineEdit::Split {
                    clip_id: id,
                    frame: cut,
                },
                None,
            )
            .unwrap();
            let after = session.project.tracks.clone();
            let left = &after[2].clips[0];
            let right = &after[2].clips[1];
            assert_eq!(
                (left.start_frame, left.source_in_frame, left.duration_frames),
                (0, 0, cut)
            );
            assert_eq!(
                (
                    right.start_frame,
                    right.source_in_frame,
                    right.duration_frames
                ),
                (cut, cut, 120 - cut)
            );
            assert_eq!(
                (left.fade_in_frames, left.fade_out_frames),
                (30.min(cut), 0)
            );
            assert_eq!(
                (right.fade_in_frames, right.fade_out_frames),
                (0, 30.min(120 - cut))
            );
            assert_eq!(
                (left.source_duration_frames, right.source_duration_frames),
                (120, 120)
            );
            step_history(&mut session, false).unwrap();
            assert_eq!(session.project.tracks, before);
            step_history(&mut session, true).unwrap();
            assert_eq!(session.project.tracks, after);
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("split-fades.kaicut");
            save_project(&path, &session.project).unwrap();
            assert_eq!(read_project(&path).unwrap(), session.project);
        }
    }

    #[test]
    fn trim_shortens_audio_fades_proportionally_without_expanding_them_on_extension() {
        for (fade_in, fade_out, duration, expected_in, expected_out) in [
            (80, 20, 40, 32, 8),
            (5, 4, 3, 1, 2),
            (0, 100, 15, 0, 15),
            (100, 0, 15, 15, 0),
            (5, 4, 20, 5, 4),
        ] {
            let mut session = audio_session_with_fades(fade_in, fade_out);
            let before = session.project.tracks.clone();
            let id = before[2].clips[0].id.clone();
            edit_session(
                &mut session,
                TimelineEdit::Trim {
                    clip_id: id.clone(),
                    source_in_frame: 20,
                    start_frame: 15,
                    duration_frames: duration,
                },
                None,
            )
            .unwrap();
            let after = session.project.tracks.clone();
            let clip = &after[2].clips[0];
            assert_eq!(
                (clip.fade_in_frames, clip.fade_out_frames),
                (expected_in, expected_out)
            );
            assert_eq!(
                (clip.source_in_frame, clip.start_frame, clip.duration_frames),
                (20, 15, duration)
            );
            step_history(&mut session, false).unwrap();
            assert_eq!(session.project.tracks, before);
            step_history(&mut session, true).unwrap();
            assert_eq!(session.project.tracks, after);
            edit_session(
                &mut session,
                TimelineEdit::Trim {
                    clip_id: id,
                    source_in_frame: 20,
                    start_frame: 15,
                    duration_frames: 80,
                },
                None,
            )
            .unwrap();
            let clip = &session.project.tracks[2].clips[0];
            assert_eq!(
                (clip.fade_in_frames, clip.fade_out_frames),
                (expected_in, expected_out)
            );
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("trim-fades.kaicut");
            save_project(&path, &session.project).unwrap();
            assert_eq!(read_project(&path).unwrap(), session.project);
        }
    }

    #[test]
    fn invalid_or_locked_faded_edits_preserve_project_and_history() {
        let mut session = audio_session_with_fades(80, 20);
        let id = session.project.tracks[2].clips[0].id.clone();
        for (source_in_frame, duration_frames) in [(0, 0), (119, 2), (MAX_SAFE_INTEGER, 1)] {
            let before = session.clone();
            assert!(
                edit_session(
                    &mut session,
                    TimelineEdit::Trim {
                        clip_id: id.clone(),
                        source_in_frame,
                        start_frame: 0,
                        duration_frames,
                    },
                    None
                )
                .is_err()
            );
            assert_eq!(session, before);
        }
        edit_session(
            &mut session,
            TimelineEdit::TrackState {
                track_id: "audio-1".into(),
                locked: true,
                muted: false,
            },
            None,
        )
        .unwrap();
        for command in [
            TimelineEdit::Trim {
                clip_id: id.clone(),
                source_in_frame: 20,
                start_frame: 15,
                duration_frames: 40,
            },
            TimelineEdit::Split {
                clip_id: id,
                frame: 15,
            },
        ] {
            let before = session.clone();
            assert!(
                edit_session(&mut session, command, None)
                    .unwrap_err()
                    .contains("锁定")
            );
            assert_eq!(session, before);
        }
    }

    #[test]
    fn relink_rejects_short_sources_and_invalidates_obsolete_undo_bounds() {
        let mut session = session_with_clip();
        let before = session.clone();
        assert!(session.reconcile_relinked_media("media-1", 299).is_err());
        assert_eq!(session, before);
        session.reconcile_relinked_media("media-1", 400).unwrap();
        assert_eq!(
            session.project.tracks[0].clips[0].source_duration_frames,
            400
        );
        assert!(!session.can_undo && !session.can_redo);
        let clip_id = first_id(&session);
        edit_session(
            &mut session,
            TimelineEdit::Delete {
                clip_ids: vec![clip_id],
            },
            None,
        )
        .unwrap();
        assert!(session.timeline_references_media("media-1"));
        session.reconcile_relinked_media("media-1", 10).unwrap();
        assert!(!session.timeline_references_media("media-1"));
        assert!(!session.can_undo);
    }

    #[test]
    fn still_images_start_at_five_seconds_but_can_be_extended() {
        let mut session = session_with_clip();
        session.project.media_refs[0].kind = MediaKind::Image;
        session.project.media_refs[0].extension = "png".into();
        session.project.tracks[0].clips.clear();
        edit_session(
            &mut session,
            TimelineEdit::Add {
                media_id: "media-1".into(),
                track_id: "video-1".into(),
            },
            Some(150),
        )
        .unwrap();
        let clip_id = first_id(&session);
        assert_eq!(session.project.tracks[0].clips[0].duration_frames, 150);
        edit_session(
            &mut session,
            TimelineEdit::Trim {
                clip_id,
                source_in_frame: 0,
                start_frame: 0,
                duration_frames: 300,
            },
            None,
        )
        .unwrap();
        assert_eq!(session.project.tracks[0].clips[0].duration_frames, 300);
    }

    #[test]
    fn edit_ipc_uses_camel_case_and_rejects_extra_fields() {
        assert!(serde_json::from_str::<TimelineEdit>(r#"{"type":"trim","clipId":"c","sourceInFrame":3,"startFrame":5,"durationFrames":2}"#).is_ok());
        assert!(
            serde_json::from_str::<TimelineEdit>(r#"{"type":"rippleDelete","clipIds":["a","b"]}"#)
                .is_ok()
        );
        assert!(
            serde_json::from_str::<TimelineEdit>(
                r#"{"type":"moveMany","moves":[{"clipId":"a","trackId":"video-1","startFrame":0}]}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<TimelineEdit>(
                r#"{"type":"add","mediaId":"m","trackId":"video-1","sourcePath":"evil"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<TimelineEdit>(r#"{"type":"split","clipId":"c","frame":-1}"#)
                .is_err()
        );
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn actual_wav_probe_add_and_save_open_roundtrip() {
        let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries/ffprobe-x86_64-pc-windows-msvc.exe");
        if !binary.is_file() {
            eprintln!("SKIP actual sidecar test: ffprobe is not bundled on this checkout");
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("真实音频 验证.wav");
        let sample_rate: u32 = 48_000;
        let data_size = sample_rate * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_size).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());
        wav.resize(44 + data_size as usize, 0);
        fs::write(&path, wav).unwrap();
        let mut session = ProjectSession::default();
        import_media(&mut session.project, vec![path.clone()]).unwrap();
        let media = session.project.media_refs[0].clone();
        let frames = source_frames(&binary, &media, &session.project).unwrap();
        assert_eq!(frames, 30);
        edit_session(
            &mut session,
            TimelineEdit::Add {
                media_id: media.id.clone(),
                track_id: "audio-1".into(),
            },
            Some(frames),
        )
        .unwrap();
        let project_path = directory.path().join("真实项目.kaicut");
        save_project(&project_path, &session.project).unwrap();
        assert_eq!(read_project(&project_path).unwrap(), session.project);
        fs::write(&path, b"changed").unwrap();
        assert!(
            source_frames(&binary, &media, &session.project)
                .unwrap_err()
                .contains("更改")
        );
    }

    // This regression exercises the production editing and persistence path,
    // rather than constructing the export timeline by hand. Fixtures and
    // results intentionally remain available for manual review after the run.
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    mod workflow {
        use super::*;
        use crate::export_job::{
            EXPORT_SNAPSHOT_FORMAT_VERSION, ExportSnapshot, media_snapshot, new_job_id,
            validate_output_metadata, write_snapshot_to_dir,
        };
        use sha2::{Digest, Sha256};
        use std::{
            ffi::OsString,
            process::{Command, Stdio},
        };

        fn run_tool(binary: &Path, args: &[OsString]) -> Vec<u8> {
            let mut command = Command::new(binary);
            command
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            crate::media_engine::hide_console(&mut command);
            let (status, stdout, stderr) = crate::media_engine::collect_child_output(
                command.spawn().expect("packaged sidecar must start"),
                Duration::from_secs(90),
            )
            .unwrap();
            assert!(
                status.success(),
                "{} failed: {}",
                binary.display(),
                String::from_utf8_lossy(&stderr)
            );
            stdout
        }

        fn audio_fixture(path: &Path) {
            let samples = 4 * 48_000_u32;
            let data_size = samples * 2;
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"RIFF");
            bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
            bytes.extend_from_slice(b"WAVEfmt ");
            bytes.extend_from_slice(&16_u32.to_le_bytes());
            bytes.extend_from_slice(&1_u16.to_le_bytes());
            bytes.extend_from_slice(&1_u16.to_le_bytes());
            bytes.extend_from_slice(&48_000_u32.to_le_bytes());
            bytes.extend_from_slice(&96_000_u32.to_le_bytes());
            bytes.extend_from_slice(&2_u16.to_le_bytes());
            bytes.extend_from_slice(&16_u16.to_le_bytes());
            bytes.extend_from_slice(b"data");
            bytes.extend_from_slice(&data_size.to_le_bytes());
            for sample in 0..samples {
                let amplitude = if sample < 48_000 {
                    0.0
                } else {
                    0.4 * (std::f64::consts::TAU * 440.0 * f64::from(sample) / 48_000.0).sin()
                };
                bytes.extend_from_slice(&((amplitude * f64::from(i16::MAX)) as i16).to_le_bytes());
            }
            fs::write(path, bytes).unwrap();
        }

        fn add(
            session: &mut ProjectSession,
            ffprobe: &PathBuf,
            media: usize,
            track: usize,
        ) -> String {
            let asset = session.project.media_refs[media].clone();
            let duration = source_frames(ffprobe, &asset, &session.project).unwrap();
            assert_eq!(
                duration,
                if asset.kind == MediaKind::Image {
                    150
                } else {
                    120
                }
            );
            let track_id = session.project.tracks[track].id.clone();
            edit_session(
                session,
                TimelineEdit::Add {
                    media_id: asset.id,
                    track_id,
                },
                Some(duration),
            )
            .unwrap();
            session.project.tracks[track]
                .clips
                .last()
                .unwrap()
                .id
                .clone()
        }

        fn edit(session: &mut ProjectSession, command: TimelineEdit) {
            edit_session(session, command, None).unwrap();
        }

        fn rms(wav: &[u8], start: f64, end: f64) -> f64 {
            // WAV can contain metadata chunks before its interleaved stereo PCM.
            let mut cursor = 12;
            let pcm = loop {
                let length =
                    u32::from_le_bytes(wav[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
                if &wav[cursor..cursor + 4] == b"data" {
                    break &wav[cursor + 8..cursor + 8 + length];
                }
                cursor += 8 + length + length % 2;
            };
            let from = (start * 48_000.0) as usize;
            let to = (end * 48_000.0) as usize;
            let energy: f64 = pcm[from * 4..to * 4]
                .chunks_exact(4)
                .map(|frame| {
                    let sample =
                        f64::from(i16::from_le_bytes([frame[0], frame[1]])) / f64::from(i16::MAX);
                    sample * sample
                })
                .sum();
            (energy / (to - from) as f64).sqrt()
        }

        #[test]
        #[ignore = "requires packaged LGPL sidecars and Windows h264_mf; retains target/workflow-validation artifacts"]
        fn real_import_edit_save_reopen_snapshot_and_export() {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let directory = manifest
                .join("target/workflow-validation")
                .join(Uuid::new_v4().to_string());
            fs::create_dir_all(&directory).unwrap();
            println!("Workflow validation artifacts: {}", directory.display());
            let ffmpeg = manifest.join("binaries/ffmpeg-x86_64-pc-windows-msvc.exe");
            let ffprobe = manifest.join("binaries/ffprobe-x86_64-pc-windows-msvc.exe");
            let video = directory.join("原始 视频;[literal].mp4");
            let audio = directory.join("原始 音频.wav");
            let image = directory.join("原始 图片%03d.png");
            run_tool(
                &ffmpeg,
                &[
                    "-hide_banner".into(),
                    "-nostdin".into(),
                    "-n".into(),
                    "-f".into(),
                    "lavfi".into(),
                    "-i".into(),
                    "color=c=red:s=160x90:r=30:d=1".into(),
                    "-f".into(),
                    "lavfi".into(),
                    "-i".into(),
                    "color=c=blue:s=160x90:r=30:d=3".into(),
                    "-filter_complex".into(),
                    "[0:v][1:v]concat=n=2:v=1:a=0,format=nv12[v]".into(),
                    "-map".into(),
                    "[v]".into(),
                    "-c:v".into(),
                    "h264_mf".into(),
                    "-b:v".into(),
                    "1M".into(),
                    "-t".into(),
                    "4".into(),
                    video.as_os_str().into(),
                ],
            );
            run_tool(
                &ffmpeg,
                &[
                    "-hide_banner".into(),
                    "-nostdin".into(),
                    "-n".into(),
                    "-f".into(),
                    "lavfi".into(),
                    "-i".into(),
                    "color=c=yellow:s=160x90:d=1".into(),
                    "-frames:v".into(),
                    "1".into(),
                    "-threads".into(),
                    "1".into(),
                    "-update".into(),
                    "1".into(),
                    image.as_os_str().into(),
                ],
            );
            audio_fixture(&audio);
            let source_paths = [video, audio, image];
            let originals: Vec<_> = source_paths
                .iter()
                .map(|path| {
                    (
                        Sha256::digest(fs::read(path).unwrap()),
                        fs::metadata(path).unwrap().modified().unwrap(),
                    )
                })
                .collect();
            let mut session = ProjectSession::default();
            session.project.name = "剪辑至导出回归项目".into();
            assert_eq!(
                import_media(&mut session.project, source_paths.to_vec()).unwrap(),
                (3, 0)
            );
            assert_eq!(
                import_media(&mut session.project, vec![source_paths[0].clone()]).unwrap(),
                (0, 1)
            );

            let left = add(&mut session, &ffprobe, 0, 0);
            edit(
                &mut session,
                TimelineEdit::Split {
                    clip_id: left.clone(),
                    frame: 30,
                },
            );
            let right = session.project.tracks[0].clips[1].id.clone();
            let before_delete = session.project.tracks.clone();
            edit(
                &mut session,
                TimelineEdit::Delete {
                    clip_ids: vec![left],
                },
            );
            let after_delete = session.project.tracks.clone();
            step_history(&mut session, false).unwrap();
            assert_eq!(session.project.tracks, before_delete);
            step_history(&mut session, true).unwrap();
            assert_eq!(session.project.tracks, after_delete);
            edit(
                &mut session,
                TimelineEdit::Trim {
                    clip_id: right,
                    source_in_frame: 30,
                    start_frame: 30,
                    duration_frames: 30,
                },
            );
            let lower_video = add(&mut session, &ffprobe, 0, 1);
            edit(
                &mut session,
                TimelineEdit::Trim {
                    clip_id: lower_video,
                    source_in_frame: 0,
                    start_frame: 0,
                    duration_frames: 15,
                },
            );
            let picture = add(&mut session, &ffprobe, 2, 0);
            edit(
                &mut session,
                TimelineEdit::Trim {
                    clip_id: picture.clone(),
                    source_in_frame: 0,
                    start_frame: 60,
                    duration_frames: 30,
                },
            );
            edit(
                &mut session,
                TimelineEdit::Move {
                    clip_id: picture,
                    track_id: "video-1".into(),
                    start_frame: 90,
                },
            );
            let voice = add(&mut session, &ffprobe, 1, 2);
            edit(
                &mut session,
                TimelineEdit::ClipFade {
                    clip_id: voice.clone(),
                    fade_in_frames: 30,
                    fade_out_frames: 30,
                },
            );
            let unsplit_audio = session.project.tracks.clone();
            edit(
                &mut session,
                TimelineEdit::Split {
                    clip_id: voice.clone(),
                    frame: 15,
                },
            );
            assert_eq!(session.project.tracks[2].clips[0].fade_in_frames, 15);
            assert_eq!(session.project.tracks[2].clips[1].source_in_frame, 15);
            step_history(&mut session, false).unwrap();
            assert_eq!(session.project.tracks, unsplit_audio);
            edit(
                &mut session,
                TimelineEdit::Trim {
                    clip_id: voice.clone(),
                    source_in_frame: 30,
                    start_frame: 30,
                    duration_frames: 90,
                },
            );
            edit(
                &mut session,
                TimelineEdit::TrackVolume {
                    track_id: "audio-1".into(),
                    volume: 0.5,
                },
            );
            edit(
                &mut session,
                TimelineEdit::ClipFade {
                    clip_id: voice,
                    fade_in_frames: 30,
                    fade_out_frames: 30,
                },
            );
            add(&mut session, &ffprobe, 1, 3);
            edit(
                &mut session,
                TimelineEdit::TrackState {
                    track_id: "audio-2".into(),
                    locked: false,
                    muted: true,
                },
            );

            let project_path = directory.join("剪辑验收.kaicut");
            save_project(&project_path, &session.project).unwrap();
            let reopened = read_project(&project_path).unwrap();
            assert_eq!(reopened, session.project);
            let track = &reopened.tracks[2];
            let clip = &track.clips[0];
            assert_eq!(
                (
                    clip.source_in_frame,
                    clip.duration_frames,
                    clip.source_duration_frames
                ),
                (30, 90, 120)
            );
            assert_eq!((clip.fade_in_frames, clip.fade_out_frames), (30, 30));
            assert_eq!(track.volume, 0.5);
            assert_eq!(audio_gain(track, clip, 30), 0.0);
            assert!((audio_gain(track, clip, 45) - 0.25).abs() < 0.0001);
            assert_eq!(audio_gain(track, clip, 75), 0.5);
            assert_eq!(audio_gain(track, clip, 119), 0.0);
            assert!(reopened.tracks[3].muted);
            for clip in reopened.tracks.iter().flat_map(|track| &track.clips) {
                assert!(clip.source_in_frame + clip.duration_frames <= clip.source_duration_frames);
            }
            let output = directory.join("workflow-sample.mp4");
            let snapshot = ExportSnapshot {
                format_version: EXPORT_SNAPSHOT_FORMAT_VERSION,
                job_id: new_job_id(),
                created_at: timestamp(),
                project_id: reopened.project_id.clone(),
                revision: session.revision,
                output_path: output.to_string_lossy().into_owned(),
                duration_frames: 120,
                media: media_snapshot(&reopened),
                project: reopened,
            };
            assert!(snapshot.media.iter().all(|media| media.state == "online"));
            let snapshot_path =
                write_snapshot_to_dir(&directory.join("export-jobs"), &snapshot).unwrap();
            let persisted: ExportSnapshot =
                serde_json::from_slice(&fs::read(&snapshot_path).unwrap()).unwrap();
            assert_eq!(persisted, snapshot);
            // Editing after snapshot creation must not leak into the rendered result.
            edit(
                &mut session,
                TimelineEdit::TrackVolume {
                    track_id: "audio-1".into(),
                    volume: 0.0,
                },
            );
            assert_eq!(snapshot.project.tracks[2].volume, 0.5);
            let script = directory.join("filters.txt");
            let plan =
                crate::export_render::render_plan(&persisted, &ffprobe, &output, &script).unwrap();
            fs::write(&script, &plan.filter_graph).unwrap();
            assert!(!output.exists(), "each run must use a fresh output");
            let progress = run_tool(&ffmpeg, &plan.args);
            assert!(String::from_utf8_lossy(&progress).contains("progress=end"));
            fs::write(directory.join("progress.txt"), progress).unwrap();
            let metadata = run_tool(
                &ffprobe,
                &[
                    "-v".into(),
                    "error".into(),
                    "-show_streams".into(),
                    "-show_format".into(),
                    "-of".into(),
                    "json".into(),
                    output.as_os_str().into(),
                ],
            );
            validate_output_metadata(&metadata, &persisted).unwrap();
            fs::write(directory.join("workflow-sample.ffprobe.json"), metadata).unwrap();
            let rgb_path = directory.join("decoded.rgb");
            run_tool(
                &ffmpeg,
                &[
                    "-nostdin".into(),
                    "-n".into(),
                    "-i".into(),
                    output.as_os_str().into(),
                    "-map".into(),
                    "0:v:0".into(),
                    "-vf".into(),
                    "scale=8:8,format=rgb24".into(),
                    "-c:v".into(),
                    "rawvideo".into(),
                    "-f".into(),
                    "rawvideo".into(),
                    rgb_path.as_os_str().into(),
                ],
            );
            let rgb = fs::read(rgb_path).unwrap();
            assert_eq!(rgb.len(), 120 * 8 * 8 * 3);
            let frame = |index: usize| &rgb[index * 192..(index + 1) * 192];
            assert!(
                frame(5)
                    .chunks_exact(3)
                    .all(|pixel| pixel[0] > 180 && pixel[1] < 30 && pixel[2] < 30)
            );
            for gap in [15, 25, 60, 75, 89] {
                assert!(
                    frame(gap).iter().all(|value| *value < 8),
                    "gap frame {gap} must be black"
                );
            }
            assert!(
                frame(45)
                    .chunks_exact(3)
                    .all(|pixel| pixel[2] > 180 && pixel[0] < 30)
            );
            assert!(
                frame(100)
                    .chunks_exact(3)
                    .all(|pixel| pixel[0] > 180 && pixel[1] > 180 && pixel[2] < 30)
            );
            let pcm_path = directory.join("decoded.wav");
            run_tool(
                &ffmpeg,
                &[
                    "-nostdin".into(),
                    "-n".into(),
                    "-i".into(),
                    output.as_os_str().into(),
                    "-map".into(),
                    "0:a:0".into(),
                    "-c:a".into(),
                    "pcm_s16le".into(),
                    pcm_path.as_os_str().into(),
                ],
            );
            let pcm = fs::read(pcm_path).unwrap();
            let silence = rms(&pcm, 0.1, 0.8);
            let full = rms(&pcm, 2.1, 2.7);
            let fade_in = rms(&pcm, 1.18, 1.22);
            let fade_out = rms(&pcm, 3.68, 3.72);
            assert!(
                silence < 0.0005,
                "gap and muted lane must remain silent: {silence}"
            );
            assert!(
                (full - 0.1).abs() < 0.015,
                "saved gain was not rendered: {full}"
            );
            assert!(
                (fade_in / full - 0.2).abs() < 0.06,
                "saved fade-in was not rendered"
            );
            assert!(
                (fade_out / full - (4.0 - 1.0 / 30.0 - 3.7)).abs() < 0.07,
                "saved fade-out was not rendered"
            );
            for (path, (hash, modified)) in source_paths.iter().zip(&originals) {
                assert_eq!(
                    &Sha256::digest(fs::read(path).unwrap()),
                    hash,
                    "source bytes must stay intact"
                );
                assert_eq!(&fs::metadata(path).unwrap().modified().unwrap(), modified);
            }
            assert_eq!(read_project(&project_path).unwrap(), snapshot.project);
            assert_eq!(media_snapshot(&snapshot.project), snapshot.media);
            fs::write(directory.join("validation.json"), serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true, "completedAt": timestamp(), "frames": 120, "seconds": 4,
                "project": project_path, "output": output, "snapshot": snapshot_path,
                "silenceRms": silence, "steadyRms": full, "fadeInRms": fade_in, "fadeOutRms": fade_out,
                "sourceSha256": source_paths.iter().zip(&originals).map(|(path, (hash, _))|
                    serde_json::json!({"path": path, "sha256": format!("{hash:x}")})).collect::<Vec<_>>(),
                "coverage": "production import/edit/history/save/open/snapshot/planner/encode/metadata validation; no native UI or job supervisor",
            })).unwrap()).unwrap();
        }
    }
}
