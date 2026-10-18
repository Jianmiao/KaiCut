//! Single-owner control gate for a presentation backend; no device or window I/O.
//! The owner must serialize backend calls and wait for old work before changing
//! epochs or resizing. This gate invalidates permissions, not submitted GPU work.

use serde::Serialize;

const MAX_WIDTH: u32 = 1920;
const MAX_HEIGHT: u32 = 1080;
const MAX_ERROR_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GatePhase {
    Ready,
    Suspended,
    Resizing,
    Failed,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResizeTicket {
    pub epoch: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateSnapshot {
    pub epoch: u64,
    pub phase: GatePhase,
    /// Last successfully confirmed extent, not a claim about an in-flight resize.
    pub width: u32,
    pub height: u32,
    pub pending_resize: Option<ResizeTicket>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct PresentationGate {
    state: GateSnapshot,
}

impl PresentationGate {
    /// Call after the owner has established its initial backend extent. An
    /// empty extent suspends submission; no pixels are implied by construction.
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        validate_extent(width, height)?;
        Ok(Self {
            state: GateSnapshot {
                epoch: 1,
                phase: extent_phase(width, height),
                width,
                height,
                pending_resize: None,
                error: None,
            },
        })
    }

    pub fn epoch(&self) -> u64 {
        self.state.epoch
    }

    pub fn snapshot(&self) -> GateSnapshot {
        self.state.clone()
    }

    /// Check immediately before a synchronous backend submission on the same
    /// owner. Success is permission only, not a rendering/presentation result.
    pub fn can_submit(&self, epoch: u64) -> Result<(), String> {
        if epoch != self.state.epoch {
            return Err("Presentation frame belongs to an obsolete epoch".into());
        }
        if self.state.phase != GatePhase::Ready {
            return Err(format!(
                "Presentation cannot submit while {:?}",
                self.state.phase
            ));
        }
        Ok(())
    }

    /// Invalidate frames at a seek/control boundary after the owner has handled
    /// backend in-flight work. Failed arithmetic leaves all state untouched.
    pub fn advance_epoch(&mut self) -> Result<u64, String> {
        self.require_stable()?;
        let next = self.next_epoch()?;
        self.state.epoch = next;
        Ok(next)
    }

    /// Block all submission before the backend starts resizing. A zero in
    /// either dimension requests suspension; it never produces a valid surface.
    /// The ticket belongs to this single owner, not to a cross-process protocol.
    pub fn begin_resize(&mut self, width: u32, height: u32) -> Result<ResizeTicket, String> {
        self.require_stable()?;
        validate_extent(width, height)?;
        let ticket = ResizeTicket {
            epoch: self.next_epoch()?,
            width,
            height,
        };
        self.state.epoch = ticket.epoch;
        self.state.phase = GatePhase::Resizing;
        self.state.pending_resize = Some(ticket);
        Ok(ticket)
    }

    /// Report a completed backend resize. A mismatched or duplicate completion
    /// does not mutate the gate, including when it carries a backend error.
    /// A genuine backend failure is terminal and retains the last good extent.
    pub fn finish_resize(
        &mut self,
        ticket: ResizeTicket,
        outcome: Result<(), String>,
    ) -> Result<(), String> {
        if self.state.phase != GatePhase::Resizing
            || self.state.pending_resize != Some(ticket)
            || ticket.epoch != self.state.epoch
        {
            return Err("Resize completion does not match the active ticket".into());
        }
        match outcome {
            Ok(()) => {
                self.state.width = ticket.width;
                self.state.height = ticket.height;
                self.state.phase = extent_phase(ticket.width, ticket.height);
                self.state.pending_resize = None;
                Ok(())
            }
            Err(error) => {
                self.fail(error)?;
                Err(self.state.error.clone().unwrap_or_default())
            }
        }
    }

    /// Latch the first backend failure. A failure clears pending completion
    /// permission, so even a later successful callback cannot revive the gate.
    pub fn fail(&mut self, error: impl Into<String>) -> Result<(), String> {
        if self.state.phase == GatePhase::Closed {
            return Err("Closed presentation gate cannot accept a backend callback".into());
        }
        if self.state.phase == GatePhase::Failed {
            return Ok(());
        }
        self.state.phase = GatePhase::Failed;
        self.state.pending_resize = None;
        self.state.error = Some(bounded_error(error.into()));
        Ok(())
    }

    /// Closing must remain possible after counter exhaustion. At u64::MAX the
    /// epoch stays saturated and the Closed phase rejects every old permission.
    /// Repeated close is an exact no-op and never erases an earlier failure.
    pub fn close(&mut self) -> GateSnapshot {
        if self.state.phase != GatePhase::Closed {
            self.state.epoch = self.state.epoch.saturating_add(1);
            self.state.phase = GatePhase::Closed;
            self.state.pending_resize = None;
        }
        self.snapshot()
    }

    fn require_stable(&self) -> Result<(), String> {
        if !matches!(self.state.phase, GatePhase::Ready | GatePhase::Suspended) {
            return Err(format!(
                "Presentation control cannot start while {:?}",
                self.state.phase
            ));
        }
        Ok(())
    }

    fn next_epoch(&self) -> Result<u64, String> {
        self.state
            .epoch
            .checked_add(1)
            .ok_or_else(|| "Presentation epoch counter is exhausted".into())
    }
}

fn validate_extent(width: u32, height: u32) -> Result<(), String> {
    if width > MAX_WIDTH || height > MAX_HEIGHT {
        return Err("Presentation extent exceeds the 1920 by 1080 diagnostic limit".into());
    }
    Ok(())
}

fn extent_phase(width: u32, height: u32) -> GatePhase {
    if width == 0 || height == 0 {
        GatePhase::Suspended
    } else {
        GatePhase::Ready
    }
}

fn bounded_error(error: String) -> String {
    if error.is_empty() {
        return "Presentation backend failed without an error message".into();
    }
    let mut end = error.len().min(MAX_ERROR_BYTES);
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    error[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seek_invalidates_old_frames_before_the_backend_is_called() {
        let mut gate = PresentationGate::new(320, 180).unwrap();
        let mut backend_calls = 0;
        let old = gate.epoch();
        gate.can_submit(old).unwrap();
        let next = gate.advance_epoch().unwrap();
        let before = gate.snapshot();
        for frame_epoch in [old, next] {
            if gate.can_submit(frame_epoch).is_ok() {
                backend_calls += 1;
            }
        }
        assert_eq!(backend_calls, 1);
        assert_eq!(next, old + 1);
        assert_eq!(gate.snapshot(), before);
    }

    #[test]
    fn resize_blocks_submission_until_the_backend_confirms_the_new_extent() {
        let mut gate = PresentationGate::new(320, 180).unwrap();
        let previous_epoch = gate.epoch();
        let ticket = gate.begin_resize(640, 360).unwrap();
        assert_eq!(gate.snapshot().phase, GatePhase::Resizing);
        assert_eq!((gate.snapshot().width, gate.snapshot().height), (320, 180));
        assert_eq!(gate.snapshot().pending_resize, Some(ticket));
        assert!(gate.can_submit(previous_epoch).is_err());
        assert!(gate.can_submit(ticket.epoch).is_err());
        let before = gate.snapshot();
        assert!(gate.begin_resize(1280, 720).is_err());
        assert!(gate.advance_epoch().is_err());
        assert_eq!(gate.snapshot(), before);
        gate.finish_resize(ticket, Ok(())).unwrap();
        gate.can_submit(ticket.epoch).unwrap();
        assert_eq!((gate.snapshot().width, gate.snapshot().height), (640, 360));
    }

    #[test]
    fn delayed_duplicate_and_mismatched_resize_completions_cannot_change_state() {
        let mut gate = PresentationGate::new(320, 180).unwrap();
        let first = gate.begin_resize(640, 360).unwrap();
        gate.finish_resize(first, Ok(())).unwrap();
        let second = gate.begin_resize(1920, 1080).unwrap();
        let before = gate.snapshot();
        assert!(
            gate.finish_resize(first, Err("late failure".into()))
                .is_err()
        );
        assert!(
            gate.finish_resize(
                ResizeTicket {
                    width: 640,
                    ..second
                },
                Ok(())
            )
            .is_err()
        );
        assert_eq!(gate.snapshot(), before);
        gate.finish_resize(second, Ok(())).unwrap();
        let done = gate.snapshot();
        assert!(gate.finish_resize(second, Ok(())).is_err());
        assert_eq!(gate.snapshot(), done);
    }

    #[test]
    fn either_zero_dimension_suspends_until_a_successful_nonzero_resize() {
        for (width, height) in [(0, 0), (0, 180), (320, 0)] {
            let mut gate = PresentationGate::new(width, height).unwrap();
            assert_eq!(gate.snapshot().phase, GatePhase::Suspended);
            assert!(gate.can_submit(gate.epoch()).is_err());
            let epoch = gate.advance_epoch().unwrap();
            assert!(gate.can_submit(epoch).is_err());
            let restored = gate.begin_resize(320, 180).unwrap();
            gate.finish_resize(restored, Ok(())).unwrap();
            gate.can_submit(restored.epoch).unwrap();
            let hidden = gate.begin_resize(width, height).unwrap();
            assert!(gate.can_submit(hidden.epoch).is_err());
            gate.finish_resize(hidden, Ok(())).unwrap();
            assert_eq!(gate.snapshot().phase, GatePhase::Suspended);
        }
    }

    #[test]
    fn rejected_dimensions_do_not_consume_an_epoch_or_hide_a_ready_surface() {
        for (width, height) in [(1921, 1080), (1920, 1081), (u32::MAX, 0), (0, u32::MAX)] {
            assert!(PresentationGate::new(width, height).is_err());
            let mut gate = PresentationGate::new(1920, 1080).unwrap();
            let before = gate.snapshot();
            assert!(gate.begin_resize(width, height).is_err());
            assert_eq!(gate.snapshot(), before);
            gate.can_submit(gate.epoch()).unwrap();
        }
    }

    #[test]
    fn backend_resize_failure_is_terminal_and_preserves_the_last_good_extent() {
        let mut gate = PresentationGate::new(320, 180).unwrap();
        let ticket = gate.begin_resize(640, 360).unwrap();
        assert_eq!(
            gate.finish_resize(ticket, Err("device removed".into())),
            Err("device removed".into())
        );
        let failed = gate.snapshot();
        assert_eq!(failed.phase, GatePhase::Failed);
        assert_eq!((failed.width, failed.height), (320, 180));
        assert_eq!(failed.pending_resize, None);
        assert!(gate.can_submit(ticket.epoch).is_err());
        assert!(gate.begin_resize(320, 180).is_err());
        assert!(gate.advance_epoch().is_err());
        assert!(gate.finish_resize(ticket, Ok(())).is_err());
        gate.fail("a later failure").unwrap();
        assert_eq!(gate.snapshot(), failed);
    }

    #[test]
    fn device_failure_bounds_unicode_errors_and_cancels_a_pending_completion() {
        let mut gate = PresentationGate::new(320, 180).unwrap();
        let ticket = gate.begin_resize(640, 360).unwrap();
        let message = "设备错误🙂".repeat(100);
        gate.fail(message.clone()).unwrap();
        let failed = gate.snapshot();
        let error = failed.error.as_ref().unwrap();
        assert!(error.len() <= 512 && message.starts_with(error));
        assert!(gate.finish_resize(ticket, Ok(())).is_err());
        assert_eq!(gate.snapshot(), failed);
        let closed = gate.close();
        assert_eq!(closed.error, failed.error);
        assert!(gate.fail("late callback").is_err());
        assert_eq!(gate.snapshot(), closed);
    }

    #[test]
    fn close_is_terminal_idempotent_and_invalidates_an_inflight_resize() {
        let mut gate = PresentationGate::new(320, 180).unwrap();
        let ticket = gate.begin_resize(640, 360).unwrap();
        let closed = gate.close();
        assert_eq!(closed.epoch, ticket.epoch + 1);
        assert_eq!(closed.phase, GatePhase::Closed);
        assert_eq!(closed.pending_resize, None);
        assert_eq!(gate.close(), closed);
        assert!(gate.can_submit(closed.epoch).is_err());
        assert!(gate.advance_epoch().is_err());
        assert!(gate.begin_resize(320, 180).is_err());
        assert!(gate.finish_resize(ticket, Ok(())).is_err());
        assert_eq!(gate.snapshot(), closed);
    }

    #[test]
    fn exhausted_epoch_rejects_new_transactions_but_still_allows_terminal_close() {
        let mut gate = PresentationGate::new(320, 180).unwrap();
        gate.state.epoch = u64::MAX;
        let before = gate.snapshot();
        assert!(gate.advance_epoch().is_err());
        assert_eq!(gate.snapshot(), before);
        assert!(gate.begin_resize(640, 360).is_err());
        assert_eq!(gate.snapshot(), before);
        let closed = gate.close();
        assert_eq!(closed.epoch, u64::MAX);
        assert_eq!(closed.phase, GatePhase::Closed);
        assert!(gate.can_submit(u64::MAX).is_err());
        assert_eq!(gate.close(), closed);
    }

    #[test]
    fn empty_backend_failure_still_leaves_an_actionable_terminal_error() {
        let mut gate = PresentationGate::new(320, 180).unwrap();
        gate.fail("").unwrap();
        assert_eq!(gate.snapshot().phase, GatePhase::Failed);
        assert!(!gate.snapshot().error.unwrap().is_empty());
        assert!(gate.can_submit(gate.epoch()).is_err());
    }
}
