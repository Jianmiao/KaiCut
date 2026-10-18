//! Bounded selection of already-decoded frames by their real presentation times.
//! This selector does not decode or display pixels, advance a clock, or infer
//! EOF from a temporarily empty source. It owns at most a current and a future
//! frame; a caller may poll again when delayed decoder output becomes available.

use serde::Serialize;

const MAX_FETCHES_PER_POLL: usize = 8;

#[derive(Debug)]
pub struct TimedFrame<T> {
    pub epoch: u64,
    pub index: usize,
    pub pts_ns: u64,
    pub payload: T,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoTimelineSnapshot {
    pub epoch: u64,
    pub current_index: Option<usize>,
    pub current_pts_ns: Option<u64>,
    pub future_index: Option<usize>,
    pub future_pts_ns: Option<u64>,
    pub last_query_ns: Option<u64>,
    pub buffered_frames: usize,
}

/// Single-owner state. Query time must be monotonic within an epoch. A seek
/// requires reset to a strictly newer epoch; old decoder frames are errors,
/// not silently accepted into the new source mapping. Payload size is bounded
/// by the owning decoder; this type only bounds the number of retained frames.
#[derive(Debug)]
pub struct VideoTimeline<T> {
    epoch: u64,
    current: Option<TimedFrame<T>>,
    future: Option<TimedFrame<T>>,
    last_query_ns: Option<u64>,
    last_seen: Option<(usize, u64)>,
}

impl<T> VideoTimeline<T> {
    pub fn new(epoch: u64) -> Result<Self, String> {
        if epoch == 0 {
            return Err("Video selector epoch must be positive".into());
        }
        Ok(Self {
            epoch,
            current: None,
            future: None,
            last_query_ns: None,
            last_seen: None,
        })
    }

    pub fn reset(&mut self, epoch: u64) -> Result<(), String> {
        if epoch <= self.epoch {
            return Err("Video selector reset requires a strictly newer epoch".into());
        }
        self.epoch = epoch;
        self.current = None;
        self.future = None;
        self.last_query_ns = None;
        self.last_seen = None;
        Ok(())
    }

    pub fn snapshot(&self) -> VideoTimelineSnapshot {
        VideoTimelineSnapshot {
            epoch: self.epoch,
            current_index: self.current.as_ref().map(|frame| frame.index),
            current_pts_ns: self.current.as_ref().map(|frame| frame.pts_ns),
            future_index: self.future.as_ref().map(|frame| frame.index),
            future_pts_ns: self.future.as_ref().map(|frame| frame.pts_ns),
            last_query_ns: self.last_query_ns,
            buffered_frames: usize::from(self.current.is_some())
                + usize::from(self.future.is_some()),
        }
    }

