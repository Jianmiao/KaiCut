//! Pure lifecycle policy for a native preview probe, not an audio/video device.
//! Readiness and EOF flags mean the driver has actually observed those events.

use serde::Serialize;
use std::fmt;

const MAX_ERROR_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionPhase {
    Preparing,
    Ready,
    Playing,
    Paused,
    Ended,
    Cancelled,
    Closed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub epoch: u64,
    pub phase: SessionPhase,
    pub audio_ready: bool,
    pub video_ready: bool,
    pub audio_source_eof: bool,
    pub device_drained: bool,
    pub video_eof: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    StaleEpoch {
        expected: u64,
        received: u64,
    },
    InvalidTransition {
        phase: SessionPhase,
        operation: &'static str,
    },
    EpochExhausted,
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleEpoch { expected, received } => write!(
                formatter,
                "Callback epoch {received} is obsolete; current epoch is {expected}"
            ),
            Self::InvalidTransition { phase, operation } => write!(
                formatter,
                "Cannot {operation} while preview session is {phase:?}"
            ),
            Self::EpochExhausted => {
                formatter.write_str("Preview session epoch cannot advance without wrapping")
            }
        }
    }
}
impl std::error::Error for SessionError {}

/// Lifecycle policy only. The driver must independently stop/reset devices,
/// invalidate renderer work, preserve paused buffers, and observe EOF/drain.
/// Construct a new policy for a new session after cancellation or failure.
#[derive(Debug)]
pub struct NativePreviewSession {
    epoch: u64,
    phase: SessionPhase,
    audio_ready: bool,
    video_ready: bool,
    audio_source_eof: bool,
    device_drained: bool,
    video_eof: bool,
    error: Option<String>,
}

impl Default for NativePreviewSession {
    fn default() -> Self {
        Self::new()
    }
}

impl NativePreviewSession {
    pub fn new() -> Self {
        Self {
            epoch: 1,
            phase: SessionPhase::Preparing,
            audio_ready: false,
            video_ready: false,
            audio_source_eof: false,
            device_drained: false,
            video_eof: false,
            error: None,
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn phase(&self) -> SessionPhase {
        self.phase
    }
    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            epoch: self.epoch,
            phase: self.phase,
            audio_ready: self.audio_ready,
            video_ready: self.video_ready,
            audio_source_eof: self.audio_source_eof,
            device_drained: self.device_drained,
            video_eof: self.video_eof,
            error: self.error.clone(),
        }
    }

    /// The driver has queued playable audio for this epoch, not merely opened
    /// a decoder. Video-only sessions need an explicit driver silence policy.
    pub fn mark_audio_ready(&mut self, epoch: u64) -> Result<SessionPhase, SessionError> {
        self.accept_callback(epoch, "mark audio ready")?;
        self.require_phase(
            &[SessionPhase::Preparing, SessionPhase::Ready],
            "mark audio ready",
        )?;
        self.audio_ready = true;
        self.finish_preparation();
        Ok(self.phase)
    }

    /// The driver has a usable first decoded video frame for this epoch.
    pub fn mark_video_ready(&mut self, epoch: u64) -> Result<SessionPhase, SessionError> {
        self.accept_callback(epoch, "mark video ready")?;
        self.require_phase(
            &[SessionPhase::Preparing, SessionPhase::Ready],
            "mark video ready",
        )?;
        self.video_ready = true;
        self.finish_preparation();
        Ok(self.phase)
    }

    pub fn play(&mut self) -> Result<SessionPhase, SessionError> {
        self.require_phase(&[SessionPhase::Ready, SessionPhase::Paused], "play")?;
        if !self.audio_ready || !self.video_ready {
            return Err(self.invalid("play without prepared audio and video"));
        }
        self.phase = SessionPhase::Playing;
        self.finish_playback();
        Ok(self.phase)
    }

    /// Pause leaves this epoch and decoder readiness intact. The driver owns
    /// renderer invalidation and calls this only after a successful pause.
    pub fn pause(&mut self) -> Result<SessionPhase, SessionError> {
        self.require_phase(&[SessionPhase::Playing, SessionPhase::Paused], "pause")?;
        self.phase = SessionPhase::Paused;
        Ok(self.phase)
    }

    /// Invalidate old decoder callbacks before preparing an exact new source
    /// position. The driver's source/clock offsets are not guessed here.
    pub fn begin_seek(&mut self) -> Result<u64, SessionError> {
        self.require_phase(
            &[
                SessionPhase::Ready,
                SessionPhase::Playing,
                SessionPhase::Paused,
                SessionPhase::Ended,
            ],
            "begin seek",
        )?;
        let next = self
            .epoch
            .checked_add(1)
            .ok_or(SessionError::EpochExhausted)?;
        self.clear_observations();
        self.epoch = next;
        self.phase = SessionPhase::Preparing;
        Ok(next)
    }

    /// Source EOF may arrive while a short fixture is still preparing. It does
    /// not imply that queued audio was rendered or that video is complete.
    pub fn mark_audio_eof(&mut self, epoch: u64) -> Result<SessionPhase, SessionError> {
        self.accept_callback(epoch, "mark audio EOF")?;
        self.audio_source_eof = true;
        self.finish_playback();
        Ok(self.phase)
    }

