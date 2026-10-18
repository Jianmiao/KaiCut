//! Checked rational mapping from an audio device position into project time.
//!
//! Device frequency means the units per second returned by the device clock;
//! it is not assumed to be the PCM sample rate. This module never reads a wall
//! clock and does not infer whether a device is paused. The caller owns device
//! lifecycle and must rebase on a new stream or an explicit project seek.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockError {
    InvalidTimebase,
    ZeroFrequency,
    NotRebased,
    StaleEpoch,
    FrequencyChanged,
    PositionRegressed,
    Overflow,
    InvalidFrameRange,
}

impl fmt::Display for ClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidTimebase => {
                "project timebase must have a positive numerator and denominator"
            }
            Self::ZeroFrequency => "audio device clock frequency must be positive",
            Self::NotRebased => "audio timeline clock has not been rebased",
            Self::StaleEpoch => "audio sample belongs to an obsolete clock epoch",
            Self::FrequencyChanged => "device clock frequency changed; rebase the clock explicitly",
            Self::PositionRegressed => "device position or scheduled project frame moved backwards",
            Self::Overflow => "clock arithmetic exceeds its representable range",
            Self::InvalidFrameRange => "scheduler needs a nonempty half-open project frame range",
        })
    }
}

impl std::error::Error for ClockError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockEpoch(u64);

impl ClockEpoch {
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockPosition {
    pub epoch: ClockEpoch,
    pub device_position: u64,
    pub elapsed_device_units: u64,
    pub project_frame: u64,
    /// Exact fractional frame remainder; do not serialize u128 as an unsafe JS number.
    pub subframe_numerator: u128,
    pub subframe_denominator: u128,
    /// Absolute project presentation time, floored only at this API boundary.
    pub project_time_ns: u64,
}

#[derive(Debug, Clone, Copy)]
struct Anchor {
    epoch: ClockEpoch,
    device_position: u64,
    last_device_position: u64,
    frequency: u64,
    project_start_frame: u64,
}

#[derive(Debug)]
pub struct AudioTimelineClock {
    numerator: u32,
    denominator: u32,
    epoch_counter: u64,
    anchor: Option<Anchor>,
}

impl AudioTimelineClock {
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, ClockError> {
        if numerator == 0 || denominator == 0 {
            return Err(ClockError::InvalidTimebase);
        }
        Ok(Self {
            numerator,
            denominator,
            epoch_counter: 0,
            anchor: None,
        })
    }

    pub fn rebase(
        &mut self,
        device_position: u64,
        frequency: u64,
        project_start_frame: u64,
    ) -> Result<ClockEpoch, ClockError> {
        if frequency == 0 {
            return Err(ClockError::ZeroFrequency);
        }
        let epoch = ClockEpoch(
            self.epoch_counter
                .checked_add(1)
                .ok_or(ClockError::Overflow)?,
        );
        let anchor = Anchor {
            epoch,
            device_position,
            last_device_position: device_position,
            frequency,
            project_start_frame,
        };
        self.map(anchor, device_position)?;
        self.epoch_counter = epoch.0;
        self.anchor = Some(anchor);
        Ok(epoch)
    }

    pub fn observe(
        &mut self,
        epoch: ClockEpoch,
        device_position: u64,
        frequency: u64,
    ) -> Result<ClockPosition, ClockError> {
        let mut anchor = self.anchor.ok_or(ClockError::NotRebased)?;
        if epoch != anchor.epoch {
            return Err(ClockError::StaleEpoch);
        }
        if frequency == 0 {
            return Err(ClockError::ZeroFrequency);
        }
        if frequency != anchor.frequency {
            return Err(ClockError::FrequencyChanged);
        }
        if device_position < anchor.last_device_position {
            return Err(ClockError::PositionRegressed);
        }
        let position = self.map(anchor, device_position)?;
        anchor.last_device_position = device_position;
        self.anchor = Some(anchor);
        Ok(position)
    }

