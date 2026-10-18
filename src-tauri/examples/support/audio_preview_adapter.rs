//! Single-owner diagnostic audio session adapter. No UI, public IPC or GPU.
//! Device acceptance is committed to the transfer only after a successful write.

use super::{
    audio_cursor::{CursorSnapshot, Ticket},
    audio_transfer::{AudioTransfer, VideoTimelineSnapshot},
    streaming_decoder::{DecoderStats, VideoFrame},
    wasapi_output::WasapiOutput,
};
use serde::Serialize;
use serde_json::{Value, json};

const MAX_REQUEST_FRAMES: u32 = 4800;
const CHANNELS: usize = 2;
const MAX_ERROR_BYTES: usize = 512;
const MAX_CLEANUP_ERRORS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AdapterPhase {
    Preparing,
    Ready,
    Playing,
    Paused,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub capacity_frames: u32,
    pub session_muted: bool,
    pub event_driven: bool,
}

pub trait TransferBackend {
    /// Maximum useful size of a newly prepared packet. Existing pending tails
    /// remain owned by the transfer and need not be resized to this preference.
    fn preferred_packet_frames(&self) -> u32 {
        MAX_REQUEST_FRAMES
    }
    fn snapshot(&self) -> CursorSnapshot;
    fn prepare(&mut self, request: u32) -> Result<bool, String>;
    fn packet(&self) -> Result<(&[f32], Ticket), String>;
    fn commit(&mut self, accepted: u32) -> Result<CursorSnapshot, String>;
    fn pause(&mut self) -> Result<CursorSnapshot, String>;
    fn resume(&mut self) -> Result<CursorSnapshot, String>;
    fn seek(&mut self, target_ns: u64) -> Result<Value, String>;
    fn close(&mut self) -> Result<Value, String>;
    fn reject_stale_ack_zero(&mut self, ticket: Ticket) -> bool;
}
impl TransferBackend for AudioTransfer {
    fn preferred_packet_frames(&self) -> u32 {
        // StreamingDecoder's existing PCM queue emits at most 480 frames per
        // block. Preparing one such quantum avoids waiting for several blocks
        // to fill a larger endpoint-sized packet; this does not enlarge either
        // queue or guarantee that every source offset aligns to a block edge.
        480
    }
    fn snapshot(&self) -> CursorSnapshot {
        AudioTransfer::snapshot(self)
    }
    fn prepare(&mut self, request: u32) -> Result<bool, String> {
        AudioTransfer::prepare(self, request)
    }
    fn packet(&self) -> Result<(&[f32], Ticket), String> {
        AudioTransfer::packet(self)
    }
    fn commit(&mut self, accepted: u32) -> Result<CursorSnapshot, String> {
        AudioTransfer::commit(self, accepted)
    }
    fn pause(&mut self) -> Result<CursorSnapshot, String> {
        AudioTransfer::pause(self)
    }
    fn resume(&mut self) -> Result<CursorSnapshot, String> {
        AudioTransfer::resume(self)
    }
    fn seek(&mut self, target_ns: u64) -> Result<Value, String> {
        AudioTransfer::seek(self, target_ns)
    }
    fn close(&mut self) -> Result<Value, String> {
        AudioTransfer::close(self)
    }
    fn reject_stale_ack_zero(&mut self, ticket: Ticket) -> bool {
        AudioTransfer::reject_stale_ack_zero(self, ticket)
    }
}

