//! Render acknowledgments are isolated from the independent decoder epoch.
use std::collections::BTreeMap;

/// A temporarily empty queue is not EOF. The decoder must confirm EOF and
/// the selected, accepted frame must be the actual final indexed frame.
pub(super) fn latest_due_ready(
    project_ns: u64,
    future_pts_ns: Option<u64>,
    current: Option<usize>,
    accepted: Option<usize>,
    video_eof: bool,
    video_pending: usize,
    source_index_frames: usize,
) -> bool {
    current.is_some()
        && current == accepted
        && (future_pts_ns.is_some_and(|pts| pts > project_ns)
            || (future_pts_ns.is_none()
                && video_eof
                && video_pending == 0
                && source_index_frames.checked_sub(1) == current))
}

/// Exclusive end and target use the same 48 kHz integer timeline grid.
/// Never queue a rounded-up sample beyond the selected source endpoint.
pub(super) fn remaining_audio_frames(target: u64, end: u64, written: u64) -> Result<u64, String> {
    if target > end {
        return Err("Source target exceeds its finite endpoint".into());
    }
    // Match AudioTimeline's floor(PTS) <= timestamp rule. Directly flooring
    // timestamp * rate loses one sample at rounded boundaries such as
    // 333333 ns (the 16th sample boundary on a 48 kHz grid).
    let frame = |ns: u64| ((u128::from(ns) + 1) * 48_000 - 1) / 1_000_000_000;
    let total = u64::try_from(frame(end) - frame(target))
        .map_err(|_| "Source audio frame count overflow")?;
    total
        .checked_sub(written)
        .ok_or_else(|| "Source audio output exceeded its finite endpoint".into())
}