    pub fn mark_video_eof(&mut self, epoch: u64) -> Result<SessionPhase, SessionError> {
        self.accept_callback(epoch, "mark video EOF")?;
        self.video_eof = true;
        self.finish_playback();
        Ok(self.phase)
    }

    /// A temporarily empty device queue before source EOF is not final drain.
    /// The driver must observe both source EOF and actual device queue drain.
    pub fn mark_device_drained(&mut self, epoch: u64) -> Result<SessionPhase, SessionError> {
        self.accept_callback(epoch, "mark device drained")?;
        self.require_phase(
            &[SessionPhase::Playing, SessionPhase::Paused],
            "mark device drained",
        )?;
        if !self.audio_source_eof {
            return Err(self.invalid("mark final drain before audio source EOF"));
        }
        self.device_drained = true;
        self.finish_playback();
        Ok(self.phase)
    }

    pub fn cancel(&mut self) -> Result<u64, SessionError> {
        if self.phase == SessionPhase::Cancelled {
            return Ok(self.epoch);
        }
        self.require_phase(
            &[
                SessionPhase::Preparing,
                SessionPhase::Ready,
                SessionPhase::Playing,
                SessionPhase::Paused,
                SessionPhase::Ended,
            ],
            "cancel",
        )?;
        let next = self
            .epoch
            .checked_add(1)
            .ok_or(SessionError::EpochExhausted)?;
        self.clear_observations();
        self.epoch = next;
        self.phase = SessionPhase::Cancelled;
        Ok(next)
    }

    /// Closing is terminal even if the epoch counter is exhausted. Callback
    /// guards reject Closed without needing to wrap or increment that counter.
    pub fn close(&mut self) -> Result<(), SessionError> {
        self.clear_observations();
        self.phase = SessionPhase::Closed;
        Ok(())
    }

    pub fn fail(&mut self, epoch: u64, message: &str) -> Result<SessionPhase, SessionError> {
        self.accept_callback(epoch, "fail")?;
        let mut end = message.len().min(MAX_ERROR_BYTES);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        self.error = Some(if end == 0 {
            "Native preview failed".into()
        } else {
            message[..end].to_owned()
        });
        self.phase = SessionPhase::Failed;
        Ok(self.phase)
    }