    /// Return the latest due frame observed so far, or None if no due frame is
    /// available. A future frame is retained and never returned early. An empty
    /// fetch means "not available now", not EOF. After eight fetches this call
    /// yields to the owner even if more delayed due frames may exist; callers
    /// needing exact preparation must independently verify their expected PTS.
    pub fn poll_at(
        &mut self,
        target_ns: u64,
        mut fetch: impl FnMut() -> Result<Option<TimedFrame<T>>, String>,
    ) -> Result<Option<&TimedFrame<T>>, String> {
        if self
            .last_query_ns
            .is_some_and(|previous| target_ns < previous)
        {
            return Err(
                "Video query moved backwards; reset the selector with a new seek epoch".into(),
            );
        }
        self.last_query_ns = Some(target_ns);
        if self
            .future
            .as_ref()
            .is_some_and(|frame| frame.pts_ns > target_ns)
        {
            return Ok(self.current.as_ref());
        }
        if let Some(frame) = self.future.take() {
            self.current = Some(frame);
        }
        for _ in 0..MAX_FETCHES_PER_POLL {
            let Some(frame) = fetch()? else {
                break;
            };
            if frame.epoch != self.epoch {
                return Err("Decoded video frame belongs to a stale source epoch".into());
            }
            if self
                .last_seen
                .is_some_and(|(index, pts)| frame.index <= index || frame.pts_ns <= pts)
            {
                return Err(
                    "Decoded video source index or real PTS repeated or moved backwards".into(),
                );
            }
            self.last_seen = Some((frame.index, frame.pts_ns));
            if frame.pts_ns > target_ns {
                self.future = Some(frame);
                break;
            }
            self.current = Some(frame);
        }
        Ok(self.current.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn frame(index: usize, pts_ns: u64) -> TimedFrame<u8> {
        TimedFrame {
            epoch: 1,
            index,
            pts_ns,
            payload: index as u8,
        }
    }

    #[test]
    fn vfr_selection_returns_latest_observed_due_frame_and_keeps_the_future() {
        let mut selector = VideoTimeline::new(1).unwrap();
        let mut source = VecDeque::from([
            frame(0, 0),
            frame(1, 40_000_000),
            frame(2, 170_000_000),
            frame(3, 190_000_000),
        ]);
        assert_eq!(
            selector
                .poll_at(50_000_000, || Ok(source.pop_front()))
                .unwrap()
                .unwrap()
                .index,
            1
        );
        assert_eq!(selector.snapshot().future_pts_ns, Some(170_000_000));
        assert_eq!(selector.snapshot().buffered_frames, 2);
        let before = source.len();
        assert_eq!(
            selector
                .poll_at(169_999_999, || Ok(source.pop_front()))
                .unwrap()
                .unwrap()
                .index,
            1
        );
        assert_eq!(source.len(), before);
        assert_eq!(
            selector
                .poll_at(170_000_000, || Ok(source.pop_front()))
                .unwrap()
                .unwrap()
                .index,
            2
        );
        assert_eq!(selector.snapshot().future_pts_ns, Some(190_000_000));
    }

    #[test]
    fn a_first_future_frame_is_held_without_being_returned_or_consumed_again() {
        let mut selector = VideoTimeline::new(1).unwrap();
        let mut source = VecDeque::from([frame(5, 500), frame(6, 900)]);
        assert!(
            selector
                .poll_at(499, || Ok(source.pop_front()))
                .unwrap()
                .is_none()
        );
        assert_eq!(selector.snapshot().buffered_frames, 1);
        assert!(
            selector
                .poll_at(499, || panic!("Lookahead must prevent another source poll"))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            selector
                .poll_at(500, || Ok(source.pop_front()))
                .unwrap()
                .unwrap()
                .payload,
            5
        );
        assert_eq!(selector.snapshot().future_index, Some(6));
    }

    #[test]
    fn reverse_time_requires_an_explicit_new_epoch_and_old_callbacks_are_rejected() {
        let mut selector = VideoTimeline::new(1).unwrap();
        let mut source = VecDeque::from([frame(4, 400), frame(5, 500)]);
        selector.poll_at(450, || Ok(source.pop_front())).unwrap();
        let previous = selector.snapshot();
        assert!(
            selector
                .poll_at(449, || panic!("Invalid query must not fetch"))
                .is_err()
        );
        assert_eq!(selector.snapshot(), previous);
        assert!(selector.reset(1).is_err());
        selector.reset(2).unwrap();
        assert_eq!(selector.snapshot().buffered_frames, 0);
        assert_eq!(selector.snapshot().last_query_ns, None);
        assert!(selector.poll_at(0, || Ok(Some(frame(0, 0)))).is_err());
        assert_eq!(selector.snapshot().buffered_frames, 0);
        let fresh = TimedFrame {
            epoch: 2,
            ..frame(0, 0)
        };
        let mut source = VecDeque::from([fresh]);
        assert_eq!(
            selector
                .poll_at(0, || Ok(source.pop_front()))
                .unwrap()
                .unwrap()
                .epoch,
            2
        );
    }

    #[test]
    fn delayed_due_frame_replaces_the_held_current_without_inventing_eof() {
        let mut selector = VideoTimeline::new(1).unwrap();
        let mut source = VecDeque::from([frame(0, 0)]);
        selector.poll_at(50, || Ok(source.pop_front())).unwrap();
        assert_eq!(
            selector.poll_at(100, || Ok(None)).unwrap().unwrap().index,
            0
        );
        source.push_back(frame(1, 70));
        assert_eq!(
            selector
                .poll_at(100, || Ok(source.pop_front()))
                .unwrap()
                .unwrap()
                .index,
            1
        );
        assert_eq!(selector.snapshot().current_pts_ns, Some(70));
    }

    #[test]
    fn one_poll_fetches_at_most_eight_frames_without_unbounded_storage() {
        let mut selector = VideoTimeline::new(1).unwrap();
        let mut source: VecDeque<_> = (0..30).map(|index| frame(index, index as u64)).collect();
        let mut polls = 0;
        let selected = selector
            .poll_at(29, || {
                polls += 1;
                Ok(source.pop_front())
            })
            .unwrap()
            .unwrap();
        assert_eq!(polls, 8);
        assert_eq!(selected.index, 7);
        assert_eq!(source.len(), 22);
        assert_eq!(selector.snapshot().buffered_frames, 1);
        for _ in 0..3 {
            selector.poll_at(29, || Ok(source.pop_front())).unwrap();
        }
        assert_eq!(selector.snapshot().current_index, Some(29));
        assert!(selector.snapshot().buffered_frames <= 2);
    }

    #[test]
    fn duplicate_or_regressed_source_indices_and_timestamps_fail_instead_of_sorting() {
        for invalid in [frame(0, 2), frame(1, 1), frame(1, 0)] {
            let mut selector = VideoTimeline::new(1).unwrap();
            let mut source = VecDeque::from([frame(0, 1)]);
            selector.poll_at(10, || Ok(source.pop_front())).unwrap();
            let mut source = VecDeque::from([invalid]);
            assert!(selector.poll_at(10, || Ok(source.pop_front())).is_err());
            assert_eq!(selector.snapshot().current_index, Some(0));
        }
        assert!(VideoTimeline::<u8>::new(0).is_err());
        let mut selector = VideoTimeline::<u8>::new(u64::MAX).unwrap();
        assert!(selector.reset(0).is_err());
    }
}
