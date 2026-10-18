//! Bounded FFmpeg graph planner. Paths are arguments, never filter syntax.
use crate::{
    export_job::ExportSnapshot,
    media_engine::{MediaProbeState, probe_one},
    project::{MediaAsset, MediaKind, TimelineClip, TrackKind},
};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsString,
    path::Path,
};

pub(crate) const OUTPUT_WIDTH: u32 = 1920;
pub(crate) const OUTPUT_HEIGHT: u32 = 1080;
pub(crate) const OUTPUT_SAMPLE_RATE: u32 = 48_000;
const MAX_RENDER_CLIPS: usize = 64;
const MAX_RENDER_INPUTS: usize = 64;
const MAX_WINDOWS_ARGUMENT_UNITS: usize = 24_000;

pub(crate) struct RenderPlan {
    pub args: Vec<OsString>,
    pub filter_graph: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Segment {
    start: u64,
    end: u64,
    /// Project track and clip indices. None denotes a black/silent gap.
    selection: Option<(usize, usize)>,
}

fn seconds(snapshot: &ExportSnapshot, frames: u64) -> String {
    format!(
        "{:.9}",
        frames as f64 * f64::from(snapshot.project.timebase.fps_denominator)
            / f64::from(snapshot.project.timebase.fps_numerator)
    )
}

fn video_segments(snapshot: &ExportSnapshot) -> Vec<Segment> {
    let mut boundaries = BTreeSet::from([0, snapshot.duration_frames]);
    for track in snapshot
        .project
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video && !track.muted)
    {
        for clip in &track.clips {
            boundaries.insert(clip.start_frame);
            boundaries.insert(clip.start_frame + clip.duration_frames);
        }
    }
    let points: Vec<_> = boundaries.into_iter().collect();
    let mut result: Vec<Segment> = Vec::new();
    for pair in points.windows(2) {
        let selection = snapshot
            .project
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.kind == TrackKind::Video && !track.muted)
            .find_map(|(ti, track)| {
                track
                    .clips
                    .iter()
                    .enumerate()
                    .find(|(_, clip)| {
                        pair[0] >= clip.start_frame
                            && pair[0] < clip.start_frame + clip.duration_frames
                    })
                    .map(|(ci, _)| (ti, ci))
            });
        if let Some(last) = result.last_mut().filter(|last| last.selection == selection) {
            last.end = pair[1];
        } else {
            result.push(Segment {
                start: pair[0],
                end: pair[1],
                selection,
            });
        }
    }
    result
}

#[derive(Clone, Copy)]
struct StreamInfo {
    audio: bool,
}

struct Planner<'a> {
    snapshot: &'a ExportSnapshot,
    info: &'a HashMap<String, StreamInfo>,
    args: Vec<OsString>,
    graph: Vec<String>,
    inputs: usize,
}

