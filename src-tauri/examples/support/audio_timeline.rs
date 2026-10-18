//! Runtime-only audio timeline placement from actual decoded frame origins.
//! No stream-start assumptions, codec-priming subtraction, device or I/O work.

use serde::Serialize;

const NANOS_PER_SECOND: i128 = 1_000_000_000;
const MAX_BLOCK_FRAMES: u32 = 4800;
const MAX_DURATION_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Timestamp {
    pub pts: i64,
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSpan {
    pub leading_silence_frames: u32,
    pub source_start_frame: u64,
    pub source_frames: u32,
    pub trailing_silence_frames: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTimelineDiagnostics {
    pub audio_origin: Timestamp,
    pub video_origin: Timestamp,
    pub sample_rate: u32,
    pub total_frames: u64,
    /// Signed exact rational nanosecond offset, stringified for JSON safety.
    pub origin_difference_ns_numerator: String,
    pub origin_difference_ns_denominator: String,
    pub first_source_timeline_ns: String,
    pub source_end_timeline_ns: String,
}

#[derive(Debug, Clone)]
pub struct AudioTimeline {
    audio_origin: Timestamp,
    video_origin: Timestamp,
    sample_rate: u32,
    total_frames: u64,
    origin_ns_numerator: i128,
    origin_ns_denominator: i128,
    origin_rate_scaled: i128,
    sample_step_scaled: i128,
    scaled_denominator: i128,
    first_source_ns: i128,
    source_end_ns: i128,
}

impl AudioTimeline {
    /// Origins are the first actual decoded audio/video frame PTS, not stream
    /// metadata start_time. Already-applied codec delay must not be subtracted
    /// again. Source duration is the actual decoded sample-frame count.
    pub fn new(
        audio_origin: Timestamp,
        video_origin: Timestamp,
        sample_rate: u32,
        total_frames: u64,
    ) -> Result<Self, String> {
        if !(8000..=192000).contains(&sample_rate)
            || total_frames > u64::from(sample_rate) * MAX_DURATION_SECONDS
        {
            return Err(
                "Audio timeline requires 8–192 kHz and at most 24 hours of decoded samples".into(),
            );
        }
        if [
            audio_origin.numerator,
            audio_origin.denominator,
            video_origin.numerator,
            video_origin.denominator,
        ]
        .contains(&0)
        {
            return Err("Decoded origins require positive rational timebases".into());
        }
        let audio_scaled = i128::from(audio_origin.pts)
            .checked_mul(i128::from(audio_origin.numerator))
            .and_then(|value| value.checked_mul(i128::from(video_origin.denominator)))
            .ok_or("Audio origin rational overflow")?;
        let video_scaled = i128::from(video_origin.pts)
            .checked_mul(i128::from(video_origin.numerator))
            .and_then(|value| value.checked_mul(i128::from(audio_origin.denominator)))
            .ok_or("Video origin rational overflow")?;
        let difference = audio_scaled
            .checked_sub(video_scaled)
            .ok_or("Decoded origin difference overflow")?;
        let denominator = i128::from(audio_origin.denominator)
            .checked_mul(i128::from(video_origin.denominator))
            .ok_or("Decoded origin denominator overflow")?;
        let divisor = gcd(difference.unsigned_abs(), denominator as u128);
        let seconds_num = difference / divisor as i128;
        let seconds_den = denominator / divisor as i128;
        let nanos_num = seconds_num
            .checked_mul(NANOS_PER_SECOND)
            .ok_or("Decoded origin nanosecond overflow")?;
        let divisor = gcd(nanos_num.unsigned_abs(), seconds_den as u128) as i128;
        let origin_ns_numerator = nanos_num / divisor;
        let origin_ns_denominator = seconds_den / divisor;
        let origin_rate_scaled = origin_ns_numerator
            .checked_mul(i128::from(sample_rate))
            .ok_or("Decoded origin sample-grid overflow")?;
        let sample_step_scaled = origin_ns_denominator
            .checked_mul(NANOS_PER_SECOND)
            .ok_or("Sample period rational overflow")?;
        let scaled_denominator = origin_ns_denominator
            .checked_mul(i128::from(sample_rate))
            .ok_or("Sample-grid denominator overflow")?;
        let first_source_ns = origin_ns_numerator.div_euclid(origin_ns_denominator);
        let source_end_ns = sample_step_scaled
            .checked_mul(i128::from(total_frames))
            .and_then(|value| origin_rate_scaled.checked_add(value))
            .ok_or("Audio source end timestamp overflow")?
            .div_euclid(scaled_denominator);
        Ok(Self {
            audio_origin,
            video_origin,
            sample_rate,
            total_frames,
            origin_ns_numerator,
            origin_ns_denominator,
            origin_rate_scaled,
            sample_step_scaled,
            scaled_denominator,
            first_source_ns,
            source_end_ns,
        })
    }

    /// One block-start query snaps to the latest source sample whose floored
    /// rational PTS is <= target_ns; the remainder of that block advances by
    /// sample count, without re-quantizing each sample's time. Before source
    /// start, ceil-to-output-grid silence is followed by source sample zero.
    /// Callers must preserve the resulting sample cursor between blocks; they
    /// must not independently round/re-query every block boundary and claim
    /// that this API guarantees gapless transport for those altered queries.
    pub fn span(&self, target_ns: u64, requested_frames: u32) -> Result<AudioSpan, String> {
        if requested_frames > MAX_BLOCK_FRAMES {
            return Err("Audio timeline block exceeds 4800 sample frames".into());
        }
        let target = i128::from(target_ns);
        if self.total_frames == 0 || target >= self.source_end_ns {
            return Ok(AudioSpan {
                leading_silence_frames: 0,
                source_start_frame: self.total_frames,
                source_frames: 0,
                trailing_silence_frames: requested_frames,
            });
        }
        let (leading_silence_frames, source_start_frame) = if target < self.first_source_ns {
            let delta = self
                .first_source_ns
                .checked_sub(target)
                .ok_or("Leading silence time overflow")?;
            let frames_num = delta
                .checked_mul(i128::from(self.sample_rate))
                .ok_or("Leading silence frame count overflow")?;
            let frames =
                frames_num / NANOS_PER_SECOND + i128::from(frames_num % NANOS_PER_SECOND != 0);
            (
                u32::try_from(frames.min(i128::from(requested_frames)))
                    .map_err(|_| "Leading silence count overflow")?,
                0,
            )
        } else {
            // floor(origin + n/rate) <= target is equivalent to
            // origin_scaled + n*sample_step < (target+1)*scaled_denominator.
            let upper = target
                .checked_add(1)
                .and_then(|value| value.checked_mul(self.scaled_denominator))
                .and_then(|value| value.checked_sub(self.origin_rate_scaled))
                .and_then(|value| value.checked_sub(1))
                .ok_or("Sample query arithmetic overflow")?;
            let frame = upper.div_euclid(self.sample_step_scaled);
            (
                0,
                u64::try_from(frame)
                    .map_err(|_| "Audio source frame cannot be represented")?
                    .min(self.total_frames),
            )
        };
        let available = self.total_frames - source_start_frame;
        let remaining = requested_frames - leading_silence_frames;
        let source_frames = u32::try_from(available.min(u64::from(remaining)))
            .map_err(|_| "Audio span length overflow")?;
        Ok(AudioSpan {
            leading_silence_frames,
            source_start_frame,
            source_frames,
            trailing_silence_frames: remaining - source_frames,
        })
    }

    pub fn diagnostics(&self) -> AudioTimelineDiagnostics {
        AudioTimelineDiagnostics {
            audio_origin: self.audio_origin,
            video_origin: self.video_origin,
            sample_rate: self.sample_rate,
            total_frames: self.total_frames,
            origin_difference_ns_numerator: self.origin_ns_numerator.to_string(),
            origin_difference_ns_denominator: self.origin_ns_denominator.to_string(),
            first_source_timeline_ns: self.first_source_ns.to_string(),
            source_end_timeline_ns: self.source_end_ns.to_string(),
        }
    }
}

fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(pts: i64) -> Timestamp {
        Timestamp {
            pts,
            numerator: 1,
            denominator: 1,
        }
    }

    #[test]
    fn positive_two_second_origin_inserts_only_the_required_leading_silence() {
        let timeline = AudioTimeline::new(seconds(2), seconds(0), 48000, 480000).unwrap();
        assert_eq!(
            timeline.span(0, 4800).unwrap(),
            AudioSpan {
                leading_silence_frames: 4800,
                source_start_frame: 0,
                source_frames: 0,
                trailing_silence_frames: 0
            }
        );
        assert_eq!(
            timeline.span(1_999_979_167, 4).unwrap(),
            AudioSpan {
                leading_silence_frames: 1,
                source_start_frame: 0,
                source_frames: 3,
                trailing_silence_frames: 0
            }
        );
        assert_eq!(
            timeline.span(2_000_000_000, 4).unwrap(),
            AudioSpan {
                leading_silence_frames: 0,
                source_start_frame: 0,
                source_frames: 4,
                trailing_silence_frames: 0
            }
        );
    }

    #[test]
    fn negative_two_second_origin_starts_from_the_corresponding_source_sample() {
        let timeline = AudioTimeline::new(seconds(-2), seconds(0), 48000, 144000).unwrap();
        assert_eq!(
            timeline.span(0, 4800).unwrap(),
            AudioSpan {
                leading_silence_frames: 0,
                source_start_frame: 96000,
                source_frames: 4800,
                trailing_silence_frames: 0
            }
        );
        let shifted_origins = AudioTimeline::new(seconds(5), seconds(7), 48000, 144000).unwrap();
        assert_eq!(
            shifted_origins.span(0, 4800).unwrap(),
            timeline.span(0, 4800).unwrap()
        );
    }

    #[test]
    fn nanosecond_floor_at_sample_one_never_selects_sample_zero() {
        let timeline = AudioTimeline::new(seconds(0), seconds(0), 48000, 10).unwrap();
        assert_eq!(timeline.span(20_832, 3).unwrap().source_start_frame, 0);
        assert_eq!(timeline.span(20_833, 3).unwrap().source_start_frame, 1);
        assert_eq!(timeline.span(20_834, 3).unwrap().source_start_frame, 1);
        assert_eq!(timeline.span(20_833, 3).unwrap().source_frames, 3);
        assert_eq!(timeline.span(41_665, 1).unwrap().source_start_frame, 1);
        assert_eq!(timeline.span(41_666, 1).unwrap().source_start_frame, 2);
        assert_eq!(timeline.span(62_500, 1).unwrap().source_start_frame, 3);
    }

    #[test]
    fn block_crossing_both_source_edges_has_exact_silence_and_source_counts() {
        let timeline = AudioTimeline::new(
            Timestamp {
                pts: 1,
                numerator: 1,
                denominator: 48000,
            },
            seconds(0),
            48000,
            2,
        )
        .unwrap();
        assert_eq!(
            timeline.span(0, 5).unwrap(),
            AudioSpan {
                leading_silence_frames: 1,
                source_start_frame: 0,
                source_frames: 2,
                trailing_silence_frames: 2
            }
        );
        assert_eq!(
            timeline.span(20_832, 5).unwrap(),
            AudioSpan {
                leading_silence_frames: 1,
                source_start_frame: 0,
                source_frames: 2,
                trailing_silence_frames: 2
            }
        );
        assert_eq!(
            timeline.span(41_666, 5).unwrap(),
            AudioSpan {
                leading_silence_frames: 0,
                source_start_frame: 1,
                source_frames: 1,
                trailing_silence_frames: 4
            }
        );
        assert_eq!(
            timeline.span(62_500, 5).unwrap(),
            AudioSpan {
                leading_silence_frames: 0,
                source_start_frame: 2,
                source_frames: 0,
                trailing_silence_frames: 5
            }
        );
    }

    #[test]
    fn rational_origin_and_negative_fraction_use_signed_floor_not_float_or_truncation() {
        let positive = AudioTimeline::new(
            Timestamp {
                pts: 1,
                numerator: 1,
                denominator: 3,
            },
            seconds(0),
            48000,
            48000,
        )
        .unwrap();
        assert_eq!(
            positive
                .span(333_333_332, 2)
                .unwrap()
                .leading_silence_frames,
            1
        );
        assert_eq!(positive.span(333_333_333, 2).unwrap().source_start_frame, 0);
        let negative = AudioTimeline::new(
            Timestamp {
                pts: -1,
                numerator: 1,
                denominator: 3,
            },
            seconds(0),
            48000,
            48000,
        )
        .unwrap();
        assert_eq!(negative.span(0, 2).unwrap().source_start_frame, 16000);
        assert_eq!(
            negative.diagnostics().first_source_timeline_ns,
            "-333333334"
        );
        let actual_aac_origin = AudioTimeline::new(
            Timestamp {
                pts: 94944,
                numerator: 1,
                denominator: 48000,
            },
            seconds(2),
            48000,
            481280,
        )
        .unwrap();
        assert_eq!(
            actual_aac_origin.span(0, 2).unwrap().source_start_frame,
            1056
        );
    }

    #[test]
    fn eof_and_empty_source_return_silence_without_reading_out_of_bounds() {
        let timeline = AudioTimeline::new(seconds(0), seconds(0), 48000, 48000).unwrap();
        assert_eq!(
            timeline.span(999_979_166, 4).unwrap(),
            AudioSpan {
                leading_silence_frames: 0,
                source_start_frame: 47999,
                source_frames: 1,
                trailing_silence_frames: 3
            }
        );
        assert_eq!(
            timeline.span(1_000_000_000, 4).unwrap(),
            AudioSpan {
                leading_silence_frames: 0,
                source_start_frame: 48000,
                source_frames: 0,
                trailing_silence_frames: 4
            }
        );
        assert_eq!(
            timeline.span(u64::MAX, 4).unwrap().trailing_silence_frames,
            4
        );
        let empty = AudioTimeline::new(seconds(2), seconds(0), 48000, 0).unwrap();
        assert_eq!(
            empty.span(0, 4).unwrap(),
            AudioSpan {
                leading_silence_frames: 0,
                source_start_frame: 0,
                source_frames: 0,
                trailing_silence_frames: 4
            }
        );
        assert_eq!(empty.span(0, 0).unwrap().source_frames, 0);
    }

    #[test]
    fn invalid_rates_duration_block_sizes_and_checked_origin_overflow_are_rejected() {
        for rate in [0, 7999, 192001, u32::MAX] {
            assert!(AudioTimeline::new(seconds(0), seconds(0), rate, 0).is_err());
        }
        assert!(AudioTimeline::new(seconds(0), seconds(0), 48000, 48000 * 86400 + 1).is_err());
        assert!(
            AudioTimeline::new(
                Timestamp {
                    pts: 1,
                    numerator: 0,
                    denominator: 1
                },
                seconds(0),
                48000,
                1
            )
            .is_err()
        );
        assert!(
            AudioTimeline::new(
                Timestamp {
                    pts: 1,
                    numerator: 1,
                    denominator: 0
                },
                seconds(0),
                48000,
                1
            )
            .is_err()
        );
        assert!(
            AudioTimeline::new(
                Timestamp {
                    pts: i64::MAX,
                    numerator: u32::MAX,
                    denominator: u32::MAX - 1
                },
                Timestamp {
                    pts: i64::MIN,
                    numerator: u32::MAX,
                    denominator: u32::MAX - 3
                },
                192000,
                1
            )
            .is_err()
        );
        assert!(
            AudioTimeline::new(seconds(0), seconds(0), 48000, 1)
                .unwrap()
                .span(0, 4801)
                .is_err()
        );
    }

    #[test]
    fn every_span_conserves_requested_frames_and_keeps_source_range_bounded() {
        for origin in [-2, 0, 2] {
            let timeline = AudioTimeline::new(seconds(origin), seconds(0), 48000, 4800).unwrap();
            for target in [
                0, 1, 20832, 20833, 99999999, 1999999999, 2000000000, 2100000000,
            ] {
                for requested in [0, 1, 32, 480, 4800] {
                    let span = timeline.span(target, requested).unwrap();
                    assert_eq!(
                        span.leading_silence_frames
                            + span.source_frames
                            + span.trailing_silence_frames,
                        requested
                    );
                    assert!(span.source_start_frame + u64::from(span.source_frames) <= 4800);
                }
            }
        }
    }
}