pub trait OutputBackend {
    fn format(&self) -> OutputFormat;
    fn padding_frames(&self) -> Result<u32, String>;
    fn write_pcm(&mut self, samples: &[f32]) -> Result<u32, String>;
    fn start(&mut self) -> Result<(), String>;
    fn stop(&mut self) -> Result<(), String>;
    fn reset(&mut self) -> Result<(), String>;
}
impl OutputBackend for WasapiOutput {
    fn format(&self) -> OutputFormat {
        let info = self.info();
        OutputFormat {
            sample_rate: info.sample_rate,
            channels: info.channels,
            capacity_frames: info.buffer_frames,
            session_muted: info.session_muted,
            event_driven: info.event_driven,
        }
    }
    fn padding_frames(&self) -> Result<u32, String> {
        WasapiOutput::padding_frames(self)
    }
    fn write_pcm(&mut self, samples: &[f32]) -> Result<u32, String> {
        WasapiOutput::write_pcm(self, samples)
    }
    fn start(&mut self) -> Result<(), String> {
        WasapiOutput::start(self)
    }
    fn stop(&mut self) -> Result<(), String> {
        WasapiOutput::stop(self)
    }
    fn reset(&mut self) -> Result<(), String> {
        WasapiOutput::reset(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterSnapshot {
    pub phase: AdapterPhase,
    pub cursor: CursorSnapshot,
    /// True also after an unsuccessful stop, because idle cannot be asserted.
    pub device_running: bool,
    pub failure: Option<String>,
    pub cleanup_errors: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterEvidence {
    pub transfer_packet_frames: u32,
    /// Device/transfer limit before applying a particular caller request.
    pub prepared_packet_limit_frames: u32,
    pub last_seek: Option<Value>,
    pub transfer_close: Option<Value>,
    pub stop_attempts: u64,
    pub stop_successes: u64,
    pub reset_attempts: u64,
    pub reset_successes: u64,
    pub transfer_close_attempts: u64,
    pub transfer_close_successes: u64,
    pub accepted_frames_before_failure: u32,
    pub last_cleanup: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TickOutcome {
    /// Exact bounded prefix accepted by write_pcm, including real zero samples.
    pub accepted_samples: Vec<f32>,
    pub written_frames: u32,
    /// Ticket used for this device write, before a partial commit rotates its ID.
    pub ticket: Option<Ticket>,
    pub before: CursorSnapshot,
    pub after: CursorSnapshot,
}
#[derive(Debug)]
pub struct AdapterStartError {
    pub message: String,
    pub state: AdapterSnapshot,
    pub evidence: AdapterEvidence,
}

/// This adapter is single-thread owned. Construct it with a fresh stopped
/// output; it neither creates devices nor changes endpoint/global settings.
/// Fields drop in order, so the output is released before decoder teardown even
/// if best-effort cleanup fails. No caller gets mutable device/transfer access.
pub struct AudioPreviewAdapter<T: TransferBackend, O: OutputBackend> {
    output: O,
    transfer: T,
    format: OutputFormat,
    phase: AdapterPhase,
    device_running: bool,
    transfer_closed: bool,
    failure: Option<String>,
    cleanup_errors: Vec<String>,
    evidence: AdapterEvidence,
    output_limit: Option<u64>,
    capture_samples: bool,
}

impl<T: TransferBackend, O: OutputBackend> AudioPreviewAdapter<T, O> {
    pub fn new(transfer: T, output: O) -> Result<Self, String> {
        Self::configured(transfer, output, None).map_err(|error| error.message)
    }

    /// Finite user-source playback keeps counters rather than PCM copies. Its
    /// own session may be audible; diagnostic construction still requires mute.
    pub fn new_source(transfer: T, output: O, output_frames: u64) -> Result<Self, String> {
        Self::new_source_with_report(transfer, output, output_frames).map_err(|error| error.message)
    }

    /// Retains explicit teardown evidence when construction consumes resources
    /// but cannot return a usable adapter. The error owns no device or decoder.
    pub fn new_source_with_report(
        transfer: T,
        output: O,
        output_frames: u64,
    ) -> Result<Self, AdapterStartError> {
        Self::configured(transfer, output, Some(output_frames))
    }

    fn initialization_error(&mut self, message: String) -> AdapterStartError {
        let message = self.fail(message, 0);
        AdapterStartError {
            message,
            state: self.snapshot(),
            evidence: self.evidence.clone(),
        }
    }

    fn configured(
        transfer: T,
        output: O,
        output_limit: Option<u64>,
    ) -> Result<Self, AdapterStartError> {
        let format = output.format();
        let mut adapter = Self {
            output,
            transfer,
            format,
            phase: AdapterPhase::Preparing,
            device_running: false,
            transfer_closed: false,
            failure: None,
            cleanup_errors: Vec::new(),
            evidence: AdapterEvidence::default(),
            output_limit,
            capture_samples: output_limit.is_none(),
        };
        if format.sample_rate != 48000
            || format.channels != 2
            || format.capacity_frames == 0
            || (output_limit.is_none() && !format.session_muted)
            || !format.event_driven
        {
            return Err(adapter.initialization_error("Audio preview adapter requires a stopped, muted 48 kHz stereo event output with a nonempty buffer".into()));
        }
        let preferred = adapter.transfer.preferred_packet_frames();
        if !(1..=MAX_REQUEST_FRAMES).contains(&preferred) {
            return Err(adapter.initialization_error(
                "Audio transfer packet preference must be within 1..=4800 frames".into(),
            ));
        }
        adapter.evidence.transfer_packet_frames = preferred;
        adapter.evidence.prepared_packet_limit_frames = preferred.min(format.capacity_frames);
        if adapter.transfer.snapshot().cancelled {
            return Err(
                adapter.initialization_error("Cannot attach a cancelled audio transfer".into())
            );
        }
        match adapter.output.padding_frames() {
            Ok(0) => {}
            Ok(_) => {
                return Err(adapter.initialization_error(
                    "Audio preview requires a fresh output with no previously queued PCM".into(),
                ));
            }
            Err(error) => return Err(adapter.initialization_error(error)),
        }
        if adapter.transfer.snapshot().paused {
            if let Err(error) = adapter.transfer.resume() {
                return Err(adapter.initialization_error(error));
            }
        }
        Ok(adapter)
    }

    /// Seek resets the output cursor and endpoint before replacing this bound.
    pub fn set_source_output_limit(&mut self, frames: u64) -> Result<(), String> {
        if self.output_limit.is_none()
            || self.phase != AdapterPhase::Preparing
            || self.device_running
            || self.transfer.snapshot().output_frames != 0
        {
            return Err("Source output limit requires a freshly sought stopped source".into());
        }
        self.output_limit = Some(frames);
        Ok(())
    }

    pub fn snapshot(&self) -> AdapterSnapshot {
        AdapterSnapshot {
            phase: self.phase,
            cursor: self.transfer.snapshot(),
            device_running: self.device_running,
            failure: self.failure.clone(),
            cleanup_errors: self.cleanup_errors.clone(),
        }
    }
    pub fn output(&self) -> &O {
        &self.output
    }
    pub fn evidence(&self) -> &AdapterEvidence {
        &self.evidence
    }

    pub fn prepare_tick(&mut self, request: u32) -> Result<TickOutcome, String> {
        self.require_phase(&[AdapterPhase::Preparing, AdapterPhase::Ready], "prepare")?;
        self.tick(request, true)
    }
    pub fn pump(&mut self, request: u32) -> Result<TickOutcome, String> {
        self.require_phase(&[AdapterPhase::Playing], "pump")?;
        self.tick(request, false)
    }
    fn tick(&mut self, request: u32, preparing: bool) -> Result<TickOutcome, String> {
        if !(1..=MAX_REQUEST_FRAMES).contains(&request) {
            return Err("Audio preview tick request must be within 1..=4800 frames".into());
        }
        let before = self.transfer.snapshot();
        let mut written = 0;
        let result = (|| {
            let padding = self.output.padding_frames()?;
            if padding > self.format.capacity_frames {
                return Err("Audio output padding exceeded its declared capacity".into());
            }
            let mut outcome = TickOutcome {
                accepted_samples: Vec::new(),
                written_frames: 0,
                ticket: None,
                before,
                after: before,
            };
            let remaining = self
                .output_limit
                .map(|limit| {
                    limit
                        .checked_sub(before.output_frames)
                        .ok_or("Finite audio output exceeded its endpoint")
                })
                .transpose()?;
            if remaining == Some(0) {
                if preparing {
                    self.phase = AdapterPhase::Ready;
                }
                return Ok(outcome);
            }
            let request = request.min(
                remaining
                    .unwrap_or(u64::from(request))
                    .min(u64::from(u32::MAX)) as u32,
            );
            // Prepare at most one decoder-preferred quantum and one endpoint
            // buffer. Existing pending tails retain their ticket and data.
            let preferred = self.transfer.preferred_packet_frames();
            if !(1..=MAX_REQUEST_FRAMES).contains(&preferred) {
                return Err(
                    "Audio transfer packet preference must be within 1..=4800 frames".into(),
                );
            }
            let prepare_frames = request.min(self.format.capacity_frames).min(preferred);
            if padding < self.format.capacity_frames && self.transfer.prepare(prepare_frames)? {
                let (samples, ticket) = self.transfer.packet()?;
                let pending = self.transfer.snapshot();
                let packet_frames = ticket
                    .span
                    .leading_silence_frames
                    .checked_add(ticket.span.source_frames)
                    .and_then(|value| value.checked_add(ticket.span.trailing_silence_frames))
                    .ok_or("Audio ticket length overflow")?;
                if pending.pending != Some(ticket)
                    || ticket.output_start_frame != before.output_frames
                    || packet_frames == 0
                    || packet_frames > MAX_REQUEST_FRAMES
                    || samples.len() != packet_frames as usize * CHANNELS
                    || samples
                        .iter()
                        .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
                {
                    return Err(
                        "Prepared audio packet does not match its bounded current ticket".into(),
                    );
                }
                // The endpoint may consume more audio while prepare() runs.
                // Offer only the prefix fitting the observed capacity, while
                // retaining the complete transfer ticket for partial commit.
                let offered_frames = packet_frames
                    .min(self.format.capacity_frames - padding)
                    .min(request);
                written = self
                    .output
                    .write_pcm(&samples[..offered_frames as usize * CHANNELS])?;
                if written > offered_frames {
                    return Err(
                        "Audio output accepted more frames than supplied or available".into(),
                    );
                }
                let accepted = if self.capture_samples {
                    samples[..written as usize * CHANNELS].to_vec()
                } else {
                    Vec::new()
                };
                let after = self.transfer.commit(written)?;
                let expected_output = before
                    .output_frames
                    .checked_add(u64::from(written))
                    .ok_or("Audio output cursor overflow")?;
                if after.output_frames != expected_output || after.epoch != ticket.epoch {
                    return Err(
                        "Audio transfer acknowledgment did not preserve accepted output progress"
                            .into(),
                    );
                }
                outcome.accepted_samples = accepted;
                outcome.written_frames = written;
                outcome.ticket = Some(ticket);
                outcome.after = after;
            } else {
                outcome.after = self.transfer.snapshot();
            }
            if preparing {
                let padding = self.output.padding_frames()?;
                if padding > self.format.capacity_frames {
                    return Err("Audio preparation exceeded device capacity".into());
                }
                // A full endpoint alone is insufficient: its next bounded
                // packet must already be decoded before Start releases it.
                // This uses the transfer's existing single prepared slot and
                // does not acknowledge or submit these lookahead samples.
                let remaining = self
                    .output_limit
                    .map(|limit| {
                        limit
                            .checked_sub(self.transfer.snapshot().output_frames)
                            .ok_or("Finite audio output exceeded its endpoint")
                    })
                    .transpose()?;
                let next_packet_ready = remaining == Some(0)
                    || (padding == self.format.capacity_frames
                        && self.transfer.prepare(
                            prepare_frames
                                .min(remaining.unwrap_or(u64::from(prepare_frames)) as u32),
                        )?);
                outcome.after = self.transfer.snapshot();
                self.phase = if next_packet_ready {
                    AdapterPhase::Ready
                } else {
                    AdapterPhase::Preparing
                };
            }
            Ok(outcome)
        })();
        result.map_err(|error| self.fail(error, written))
    }

    pub fn start(&mut self) -> Result<AdapterSnapshot, String> {
        self.require_phase(&[AdapterPhase::Ready, AdapterPhase::Paused], "start")?;
        if self.output_limit == Some(0) {
            return Err("An empty source endpoint cannot start an audio stream".into());
        }
        let result = (|| {
            // Cursor permission changes first; a failed device Start is then
            // terminal cleanup, never a hidden successful resume.
            self.transfer.resume()?;
            self.device_running = true; // conservatively own an attempted Start
            self.output.start()?;
            self.phase = AdapterPhase::Playing;
            Ok(self.snapshot())
        })();
        result.map_err(|error| self.fail(error, 0))
    }

    pub fn pause(&mut self) -> Result<AdapterSnapshot, String> {
        self.require_phase(&[AdapterPhase::Playing, AdapterPhase::Paused], "pause")?;
        if self.phase == AdapterPhase::Paused {
            return Ok(self.snapshot());
        }
        let result = (|| {
            self.stop_output()?;
            self.transfer.pause()?;
            self.phase = AdapterPhase::Paused;
            Ok(self.snapshot())
        })();
        result.map_err(|error| self.fail(error, 0))
    }

    pub fn seek(&mut self, target_ns: u64) -> Result<AdapterSnapshot, String> {
        self.require_phase(
            &[
                AdapterPhase::Preparing,
                AdapterPhase::Ready,
                AdapterPhase::Playing,
                AdapterPhase::Paused,
            ],
            "seek",
        )?;
        let result = (|| {
            if self.device_running {
                self.stop_output()?;
            }
            self.reset_output()?;
            self.evidence.last_seek = Some(self.transfer.seek(target_ns)?);
            // A paused cursor retains pause across seek. Enable only PCM
            // preparation here; the audio device remains stopped until start.
            self.transfer.resume()?;
            self.phase = AdapterPhase::Preparing;
            Ok(self.snapshot())
        })();
        result.map_err(|error| self.fail(error, 0))
    }

    pub fn cancel(&mut self) -> Result<AdapterSnapshot, String> {
        if self.phase == AdapterPhase::Cancelled {
            let errors = self
                .evidence
                .last_cleanup
                .as_ref()
                .and_then(|value| value.get("errors"))
                .and_then(Value::as_array);
            if errors.is_some_and(|errors| !errors.is_empty()) {
                return Err(
                    "Audio adapter is cancelled, but its recorded cleanup did not fully succeed"
                        .into(),
                );
            }
            return Ok(self.snapshot());
        }
        let errors = self.cleanup();
        self.phase = AdapterPhase::Cancelled;
        if errors.is_empty() {
            Ok(self.snapshot())
        } else {
            Err(format!(
                "Audio cancellation cleanup failed: {}",
                errors.join("; ")
            ))
        }
    }

    /// Only probes an already-obsolete ticket with an acceptance of zero.
    /// It refuses the current ID and verifies that no cursor field changed.
    pub fn reject_stale_ticket(&mut self, ticket: Ticket) -> Result<bool, String> {
        self.require_phase(
            &[
                AdapterPhase::Preparing,
                AdapterPhase::Ready,
                AdapterPhase::Playing,
                AdapterPhase::Paused,
            ],
            "check obsolete acknowledgment",
        )?;
        let before = self.transfer.snapshot();
        if before.epoch == ticket.epoch
            && before
                .pending
                .is_some_and(|pending| pending.id == ticket.id)
        {
            return Err("Refusing to probe a current audio acknowledgment ticket".into());
        }
        let rejected = self.transfer.reject_stale_ack_zero(ticket);
        if self.transfer.snapshot() != before || !rejected {
            return Err(self.fail(
                "Obsolete audio acknowledgment was accepted or changed the cursor".into(),
                0,
            ));
        }
        Ok(true)
    }

    fn require_phase(&self, allowed: &[AdapterPhase], action: &str) -> Result<(), String> {
        if allowed.contains(&self.phase) {
            Ok(())
        } else {
            Err(format!(
                "Cannot {action} while audio adapter is {:?}",
                self.phase
            ))
        }
    }
    fn stop_output(&mut self) -> Result<(), String> {
        self.evidence.stop_attempts = self.evidence.stop_attempts.saturating_add(1);
        self.output.stop()?;
        self.device_running = false;
        self.evidence.stop_successes = self.evidence.stop_successes.saturating_add(1);
        Ok(())
    }
    fn reset_output(&mut self) -> Result<(), String> {
        self.evidence.reset_attempts = self.evidence.reset_attempts.saturating_add(1);
        self.output.reset()?;
        self.evidence.reset_successes = self.evidence.reset_successes.saturating_add(1);
        Ok(())
    }
    fn cleanup(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.device_running {
            if let Err(error) = self.stop_output() {
                errors.push(bounded(&format!("stop: {error}")));
            }
        }
        if let Err(error) = self.reset_output() {
            errors.push(bounded(&format!("reset: {error}")));
        }
        if !self.transfer_closed {
            self.evidence.transfer_close_attempts =
                self.evidence.transfer_close_attempts.saturating_add(1);
            match self.transfer.close() {
                Ok(value) => {
                    self.transfer_closed = true;
                    self.evidence.transfer_close_successes =
                        self.evidence.transfer_close_successes.saturating_add(1);
                    self.evidence.transfer_close = Some(value);
                }
                Err(error) => errors.push(bounded(&format!("transfer close: {error}"))),
            }
        }
        for error in &errors {
            if self.cleanup_errors.len() < MAX_CLEANUP_ERRORS {
                self.cleanup_errors.push(error.clone());
            }
        }
        self.evidence.last_cleanup = Some(
            json!({"deviceRunning":self.device_running,"transferClosed":self.transfer_closed,"errors":errors}),
        );
        errors
    }
    fn fail(&mut self, error: String, accepted_frames: u32) -> String {
        let primary = bounded(&error);
        if self.failure.is_none() {
            self.failure = Some(primary.clone());
            self.evidence.accepted_frames_before_failure = accepted_frames;
        }
        self.phase = AdapterPhase::Failed;
        let cleanup = self.cleanup();
        if cleanup.is_empty() {
            primary
        } else {
            format!("{primary}; cleanup: {}", cleanup.join("; "))
        }
    }
}

impl<T: TransferBackend, O: OutputBackend> Drop for AudioPreviewAdapter<T, O> {
    fn drop(&mut self) {
        if self.device_running || !self.transfer_closed {
            let _ = self.cleanup();
        }
    }
}

/// The AV diagnostic surface is intentionally confined to the real shared
/// transfer/output pair; ordinary audio-only generic adapters keep their API.
impl AudioPreviewAdapter<AudioTransfer, WasapiOutput> {
    pub fn video_frame(&mut self, project_time_ns: u64) -> Result<Option<VideoFrame>, String> {
        self.require_phase(
            &[
                AdapterPhase::Preparing,
                AdapterPhase::Ready,
                AdapterPhase::Playing,
                AdapterPhase::Paused,
            ],
            "select video",
        )?;
        self.transfer
            .video_frame(project_time_ns)
            .map_err(|error| self.fail(error, 0))
    }

    pub fn video_snapshot(&self) -> Option<VideoTimelineSnapshot> {
        self.transfer.video_snapshot()
    }

    /// Call at control boundaries rather than on the audio pump's critical path.
    pub fn decoder_stats(&self) -> Result<Option<DecoderStats>, String> {
        self.transfer.decoder_stats()
    }
}

fn bounded(message: &str) -> String {
    let mut end = message.len().min(MAX_ERROR_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::super::{
        audio_cursor::AudioCursor,
        audio_timeline::{AudioTimeline, Timestamp},
    };
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    type Actions = Rc<RefCell<Vec<&'static str>>>;
    struct FakeTransfer {
        cursor: AudioCursor,
        packet: Option<(Vec<f32>, Ticket)>,
        actions: Actions,
        fail_commit: bool,
        fail_seek: bool,
        fail_close: bool,
        stall: bool,
        max_ready_request: Option<u32>,
        stall_after_output_frames: Option<u64>,
        preferred_frames: u32,
    }
    impl FakeTransfer {
        fn new(actions: Actions) -> Self {
            let origin = Timestamp {
                pts: 0,
                numerator: 1,
                denominator: 1,
            };
            Self {
                cursor: AudioCursor::new(
                    AudioTimeline::new(origin, origin, 48000, 480000).unwrap(),
                    0,
                )
                .unwrap(),
                packet: None,
                actions,
                fail_commit: false,
                fail_seek: false,
                fail_close: false,
                stall: false,
                max_ready_request: None,
                stall_after_output_frames: None,
                preferred_frames: MAX_REQUEST_FRAMES,
            }
        }
    }
    impl TransferBackend for FakeTransfer {
        fn preferred_packet_frames(&self) -> u32 {
            self.preferred_frames
        }
        fn snapshot(&self) -> CursorSnapshot {
            self.cursor.snapshot()
        }
        fn prepare(&mut self, request: u32) -> Result<bool, String> {
            self.actions.borrow_mut().push("prepare");
            if self.stall
                || self
                    .max_ready_request
                    .is_some_and(|maximum| request > maximum)
            {
                return Ok(false);
            }
            if self.packet.is_none() {
                let ticket = self.cursor.reserve(request)?;
                let samples = (0..request)
                    .flat_map(|offset| {
                        let value = ((ticket.output_start_frame + u64::from(offset)) % 100000)
                            as f32
                            / 100000.0;
                        [value, -value]
                    })
                    .collect();
                self.packet = Some((samples, ticket));
            }
            if self
                .stall_after_output_frames
                .is_some_and(|frames| self.cursor.snapshot().output_frames >= frames)
            {
                return Ok(false);
            }
            Ok(true)
        }
        fn packet(&self) -> Result<(&[f32], Ticket), String> {
            self.packet
                .as_ref()
                .map(|(samples, ticket)| (samples.as_slice(), *ticket))
                .ok_or("missing fake packet".into())
        }
        fn commit(&mut self, accepted: u32) -> Result<CursorSnapshot, String> {
            self.actions.borrow_mut().push("commit");
            if self.fail_commit {
                return Err("injected commit failure".into());
            }
            let (_, ticket) = self.packet.as_ref().ok_or("missing fake packet")?;
            let result = self.cursor.commit(ticket.epoch, ticket.id, accepted)?;
            if let Some(ticket) = result.pending {
                let (samples, current) = self.packet.as_mut().unwrap();
                samples.drain(..accepted as usize * 2);
                *current = ticket;
            } else {
                self.packet = None;
            }
            Ok(result)
        }
        fn pause(&mut self) -> Result<CursorSnapshot, String> {
            self.actions.borrow_mut().push("transfer.pause");
            self.cursor.pause()
        }
        fn resume(&mut self) -> Result<CursorSnapshot, String> {
            self.actions.borrow_mut().push("transfer.resume");
            self.cursor.resume()
        }
        fn seek(&mut self, target: u64) -> Result<Value, String> {
            self.actions.borrow_mut().push("transfer.seek");
            if self.fail_seek {
                return Err("injected seek failure".into());
            }
            let before = self.cursor.snapshot();
            let after = self.cursor.seek(target)?;
            self.packet = None;
            Ok(
                serde_json::json!({"before":before,"after":after,"oldDecoderShutdown":{"joined":true}}),
            )
        }
        fn close(&mut self) -> Result<Value, String> {
            self.actions.borrow_mut().push("transfer.close");
            self.packet = None;
            let cursor = self.cursor.cancel()?;
            if self.fail_close {
                Err("injected close failure".into())
            } else {
                Ok(
                    serde_json::json!({"cursor":cursor,"decoder":{"joined":true,"processesReaped":true}}),
                )
            }
        }
        fn reject_stale_ack_zero(&mut self, ticket: Ticket) -> bool {
            let before = self.cursor.snapshot();
            let rejected = self.cursor.commit(ticket.epoch, ticket.id, 0).is_err();
            rejected && self.cursor.snapshot() == before
        }
    }
    struct FakeOutput {
        actions: Actions,
        padding: u32,
        capacity: u32,
        accepted: VecDeque<u32>,
        running: bool,
        fail_start: bool,
        fail_stop: bool,
        fail_reset: bool,
        fail_write: bool,
        drain_before_write: u32,
    }
    impl FakeOutput {
        fn new(actions: Actions) -> Self {
            Self {
                actions,
                padding: 0,
                capacity: 4,
                accepted: VecDeque::new(),
                running: false,
                fail_start: false,
                fail_stop: false,
                fail_reset: false,
                fail_write: false,
                drain_before_write: 0,
            }
        }
    }
    impl OutputBackend for FakeOutput {
        fn format(&self) -> OutputFormat {
            OutputFormat {
                sample_rate: 48000,
                channels: 2,
                capacity_frames: self.capacity,
                session_muted: true,
                event_driven: true,
            }
        }
        fn padding_frames(&self) -> Result<u32, String> {
            Ok(self.padding)
        }
        fn write_pcm(&mut self, samples: &[f32]) -> Result<u32, String> {
            self.actions.borrow_mut().push("write");
            if self.fail_write {
                return Err("injected write failure".into());
            }
            // Simulate the endpoint consuming queued audio after the adapter's
            // padding observation but before write_pcm reads current capacity.
            self.padding = self.padding.saturating_sub(self.drain_before_write);
            self.drain_before_write = 0;
            let amount = self
                .accepted
                .pop_front()
                .unwrap_or((samples.len() / 2) as u32)
                .min(self.capacity - self.padding)
                .min((samples.len() / 2) as u32);
            self.padding += amount;
            Ok(amount)
        }
        fn start(&mut self) -> Result<(), String> {
            self.actions.borrow_mut().push("start");
            if self.fail_start {
                return Err("injected start failure".into());
            }
            self.running = true;
            Ok(())
        }
        fn stop(&mut self) -> Result<(), String> {
            self.actions.borrow_mut().push("stop");
            if self.fail_stop {
                return Err("injected stop failure".into());
            }
            self.running = false;
            Ok(())
        }
        fn reset(&mut self) -> Result<(), String> {
            self.actions.borrow_mut().push("reset");
            if self.fail_reset {
                return Err("injected reset failure".into());
            }
            self.padding = 0;
            Ok(())
        }
    }
    fn adapter() -> (AudioPreviewAdapter<FakeTransfer, FakeOutput>, Actions) {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let transfer = FakeTransfer::new(actions.clone());
        let output = FakeOutput::new(actions.clone());
        (AudioPreviewAdapter::new(transfer, output).unwrap(), actions)
    }
    #[test]
    fn source_constructor_failure_retains_explicit_cleanup_evidence() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let transfer = FakeTransfer::new(actions.clone());
        let mut output = FakeOutput::new(actions.clone());
        output.padding = 1;
        let error = match AudioPreviewAdapter::new_source_with_report(transfer, output, 48_000) {
            Ok(_) => panic!("A nonempty output must be rejected"),
            Err(error) => error,
        };
        assert!(error.message.contains("fresh output"));
        assert_eq!(error.evidence.transfer_close_successes, 1);
        assert_eq!(error.evidence.reset_successes, 1);
        assert_eq!(
            error.evidence.transfer_close.as_ref().unwrap()["decoder"]["joined"],
            true
        );
        assert!(!error.state.device_running);
        assert!(error.state.cleanup_errors.is_empty());
        assert_eq!(*actions.borrow(), ["reset", "transfer.close"]);
    }
    #[test]
    fn source_constructor_failure_retains_cleanup_failure() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let mut transfer = FakeTransfer::new(actions.clone());
        transfer.fail_close = true;
        let mut output = FakeOutput::new(actions.clone());
        output.capacity = 0;
        output.fail_reset = true;
        let error = match AudioPreviewAdapter::new_source_with_report(transfer, output, 48_000) {
            Ok(_) => panic!("An empty output must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.evidence.transfer_close_successes, 0);
        assert_eq!(error.evidence.reset_successes, 0);
        assert_eq!(error.state.cleanup_errors.len(), 2);
        assert!(error.evidence.transfer_close.is_none());
        assert!(!actions.borrow().contains(&"start"));
    }
    fn ready(adapter: &mut AudioPreviewAdapter<FakeTransfer, FakeOutput>) {
        adapter.prepare_tick(4).unwrap();
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Ready);
    }

    #[test]
    fn preparation_and_partial_device_writes_advance_only_accepted_prefix() {
        let (mut adapter, actions) = adapter();
        adapter.output.accepted = VecDeque::from([0, 1, 3]);
        let first = adapter.prepare_tick(4).unwrap();
        assert_eq!(first.written_frames, 0);
        assert_eq!(first.after.output_frames, 0);
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Preparing);
        let partial = adapter.prepare_tick(4).unwrap();
        assert_eq!(partial.written_frames, 1);
        assert_eq!(partial.accepted_samples.len(), 2);
        assert_eq!(partial.after.output_frames, 1);
        let done = adapter.prepare_tick(4).unwrap();
        assert_eq!(done.written_frames, 3);
        assert_eq!(done.after.output_frames, 4);
        assert_eq!(done.after.pending.unwrap().span.source_frames, 4);
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Ready);
        assert_eq!(
            actions
                .borrow()
                .iter()
                .filter(|&&item| item == "write")
                .count(),
            3
        );
        assert!(!adapter.snapshot().device_running);
    }

    #[test]
    fn pause_preserves_device_padding_and_transfer_packet_and_resume_never_resets() {
        let (mut adapter, actions) = adapter();
        ready(&mut adapter);
        adapter.start().unwrap();
        adapter.output.padding = 0;
        adapter.output.accepted.push_back(1);
        let part = adapter.pump(4).unwrap();
        assert!(part.after.pending.is_some());
        actions.borrow_mut().clear();
        let paused = adapter.pause().unwrap();
        assert_eq!(paused.phase, AdapterPhase::Paused);
        assert_eq!(adapter.output.padding, 1);
        assert_eq!(paused.cursor.pending, part.after.pending);
        assert_eq!(*actions.borrow(), vec!["stop", "transfer.pause"]);
        assert!(adapter.pump(4).is_err());
        adapter.start().unwrap();
        assert!(!actions.borrow().contains(&"reset"));
        assert_eq!(adapter.snapshot().cursor.pending, part.after.pending);
    }

    #[test]
    fn playing_and_paused_seek_reset_before_transfer_invalidation_and_do_not_autoplay() {
        let (mut adapter, actions) = adapter();
        ready(&mut adapter);
        adapter.start().unwrap();
        let epoch = adapter.snapshot().cursor.epoch;
        actions.borrow_mut().clear();
        adapter.seek(1_000_000_000).unwrap();
        assert_eq!(
            *actions.borrow(),
            vec!["stop", "reset", "transfer.seek", "transfer.resume"]
        );
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Preparing);
        assert!(!adapter.snapshot().device_running);
        assert!(adapter.snapshot().cursor.epoch > epoch);
        assert_eq!(adapter.snapshot().cursor.source_frame, 48000);
        assert!(adapter.start().is_err());
        ready(&mut adapter);
        adapter.start().unwrap();
        adapter.pause().unwrap();
        actions.borrow_mut().clear();
        adapter.seek(0).unwrap();
        assert_eq!(
            *actions.borrow(),
            vec!["reset", "transfer.seek", "transfer.resume"]
        );
        assert!(!adapter.snapshot().cursor.paused);
        assert!(!adapter.snapshot().device_running);
    }

    #[test]
    fn post_write_commit_failure_stops_resets_and_closes_without_faking_zero_acceptance() {
        let (mut adapter, actions) = adapter();
        ready(&mut adapter);
        adapter.start().unwrap();
        adapter.output.padding = 0;
        adapter.transfer.fail_commit = true;
        actions.borrow_mut().clear();
        assert!(adapter.pump(4).is_err());
        assert_eq!(
            *actions.borrow(),
            vec![
                "prepare",
                "write",
                "commit",
                "stop",
                "reset",
                "transfer.close"
            ]
        );
        let snapshot = adapter.snapshot();
        assert_eq!(snapshot.phase, AdapterPhase::Failed);
        assert!(!snapshot.device_running);
        assert_eq!(adapter.output.padding, 0);
        assert_eq!(adapter.evidence().accepted_frames_before_failure, 4);
        assert!(snapshot.failure.unwrap().contains("commit failure"));
        assert!(adapter.start().is_err());
    }

    #[test]
    fn cleanup_attempts_every_step_and_preserves_primary_failure() {
        let (mut adapter, actions) = adapter();
        ready(&mut adapter);
        adapter.start().unwrap();
        adapter.output.padding = 0;
        adapter.output.fail_write = true;
        adapter.output.fail_stop = true;
        adapter.output.fail_reset = true;
        adapter.transfer.fail_close = true;
        actions.borrow_mut().clear();
        assert!(adapter.pump(4).is_err());
        assert_eq!(
            *actions.borrow(),
            vec!["prepare", "write", "stop", "reset", "transfer.close"]
        );
        let failed = adapter.snapshot();
        assert_eq!(failed.phase, AdapterPhase::Failed);
        assert!(failed.failure.unwrap().contains("write failure"));
        assert_eq!(failed.cleanup_errors.len(), 3);
        assert!(failed.device_running);
        adapter.output.fail_stop = false;
        adapter.output.fail_reset = false;
        adapter.transfer.fail_close = false;
        assert_eq!(adapter.cancel().unwrap().phase, AdapterPhase::Cancelled);
        assert!(adapter.snapshot().failure.is_some());
    }

    #[test]
    fn cancellation_is_idempotent_and_all_later_control_is_rejected() {
        let (mut adapter, actions) = adapter();
        ready(&mut adapter);
        adapter.start().unwrap();
        let cancelled = adapter.cancel().unwrap();
        assert_eq!(cancelled.phase, AdapterPhase::Cancelled);
        assert!(cancelled.cursor.cancelled);
        assert_eq!(adapter.output.padding, 0);
        let before = actions.borrow().len();
        assert_eq!(adapter.cancel().unwrap(), cancelled);
        assert_eq!(actions.borrow().len(), before);
        assert!(adapter.start().is_err());
        assert!(adapter.pause().is_err());
        assert!(adapter.seek(0).is_err());
        assert!(adapter.prepare_tick(4).is_err());
        assert!(adapter.pump(4).is_err());
    }

    #[test]
    fn stale_ticket_diagnostic_rejects_old_id_and_epoch_without_allowing_current_ticket() {
        let (mut adapter, _) = adapter();
        adapter.output.accepted.push_back(1);
        let partial = adapter.prepare_tick(4).unwrap();
        let old = partial.ticket.unwrap();
        let before = adapter.snapshot();
        assert!(adapter.reject_stale_ticket(old).unwrap());
        assert_eq!(adapter.snapshot(), before);
        let current = before.cursor.pending.unwrap();
        assert!(adapter.reject_stale_ticket(current).is_err());
        assert_eq!(adapter.snapshot(), before);
        adapter.seek(0).unwrap();
        assert!(adapter.reject_stale_ticket(current).unwrap());
    }

    #[test]
    fn preparation_wait_is_nonblocking_and_start_or_seek_failure_latches_terminal_cleanup() {
        let (mut adapter, actions) = adapter();
        adapter.transfer.stall = true;
        let waiting = adapter.prepare_tick(4).unwrap();
        assert_eq!(waiting.written_frames, 0);
        assert!(waiting.ticket.is_none());
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Preparing);
        assert!(!actions.borrow().contains(&"write"));
        adapter.transfer.stall = false;
        ready(&mut adapter);
        adapter.output.fail_start = true;
        assert!(adapter.start().is_err());
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Failed);
        assert!(actions.borrow().contains(&"transfer.close"));
        let (mut seeking, events) = self::adapter();
        seeking.transfer.fail_seek = true;
        assert!(seeking.seek(0).is_err());
        assert_eq!(seeking.snapshot().phase, AdapterPhase::Failed);
        assert!(events.borrow().contains(&"transfer.close"));
    }