impl Planner<'_> {
    fn rate(&self) -> String {
        format!(
            "{}/{}",
            self.snapshot.project.timebase.fps_numerator,
            self.snapshot.project.timebase.fps_denominator
        )
    }

    fn input(&mut self, media: &MediaAsset, source_frame: u64) -> Result<usize, String> {
        if self.inputs >= MAX_RENDER_INPUTS {
            return Err(
                "本阶段导出最多支持 64 个可见视频分段与音频片段，请缩短时间线或减少交叠".into(),
            );
        }
        self.args.extend([
            "-protocol_whitelist".into(),
            "file".into(),
            "-format_whitelist".into(),
            "mov,mp4,m4a,3gp,3g2,mj2,mp3,wav,image2,image2pipe".into(),
            "-threads".into(),
            "1".into(),
        ]);
        if media.kind == MediaKind::Image {
            self.args.extend([
                "-f".into(),
                "image2".into(),
                "-pattern_type".into(),
                "none".into(),
                "-loop".into(),
                "1".into(),
                "-framerate".into(),
                self.rate().into(),
            ]);
        } else {
            self.args
                .extend(["-ss".into(), seconds(self.snapshot, source_frame).into()]);
        }
        self.args.extend([
            "-i".into(),
            media.source_path.as_deref().ok_or("素材没有路径")?.into(),
        ]);
        let index = self.inputs;
        self.inputs += 1;
        Ok(index)
    }

    fn silence(&mut self, duration: u64, label: &str) {
        self.graph.push(format!("anullsrc=r={OUTPUT_SAMPLE_RATE}:cl=stereo,atrim=duration={},asetpts=PTS-STARTPTS[{label}]", seconds(self.snapshot, duration)));
    }

    fn black(&mut self, frames: u64, label: &str) {
        self.graph.push(format!("color=c=black:s={OUTPUT_WIDTH}x{OUTPUT_HEIGHT}:r={},trim=end_frame={frames},setpts=PTS-STARTPTS[{label}]", self.rate()));
    }

    fn audio(
        &mut self,
        input: usize,
        frames: u64,
        volume: f32,
        clip: Option<&TimelineClip>,
        label: &str,
    ) {
        let duration = seconds(self.snapshot, frames);
        let mut filters = format!(
            "[{input}:a:0]atrim=duration={duration},asetpts=PTS-STARTPTS,aresample={OUTPUT_SAMPLE_RATE}:first_pts=0,aformat=sample_fmts=fltp:sample_rates={OUTPUT_SAMPLE_RATE}:channel_layouts=stereo,volume={volume:.9}"
        );
        if let Some(clip) = clip {
            if clip.fade_in_frames > 0 {
                filters.push_str(&format!(
                    ",afade=t=in:st=0:d={}:curve=tri",
                    seconds(self.snapshot, clip.fade_in_frames)
                ));
            }
            if clip.fade_out_frames > 0 {
                // Rust's gain is (duration-local-1)/fadeOut. The final project
                // frame is silent. A full-length fade has a negative ideal
                // start; scale the available interval to preserve that slope.
                let fade_end = frames - 1;
                if fade_end == 0 {
                    filters.push_str(",volume=0");
                } else {
                    let fade_duration = clip.fade_out_frames.min(fade_end);
                    if clip.fade_out_frames > fade_end {
                        filters.push_str(&format!(
                            ",volume={:.9}",
                            fade_end as f64 / clip.fade_out_frames as f64
                        ));
                    }
                    filters.push_str(&format!(
                        ",afade=t=out:st={}:d={}:curve=tri",
                        seconds(self.snapshot, fade_end - fade_duration),
                        seconds(self.snapshot, fade_duration)
                    ));
                }
            }
        }
        filters.push_str(&format!(
            ",apad=whole_dur={duration},atrim=duration={duration},asetpts=PTS-STARTPTS[{label}]"
        ));
        self.graph.push(filters);
    }

    fn build(mut self, output: &Path, filter_script: &Path) -> Result<RenderPlan, String> {
        let mut video_labels = String::new();
        let segments = video_segments(self.snapshot);
        for (index, segment) in segments.iter().enumerate() {
            let frames = segment.end - segment.start;
            let v = format!("vs{index}");
            let a = format!("va{index}");
            if let Some((ti, ci)) = segment.selection {
                let clip = &self.snapshot.project.tracks[ti].clips[ci];
                let media = self
                    .snapshot
                    .project
                    .media_refs
                    .iter()
                    .find(|media| media.id == clip.media_id)
                    .ok_or("找不到片段素材")?
                    .clone();
                let source_frame = clip.source_in_frame + segment.start - clip.start_frame;
                let input = self.input(&media, source_frame)?;
                // Per-segment finite black background fixes output frame count
                // and makes images opaque. Last-frame repeat covers input
                // timestamp rounding, never an arbitrarily missing source.
                let base = format!("vb{index}");
                let picture = format!("vp{index}");
                self.black(frames, &base);
                let format = if media.kind == MediaKind::Image {
                    "rgba"
                } else {
                    "yuv420p"
                };
                // Fit the display aspect (including pixel aspect), then turn
                // the scaled image into square pixels before letterboxing.
                // Setting SAR=1 after a coded-dimension-only fit distorts DV
                // and other anamorphic sources despite a valid MP4 header.
                self.graph.push(format!("[{input}:V:0]setpts=PTS-STARTPTS,fps={}:start_time=0,trim=end_frame={frames},scale=w='max(2,trunc(min({OUTPUT_WIDTH},{OUTPUT_HEIGHT}*dar)/2)*2)':h='max(2,trunc(min({OUTPUT_HEIGHT},{OUTPUT_WIDTH}/dar)/2)*2)',setsar=1,pad={OUTPUT_WIDTH}:{OUTPUT_HEIGHT}:(ow-iw)/2:(oh-ih)/2:color=black,format={format}[{picture}]", self.rate()));
                self.graph.push(format!("[{base}][{picture}]overlay=eof_action=repeat:shortest=0,trim=end_frame={frames},setpts=PTS-STARTPTS[{v}]"));
                if media.kind == MediaKind::Video
                    && self.info.get(&media.id).is_some_and(|info| info.audio)
                {
                    self.audio(input, frames, 1.0, None, &a);
                } else {
                    self.silence(frames, &a);
                }
            } else {
                self.black(frames, &v);
                self.silence(frames, &a);
            }
            video_labels.push_str(&format!("[{v}][{a}]"));
        }
        self.graph.push(format!(
            "{video_labels}concat=n={}:v=1:a=1[vc][embedded]",
            segments.len()
        ));
        self.graph.push(format!(
            "[vc]trim=end_frame={},setpts=N*{}/({}*TB),format=nv12[vout]",
            self.snapshot.duration_frames,
            self.snapshot.project.timebase.fps_denominator,
            self.snapshot.project.timebase.fps_numerator,
        ));
        let mut mix_labels = "[embedded]".to_string();
        let mut lanes = 1;
        for (ti, track) in self.snapshot.project.tracks.iter().enumerate() {
            if track.kind != TrackKind::Audio
                || track.muted
                || track.volume == 0.0
                || track.clips.is_empty()
            {
                continue;
            }
            let mut clips: Vec<_> = track.clips.iter().collect();
            clips.sort_by_key(|clip| clip.start_frame);
            let mut cursor = 0;
            let mut labels = String::new();
            let mut pieces = 0;
            for (ci, clip) in clips.into_iter().enumerate() {
                if clip.start_frame > cursor {
                    let label = format!("ag{ti}_{ci}");
                    self.silence(clip.start_frame - cursor, &label);
                    labels.push_str(&format!("[{label}]"));
                    pieces += 1;
                }
                let media = self
                    .snapshot
                    .project
                    .media_refs
                    .iter()
                    .find(|media| media.id == clip.media_id)
                    .ok_or("找不到音频素材")?
                    .clone();
                let input = self.input(&media, clip.source_in_frame)?;
                let label = format!("ac{ti}_{ci}");
                self.audio(
                    input,
                    clip.duration_frames,
                    track.volume,
                    Some(clip),
                    &label,
                );
                labels.push_str(&format!("[{label}]"));
                pieces += 1;
                cursor = clip.start_frame + clip.duration_frames;
            }
            if cursor < self.snapshot.duration_frames {
                let label = format!("tail{ti}");
                self.silence(self.snapshot.duration_frames - cursor, &label);
                labels.push_str(&format!("[{label}]"));
                pieces += 1;
            }
            self.graph
                .push(format!("{labels}concat=n={pieces}:v=0:a=1[lane{ti}]"));
            mix_labels.push_str(&format!("[lane{ti}]"));
            lanes += 1;
        }
        self.graph.push(format!("{mix_labels}amix=inputs={lanes}:duration=first:dropout_transition=0:normalize=0,atrim=duration={},aresample={OUTPUT_SAMPLE_RATE},aformat=channel_layouts=stereo[aout]", seconds(self.snapshot, self.snapshot.duration_frames)));
        self.args.extend([
            "-filter_complex_script".into(),
            filter_script.as_os_str().into(),
            "-map".into(),
            "[vout]".into(),
            "-map".into(),
            "[aout]".into(),
            "-map_metadata".into(),
            "-1".into(),
            "-map_chapters".into(),
            "-1".into(),
            "-c:v".into(),
            "h264_mf".into(),
            "-b:v".into(),
            "8M".into(),
            "-pix_fmt".into(),
            "nv12".into(),
            "-r".into(),
            self.rate().into(),
            "-t".into(),
            seconds(self.snapshot, self.snapshot.duration_frames).into(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "192k".into(),
            "-ar".into(),
            OUTPUT_SAMPLE_RATE.to_string().into(),
            "-ac".into(),
            "2".into(),
            "-movflags".into(),
            "+faststart".into(),
            "-progress".into(),
            "pipe:1".into(),
            "-nostats".into(),
            "-f".into(),
            "mp4".into(),
            output.as_os_str().into(),
        ]);
        // Quoting may double backslashes on Windows. Conservatively count
        // twice every UTF16 unit and leave room for the executable path.
        let units: usize = self
            .args
            .iter()
            .map(|arg| 2 * arg.to_string_lossy().encode_utf16().count() + 3)
            .sum();
        if units > MAX_WINDOWS_ARGUMENT_UNITS {
            return Err("导出命令行过长，请缩短素材路径或减少片段数量".into());
        }
        Ok(RenderPlan {
            args: self.args,
            filter_graph: self.graph.join(";\n"),
        })
    }
}

