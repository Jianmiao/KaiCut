//! Explicit bounded seek policy for controlled zero-origin 30 fps CFR fixtures.
//! Container rate declarations are only an admission check; callers must still
//! validate each observed integer frame PTS against `index_for_pts`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeekMode {
    #[default]
    Sequential,
    IndexedCfr,
    IndexedPts,
}

pub struct IndexedVideoMetadata {
    pub timebase_numerator: u32,
    pub timebase_denominator: u32,
    pub start_pts: i64,
    pub rate_numerator: u32,
    pub rate_denominator: u32,
    pub average_rate_numerator: u32,
    pub average_rate_denominator: u32,
    pub duration_ticks: u64,
    pub frame_count: u64,
}

pub struct IndexedAudioMetadata {
    pub codec_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub timebase_numerator: u32,
    pub timebase_denominator: u32,
    pub duration_ticks: u64,
}

#[derive(Debug, Clone)]
pub struct IndexedSeekPlan {
    pub video_anchor_index: u64,
    pub video_anchor_ns: u64,
    pub video_input_seconds: String,
    pub video_total_frames: u64,
    pub audio_decode_start_frame: u64,
    pub audio_frames_to_discard: u64,
    pub audio_input_seconds: String,
    pub audio_total_frames: u64,
}

const MAX_SECONDS: u64 = 24 * 60 * 60;

impl IndexedSeekPlan {
    pub fn new(
        target_ns: u64,
        target_audio_frame: u64,
        output_rate: u32,
        output_channels: u16,
        video: &IndexedVideoMetadata,
        audio: &IndexedAudioMetadata,
    ) -> Result<Self, String> {
        if video.start_pts != 0
            || video.rate_numerator != 30
            || video.rate_denominator != 1
            || video.average_rate_numerator != 30
            || video.average_rate_denominator != 1
            || video.timebase_numerator == 0
            || video.timebase_denominator == 0
            || u64::from(video.timebase_denominator) % (u64::from(video.timebase_numerator) * 30)
                != 0
        {
            return Err("Indexed CFR seek requires zero-origin video declared at exactly 30/1 fps with an exactly representable frame timebase".into());
        }
        let video_frames = u128::from(video.duration_ticks)
            .checked_mul(u128::from(video.timebase_numerator))
            .and_then(|value| value.checked_mul(30))
            .ok_or("Video duration overflow")?;
        let denominator = u128::from(video.timebase_denominator);
        if video_frames == 0
            || video_frames % denominator != 0
            || video_frames / denominator != u128::from(video.frame_count)
            || video.frame_count > MAX_SECONDS * 30
        {
            return Err(
                "Indexed CFR video requires bounded, consistent duration_ts and frame_count".into(),
            );
        }
        if audio.codec_name != "pcm_s16le"
            || audio.sample_rate != 48000
            || audio.channels != 2
            || audio.bits_per_sample != 16
            || output_rate != 48000
            || output_channels != 2
            || audio.timebase_numerator == 0
            || audio.timebase_denominator == 0
        {
            return Err(
                "Indexed audio seek requires native 48 kHz stereo PCM16 WAV without resampling"
                    .into(),
            );
        }
        let audio_frames = u128::from(audio.duration_ticks)
            .checked_mul(u128::from(audio.timebase_numerator))
            .and_then(|value| value.checked_mul(48000))
            .ok_or("Audio duration overflow")?;
        let audio_denominator = u128::from(audio.timebase_denominator);
        if audio_frames == 0
            || audio_frames % audio_denominator != 0
            || audio_frames / audio_denominator > u128::from(MAX_SECONDS * 48000)
        {
            return Err("Indexed PCM duration is not a bounded whole sample-frame count".into());
        }
        let audio_total_frames = u64::try_from(audio_frames / audio_denominator)
            .map_err(|_| "Audio frame count overflow")?;
        let duration_ns = u128::from(video.frame_count) * 1_000_000_000 / 30;
        if u128::from(target_ns) >= duration_ns || target_audio_frame >= audio_total_frames {
            return Err("Indexed seek target must precede video and audio EOF".into());
        }
        // Source frame PTS is floored to ns. Inverting that discrete sequence
        // avoids rounding 30.033333333 seconds back from frame 901 to frame 900.
        let anchor = (u128::from(target_ns)
            .checked_add(1)
            .and_then(|value| value.checked_mul(30))
            .ok_or("Seek frame overflow")?
            - 1)
            / 1_000_000_000;
        let video_anchor_index = u64::try_from(anchor).map_err(|_| "Seek frame overflow")?;
        let video_anchor_ns =
            u64::try_from(anchor * 1_000_000_000 / 30).map_err(|_| "Seek time overflow")?;
        let micros = video_anchor_ns / 1000;
        let audio_second = target_audio_frame / 48000;
        let audio_decode_start_frame = audio_second * 48000;
        Ok(Self {
            video_anchor_index,
            video_anchor_ns,
            video_input_seconds: format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000),
            video_total_frames: video.frame_count,
            audio_decode_start_frame,
            audio_frames_to_discard: target_audio_frame - audio_decode_start_frame,
            audio_input_seconds: audio_second.to_string(),
            audio_total_frames,
        })
    }
}