    #[test]
    fn repeated_cancel_does_not_turn_failed_cleanup_into_a_success_report() {
        let (mut adapter, actions) = adapter();
        ready(&mut adapter);
        adapter.output.fail_reset = true;
        assert!(adapter.cancel().is_err());
        let cancelled = adapter.snapshot();
        let count = actions.borrow().len();
        assert_eq!(cancelled.phase, AdapterPhase::Cancelled);
        assert!(!cancelled.cleanup_errors.is_empty());
        assert!(adapter.cancel().is_err());
        assert_eq!(adapter.snapshot(), cancelled);
        assert_eq!(actions.borrow().len(), count);
    }

    #[test]
    fn cancel_stop_failure_preserves_unknown_running_state_and_still_closes_transfer() {
        let (mut adapter, actions) = adapter();
        ready(&mut adapter);
        adapter.start().unwrap();
        adapter.output.fail_stop = true;
        // A real output rejects Reset while Stop has not established stopped.
        adapter.output.fail_reset = true;
        actions.borrow_mut().clear();
        assert!(adapter.cancel().unwrap_err().contains("stop"));
        assert_eq!(*actions.borrow(), vec!["stop", "reset", "transfer.close"]);
        let failed = adapter.snapshot();
        assert!(failed.device_running);
        assert!(failed.cursor.cancelled);
        assert_eq!(failed.cleanup_errors.len(), 2);
        assert_eq!(adapter.evidence().stop_attempts, 1);
        assert_eq!(adapter.evidence().stop_successes, 0);
        assert_eq!(adapter.evidence().transfer_close_successes, 1);
        // Even if the next backend call would succeed, a repeated cancellation
        // must not erase the first unconfirmed teardown or restart playback.
        adapter.output.fail_stop = false;
        adapter.output.fail_reset = false;
        let calls = actions.borrow().len();
        assert!(adapter.cancel().is_err());
        assert!(adapter.start().is_err());
        assert!(adapter.pump(4).is_err());
        assert_eq!(adapter.snapshot(), failed);
        assert_eq!(actions.borrow().len(), calls);
    }