    fn invalid(&self, operation: &'static str) -> SessionError {
        SessionError::InvalidTransition {
            phase: self.phase,
            operation,
        }
    }
    fn require_phase(
        &self,
        phases: &[SessionPhase],
        operation: &'static str,
    ) -> Result<(), SessionError> {
        if phases.contains(&self.phase) {
            Ok(())
        } else {
            Err(self.invalid(operation))
        }
    }
    fn accept_callback(&self, epoch: u64, operation: &'static str) -> Result<(), SessionError> {
        if epoch != self.epoch {
            return Err(SessionError::StaleEpoch {
                expected: self.epoch,
                received: epoch,
            });
        }
        self.require_phase(
            &[
                SessionPhase::Preparing,
                SessionPhase::Ready,
                SessionPhase::Playing,
                SessionPhase::Paused,
            ],
            operation,
        )
    }
    fn finish_preparation(&mut self) {
        if self.audio_ready && self.video_ready && self.phase == SessionPhase::Preparing {
            self.phase = SessionPhase::Ready;
        }
    }
    fn finish_playback(&mut self) {
        if self.phase == SessionPhase::Playing
            && self.audio_source_eof
            && self.device_drained
            && self.video_eof
        {
            self.phase = SessionPhase::Ended;
        }
    }
    fn clear_observations(&mut self) {
        self.audio_ready = false;
        self.video_ready = false;
        self.audio_source_eof = false;
        self.device_drained = false;
        self.video_eof = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared() -> NativePreviewSession {
        let mut session = NativePreviewSession::new();
        let epoch = session.epoch();
        session.mark_audio_ready(epoch).unwrap();
        session.mark_video_ready(epoch).unwrap();
        session
    }

    #[test]
    fn preparation_requires_both_real_readiness_observations() {
        let mut session = NativePreviewSession::new();
        assert_eq!(session.phase(), SessionPhase::Preparing);
        assert!(session.play().is_err());
        let epoch = session.epoch();
        session.mark_audio_ready(epoch).unwrap();
        assert_eq!(session.phase(), SessionPhase::Preparing);
        assert!(session.play().is_err());
        session.mark_video_ready(epoch).unwrap();
        assert_eq!(session.phase(), SessionPhase::Ready);
        assert_eq!(session.play().unwrap(), SessionPhase::Playing);
    }

    #[test]
    fn pause_and_resume_preserve_epoch_and_prepared_buffers() {
        let mut session = prepared();
        let epoch = session.epoch();
        assert!(session.pause().is_err());
        session.play().unwrap();
        assert!(session.play().is_err());
        session.pause().unwrap();
        let paused = session.snapshot();
        session.pause().unwrap();
        assert_eq!(session.snapshot(), paused);
        assert_eq!(session.epoch(), epoch);
        assert!(paused.audio_ready && paused.video_ready);
        assert_eq!(session.play().unwrap(), SessionPhase::Playing);
    }

    #[test]
    fn seek_invalidates_every_old_async_callback_and_requires_fresh_preparation() {
        let mut session = prepared();
        let old = session.epoch();
        session.play().unwrap();
        let fresh = session.begin_seek().unwrap();
        assert!(fresh > old);
        let before = session.snapshot();
        assert_eq!(before.phase, SessionPhase::Preparing);
        assert!(!before.audio_ready && !before.video_ready);
        assert!(session.mark_audio_ready(old).is_err());
        assert!(session.mark_video_ready(old).is_err());
        assert!(session.mark_audio_eof(old).is_err());
        assert!(session.mark_video_eof(old).is_err());
        assert!(session.mark_device_drained(old).is_err());
        assert!(session.fail(old, "obsolete decoder failed").is_err());
        assert_eq!(session.snapshot(), before);
        assert!(session.play().is_err());
        session.mark_audio_ready(fresh).unwrap();
        session.mark_video_ready(fresh).unwrap();
        session.play().unwrap();
    }

    #[test]
    fn ending_requires_source_eof_device_drain_and_video_eof() {
        let mut session = prepared();
        let epoch = session.epoch();
        session.play().unwrap();
        assert!(
            session.mark_device_drained(epoch).is_err(),
            "Temporary empty audio buffers are not final drain"
        );
        session.mark_audio_eof(epoch).unwrap();
        session.mark_video_eof(epoch).unwrap();
        assert_eq!(session.phase(), SessionPhase::Playing);
        session.mark_device_drained(epoch).unwrap();
        assert_eq!(session.phase(), SessionPhase::Ended);
        assert!(
            session.play().is_err(),
            "Ended sessions never silently rewind"
        );
        assert!(session.begin_seek().is_ok());
        assert_eq!(session.phase(), SessionPhase::Preparing);
    }

    #[test]
    fn offline_eof_during_preparation_and_paused_drain_do_not_start_or_end_playback() {
        let mut session = NativePreviewSession::new();
        let epoch = session.epoch();
        session.mark_audio_eof(epoch).unwrap();
        session.mark_video_eof(epoch).unwrap();
        assert!(session.mark_device_drained(epoch).is_err());
        assert_eq!(session.phase(), SessionPhase::Preparing);
        session.mark_audio_ready(epoch).unwrap();
        session.mark_video_ready(epoch).unwrap();
        session.play().unwrap();
        session.pause().unwrap();
        session.mark_device_drained(epoch).unwrap();
        assert_eq!(session.phase(), SessionPhase::Paused);
        assert_eq!(session.play().unwrap(), SessionPhase::Ended);
    }

    #[test]
    fn cancel_and_close_are_idempotent_and_cannot_be_revived_by_callbacks() {
        let mut session = prepared();
        let old = session.epoch();
        let cancelled = session.cancel().unwrap();
        assert!(cancelled > old);
        assert_eq!(session.cancel().unwrap(), cancelled);
        assert_eq!(session.phase(), SessionPhase::Cancelled);
        assert!(session.begin_seek().is_err());
        assert!(session.play().is_err());
        assert!(session.mark_audio_ready(cancelled).is_err());
        assert!(session.fail(old, "late error").is_err());
        session.close().unwrap();
        let closed = session.snapshot();
        session.close().unwrap();
        assert_eq!(session.snapshot(), closed);
        assert_eq!(closed.phase, SessionPhase::Closed);
        assert!(session.cancel().is_err());
        assert!(session.mark_audio_eof(cancelled).is_err());
        assert!(session.fail(cancelled, "late error").is_err());
    }

    #[test]
    fn failure_is_terminal_and_error_storage_is_utf8_bounded() {
        let mut session = prepared();
        let epoch = session.epoch();
        session.fail(epoch, &"解码错误".repeat(300)).unwrap();
        let failed = session.snapshot();
        assert_eq!(failed.phase, SessionPhase::Failed);
        let message = failed.error.unwrap();
        assert!(!message.is_empty() && message.len() <= 512);
        assert!(std::str::from_utf8(message.as_bytes()).is_ok());
        assert!(session.play().is_err());
        assert!(session.begin_seek().is_err());
        assert!(session.cancel().is_err());
        assert!(session.fail(epoch, "replace the original failure").is_err());
        session.close().unwrap();
        assert_eq!(session.phase(), SessionPhase::Closed);
    }

    #[test]
    fn epoch_exhaustion_does_not_wrap_or_partially_reset_the_session() {
        let mut session = prepared();
        session.epoch = u64::MAX;
        let before = session.snapshot();
        assert_eq!(session.begin_seek(), Err(SessionError::EpochExhausted));
        assert_eq!(session.snapshot(), before);
        assert_eq!(session.cancel(), Err(SessionError::EpochExhausted));
        assert_eq!(session.snapshot(), before);
        session.close().unwrap();
        assert_eq!(session.phase(), SessionPhase::Closed);
    }
}