/// Interpret one actual integer timestamp. A declared average rate alone never
/// establishes CFR; the reader additionally requires consecutive indices.
pub fn index_for_pts(pts: i64, numerator: u32, denominator: u32) -> Result<u64, String> {
    if pts < 0 || numerator == 0 || denominator == 0 {
        return Err("Indexed PTS must be nonnegative with a positive timebase".into());
    }
    let scaled = u128::try_from(pts)
        .map_err(|_| "Negative PTS")?
        .checked_mul(u128::from(numerator))
        .and_then(|value| value.checked_mul(30))
        .ok_or("Indexed PTS multiplication overflow")?;
    if scaled % u128::from(denominator) != 0 {
        return Err("Actual decoded frame PTS is not on the admitted 30 fps grid".into());
    }
    let index =
        u64::try_from(scaled / u128::from(denominator)).map_err(|_| "Indexed frame overflow")?;
    if index > MAX_SECONDS * 30 {
        return Err("Indexed frame exceeds 24-hour bound".into());
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video() -> IndexedVideoMetadata {
        IndexedVideoMetadata {
            timebase_numerator: 1,
            timebase_denominator: 15360,
            start_pts: 0,
            rate_numerator: 30,
            rate_denominator: 1,
            average_rate_numerator: 30,
            average_rate_denominator: 1,
            duration_ticks: 921600,
            frame_count: 1800,
        }
    }
    fn audio() -> IndexedAudioMetadata {
        IndexedAudioMetadata {
            codec_name: "pcm_s16le".into(),
            sample_rate: 48000,
            channels: 2,
            bits_per_sample: 16,
            timebase_numerator: 1,
            timebase_denominator: 48000,
            duration_ticks: 2880000,
        }
    }

    #[test]
    fn nanosecond_floor_boundaries_preserve_frames_899_900_and_901() {
        for (frame, nanos, decimal) in [
            (899, 29_966_666_666, "29.966666"),
            (900, 30_000_000_000, "30.000000"),
            (901, 30_033_333_333, "30.033333"),
        ] {
            let plan = IndexedSeekPlan::new(nanos, 1440001, 48000, 2, &video(), &audio()).unwrap();
            assert_eq!(plan.video_anchor_index, frame);
            assert_eq!(plan.video_anchor_ns, nanos);
            assert_eq!(plan.video_input_seconds, decimal);
            assert_eq!(
                IndexedSeekPlan::new(nanos - 1, 0, 48000, 2, &video(), &audio())
                    .unwrap()
                    .video_anchor_index,
                frame - 1
            );
            assert_eq!(
                IndexedSeekPlan::new(nanos + 1, 0, 48000, 2, &video(), &audio())
                    .unwrap()
                    .video_anchor_index,
                frame
            );
        }
    }

    #[test]
    fn audio_anchor_skips_less_than_one_second_without_resampling_coordinates() {
        let plan =
            IndexedSeekPlan::new(30_033_333_333, 1_441_601, 48000, 2, &video(), &audio()).unwrap();
        assert_eq!(plan.audio_decode_start_frame, 1_440_000);
        assert_eq!(plan.audio_frames_to_discard, 1601);
        assert_eq!(plan.audio_input_seconds, "30");
        assert_eq!(plan.video_anchor_index, 901);
        let end =
            IndexedSeekPlan::new(59_999_999_999, 2_879_999, 48000, 2, &video(), &audio()).unwrap();
        assert_eq!(end.video_anchor_index, 1799);
        assert_eq!(end.audio_frames_to_discard, 47999);
    }

    #[test]
    fn metadata_rejects_unproven_rate_origins_timebases_and_audio_formats() {
        let check_video = |metadata: IndexedVideoMetadata| {
            IndexedSeekPlan::new(0, 0, 48000, 2, &metadata, &audio())
        };
        let mut value = video();
        value.start_pts = 1;
        assert!(check_video(value).is_err());
        let mut value = video();
        value.rate_numerator = 29;
        assert!(check_video(value).is_err());
        let mut value = video();
        value.average_rate_numerator = 29;
        assert!(check_video(value).is_err());
        let mut value = video();
        value.timebase_denominator = 1000;
        assert!(check_video(value).is_err());
        let mut value = video();
        value.frame_count = 1799;
        assert!(check_video(value).is_err());
        let check_audio = |metadata: IndexedAudioMetadata| {
            IndexedSeekPlan::new(0, 0, 48000, 2, &video(), &metadata)
        };
        let mut value = audio();
        value.codec_name = "aac".into();
        assert!(check_audio(value).is_err());
        let mut value = audio();
        value.sample_rate = 44100;
        assert!(check_audio(value).is_err());
        let mut value = audio();
        value.channels = 1;
        assert!(check_audio(value).is_err());
        let mut value = audio();
        value.bits_per_sample = 24;
        assert!(check_audio(value).is_err());
        assert!(IndexedSeekPlan::new(0, 0, 44100, 2, &video(), &audio()).is_err());
    }

    #[test]
    fn bounded_targets_reject_eof_overflow_and_noninteger_actual_pts() {
        for (nanos, frame) in [
            (60_000_000_000, 0),
            (0, 2_880_000),
            (u64::MAX, 0),
            (0, u64::MAX),
        ] {
            assert!(IndexedSeekPlan::new(nanos, frame, 48000, 2, &video(), &audio()).is_err());
        }
        assert_eq!(index_for_pts(461312, 1, 15360).unwrap(), 901);
        assert!(index_for_pts(461313, 1, 15360).is_err());
        assert!(index_for_pts(-1, 1, 15360).is_err());
        assert!(index_for_pts(1, 0, 15360).is_err());
        assert!(index_for_pts(i64::MAX, u32::MAX, 1).is_err());
    }
}