    fn map(&self, anchor: Anchor, device_position: u64) -> Result<ClockPosition, ClockError> {
        let elapsed_device_units = device_position
            .checked_sub(anchor.device_position)
            .ok_or(ClockError::PositionRegressed)?;
        let subframe_denominator = u128::from(anchor.frequency)
            .checked_mul(u128::from(self.denominator))
            .ok_or(ClockError::Overflow)?;
        let elapsed_frames = u128::from(elapsed_device_units)
            .checked_mul(u128::from(self.numerator))
            .ok_or(ClockError::Overflow)?;
        let whole_frames = u64::try_from(elapsed_frames / subframe_denominator)
            .map_err(|_| ClockError::Overflow)?;
        let project_frame = anchor
            .project_start_frame
            .checked_add(whole_frames)
            .ok_or(ClockError::Overflow)?;

        // Split each rational time into whole nanoseconds and a remainder before
        // summing. Combining their large denominators first needlessly overflows.
        let initial_ns_numerator = u128::from(anchor.project_start_frame)
            .checked_mul(u128::from(self.denominator))
            .and_then(|value| value.checked_mul(1_000_000_000))
            .ok_or(ClockError::Overflow)?;
        let elapsed_ns_numerator = u128::from(elapsed_device_units)
            .checked_mul(1_000_000_000)
            .ok_or(ClockError::Overflow)?;
        let timebase_num = u128::from(self.numerator);
        let frequency = u128::from(anchor.frequency);
        let fractional_sum = (initial_ns_numerator % timebase_num)
            .checked_mul(frequency)
            .and_then(|value| {
                value.checked_add((elapsed_ns_numerator % frequency).checked_mul(timebase_num)?)
            })
            .ok_or(ClockError::Overflow)?;
        let fractional_denominator = timebase_num
            .checked_mul(frequency)
            .ok_or(ClockError::Overflow)?;
        let project_time_ns = (initial_ns_numerator / timebase_num)
            .checked_add(elapsed_ns_numerator / frequency)
            .and_then(|value| value.checked_add(fractional_sum / fractional_denominator))
            .ok_or(ClockError::Overflow)?;
        Ok(ClockPosition {
            epoch: anchor.epoch,
            device_position,
            elapsed_device_units,
            project_frame,
            subframe_numerator: elapsed_frames % subframe_denominator,
            subframe_denominator,
            project_time_ns: u64::try_from(project_time_ns).map_err(|_| ClockError::Overflow)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SchedulerTotals {
    pub selected_frames: u64,
    /// Due numeric PTS positions omitted between selections, not decoded drops.
    pub skipped_frames: u64,
    pub duplicate_observations: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSelection {
    pub frame: u64,
    pub skipped_frames: u64,
    /// The caller must not submit the same synthetic frame again when true.
    pub duplicate: bool,
}

/// Select synthetic frame PTS indices only. This is not a video decoder or a
/// claim about physical presentation; the driver separately records that work.
#[derive(Debug)]
pub struct FrameScheduler {
    epoch: ClockEpoch,
    first_frame: u64,
    exclusive_end_frame: u64,
    last_project_frame: Option<u64>,
    last_selected: Option<u64>,
    totals: SchedulerTotals,
}

impl FrameScheduler {
    pub fn new(
        epoch: ClockEpoch,
        first_frame: u64,
        exclusive_end_frame: u64,
    ) -> Result<Self, ClockError> {
        if first_frame >= exclusive_end_frame {
            return Err(ClockError::InvalidFrameRange);
        }
        Ok(Self {
            epoch,
            first_frame,
            exclusive_end_frame,
            last_project_frame: None,
            last_selected: None,
            totals: SchedulerTotals::default(),
        })
    }

    pub fn observe(
        &mut self,
        position: &ClockPosition,
    ) -> Result<Option<FrameSelection>, ClockError> {
        if position.epoch != self.epoch {
            return Err(ClockError::StaleEpoch);
        }
        if self
            .last_project_frame
            .is_some_and(|previous| position.project_frame < previous)
        {
            return Err(ClockError::PositionRegressed);
        }
        if position.project_frame < self.first_frame {
            self.last_project_frame = Some(position.project_frame);
            return Ok(None);
        }
        let frame = position.project_frame.min(self.exclusive_end_frame - 1);
        let duplicate = self.last_selected == Some(frame);
        let skipped_frames = if duplicate {
            0
        } else {
            frame
                - self
                    .last_selected
                    .map_or(self.first_frame, |previous| previous + 1)
        };
        let mut totals = self.totals;
        if duplicate {
            totals.duplicate_observations = totals
                .duplicate_observations
                .checked_add(1)
                .ok_or(ClockError::Overflow)?;
        } else {
            totals.selected_frames = totals
                .selected_frames
                .checked_add(1)
                .ok_or(ClockError::Overflow)?;
            totals.skipped_frames = totals
                .skipped_frames
                .checked_add(skipped_frames)
                .ok_or(ClockError::Overflow)?;
        }
        self.last_project_frame = Some(position.project_frame);
        self.last_selected = Some(frame);
        self.totals = totals;
        Ok(Some(FrameSelection {
            frame,
            skipped_frames,
            duplicate,
        }))
    }

    pub fn totals(&self) -> SchedulerTotals {
        self.totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_frequency_is_not_assumed_to_be_pcm_sample_rate() {
        let mut clock = AudioTimelineClock::new(30, 1).unwrap();
        let epoch = clock.rebase(1_000, 10_000_000, 90).unwrap();
        let result = clock.observe(epoch, 2_501_000, 10_000_000).unwrap();
        assert_eq!(result.project_frame, 97);
        assert_eq!(
            (result.subframe_numerator, result.subframe_denominator),
            (5_000_000, 10_000_000)
        );
        assert_eq!(result.project_time_ns, 3_250_000_000);
    }

    #[test]
    fn rational_timebase_retains_fractional_frames_after_many_hours() {
        let mut clock = AudioTimelineClock::new(30_000, 1_001).unwrap();
        let epoch = clock.rebase(77, 10_000_000, 10).unwrap();
        let position = clock
            .observe(epoch, 216_216_000_000_077, 10_000_000)
            .unwrap();
        assert_eq!(position.project_frame, 648_000_010);
        assert_eq!(position.subframe_numerator, 0);
        assert_eq!(position.project_time_ns, 21_621_600_333_666_666);
        let fraction = clock
            .observe(epoch, 216_216_005_000_077, 10_000_000)
            .unwrap();
        assert_eq!(fraction.project_frame, 648_000_024);
        assert_eq!(fraction.subframe_numerator, 9_860_000_000);
        assert_eq!(fraction.subframe_denominator, 10_010_000_000);
    }

    #[test]
    fn rejects_backwards_positions_and_frequency_changes_without_committing_them() {
        let mut clock = AudioTimelineClock::new(25, 1).unwrap();
        let epoch = clock.rebase(100, 1_000, 0).unwrap();
        clock.observe(epoch, 200, 1_000).unwrap();
        assert_eq!(
            clock.observe(epoch, 199, 1_000),
            Err(ClockError::PositionRegressed)
        );
        assert_eq!(
            clock.observe(epoch, 250, 48_000),
            Err(ClockError::FrequencyChanged)
        );
        assert_eq!(clock.observe(epoch, 200, 1_000).unwrap().project_frame, 2);
    }

    #[test]
    fn rebase_allows_device_reset_but_rejects_an_old_decoder_epoch() {
        let mut clock = AudioTimelineClock::new(30, 1).unwrap();
        let old = clock.rebase(500, 1_000, 0).unwrap();
        clock.observe(old, 1_500, 1_000).unwrap();
        let fresh = clock.rebase(0, 2_000, 900).unwrap();
        assert_ne!(old, fresh);
        assert_eq!(
            clock.observe(old, 1_600, 1_000),
            Err(ClockError::StaleEpoch)
        );
        assert_eq!(
            clock.observe(fresh, 1_000, 2_000).unwrap().project_frame,
            915
        );
    }

    #[test]
    fn repeated_device_position_does_not_advance_project_time() {
        let mut clock = AudioTimelineClock::new(24, 1).unwrap();
        let epoch = clock.rebase(0, 192_000, 48).unwrap();
        let first = clock.observe(epoch, 96_000, 192_000).unwrap();
        for _ in 0..100 {
            assert_eq!(clock.observe(epoch, 96_000, 192_000).unwrap(), first);
        }
        assert_eq!(first.project_frame, 60);
    }

    #[test]
    fn carries_fractional_nanoseconds_without_accumulating_rounding_error() {
        let mut clock = AudioTimelineClock::new(3, 1).unwrap();
        let epoch = clock.rebase(0, 3, 2).unwrap();
        let anchor = clock.observe(epoch, 0, 3).unwrap();
        assert_eq!(anchor.project_time_ns, 666_666_666);
        assert_eq!(
            clock.observe(epoch, 1, 3).unwrap().project_time_ns,
            1_000_000_000
        );
        assert_eq!(
            clock.observe(epoch, 2, 3).unwrap().project_time_ns,
            1_333_333_333
        );
    }

    #[test]
    fn rejects_zero_frequency_uninitialized_and_out_of_range_time() {
        assert!(matches!(
            AudioTimelineClock::new(0, 1),
            Err(ClockError::InvalidTimebase)
        ));
        assert!(matches!(
            AudioTimelineClock::new(30, 0),
            Err(ClockError::InvalidTimebase)
        ));
        let mut clock = AudioTimelineClock::new(30, 1).unwrap();
        assert_eq!(
            clock.observe(ClockEpoch(1), 0, 1),
            Err(ClockError::NotRebased)
        );
        assert_eq!(clock.rebase(0, 0, 0), Err(ClockError::ZeroFrequency));
        assert_eq!(clock.rebase(0, 1, u64::MAX), Err(ClockError::Overflow));
        let epoch = clock.rebase(0, 1, 0).unwrap();
        assert_eq!(clock.observe(epoch, u64::MAX, 1), Err(ClockError::Overflow));
        assert_eq!(clock.observe(epoch, 1, 1).unwrap().project_frame, 30);
    }

    #[test]
    fn selects_only_due_frames_inside_the_video_range_and_counts_skipped_pts() {
        let mut clock = AudioTimelineClock::new(30, 1).unwrap();
        let epoch = clock.rebase(0, 1_000, 0).unwrap();
        let mut scheduler = FrameScheduler::new(epoch, 10, 15).unwrap();
        assert_eq!(
            scheduler
                .observe(&clock.observe(epoch, 300, 1_000).unwrap())
                .unwrap(),
            None
        );
        let first = scheduler
            .observe(&clock.observe(epoch, 366, 1_000).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(first.frame, 10);
        assert_eq!(first.skipped_frames, 0);
        assert!(!first.duplicate);
        let same = scheduler
            .observe(&clock.observe(epoch, 366, 1_000).unwrap())
            .unwrap()
            .unwrap();
        assert!(same.duplicate);
        let skipped = scheduler
            .observe(&clock.observe(epoch, 470, 1_000).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(skipped.frame, 14);
        assert_eq!(skipped.skipped_frames, 3);
        assert_eq!(
            scheduler.totals(),
            SchedulerTotals {
                selected_frames: 2,
                skipped_frames: 3,
                duplicate_observations: 1
            }
        );
        assert_eq!(
            scheduler
                .observe(&clock.observe(epoch, 1_000, 1_000).unwrap())
                .unwrap()
                .unwrap()
                .frame,
            14
        );
    }

    #[test]
    fn scheduler_requires_a_new_epoch_after_seek_and_rejects_a_reversed_frame() {
        let mut clock = AudioTimelineClock::new(30, 1).unwrap();
        let epoch = clock.rebase(0, 1_000, 0).unwrap();
        let older = clock.observe(epoch, 100, 1_000).unwrap();
        let newer = clock.observe(epoch, 200, 1_000).unwrap();
        let mut scheduler = FrameScheduler::new(epoch, 0, 30).unwrap();
        scheduler.observe(&newer).unwrap();
        assert_eq!(
            scheduler.observe(&older),
            Err(ClockError::PositionRegressed)
        );
        let fresh = clock.rebase(0, 1_000, 0).unwrap();
        assert_eq!(
            scheduler.observe(&clock.observe(fresh, 0, 1_000).unwrap()),
            Err(ClockError::StaleEpoch)
        );
        assert!(matches!(
            FrameScheduler::new(fresh, 10, 10),
            Err(ClockError::InvalidFrameRange)
        ));
    }

    #[test]
    fn first_late_scheduler_sample_counts_only_due_frames_inside_its_range() {
        let mut clock = AudioTimelineClock::new(30, 1).unwrap();
        let epoch = clock.rebase(0, 30, 0).unwrap();
        let mut scheduler = FrameScheduler::new(epoch, 10, 15).unwrap();
        let first = scheduler
            .observe(&clock.observe(epoch, 13, 30).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(first.frame, 13);
        assert_eq!(first.skipped_frames, 3);
        assert_eq!(scheduler.totals().selected_frames, 1);
        assert_eq!(scheduler.totals().skipped_frames, 3);
    }
}