#[derive(Debug)]
pub(super) struct RenderPolicy {
    pub epoch: u64,
    pub source_epoch: u64,
    pub last_submitted: Option<usize>,
    pub last_completed: Option<usize>,
    submitted: BTreeMap<usize, (u64, bool)>,
}
impl RenderPolicy {
    pub fn new(source_epoch: u64) -> Self {
        Self {
            epoch: 1,
            source_epoch,
            last_submitted: None,
            last_completed: None,
            submitted: BTreeMap::new(),
        }
    }
    pub fn retire(&mut self, source_epoch: u64) -> Result<u64, String> {
        self.epoch = self.epoch.checked_add(1).ok_or("Render epoch exhausted")?;
        self.source_epoch = source_epoch;
        self.last_submitted = None;
        self.last_completed = None;
        self.submitted.clear();
        Ok(self.epoch)
    }
    pub fn submitted(
        &mut self,
        index: usize,
        project_ns: u64,
        diagnostic: bool,
    ) -> Result<(), String> {
        if self.submitted.len() >= 3000 || self.last_submitted.is_some_and(|last| index <= last) {
            return Err("Video submissions exceed their bound or regress".into());
        }
        self.submitted.insert(index, (project_ns, diagnostic));
        self.last_submitted = Some(index);
        Ok(())
    }
    pub fn acknowledge(
        &mut self,
        epoch: u64,
        source_epoch: u64,
        index: usize,
        accepted: bool,
    ) -> Result<Option<(u64, bool)>, String> {
        if epoch != self.epoch {
            return Ok(None);
        }
        if source_epoch != self.source_epoch {
            return Err("Acknowledgment source epoch differs from current decoder".into());
        }
        let submitted = self
            .submitted
            .remove(&index)
            .ok_or("Acknowledgment has no matching submission")?;
        if accepted {
            if self.last_completed.is_some_and(|last| index < last) {
                return Err("Accepted video index regressed".into());
            }
            self.last_completed = Some(index);
        } else if self.last_submitted == Some(index) {
            self.last_submitted = None;
        }
        Ok(Some(submitted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pause_retirement_discards_old_result_even_when_source_epoch_did_not_change() {
        let mut p = RenderPolicy::new(7);
        p.submitted(50, 2_000, true).unwrap();
        assert_eq!(p.retire(7).unwrap(), 2);
        assert_eq!(p.acknowledge(1, 7, 50, true).unwrap(), None);
        assert_eq!(p.last_completed, None);
    }
    #[test]
    fn busy_result_retries_same_frame_without_erasing_newer_submission() {
        let mut p = RenderPolicy::new(1);
        p.submitted(2, 2_000, false).unwrap();
        p.submitted(3, 3_000, false).unwrap();
        p.acknowledge(1, 1, 2, false).unwrap();
        assert_eq!(p.last_submitted, Some(3));
        p.acknowledge(1, 1, 3, false).unwrap();
        assert_eq!(p.last_submitted, None);
        p.submitted(3, 4_000, false).unwrap();
        assert_eq!(p.acknowledge(1, 1, 3, true).unwrap(), Some((4_000, false)));
    }
    #[test]
    fn unrelated_or_repeated_acknowledgment_cannot_make_first_frame_ready() {
        let mut p = RenderPolicy::new(4);
        p.submitted(9, 90, true).unwrap();
        assert!(p.acknowledge(1, 3, 9, true).is_err());
        assert!(p.acknowledge(1, 4, 8, true).is_err());
        assert_eq!(p.last_completed, None);
        p.acknowledge(1, 4, 9, true).unwrap();
        assert!(p.acknowledge(1, 4, 9, true).is_err());
    }
    #[test]
    fn seek_can_submit_lower_index_only_after_new_generation() {
        let mut p = RenderPolicy::new(1);
        p.submitted(100, 1_000, false).unwrap();
        assert!(p.submitted(1, 10, true).is_err());
        p.retire(2).unwrap();
        p.submitted(1, 10, true).unwrap();
        p.acknowledge(2, 2, 1, true).unwrap();
        assert_eq!(p.last_completed, Some(1));
    }
    #[test]
    fn generation_never_wraps() {
        let mut p = RenderPolicy::new(1);
        p.epoch = u64::MAX;
        assert!(p.retire(2).is_err());
        assert_eq!(p.epoch, u64::MAX);
        assert_eq!(p.source_epoch, 1);
    }
    #[test]
    fn eof_requires_the_last_index_actual_eof_and_an_empty_decoder_queue() {
        assert!(latest_due_ready(90, None, Some(2), Some(2), true, 0, 3));
        assert!(!latest_due_ready(90, None, Some(2), Some(2), false, 0, 3));
        assert!(!latest_due_ready(90, None, Some(2), Some(2), true, 1, 3));
        assert!(!latest_due_ready(90, None, Some(1), Some(1), true, 0, 3));
        assert!(!latest_due_ready(90, None, Some(2), Some(1), true, 0, 3));
        assert!(!latest_due_ready(90, None, None, None, true, 0, 0));
        assert!(latest_due_ready(
            90,
            Some(100),
            Some(1),
            Some(1),
            false,
            4,
            3
        ));
    }
    #[test]
    fn finite_audio_count_uses_integer_grid_and_never_underflows() {
        assert_eq!(remaining_audio_frames(0, 1_000_000_000, 0).unwrap(), 48_000);
        assert_eq!(
            remaining_audio_frames(999_979_167, 1_000_000_000, 0).unwrap(),
            1
        );
        assert_eq!(
            remaining_audio_frames(1_000_000_000, 1_000_000_000, 0).unwrap(),
            0
        );
        assert_eq!(remaining_audio_frames(0, 1_000_000_000, 48_000).unwrap(), 0);
        assert!(remaining_audio_frames(2, 1, 0).is_err());
        assert!(remaining_audio_frames(0, 1_000_000_000, 48_001).is_err());
        assert_eq!(
            remaining_audio_frames(999_979_166, 1_000_000_000, 0).unwrap(),
            1
        );
        assert_eq!(remaining_audio_frames(0, 333_333, 0).unwrap(), 16);
    }
}