    #[test]
    fn cancel_reset_failure_retains_queued_audio_evidence_after_successful_stop() {
        let (mut adapter, actions) = adapter();
        ready(&mut adapter);
        adapter.start().unwrap();
        let padding = adapter.output.padding;
        assert!(padding > 0);
        adapter.output.fail_reset = true;
        actions.borrow_mut().clear();
        assert!(adapter.cancel().unwrap_err().contains("reset"));
        assert_eq!(*actions.borrow(), vec!["stop", "reset", "transfer.close"]);
        assert!(!adapter.snapshot().device_running);
        assert_eq!(adapter.output.padding, padding);
        assert_eq!(adapter.evidence().stop_successes, 1);
        assert_eq!(adapter.evidence().reset_successes, 0);
        assert_eq!(adapter.evidence().transfer_close_successes, 1);
        assert!(!adapter.snapshot().cleanup_errors.is_empty());
        let before = adapter.snapshot();
        assert!(adapter.seek(0).is_err());
        assert!(adapter.prepare_tick(4).is_err());
        assert_eq!(adapter.snapshot(), before);
    }

    #[test]
    fn adapter_rejects_prequeued_unknown_audio_and_closes_its_transfer() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let transfer = FakeTransfer::new(actions.clone());
        let mut output = FakeOutput::new(actions.clone());
        output.padding = 1;
        assert!(AudioPreviewAdapter::new(transfer, output).is_err());
        assert_eq!(*actions.borrow(), vec!["reset", "transfer.close"]);
    }

    #[test]
    fn device_progress_between_padding_and_write_does_not_trigger_false_overacceptance() {
        let (mut adapter, actions) = adapter();
        ready(&mut adapter);
        adapter.start().unwrap();
        adapter.output.padding = 2;
        adapter.output.drain_before_write = 2;
        actions.borrow_mut().clear();
        let accepted = adapter.pump(4).unwrap();
        assert_eq!(accepted.written_frames, 2);
        assert_eq!(
            accepted.accepted_samples,
            vec![
                4.0 / 100000.0,
                -4.0 / 100000.0,
                5.0 / 100000.0,
                -5.0 / 100000.0
            ]
        );
        assert_eq!(accepted.after.output_frames, 6);
        assert_eq!(accepted.after.pending.unwrap().span.source_frames, 2);
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Playing);
        assert!(!actions.borrow().contains(&"stop"));
        let remaining = adapter.pump(4).unwrap();
        assert_eq!(remaining.written_frames, 2);
        assert_eq!(remaining.after.output_frames, 8);
        assert!(remaining.after.pending.is_none());
    }

    #[test]
    fn fresh_packet_request_never_exceeds_endpoint_capacity_even_when_caller_requests_4800() {
        let (mut adapter, _) = adapter();
        adapter.transfer.max_ready_request = Some(4);
        let prepared = adapter.prepare_tick(4800).unwrap();
        assert_eq!(prepared.written_frames, 4);
        assert_eq!(prepared.ticket.unwrap().span.source_frames, 4);
        assert_eq!(prepared.after.output_frames, 4);
        assert_eq!(prepared.after.pending.unwrap().span.source_frames, 4);
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Ready);
        adapter.start().unwrap();
        adapter.output.padding = 0;
        adapter.output.accepted.push_back(1);
        let partial = adapter.pump(4800).unwrap();
        assert_eq!(partial.written_frames, 1);
        assert_eq!(partial.after.pending.unwrap().span.source_frames, 3);
        let remaining = adapter.pump(4800).unwrap();
        assert_eq!(remaining.written_frames, 3);
        assert_eq!(remaining.after.output_frames, 8);
        assert!(remaining.after.pending.is_none());
    }

    #[test]
    fn start_waits_for_the_next_bounded_packet_after_the_endpoint_is_full() {
        let (mut adapter, actions) = adapter();
        adapter.transfer.stall_after_output_frames = Some(4);
        let filled = adapter.prepare_tick(4800).unwrap();
        assert_eq!(filled.written_frames, 4);
        assert_eq!(adapter.output.padding, 4);
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Preparing);
        assert!(adapter.start().is_err());
        assert_eq!(filled.after.output_frames, 4);
        assert_eq!(filled.after.source_frame, 4);
        assert_eq!(filled.after.pending.unwrap().span.source_frames, 4);
        let writes = actions
            .borrow()
            .iter()
            .filter(|&&action| action == "write")
            .count();
        adapter.transfer.stall_after_output_frames = None;
        let ready = adapter.prepare_tick(4800).unwrap();
        assert_eq!(ready.written_frames, 0);
        assert_eq!(ready.after.output_frames, 4);
        assert_eq!(ready.after.source_frame, 4);
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Ready);
        assert_eq!(
            actions
                .borrow()
                .iter()
                .filter(|&&action| action == "write")
                .count(),
            writes
        );
        adapter.start().unwrap();
        adapter.output.padding = 0;
        let final_prefix = adapter.pump(1).unwrap();
        assert_eq!(final_prefix.written_frames, 1);
        assert_eq!(final_prefix.after.output_frames, 5);
        assert_eq!(final_prefix.after.pending.unwrap().span.source_frames, 3);
    }

    #[test]
    fn preparation_uses_decoder_packet_preference_instead_of_waiting_for_a_larger_endpoint_packet()
    {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let mut transfer = FakeTransfer::new(actions.clone());
        transfer.preferred_frames = 4;
        transfer.max_ready_request = Some(4);
        let mut output = FakeOutput::new(actions);
        output.capacity = 8;
        let mut adapter = AudioPreviewAdapter::new(transfer, output).unwrap();
        assert_eq!(adapter.evidence().transfer_packet_frames, 4);
        assert_eq!(adapter.evidence().prepared_packet_limit_frames, 4);
        let first = adapter.prepare_tick(4800).unwrap();
        assert_eq!(first.written_frames, 4);
        assert_eq!(first.ticket.unwrap().span.source_frames, 4);
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Preparing);
        let second = adapter.prepare_tick(4800).unwrap();
        assert_eq!(second.written_frames, 4);
        assert_eq!(second.after.output_frames, 8);
        assert_eq!(second.after.pending.unwrap().span.source_frames, 4);
        assert_eq!(adapter.snapshot().phase, AdapterPhase::Ready);
        adapter.start().unwrap();
        adapter.output.padding = 0;
        adapter.output.accepted.push_back(1);
        let prefix = adapter.pump(4800).unwrap();
        assert_eq!(prefix.written_frames, 1);
        assert_eq!(prefix.after.pending.unwrap().span.source_frames, 3);
        let tail = adapter.pump(4800).unwrap();
        assert_eq!(tail.written_frames, 3);
        assert_eq!(tail.after.output_frames, 12);
        assert!(tail.after.pending.is_none());
    }

    #[test]
    fn invalid_decoder_packet_preference_fails_before_preparation_or_device_write() {
        for invalid in [0, 4801, u32::MAX] {
            let (mut adapter, actions) = adapter();
            adapter.transfer.preferred_frames = invalid;
            actions.borrow_mut().clear();
            assert!(adapter.prepare_tick(4800).is_err());
            assert_eq!(adapter.snapshot().phase, AdapterPhase::Failed);
            assert!(!actions.borrow().contains(&"prepare"));
            assert!(!actions.borrow().contains(&"write"));
            assert!(actions.borrow().contains(&"transfer.close"));
        }
    }

    #[test]
    fn finite_source_prepares_short_tail_without_lookahead_or_pcm_capture() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let transfer = FakeTransfer::new(actions.clone());
        let output = FakeOutput::new(actions.clone());
        let mut source = AudioPreviewAdapter::new_source(transfer, output, 2).unwrap();
        let tick = source.prepare_tick(4800).unwrap();
        assert_eq!(tick.written_frames, 2);
        assert!(tick.accepted_samples.is_empty());
        assert_eq!(tick.after.output_frames, 2);
        assert!(tick.after.pending.is_none());
        assert_eq!(source.snapshot().phase, AdapterPhase::Ready);
        source.start().unwrap();
        source.output.padding = 0;
        actions.borrow_mut().clear();
        assert_eq!(source.pump(4800).unwrap().written_frames, 0);
        assert!(!actions.borrow().contains(&"prepare"));
        assert!(!actions.borrow().contains(&"write"));
    }

    #[test]
    fn finite_source_partial_writes_never_cross_end_and_seek_resets_the_limit() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let transfer = FakeTransfer::new(actions.clone());
        let mut output = FakeOutput::new(actions);
        output.accepted.push_back(1);
        let mut source = AudioPreviewAdapter::new_source(transfer, output, 7).unwrap();
        assert_eq!(source.prepare_tick(4800).unwrap().written_frames, 1);
        assert_eq!(source.prepare_tick(4800).unwrap().written_frames, 3);
        source.start().unwrap();
        source.output.padding = 0;
        assert_eq!(source.pump(4800).unwrap().written_frames, 3);
        assert_eq!(source.snapshot().cursor.output_frames, 7);
        assert!(source.snapshot().cursor.pending.is_none());
        assert_eq!(source.pump(4800).unwrap().written_frames, 0);
        source.seek(0).unwrap();
        source.set_source_output_limit(0).unwrap();
        let end = source.prepare_tick(4800).unwrap();
        assert_eq!(end.written_frames, 0);
        assert_eq!(source.snapshot().phase, AdapterPhase::Ready);
        assert!(!source.snapshot().device_running);
        assert!(source.start().is_err());
    }

    #[test]
    fn finite_source_runs_beyond_two_seconds_without_captures_or_unbounded_tail() {
        let actions = Rc::new(RefCell::new(Vec::new()));
        let mut transfer = FakeTransfer::new(actions.clone());
        transfer.preferred_frames = 480;
        let mut output = FakeOutput::new(actions.clone());
        output.capacity = 960;
        let mut source = AudioPreviewAdapter::new_source(transfer, output, 144_001).unwrap();
        while source.snapshot().phase == AdapterPhase::Preparing {
            assert!(
                source
                    .prepare_tick(4800)
                    .unwrap()
                    .accepted_samples
                    .is_empty()
            );
        }
        source.start().unwrap();
        let mut written = source.snapshot().cursor.output_frames;
        while written < 144_001 {
            source.output.padding = 0;
            let tick = source.pump(4800).unwrap();
            assert!(tick.written_frames > 0);
            assert!(tick.accepted_samples.is_empty());
            written += u64::from(tick.written_frames);
            assert_eq!(tick.after.output_frames, written);
            assert!(written <= 144_001);
        }
        assert_eq!(written, 144_001);
        assert!(source.snapshot().cursor.pending.is_none());
        source.output.padding = 0;
        actions.borrow_mut().clear();
        for _ in 0..16 {
            assert_eq!(source.pump(4800).unwrap().written_frames, 0);
        }
        assert!(!actions.borrow().contains(&"prepare"));
        assert!(!actions.borrow().contains(&"write"));
        source.pause().unwrap();
        assert!(!source.snapshot().device_running);
        source.cancel().unwrap();
        assert_eq!(source.snapshot().phase, AdapterPhase::Cancelled);
    }
}
