//! Device-independent lifecycle for a native preview surface.
//!
//! A single backend owner serializes all operations and confirms a ticket only
//! after the corresponding backend work succeeds. This module revokes frame
//! permission; it does not cancel GPU work or establish that pixels are visible.

use serde::Serialize;

const MAX_WIDTH: u32 = 1920;
const MAX_HEIGHT: u32 = 1080;
const MAX_ERROR_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceMetrics {
    pub logical_width: f64,
    pub logical_height: f64,
    pub scale_factor: f64,
    /// Explicit minimization, not visibility. A hidden diagnostic host can still
    /// render into its nonzero backbuffer and must report visibility separately.
    pub minimized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalExtent {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedMetrics {
    pub logical: SurfaceMetrics,
    pub physical: PhysicalExtent,
    /// A suspended layout must not be passed to DXGI ResizeBuffers. Minimized
    /// layouts retain their physical size; a zero extent also suspends.
    pub suspended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfacePhase {
    Detached,
    Attaching,
    Ready,
    Suspended,
    Updating,
    Detaching,
    Failed,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceOperation {
    Attach,
    Layout,
    Detach,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceTicket {
    pub epoch: u64,
    pub operation: SurfaceOperation,
    pub metrics: Option<ValidatedMetrics>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceSnapshot {
    pub epoch: u64,
    pub phase: SurfacePhase,
    /// Last successfully completed attachment/layout. During a pending update
    /// or backend failure this remains the last good value. Successful detach
    /// clears it. Close preserves it only for diagnostics, never for submission.
    pub confirmed: Option<ValidatedMetrics>,
    pub pending: Option<SurfaceTicket>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct SurfaceLifecycle {
    state: SurfaceSnapshot,
}

impl SurfaceLifecycle {
    pub fn new() -> Self {
        Self {
            state: SurfaceSnapshot {
                epoch: 1,
                phase: SurfacePhase::Detached,
                confirmed: None,
                pending: None,
                error: None,
            },
        }
    }

    pub fn epoch(&self) -> u64 {
        self.state.epoch
    }

    pub fn snapshot(&self) -> SurfaceSnapshot {
        self.state.clone()
    }

    /// Check immediately before submitting a frame on this owner. This is a
    /// permission check only, not an assertion that a frame was displayed.
    pub fn can_submit(&self, epoch: u64) -> Result<(), String> {
        self.require_epoch(epoch)?;
        if self.state.phase != SurfacePhase::Ready {
            return Err(format!(
                "Preview surface cannot submit while {:?}",
                self.state.phase
            ));
        }
        Ok(())
    }

    /// Validate and reserve an attachment. No backend has been created by this
    /// call. A zero or minimized layout permits attachment in suspended state;
    /// the owner must defer nonzero GPU resource allocation until restoration.
    pub fn begin_attach(&mut self, metrics: SurfaceMetrics) -> Result<SurfaceTicket, String> {
        if self.state.phase != SurfacePhase::Detached {
            return Err("Preview surface must be detached before attachment".into());
        }
        let metrics = validate_metrics(metrics)?;
        self.begin(SurfaceOperation::Attach, Some(metrics))
    }

    pub fn begin_layout(
        &mut self,
        expected_epoch: u64,
        metrics: SurfaceMetrics,
    ) -> Result<SurfaceTicket, String> {
        self.require_epoch(expected_epoch)?;
        self.require_attached()?;
        let metrics = validate_metrics(metrics)?;
        self.begin(SurfaceOperation::Layout, Some(metrics))
    }

    /// Revoke frames before the owner detaches its composition visual. Confirm
    /// this ticket after the backend has committed and completed detachment.
    pub fn begin_detach(&mut self, expected_epoch: u64) -> Result<SurfaceTicket, String> {
        self.require_epoch(expected_epoch)?;
        self.require_attached()?;
        self.begin(SurfaceOperation::Detach, None)
    }

    pub fn finish(
        &mut self,
        ticket: SurfaceTicket,
        outcome: Result<(), String>,
    ) -> Result<(), String> {
        if self.state.pending != Some(ticket)
            || self.state.epoch != ticket.epoch
            || self.state.phase != pending_phase(ticket.operation)
        {
            return Err("Preview surface completion does not match the pending ticket".into());
        }
        if let Err(error) = outcome {
            self.fail(ticket.epoch, error)?;
            return Err(self.state.error.clone().unwrap_or_default());
        }

        match ticket.operation {
            SurfaceOperation::Attach | SurfaceOperation::Layout => {
                // begin() only creates these operations with validated metrics;
                // matching the pending ticket prevents caller-forged completion.
                let metrics = ticket
                    .metrics
                    .ok_or_else(|| "Preview surface ticket lacks validated metrics".to_owned())?;
                self.state.confirmed = Some(metrics);
                self.state.phase = if metrics.suspended {
                    SurfacePhase::Suspended
                } else {
                    SurfacePhase::Ready
                };
            }
            SurfaceOperation::Detach => {
                self.state.confirmed = None;
                self.state.phase = SurfacePhase::Detached;
            }
        }
        self.state.pending = None;
        Ok(())
    }

    /// Latch the first failure only for the current generation. Delayed errors
    /// from a detached or resized surface cannot poison a newer generation.
    pub fn fail(&mut self, expected_epoch: u64, error: impl Into<String>) -> Result<(), String> {
        self.require_epoch(expected_epoch)?;
        match self.state.phase {
            SurfacePhase::Closed | SurfacePhase::Detached => {
                return Err("Inactive preview surface cannot accept a backend failure".into());
            }
            SurfacePhase::Failed => return Ok(()),
            _ => {}
        }
        self.state.phase = SurfacePhase::Failed;
        self.state.pending = None;
        self.state.error = Some(bounded_error(error.into()));
        Ok(())
    }

    /// Revokes all permission immediately. The backend owner still must release
    /// its resources separately. Close is idempotent even at epoch exhaustion.
    pub fn close(&mut self) -> SurfaceSnapshot {
        if self.state.phase != SurfacePhase::Closed {
            self.state.epoch = self.state.epoch.saturating_add(1);
            self.state.phase = SurfacePhase::Closed;
            self.state.pending = None;
        }
        self.snapshot()
    }

    fn require_epoch(&self, expected_epoch: u64) -> Result<(), String> {
        if expected_epoch != self.state.epoch {
            return Err("Preview surface update belongs to an obsolete epoch".into());
        }
        Ok(())
    }

    fn require_attached(&self) -> Result<(), String> {
        if !matches!(
            self.state.phase,
            SurfacePhase::Ready | SurfacePhase::Suspended
        ) {
            return Err(format!(
                "Preview surface control cannot start while {:?}",
                self.state.phase
            ));
        }
        Ok(())
    }

    fn begin(
        &mut self,
        operation: SurfaceOperation,
        metrics: Option<ValidatedMetrics>,
    ) -> Result<SurfaceTicket, String> {
        let epoch = self
            .state
            .epoch
            .checked_add(1)
            .ok_or_else(|| "Preview surface epoch counter is exhausted".to_owned())?;
        let ticket = SurfaceTicket {
            epoch,
            operation,
            metrics,
        };
        self.state.epoch = epoch;
        self.state.phase = pending_phase(operation);
        self.state.pending = Some(ticket);
        Ok(ticket)
    }
}

impl Default for SurfaceLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_metrics(metrics: SurfaceMetrics) -> Result<ValidatedMetrics, String> {
    if !metrics.scale_factor.is_finite() || !(0.5..=4.0).contains(&metrics.scale_factor) {
        return Err("Preview surface scale factor must be finite and between 0.5 and 4".into());
    }
    let width = physical_dimension(metrics.logical_width, metrics.scale_factor, MAX_WIDTH)?;
    let height = physical_dimension(metrics.logical_height, metrics.scale_factor, MAX_HEIGHT)?;
    Ok(ValidatedMetrics {
        logical: metrics,
        physical: PhysicalExtent { width, height },
        suspended: metrics.minimized || width == 0 || height == 0,
    })
}

/// Ceiling retains the outermost fractional logical pixel. Reject overflow and
/// out-of-range results before converting floats to integers; never saturate.
fn physical_dimension(logical: f64, scale: f64, maximum: u32) -> Result<u32, String> {
    if !logical.is_finite() || logical < 0.0 {
        return Err("Preview surface logical dimensions must be finite and nonnegative".into());
    }
    let physical = (logical * scale).ceil();
    if !physical.is_finite() || physical > f64::from(maximum) {
        return Err("Preview surface extent exceeds the 1920 by 1080 diagnostic limit".into());
    }
    Ok(physical as u32)
}

fn pending_phase(operation: SurfaceOperation) -> SurfacePhase {
    match operation {
        SurfaceOperation::Attach => SurfacePhase::Attaching,
        SurfaceOperation::Layout => SurfacePhase::Updating,
        SurfaceOperation::Detach => SurfacePhase::Detaching,
    }
}

fn bounded_error(error: String) -> String {
    if error.is_empty() {
        return "Preview surface backend failed without an error message".into();
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

    fn metrics(width: f64, height: f64, scale: f64) -> SurfaceMetrics {
        SurfaceMetrics {
            logical_width: width,
            logical_height: height,
            scale_factor: scale,
            minimized: false,
        }
    }

    fn attached() -> SurfaceLifecycle {
        let mut lifecycle = SurfaceLifecycle::new();
        let ticket = lifecycle.begin_attach(metrics(320.0, 180.0, 1.0)).unwrap();
        lifecycle.finish(ticket, Ok(())).unwrap();
        lifecycle
    }

    #[test]
    fn attachment_must_be_confirmed_before_frames_are_admitted() {
        let mut lifecycle = SurfaceLifecycle::new();
        assert_eq!(lifecycle.epoch(), 1);
        assert_eq!(lifecycle.snapshot().phase, SurfacePhase::Detached);
        assert!(lifecycle.can_submit(1).is_err());
        let ticket = lifecycle.begin_attach(metrics(320.0, 180.0, 1.0)).unwrap();
        assert_eq!(ticket.epoch, 2);
        assert_eq!(ticket.operation, SurfaceOperation::Attach);
        assert_eq!(lifecycle.snapshot().phase, SurfacePhase::Attaching);
        assert_eq!(lifecycle.snapshot().confirmed, None);
        assert!(lifecycle.can_submit(ticket.epoch).is_err());
        lifecycle.finish(ticket, Ok(())).unwrap();
        lifecycle.can_submit(ticket.epoch).unwrap();
        assert!(lifecycle.can_submit(1).is_err());
        assert_eq!(lifecycle.snapshot().confirmed, ticket.metrics);
    }

    #[test]
    fn layout_changes_block_submission_and_preserve_last_good_extent_until_confirmed() {
        let mut lifecycle = attached();
        let old_epoch = lifecycle.epoch();
        let old_layout = lifecycle.snapshot().confirmed;
        let ticket = lifecycle
            .begin_layout(old_epoch, metrics(640.0, 360.0, 1.5))
            .unwrap();
        assert_eq!(ticket.operation, SurfaceOperation::Layout);
        assert_eq!(lifecycle.snapshot().phase, SurfacePhase::Updating);
        assert_eq!(lifecycle.snapshot().confirmed, old_layout);
        assert!(lifecycle.can_submit(old_epoch).is_err());
        assert!(lifecycle.can_submit(ticket.epoch).is_err());
        let pending = lifecycle.snapshot();
        assert!(lifecycle.begin_detach(ticket.epoch).is_err());
        assert!(
            lifecycle
                .begin_layout(ticket.epoch, metrics(320.0, 180.0, 1.0))
                .is_err()
        );
        assert_eq!(lifecycle.snapshot(), pending);
        lifecycle.finish(ticket, Ok(())).unwrap();
        lifecycle.can_submit(ticket.epoch).unwrap();
        assert_eq!(
            lifecycle.snapshot().confirmed.unwrap().physical,
            PhysicalExtent {
                width: 960,
                height: 540
            }
        );
    }

    #[test]
    fn logical_sizes_use_ceil_at_supported_dpi_scales() {
        for (scale, width, height) in [(1.0, 321, 181), (1.5, 481, 271), (2.0, 641, 361)] {
            let mut lifecycle = SurfaceLifecycle::new();
            let ticket = lifecycle
                .begin_attach(metrics(320.25, 180.25, scale))
                .unwrap();
            assert_eq!(
                ticket.metrics.unwrap().physical,
                PhysicalExtent { width, height }
            );
            lifecycle.finish(ticket, Ok(())).unwrap();
            lifecycle.can_submit(ticket.epoch).unwrap();
        }
    }

    #[test]
    fn invalid_sizes_and_scales_cannot_mutate_or_invalidate_current_surface() {
        let mut lifecycle = attached();
        let before = lifecycle.snapshot();
        let cases = [
            metrics(-1.0, 180.0, 1.0),
            metrics(320.0, -1.0, 1.0),
            metrics(f64::NAN, 180.0, 1.0),
            metrics(320.0, f64::INFINITY, 1.0),
            metrics(f64::MAX, 180.0, 4.0),
            metrics(1920.01, 1080.0, 1.0),
            metrics(1920.0, 1080.01, 1.0),
            metrics(961.0, 540.0, 2.0),
            metrics(320.0, 180.0, 0.49),
            metrics(320.0, 180.0, 4.01),
            metrics(320.0, 180.0, f64::NAN),
            metrics(320.0, 180.0, f64::INFINITY),
        ];
        for invalid in cases {
            assert!(lifecycle.begin_layout(before.epoch, invalid).is_err());
            assert_eq!(lifecycle.snapshot(), before);
            lifecycle.can_submit(before.epoch).unwrap();
        }
    }

    #[test]
    fn invalid_attachment_leaves_the_detached_owner_reusable() {
        let mut lifecycle = SurfaceLifecycle::new();
        let before = lifecycle.snapshot();
        assert!(
            lifecycle
                .begin_attach(metrics(1921.0, 1080.0, 1.0))
                .is_err()
        );
        assert_eq!(lifecycle.snapshot(), before);
        let ticket = lifecycle.begin_attach(metrics(960.0, 540.0, 2.0)).unwrap();
        lifecycle.finish(ticket, Ok(())).unwrap();
        lifecycle.can_submit(ticket.epoch).unwrap();
    }

    #[test]
    fn stale_layout_events_and_backend_failures_cannot_poison_newer_state() {
        let mut lifecycle = attached();
        let old_epoch = lifecycle.epoch();
        let ticket = lifecycle
            .begin_layout(old_epoch, metrics(640.0, 360.0, 1.0))
            .unwrap();
        lifecycle.finish(ticket, Ok(())).unwrap();
        let before = lifecycle.snapshot();
        assert!(
            lifecycle
                .begin_layout(old_epoch, metrics(1.0, 1.0, 1.0))
                .is_err()
        );
        assert!(lifecycle.begin_detach(old_epoch).is_err());
        assert!(lifecycle.fail(old_epoch, "late device error").is_err());
        assert_eq!(lifecycle.snapshot(), before);
        lifecycle.can_submit(ticket.epoch).unwrap();
    }

    #[test]
    fn mismatched_and_duplicate_completions_leave_pending_transaction_unchanged() {
        let mut lifecycle = attached();
        let first = lifecycle
            .begin_layout(lifecycle.epoch(), metrics(640.0, 360.0, 1.0))
            .unwrap();
        lifecycle.finish(first, Ok(())).unwrap();
        let second = lifecycle
            .begin_layout(lifecycle.epoch(), metrics(960.0, 540.0, 1.0))
            .unwrap();
        let before = lifecycle.snapshot();
        assert!(
            lifecycle
                .finish(first, Err("stale failure".into()))
                .is_err()
        );
        assert!(
            lifecycle
                .finish(
                    SurfaceTicket {
                        operation: SurfaceOperation::Detach,
                        ..second
                    },
                    Ok(())
                )
                .is_err()
        );
        assert!(
            lifecycle
                .finish(
                    SurfaceTicket {
                        metrics: first.metrics,
                        ..second
                    },
                    Ok(())
                )
                .is_err()
        );
        assert_eq!(lifecycle.snapshot(), before);
        lifecycle.finish(second, Ok(())).unwrap();
        let done = lifecycle.snapshot();
        assert!(lifecycle.finish(second, Ok(())).is_err());
        assert_eq!(lifecycle.snapshot(), done);
    }

    #[test]
    fn failed_layout_preserves_last_confirmed_extent_and_cannot_be_revived() {
        let mut lifecycle = attached();
        let last_good = lifecycle.snapshot().confirmed;
        let ticket = lifecycle
            .begin_layout(lifecycle.epoch(), metrics(640.0, 360.0, 1.0))
            .unwrap();
        assert!(
            lifecycle
                .finish(ticket, Err("ResizeBuffers failed".into()))
                .is_err()
        );
        let failed = lifecycle.snapshot();
        assert_eq!(failed.phase, SurfacePhase::Failed);
        assert_eq!(failed.confirmed, last_good);
        assert_eq!(failed.pending, None);
        assert!(lifecycle.can_submit(ticket.epoch).is_err());
        assert!(lifecycle.finish(ticket, Ok(())).is_err());
        assert!(lifecycle.begin_attach(metrics(320.0, 180.0, 1.0)).is_err());
        assert!(
            lifecycle
                .begin_layout(ticket.epoch, metrics(320.0, 180.0, 1.0))
                .is_err()
        );
        assert!(lifecycle.begin_detach(ticket.epoch).is_err());
        lifecycle.fail(ticket.epoch, "second failure").unwrap();
        assert_eq!(lifecycle.snapshot(), failed);
    }

    #[test]
    fn zero_width_or_height_suspends_until_a_successful_nonzero_layout() {
        for (width, height) in [(0.0, 180.0), (320.0, 0.0), (0.0, 0.0)] {
            let mut lifecycle = attached();
            let ticket = lifecycle
                .begin_layout(lifecycle.epoch(), metrics(width, height, 1.0))
                .unwrap();
            assert!(ticket.metrics.unwrap().suspended);
            lifecycle.finish(ticket, Ok(())).unwrap();
            assert_eq!(lifecycle.snapshot().phase, SurfacePhase::Suspended);
            assert!(lifecycle.can_submit(ticket.epoch).is_err());
            let restored = lifecycle
                .begin_layout(lifecycle.epoch(), metrics(320.0, 180.0, 1.0))
                .unwrap();
            lifecycle.finish(restored, Ok(())).unwrap();
            lifecycle.can_submit(restored.epoch).unwrap();
            assert!(!lifecycle.snapshot().confirmed.unwrap().suspended);
        }
    }

    #[test]
    fn minimized_nonzero_surface_is_suspended_without_discarding_its_layout() {
        let mut lifecycle = attached();
        let minimized = SurfaceMetrics {
            minimized: true,
            ..metrics(320.0, 180.0, 1.5)
        };
        let ticket = lifecycle
            .begin_layout(lifecycle.epoch(), minimized)
            .unwrap();
        assert_eq!(
            ticket.metrics.unwrap().physical,
            PhysicalExtent {
                width: 480,
                height: 270
            }
        );
        assert!(ticket.metrics.unwrap().suspended);
        lifecycle.finish(ticket, Ok(())).unwrap();
        assert!(lifecycle.can_submit(ticket.epoch).is_err());
        let restored = lifecycle
            .begin_layout(lifecycle.epoch(), metrics(320.0, 180.0, 1.5))
            .unwrap();
        lifecycle.finish(restored, Ok(())).unwrap();
        lifecycle.can_submit(restored.epoch).unwrap();
    }

    #[test]
    fn attachment_can_start_suspended_without_admitting_a_frame() {
        let mut lifecycle = SurfaceLifecycle::new();
        let ticket = lifecycle.begin_attach(metrics(0.0, 0.0, 1.0)).unwrap();
        lifecycle.finish(ticket, Ok(())).unwrap();
        assert_eq!(lifecycle.snapshot().phase, SurfacePhase::Suspended);
        assert!(lifecycle.can_submit(ticket.epoch).is_err());
        let restored = lifecycle
            .begin_layout(lifecycle.epoch(), metrics(320.0, 180.0, 1.0))
            .unwrap();
        lifecycle.finish(restored, Ok(())).unwrap();
        lifecycle.can_submit(restored.epoch).unwrap();
    }

    #[test]
    fn detach_and_reattach_reject_frames_and_callbacks_from_the_original_surface() {
        let mut lifecycle = attached();
        let original_epoch = lifecycle.epoch();
        let detach = lifecycle.begin_detach(original_epoch).unwrap();
        assert_eq!(lifecycle.snapshot().phase, SurfacePhase::Detaching);
        assert!(lifecycle.can_submit(original_epoch).is_err());
        assert!(lifecycle.can_submit(detach.epoch).is_err());
        lifecycle.finish(detach, Ok(())).unwrap();
        assert_eq!(lifecycle.snapshot().phase, SurfacePhase::Detached);
        assert_eq!(lifecycle.snapshot().confirmed, None);
        let reattach = lifecycle.begin_attach(metrics(640.0, 360.0, 1.0)).unwrap();
        lifecycle.finish(reattach, Ok(())).unwrap();
        lifecycle.can_submit(reattach.epoch).unwrap();
        let before = lifecycle.snapshot();
        assert!(lifecycle.can_submit(original_epoch).is_err());
        assert!(lifecycle.finish(detach, Ok(())).is_err());
        assert_eq!(lifecycle.snapshot(), before);
        assert!(reattach.epoch > detach.epoch);
    }

    #[test]
    fn failed_attachment_or_detachment_is_terminal() {
        let mut lifecycle = SurfaceLifecycle::new();
        let attach = lifecycle.begin_attach(metrics(320.0, 180.0, 1.0)).unwrap();
        assert!(
            lifecycle
                .finish(attach, Err("attach failed".into()))
                .is_err()
        );
        assert_eq!(lifecycle.snapshot().phase, SurfacePhase::Failed);
        assert_eq!(lifecycle.snapshot().confirmed, None);
        assert!(lifecycle.finish(attach, Ok(())).is_err());

        let mut lifecycle = attached();
        let last_good = lifecycle.snapshot().confirmed;
        let detach = lifecycle.begin_detach(lifecycle.epoch()).unwrap();
        assert!(
            lifecycle
                .finish(detach, Err("detach failed".into()))
                .is_err()
        );
        assert_eq!(lifecycle.snapshot().phase, SurfacePhase::Failed);
        assert_eq!(lifecycle.snapshot().confirmed, last_good);
        assert!(lifecycle.can_submit(detach.epoch).is_err());
    }

    #[test]
    fn closing_cancels_pending_permission_and_never_allows_revival() {
        let mut lifecycle = attached();
        let pending = lifecycle
            .begin_layout(lifecycle.epoch(), metrics(640.0, 360.0, 1.0))
            .unwrap();
        let closed = lifecycle.close();
        assert_eq!(closed.phase, SurfacePhase::Closed);
        assert_eq!(closed.pending, None);
        assert!(lifecycle.can_submit(pending.epoch).is_err());
        assert!(lifecycle.can_submit(closed.epoch).is_err());
        assert!(lifecycle.finish(pending, Ok(())).is_err());
        assert!(lifecycle.begin_attach(metrics(320.0, 180.0, 1.0)).is_err());
        assert!(lifecycle.begin_detach(closed.epoch).is_err());
        assert!(lifecycle.fail(closed.epoch, "late failure").is_err());
        assert_eq!(lifecycle.close(), closed);
        assert_eq!(lifecycle.snapshot(), closed);
    }

    #[test]
    fn epoch_exhaustion_never_wraps_and_cannot_prevent_close() {
        let mut lifecycle = attached();
        lifecycle.state.epoch = u64::MAX;
        let before = lifecycle.snapshot();
        assert!(
            lifecycle
                .begin_layout(u64::MAX, metrics(640.0, 360.0, 1.0))
                .is_err()
        );
        assert!(lifecycle.begin_detach(u64::MAX).is_err());
        assert_eq!(lifecycle.snapshot(), before);
        let closed = lifecycle.close();
        assert_eq!(closed.epoch, u64::MAX);
        assert_eq!(closed.phase, SurfacePhase::Closed);
        assert!(lifecycle.can_submit(u64::MAX).is_err());
    }

    #[test]
    fn the_first_backend_error_is_utf8_bounded_and_retained_after_close() {
        let mut lifecycle = attached();
        lifecycle
            .fail(lifecycle.epoch(), "错误".repeat(300))
            .unwrap();
        let failed = lifecycle.snapshot();
        let error = failed.error.as_ref().unwrap();
        assert!(!error.is_empty());
        assert!(error.len() <= 512);
        assert!(std::str::from_utf8(error.as_bytes()).is_ok());
        lifecycle.fail(lifecycle.epoch(), "replacement").unwrap();
        assert_eq!(lifecycle.snapshot(), failed);
        assert_eq!(lifecycle.close().error, failed.error);
    }
}