fn validate_snapshot(snapshot: &ExportSnapshot) -> Result<(), String> {
    snapshot.project.validate()?;
    if snapshot.format_version != crate::export_job::EXPORT_SNAPSHOT_FORMAT_VERSION
        || snapshot.project_id != snapshot.project.project_id
    {
        return Err("导出快照身份或版本无效".into());
    }
    let duration = snapshot
        .project
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .map(|clip| clip.start_frame + clip.duration_frames)
        .max()
        .unwrap_or(0);
    if duration == 0 || duration != snapshot.duration_frames {
        return Err("导出快照时长与时间线不一致".into());
    }
    let count: usize = snapshot
        .project
        .tracks
        .iter()
        .map(|track| track.clips.len())
        .sum();
    if count > MAX_RENDER_CLIPS {
        return Err("本阶段导出最多支持 64 个片段，请拆分项目后导出".into());
    }
    Ok(())
}

fn plan_graph(
    snapshot: &ExportSnapshot,
    info: &HashMap<String, StreamInfo>,
    output: &Path,
    filter_script: &Path,
) -> Result<RenderPlan, String> {
    validate_snapshot(snapshot)?;
    Planner {
        snapshot,
        info,
        args: vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            "-y".into(),
            "-filter_complex_threads".into(),
            "1".into(),
        ],
        graph: Vec::new(),
        inputs: 0,
    }
    .build(output, filter_script)
}

#[cfg(test)]
pub(crate) fn render_plan(
    snapshot: &ExportSnapshot,
    ffprobe: &Path,
    output: &Path,
    filter_script: &Path,
) -> Result<RenderPlan, String> {
    render_plan_cancellable(snapshot, ffprobe, output, filter_script, || false)
}

pub(crate) fn render_plan_cancellable(
    snapshot: &ExportSnapshot,
    ffprobe: &Path,
    output: &Path,
    filter_script: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<RenderPlan, String> {
    validate_snapshot(snapshot)?;
    if !output.is_absolute() || !filter_script.is_absolute() {
        return Err("导出输出与滤镜脚本必须使用绝对路径".into());
    }
    let mut info = HashMap::new();
    let started = std::time::Instant::now();
    for clip in snapshot
        .project
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
    {
        if cancelled() {
            return Err("导出已取消".into());
        }
        if started.elapsed() > std::time::Duration::from_secs(60) {
            return Err("导出素材探测超过 60 秒，请检查素材磁盘后重试".into());
        }
        if info.contains_key(&clip.media_id) {
            continue;
        }
        let media = snapshot
            .project
            .media_refs
            .iter()
            .find(|media| media.id == clip.media_id)
            .ok_or("素材引用不存在")?;
        let path = Path::new(media.source_path.as_deref().ok_or("素材路径不存在")?);
        if !path.is_absolute() || !path.is_file() {
            return Err(format!("导出素材必须是本地绝对文件路径：{}", media.name));
        }
        let probe = probe_one(&ffprobe.to_path_buf(), media);
        if cancelled() {
            return Err("导出已取消".into());
        }
        if probe.state != MediaProbeState::Ready {
            return Err(format!(
                "无法读取导出素材 {}：{}",
                media.name,
                probe.error.unwrap_or_else(|| "探测失败".into())
            ));
        }
        if media.kind != MediaKind::Audio
            && (!probe.width.is_some_and(|value| value > 0)
                || !probe.height.is_some_and(|value| value > 0))
        {
            return Err(format!("素材缺少有效视频或图片流：{}", media.name));
        }
        let audio = probe.audio_channels.is_some_and(|channels| channels > 0);
        if media.kind == MediaKind::Audio && !audio {
            return Err(format!("素材缺少音频流：{}", media.name));
        }
        if media.kind != MediaKind::Image {
            let required_frame = snapshot
                .project
                .tracks
                .iter()
                .flat_map(|track| &track.clips)
                .filter(|other| other.media_id == media.id)
                .map(|other| other.source_in_frame + other.duration_frames)
                .max()
                .unwrap_or(0);
            let required_ms = required_frame as f64
                * f64::from(snapshot.project.timebase.fps_denominator)
                * 1000.0
                / f64::from(snapshot.project.timebase.fps_numerator);
            if probe
                .duration_ms
                .is_none_or(|actual| (actual as f64) + 1.0 < required_ms)
            {
                return Err(format!("素材实际时长不足以覆盖片段：{}", media.name));
            }
        }
        info.insert(media.id.clone(), StreamInfo { audio });
    }
    plan_graph(snapshot, &info, output, filter_script)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{KaicutProject, TimelineClip};

    fn snapshot() -> ExportSnapshot {
        let project = KaicutProject::new();
        ExportSnapshot {
            format_version: 1,
            job_id: "job-render-test".into(),
            created_at: project.created_at.clone(),
            project_id: project.project_id.clone(),
            revision: 1,
            output_path: "C:\\test.mp4".into(),
            duration_frames: 90,
            project,
            media: vec![],
        }
    }
    fn clip(
        snapshot: &mut ExportSnapshot,
        track: usize,
        id: &str,
        start: u64,
        duration: u64,
        source_in: u64,
    ) {
        let kind = if snapshot.project.tracks[track].kind == TrackKind::Audio {
            MediaKind::Audio
        } else {
            MediaKind::Video
        };
        let extension = if kind == MediaKind::Audio {
            "wav"
        } else {
            "mp4"
        };
        snapshot.project.media_refs.push(MediaAsset {
            id: id.into(),
            name: id.into(),
            kind,
            extension: extension.into(),
            size: 0,
            last_modified: 0,
            source_path: Some(format!("C:\\path;[unsafe]\\{id}.{extension}")),
        });
        let track_id = snapshot.project.tracks[track].id.clone();
        snapshot.project.tracks[track].clips.push(TimelineClip {
            id: id.into(),
            media_id: id.into(),
            track_id,
            start_frame: start,
            duration_frames: duration,
            source_in_frame: source_in,
            source_duration_frames: source_in + duration,
            fade_in_frames: 0,
            fade_out_frames: 0,
        });
    }
    fn plan(snapshot: &ExportSnapshot) -> RenderPlan {
        let info = snapshot
            .project
            .media_refs
            .iter()
            .map(|m| {
                (
                    m.id.clone(),
                    StreamInfo {
                        audio: m.kind == MediaKind::Audio,
                    },
                )
            })
            .collect();
        plan_graph(
            snapshot,
            &info,
            Path::new("out.mp4"),
            Path::new("filters.txt"),
        )
        .unwrap()
    }

    #[test]
    fn shared_preview_fixture_matches_export_frames_and_audio_gains() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ExpectedSource {
            clip_id: String,
            source_seconds: f64,
            gain: Option<f64>,
        }
        #[derive(serde::Deserialize)]
        struct ExpectedFrame {
            frame: u64,
            visual: Option<ExpectedSource>,
            audio: Vec<ExpectedSource>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Fixture {
            project: KaicutProject,
            duration_frames: u64,
            frames: Vec<ExpectedFrame>,
        }

        // Both runtimes consume fixed outcomes, independent of media decoders
        // and machine-local validation artifacts.
        let fixture: Fixture =
            serde_json::from_str(include_str!("../../src/test/fixtures/preview-parity.json"))
                .unwrap();
        let mut snapshot = snapshot();
        snapshot.project_id = fixture.project.project_id.clone();
        snapshot.project = fixture.project;
        snapshot.duration_frames = fixture.duration_frames;
        validate_snapshot(&snapshot).unwrap();
        let segments = video_segments(&snapshot);
        let graph = plan(&snapshot).filter_graph;
        assert!(graph.contains("amix=inputs=2"));

        for expected in fixture.frames {
            let visual = segments
                .iter()
                .find(|segment| expected.frame >= segment.start && expected.frame < segment.end)
                .and_then(|segment| segment.selection)
                .map(|(ti, ci)| &snapshot.project.tracks[ti].clips[ci]);
            assert_eq!(
                visual.map(|clip| clip.id.as_str()),
                expected
                    .visual
                    .as_ref()
                    .map(|source| source.clip_id.as_str()),
                "visual selection at frame {}",
                expected.frame
            );
            if let (Some(clip), Some(source)) = (visual, expected.visual) {
                let source_seconds: f64 = seconds(
                    &snapshot,
                    clip.source_in_frame + expected.frame - clip.start_frame,
                )
                .parse()
                .unwrap();
                assert!((source_seconds - source.source_seconds).abs() < 1e-8);
            }

            let audio: Vec<_> = snapshot
                .project
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Audio && !track.muted)
                .flat_map(|track| {
                    track
                        .clips
                        .iter()
                        .filter(|clip| {
                            expected.frame >= clip.start_frame
                                && expected.frame < clip.start_frame + clip.duration_frames
                        })
                        .map(move |clip| (track, clip))
                })
                .collect();
            assert_eq!(audio.len(), expected.audio.len());
            for ((track, clip), source) in audio.into_iter().zip(expected.audio) {
                assert_eq!(clip.id, source.clip_id);
                let gain = crate::timeline::audio_gain(track, clip, expected.frame);
                assert!((f64::from(gain) - source.gain.unwrap()).abs() < 1e-6);
                let source_seconds: f64 = seconds(
                    &snapshot,
                    clip.source_in_frame + expected.frame - clip.start_frame,
                )
                .parse()
                .unwrap();
                assert!((source_seconds - source.source_seconds).abs() < 1e-8);
            }
        }
    }

    #[test]
    fn rational_timebase_and_source_trim_are_not_integer_fps() {
        let mut snapshot = snapshot();
        snapshot.project.timebase.fps_numerator = 30000;
        snapshot.project.timebase.fps_denominator = 1001;
        clip(&mut snapshot, 0, "v", 30, 60, 15);
        assert_eq!(seconds(&snapshot, 30), "1.001000000");
        let plan = plan(&snapshot);
        assert!(
            plan.args
                .windows(2)
                .any(|args| args == [OsString::from("-ss"), OsString::from("0.500500000")])
        );
        assert!(plan.filter_graph.contains("fps=30000/1001"));
        assert!(plan.filter_graph.contains("trim=end_frame=30"));
        assert!(!plan.filter_graph.contains("unsafe"));
    }

    #[test]
    fn video_intervals_choose_first_unmuted_lane_and_preserve_gaps() {
        let mut snapshot = snapshot();
        clip(&mut snapshot, 0, "top", 30, 30, 0);
        clip(&mut snapshot, 1, "bottom", 0, 90, 0);
        let segments = video_segments(&snapshot);
        assert_eq!(
            segments,
            vec![
                Segment {
                    start: 0,
                    end: 30,
                    selection: Some((1, 0))
                },
                Segment {
                    start: 30,
                    end: 60,
                    selection: Some((0, 0))
                },
                Segment {
                    start: 60,
                    end: 90,
                    selection: Some((1, 0))
                }
            ]
        );
        snapshot.project.tracks[1].muted = true;
        assert_eq!(video_segments(&snapshot)[0].selection, None);
        assert_eq!(video_segments(&snapshot)[2].selection, None);
    }

    #[test]
    fn audio_fades_end_on_final_frame_and_muted_lanes_have_no_input() {
        let mut snapshot = snapshot();
        clip(&mut snapshot, 2, "audible", 30, 60, 10);
        clip(&mut snapshot, 3, "muted", 0, 90, 0);
        snapshot.project.tracks[2].volume = 0.5;
        snapshot.project.tracks[2].clips[0].fade_in_frames = 15;
        snapshot.project.tracks[2].clips[0].fade_out_frames = 30;
        snapshot.project.tracks[3].muted = true;
        let plan = plan(&snapshot);
        assert!(plan.filter_graph.contains("volume=0.500000000"));
        assert!(
            plan.filter_graph
                .contains("afade=t=out:st=0.966666667:d=1.000000000")
        );
        assert!(
            !plan
                .args
                .iter()
                .any(|arg| arg.to_string_lossy().contains("muted.wav"))
        );
        assert!(plan.filter_graph.contains("amix=inputs=2"));
        assert!(plan.filter_graph.contains("normalize=0"));
    }

    #[test]
    fn full_length_and_single_frame_fades_follow_rust_gain() {
        let mut snapshot = snapshot();
        snapshot.duration_frames = 30;
        clip(&mut snapshot, 2, "a", 0, 30, 0);
        snapshot.project.tracks[2].clips[0].fade_out_frames = 30;
        let graph = plan(&snapshot).filter_graph;
        assert!(graph.contains("volume=0.966666667,afade=t=out:st=0.000000000:d=0.966666667"));
        snapshot.duration_frames = 1;
        snapshot.project.tracks[2].clips[0].duration_frames = 1;
        snapshot.project.tracks[2].clips[0].fade_out_frames = 1;
        assert!(plan(&snapshot).filter_graph.contains(",volume=0,apad="));
    }

    #[test]
    fn images_are_single_literal_paths_and_output_has_no_unapproved_encoder() {
        let mut snapshot = snapshot();
        clip(&mut snapshot, 0, "image", 0, 90, 0);
        snapshot.project.media_refs[0].kind = MediaKind::Image;
        snapshot.project.media_refs[0].extension = "png".into();
        snapshot.project.media_refs[0].source_path = Some("C:\\image%03d.png".into());
        let plan = plan(&snapshot);
        assert!(
            plan.args
                .windows(2)
                .any(|args| args == [OsString::from("-pattern_type"), OsString::from("none")])
        );
        assert!(
            plan.args
                .windows(2)
                .any(|args| args == [OsString::from("-c:v"), OsString::from("h264_mf")])
        );
        assert!(
            !plan
                .args
                .iter()
                .any(|arg| arg.to_string_lossy().contains("x264"))
        );
        assert!(!plan.filter_graph.contains(":a:0]"));
    }

    #[test]
    fn rejects_inconsistent_snapshot_and_bounded_clip_limit() {
        let mut snapshot = snapshot();
        clip(&mut snapshot, 0, "v", 0, 30, 0);
        assert!(validate_snapshot(&snapshot).is_err());
        snapshot.duration_frames = 65;
        snapshot.project.tracks[0].clips.clear();
        snapshot.project.media_refs.clear();
        for index in 0..65 {
            clip(&mut snapshot, 0, &format!("v{index}"), index, 1, 0);
        }
        assert!(validate_snapshot(&snapshot).unwrap_err().contains("64"));
    }

    #[test]
    fn cancellation_is_checked_before_probing_any_source() {
        let mut snapshot = snapshot();
        clip(&mut snapshot, 0, "v", 0, 90, 0);
        let dir = tempfile::tempdir().unwrap();
        let result = render_plan_cancellable(
            &snapshot,
            &dir.path().join("nonexistent-ffprobe.exe"),
            &dir.path().join("out.mp4"),
            &dir.path().join("filter.txt"),
            || true,
        );
        assert_eq!(result.err().as_deref(), Some("导出已取消"));
    }

    fn run_tool(binary: &Path, args: &[OsString]) -> Vec<u8> {
        use std::{
            process::{Command, Stdio},
            time::Duration,
        };
        let mut command = Command::new(binary);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::media_engine::hide_console(&mut command);
        let child = command.spawn().expect("test sidecar must start");
        let (status, stdout, stderr) =
            crate::media_engine::collect_child_output(child, Duration::from_secs(90)).unwrap();
        assert!(
            status.success(),
            "Sidecar failed without any codec fallback: {}",
            String::from_utf8_lossy(&stderr)
        );
        stdout
    }

    #[test]
    #[cfg(target_os = "windows")]
    #[ignore = "requires packaged LGPL sidecars and Windows h264_mf; checks decoded display proportions"]
    fn real_sidecar_preserves_anamorphic_display_aspect_ratio() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let ffmpeg = root.join("binaries/ffmpeg-x86_64-pc-windows-msvc.exe");
        let ffprobe = root.join("binaries/ffprobe-x86_64-pc-windows-msvc.exe");
        let dir = tempfile::tempdir().unwrap();
        for (name, sar, expected_sar) in [("wide", "4/3", "4:3"), ("narrow", "1/2", "1:2")] {
            let source = dir.path().join(format!("{name}.mp4"));
            run_tool(
                &ffmpeg,
                &[
                    "-v".into(),
                    "error".into(),
                    "-nostdin".into(),
                    "-f".into(),
                    "lavfi".into(),
                    "-i".into(),
                    format!("color=c=red:s=160x120:r=30,setsar={sar}").into(),
                    "-t".into(),
                    "1".into(),
                    "-c:v".into(),
                    "h264_mf".into(),
                    "-b:v".into(),
                    "1M".into(),
                    "-pix_fmt".into(),
                    "nv12".into(),
                    source.as_os_str().into(),
                ],
            );
            let input: serde_json::Value = serde_json::from_slice(&run_tool(
                &ffprobe,
                &[
                    "-v".into(),
                    "error".into(),
                    "-show_streams".into(),
                    "-of".into(),
                    "json".into(),
                    source.as_os_str().into(),
                ],
            ))
            .unwrap();
            assert_eq!(input["streams"][0]["sample_aspect_ratio"], expected_sar);
            let output = dir.path().join(format!("{name}-output.mp4"));
            let script = dir.path().join(format!("{name}-filter.txt"));
            let mut snapshot = snapshot();
            snapshot.duration_frames = 30;
            clip(&mut snapshot, 0, name, 0, 30, 0);
            snapshot.project.media_refs[0].source_path =
                Some(source.to_string_lossy().into_owned());
            let plan = render_plan(&snapshot, &ffprobe, &output, &script).unwrap();
            std::fs::write(script, plan.filter_graph).unwrap();
            run_tool(&ffmpeg, &plan.args);
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
            crate::export_job::validate_output_metadata(&metadata, &snapshot).unwrap();
            let pixels = run_tool(
                &ffmpeg,
                &[
                    "-v".into(),
                    "error".into(),
                    "-i".into(),
                    output.as_os_str().into(),
                    "-vf".into(),
                    "scale=160:90,format=rgb24".into(),
                    "-frames:v".into(),
                    "1".into(),
                    "-f".into(),
                    "rawvideo".into(),
                    "-c:v".into(),
                    "rawvideo".into(),
                    "pipe:1".into(),
                ],
            );
            let pixel = |x: usize| &pixels[(45 * 160 + x) * 3..(45 * 160 + x) * 3 + 3];
            let red = |x| pixel(x)[0] > 180 && pixel(x)[1] < 25 && pixel(x)[2] < 25;
            let black = |x| pixel(x).iter().all(|channel| *channel < 10);
            if name == "wide" {
                assert!(
                    red(8) && red(151),
                    "16:9 display image must fill output width, not get false pillarboxes"
                );
            } else {
                assert!(
                    black(30) && red(55) && red(104) && black(130),
                    "2:3 display image must retain narrower geometry after square-pixel export"
                );
            }
        }
    }

    fn wav(path: &Path, silent_seconds: usize, duration_seconds: usize, amplitude: f64) {
        let samples = duration_seconds * OUTPUT_SAMPLE_RATE as usize;
        let data_bytes = (samples * 2) as u32;
        let mut data = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        data.extend_from_slice(b"WAVEfmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&OUTPUT_SAMPLE_RATE.to_le_bytes());
        data.extend_from_slice(&(OUTPUT_SAMPLE_RATE * 2).to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&16u16.to_le_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&data_bytes.to_le_bytes());
        for index in 0..samples {
            let value = if index < silent_seconds * OUTPUT_SAMPLE_RATE as usize {
                0.0
            } else {
                amplitude
                    * (std::f64::consts::TAU * 440.0 * index as f64 / OUTPUT_SAMPLE_RATE as f64)
                        .sin()
            };
            data.extend_from_slice(&((value * i16::MAX as f64) as i16).to_le_bytes());
        }
        std::fs::write(path, data).unwrap();
    }

    fn rms(data: &[u8], start: f64, end: f64) -> f64 {
        // Decoded test WAV is pcm_s16le stereo and its data chunk can follow
        // metadata; locate the actual RIFF data chunk instead of assuming 44.
        let mut cursor = 12;
        let payload = loop {
            let length =
                u32::from_le_bytes(data[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
            if &data[cursor..cursor + 4] == b"data" {
                break &data[cursor + 8..cursor + 8 + length];
            }
            cursor += 8 + length + length % 2;
        };
        let from = (start * OUTPUT_SAMPLE_RATE as f64) as usize;
        let to = (end * OUTPUT_SAMPLE_RATE as f64) as usize;
        let sum: f64 = payload[from * 4..to * 4]
            .chunks_exact(4)
            .map(|frame| {
                let sample = i16::from_le_bytes([frame[0], frame[1]]) as f64 / i16::MAX as f64;
                sample * sample
            })
            .sum();
        (sum / (to - from) as f64).sqrt()
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires packaged LGPL sidecars and an available Windows h264_mf encoder"]
    fn real_sidecar_renders_trim_gaps_gain_fades_mute_and_h264_aac() {
        let binary_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
        let ffmpeg = binary_dir.join("ffmpeg-x86_64-pc-windows-msvc.exe");
        let ffprobe = binary_dir.join("ffprobe-x86_64-pc-windows-msvc.exe");
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("源 片段;[literal].mp4");
        let audio = dir.path().join("源音频.wav");
        let muted = dir.path().join("静音轨.wav");
        let output = dir.path().join("output.mp4");
        let script = dir.path().join("filters.txt");
        // The first source second is red and the second blue. The timeline
        // selects only source second 1, exposing wrong sourceIn handling.
        run_tool(
            &ffmpeg,
            &[
                "-hide_banner".into(),
                "-nostdin".into(),
                "-y".into(),
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                "color=c=red:s=160x90:r=30:d=1".into(),
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                "color=c=blue:s=160x90:r=30:d=1".into(),
                "-filter_complex".into(),
                "[0:v][1:v]concat=n=2:v=1:a=0,format=nv12[v]".into(),
                "-map".into(),
                "[v]".into(),
                "-c:v".into(),
                "h264_mf".into(),
                "-b:v".into(),
                "1M".into(),
                "-t".into(),
                "2".into(),
                video.as_os_str().into(),
            ],
        );
        wav(&audio, 1, 4, 0.4);
        wav(&muted, 0, 4, 0.9);
        let mut snapshot = snapshot();
        snapshot.duration_frames = 120;
        clip(&mut snapshot, 0, "v", 30, 30, 30);
        clip(&mut snapshot, 2, "a", 30, 90, 30);
        clip(&mut snapshot, 3, "m", 0, 120, 0);
        snapshot.project.media_refs[0].source_path = Some(video.to_string_lossy().into());
        snapshot.project.media_refs[1].source_path = Some(audio.to_string_lossy().into());
        snapshot.project.media_refs[2].source_path = Some(muted.to_string_lossy().into());
        snapshot.project.tracks[2].volume = 0.5;
        snapshot.project.tracks[2].clips[0].fade_in_frames = 30;
        snapshot.project.tracks[2].clips[0].fade_out_frames = 30;
        snapshot.project.tracks[3].muted = true;
        let plan = render_plan(&snapshot, &ffprobe, &output, &script).unwrap();
        std::fs::write(&script, &plan.filter_graph).unwrap();
        let progress = run_tool(&ffmpeg, &plan.args);
        assert!(String::from_utf8_lossy(&progress).contains("progress=end"));
        let probe: serde_json::Value = serde_json::from_slice(&run_tool(
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
        ))
        .unwrap();
        let streams = probe["streams"].as_array().unwrap();
        crate::export_job::validate_output_metadata(
            &serde_json::to_vec(&probe).unwrap(),
            &snapshot,
        )
        .unwrap();
        let v = streams
            .iter()
            .find(|stream| stream["codec_type"] == "video")
            .unwrap();
        let a = streams
            .iter()
            .find(|stream| stream["codec_type"] == "audio")
            .unwrap();
        assert_eq!(v["codec_name"], "h264");
        assert_eq!(v["width"], OUTPUT_WIDTH);
        assert_eq!(v["height"], OUTPUT_HEIGHT);
        assert_eq!(v["nb_frames"], "120");
        assert_eq!(a["codec_name"], "aac");
        assert_eq!(a["sample_rate"], "48000");
        assert_eq!(a["channels"], 2);
        let rgb_path = dir.path().join("decoded.rgb");
        run_tool(
            &ffmpeg,
            &[
                "-nostdin".into(),
                "-y".into(),
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
        let rgb = std::fs::read(&rgb_path).unwrap();
        assert_eq!(rgb.len(), 120 * 8 * 8 * 3);
        for frame in [0usize, 15, 75, 115] {
            assert!(
                rgb[frame * 192..(frame + 1) * 192]
                    .iter()
                    .all(|value| *value < 8),
                "frame {frame} must be black"
            );
        }
        let middle = &rgb[45 * 192..46 * 192];
        assert!(
            middle
                .chunks_exact(3)
                .all(|pixel| pixel[2] > 180 && pixel[0] < 30),
            "sourceIn must select blue second"
        );
        let decoded_audio = dir.path().join("decoded.wav");
        run_tool(
            &ffmpeg,
            &[
                "-nostdin".into(),
                "-y".into(),
                "-i".into(),
                output.as_os_str().into(),
                "-map".into(),
                "0:a:0".into(),
                "-c:a".into(),
                "pcm_s16le".into(),
                decoded_audio.as_os_str().into(),
            ],
        );
        let samples = std::fs::read(&decoded_audio).unwrap();
        assert!(
            rms(&samples, 0.1, 0.8) < 0.0005,
            "gap and muted track must be silent"
        );
        let full = rms(&samples, 2.1, 2.7);
        let fade_in = rms(&samples, 1.18, 1.22);
        let fade_out = rms(&samples, 3.68, 3.72);
        // Mono->stereo downmix is sqrt(0.5); gain=0.5 and sine RMS=sqrt(0.5).
        assert!((full - 0.1).abs() < 0.015, "unexpected track gain: {full}");
        assert!(
            (fade_in / full - 0.2).abs() < 0.06,
            "incorrect fade-in: {fade_in}/{full}"
        );
        assert!(
            (fade_out / full - (4.0 - 1.0 / 30.0 - 3.7)).abs() < 0.07,
            "incorrect frame-based fade-out: {fade_out}/{full}"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires packaged LGPL sidecars and an available Windows h264_mf encoder"]
    fn real_sidecar_preserves_rational_fps_and_visible_track_embedded_audio() {
        let binaries = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
        let ffmpeg = binaries.join("ffmpeg-x86_64-pc-windows-msvc.exe");
        let ffprobe = binaries.join("ffprobe-x86_64-pc-windows-msvc.exe");
        let dir = tempfile::tempdir().unwrap();
        let wav_path = dir.path().join("tone.wav");
        let source = dir.path().join("video.mp4");
        let image = dir.path().join("transparent%03d.png");
        let output = dir.path().join("output.mp4");
        let script = dir.path().join("filters.txt");
        wav(&wav_path, 0, 4, 0.4);
        run_tool(
            &ffmpeg,
            &[
                "-nostdin".into(),
                "-y".into(),
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                "color=c=blue:s=160x90:r=30000/1001:d=3.1".into(),
                "-i".into(),
                wav_path.as_os_str().into(),
                "-map".into(),
                "0:v:0".into(),
                "-map".into(),
                "1:a:0".into(),
                "-c:v".into(),
                "h264_mf".into(),
                "-b:v".into(),
                "1M".into(),
                "-pix_fmt".into(),
                "nv12".into(),
                "-c:a".into(),
                "aac".into(),
                "-t".into(),
                "3.1".into(),
                source.as_os_str().into(),
            ],
        );
        run_tool(
            &ffmpeg,
            &[
                "-nostdin".into(),
                "-y".into(),
                "-f".into(),
                "lavfi".into(),
                "-i".into(),
                "color=c=red@0:s=160x90,format=rgba".into(),
                "-frames:v".into(),
                "1".into(),
                "-c:v".into(),
                "png".into(),
                "-f".into(),
                "image2".into(),
                "-update".into(),
                "1".into(),
                image.as_os_str().into(),
            ],
        );
        let mut snapshot = snapshot();
        snapshot.project.timebase.fps_numerator = 30000;
        snapshot.project.timebase.fps_denominator = 1001;
        clip(&mut snapshot, 0, "top", 30, 30, 0);
        clip(&mut snapshot, 1, "bottom", 0, 90, 0);
        snapshot.project.media_refs[0].kind = MediaKind::Image;
        snapshot.project.media_refs[0].extension = "png".into();
        snapshot.project.media_refs[0].source_path = Some(image.to_string_lossy().into());
        snapshot.project.media_refs[1].source_path = Some(source.to_string_lossy().into());
        let plan = render_plan(&snapshot, &ffprobe, &output, &script).unwrap();
        std::fs::write(&script, plan.filter_graph).unwrap();
        run_tool(&ffmpeg, &plan.args);
        let value: serde_json::Value = serde_json::from_slice(&run_tool(
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
        ))
        .unwrap();
        let stream = &value["streams"][0];
        crate::export_job::validate_output_metadata(
            &serde_json::to_vec(&value).unwrap(),
            &snapshot,
        )
        .unwrap();
        assert_eq!(stream["avg_frame_rate"], "30000/1001");
        assert_eq!(stream["nb_frames"], "90");
        let decoded = dir.path().join("pixels.rgb");
        run_tool(
            &ffmpeg,
            &[
                "-nostdin".into(),
                "-y".into(),
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
                decoded.as_os_str().into(),
            ],
        );
        let pixels = std::fs::read(&decoded).unwrap();
        assert_eq!(pixels.len(), 90 * 192);
        assert!(
            pixels[45 * 192..46 * 192].iter().all(|value| *value < 8),
            "transparent top image is flattened onto black, never lower video"
        );
        for frame in [15usize, 75] {
            assert!(
                pixels[frame * 192..(frame + 1) * 192]
                    .chunks_exact(3)
                    .all(|p| p[2] > 180 && p[0] < 30)
            );
        }
        let decoded_audio = dir.path().join("decoded.wav");
        run_tool(
            &ffmpeg,
            &[
                "-nostdin".into(),
                "-y".into(),
                "-i".into(),
                output.as_os_str().into(),
                "-map".into(),
                "0:a:0".into(),
                "-c:a".into(),
                "pcm_s16le".into(),
                decoded_audio.as_os_str().into(),
            ],
        );
        let samples = std::fs::read(decoded_audio).unwrap();
        assert!(
            rms(&samples, 0.2, 0.7) > 0.15,
            "visible lower video must retain embedded audio"
        );
        assert!(
            rms(&samples, 1.2, 1.8) < 0.0005,
            "occluded lower video must be silent"
        );
        assert!(
            rms(&samples, 2.2, 2.7) > 0.15,
            "lower video audio must resume"
        );
    }
}
