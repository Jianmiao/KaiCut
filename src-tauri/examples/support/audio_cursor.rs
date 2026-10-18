//! Transactional continuous PCM sample cursor, independent of devices and I/O.
//! Timestamp placement is resolved only at construction or an explicit seek.

use super::audio_timeline::{AudioSpan, AudioTimeline};
use serde::Serialize;

const MAX_PACKET_FRAMES: u32 = 4800;
const MAX_OUTPUT_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ticket {
    pub epoch: u64,
    pub id: u64,
    pub output_start_frame: u64,
    pub span: AudioSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorSnapshot {
    pub epoch: u64,
    /// Accepted output frames since the current constructor/seek anchor.
    pub output_frames: u64,
    /// Next unconsumed decoded source frame, clamped to source EOF.
    pub source_frame: u64,
    pub total_source_frames: u64,
    pub leading_silence_remaining: u64,
    pub pending: Option<Ticket>,
    pub paused: bool,
    pub cancelled: bool,
}

/// One control owner submits each actually accepted device prefix using its
/// current ticket. Every positive partial commit rotates the ticket ID, so a
/// replay of the old acknowledgment is rejected. Reserving is not submission:
/// output/source cursors advance only when commit succeeds. There is no wall
/// clock, per-block rounded-nanosecond lookup, decoder or sample-buffer storage.
#[derive(Debug)]
pub struct AudioCursor {
    timeline: AudioTimeline,
    sample_rate: u32,
    total_source_frames: u64,
    first_source_timeline_ns: i128,
    max_output_frames: u64,
    epoch: u64,
    next_ticket_id: u64,
    output_frames: u64,
    source_frame: u64,
    leading_silence_remaining: u64,
    pending: Option<Ticket>,
    paused: bool,
    cancelled: bool,
}

impl AudioCursor {
    pub fn new(timeline: AudioTimeline, start_ns: u64) -> Result<Self, String> {
        let info = timeline.diagnostics();
        let first_source_timeline_ns = info
            .first_source_timeline_ns
            .parse::<i128>()
            .map_err(|_| "Audio origin is not a valid signed timeline timestamp")?;
        let (source_frame, leading_silence_remaining) = anchor(
            &timeline,
            start_ns,
            info.sample_rate,
            info.total_frames,
            first_source_timeline_ns,
        )?;
        let max_output_frames = u64::from(info.sample_rate)
            .checked_mul(MAX_OUTPUT_SECONDS)
            .ok_or("Audio cursor output-duration limit overflow")?;
        Ok(Self {
            timeline,
            sample_rate: info.sample_rate,
            total_source_frames: info.total_frames,
            first_source_timeline_ns,
            max_output_frames,
            epoch: 1,
            next_ticket_id: 0,
            output_frames: 0,
            source_frame,
            leading_silence_remaining,
            pending: None,
            paused: false,
            cancelled: false,
        })
    }

    pub fn snapshot(&self) -> CursorSnapshot {
        CursorSnapshot {
            epoch: self.epoch,
            output_frames: self.output_frames,
            source_frame: self.source_frame,
            total_source_frames: self.total_source_frames,
            leading_silence_remaining: self.leading_silence_remaining,
            pending: self.pending,
            paused: self.paused,
            cancelled: self.cancelled,
        }
    }

    /// With an existing packet, return its current remaining ticket without
    /// changing its size, even if request differs. The caller may not build a
    /// second packet or reopen its decoder while this ticket remains pending.
    pub fn reserve(&mut self, request: u32) -> Result<Ticket, String> {
        self.require_running()?;
        if !(1..=MAX_PACKET_FRAMES).contains(&request) {
            return Err("Audio reservation must contain 1..=4800 frames".into());
        }
        if let Some(ticket) = self.pending {
            return Ok(ticket);
        }
        let end = self
            .output_frames
            .checked_add(u64::from(request))
            .ok_or("Audio output cursor overflow")?;
        if end > self.max_output_frames {
            return Err("Audio reservation exceeds the 24-hour output-frame budget".into());
        }
        let id = self
            .next_ticket_id
            .checked_add(1)
            .ok_or("Audio ticket identifier exhausted")?;
        let leading = self.leading_silence_remaining.min(u64::from(request)) as u32;
        let available = self
            .total_source_frames
            .checked_sub(self.source_frame)
            .ok_or("Audio source cursor exceeded its bound")?;
        let source = available.min(u64::from(request - leading)) as u32;
        let ticket = Ticket {
            epoch: self.epoch,
            id,
            output_start_frame: self.output_frames,
            span: AudioSpan {
                leading_silence_frames: leading,
                source_start_frame: self.source_frame,
                source_frames: source,
                trailing_silence_frames: request - leading - source,
            },
        };
        self.next_ticket_id = id;
        self.pending = Some(ticket);
        Ok(ticket)
    }

    /// Commit exactly the accepted prefix of the currently pending packet.
    /// Zero is a strict no-op. A positive partial acceptance returns a fresh
    /// remaining-ticket ID in the snapshot; old IDs may never be acknowledged
    /// again. All arithmetic and validation finish before any state changes.
    pub fn commit(
        &mut self,
        epoch: u64,
        id: u64,
        accepted_frames: u32,
    ) -> Result<CursorSnapshot, String> {
        self.require_running()?;
        if epoch != self.epoch {
            return Err("Audio acknowledgment belongs to an obsolete epoch".into());
        }
        let ticket = self
            .pending
            .ok_or("Audio acknowledgment has no pending packet")?;
        if ticket.id != id || ticket.epoch != epoch {
            return Err("Audio acknowledgment ticket is obsolete or unknown".into());
        }
        let length = ticket_length(&ticket)?;
        if accepted_frames > length {
            return Err("Audio device accepted more frames than remain in the packet".into());
        }
        if accepted_frames == 0 {
            return Ok(self.snapshot());
        }
        let next_output = self
            .output_frames
            .checked_add(u64::from(accepted_frames))
            .ok_or("Audio output frame count overflow")?;
        if next_output > self.max_output_frames {
            return Err("Audio acknowledgment exceeds the output-frame budget".into());
        }
        let accepted_leading = accepted_frames.min(ticket.span.leading_silence_frames);
        let after_leading = accepted_frames - accepted_leading;
        let accepted_source = after_leading.min(ticket.span.source_frames);
        let accepted_trailing = after_leading - accepted_source;
        let next_source = self
            .source_frame
            .checked_add(u64::from(accepted_source))
            .ok_or("Audio source frame count overflow")?;
        if next_source > self.total_source_frames {
            return Err("Audio acknowledgment reads beyond source EOF".into());
        }
        let next_leading = self
            .leading_silence_remaining
            .checked_sub(u64::from(accepted_leading))
            .ok_or("Audio leading-silence accounting underflow")?;
        let (pending, next_id) = if accepted_frames < length {
            let new_id = self
                .next_ticket_id
                .checked_add(1)
                .ok_or("Audio ticket identifier exhausted before partial acknowledgment")?;
            let remainder = Ticket {
                epoch,
                id: new_id,
                output_start_frame: next_output,
                span: AudioSpan {
                    leading_silence_frames: ticket.span.leading_silence_frames - accepted_leading,
                    source_start_frame: next_source,
                    source_frames: ticket.span.source_frames - accepted_source,
                    trailing_silence_frames: ticket
                        .span
                        .trailing_silence_frames
                        .checked_sub(accepted_trailing)
                        .ok_or("Audio trailing-silence accounting underflow")?,
                },
            };
            (Some(remainder), new_id)
        } else {
            (None, self.next_ticket_id)
        };
        self.output_frames = next_output;
        self.source_frame = next_source;
        self.leading_silence_remaining = next_leading;
        self.next_ticket_id = next_id;
        self.pending = pending;
        Ok(self.snapshot())
    }

    pub fn pause(&mut self) -> Result<CursorSnapshot, String> {
        self.require_active()?;
        self.paused = true;
        Ok(self.snapshot())
    }

    pub fn resume(&mut self) -> Result<CursorSnapshot, String> {
        self.require_active()?;
        self.paused = false;
        Ok(self.snapshot())
    }

    /// Seek invalidates old acknowledgments and computes a fresh anchor once.
    /// The caller separately stops/resets devices and invalidates prepared PCM.
    /// Existing pause state is preserved; output counts restart from local zero.
    pub fn seek(&mut self, target_ns: u64) -> Result<CursorSnapshot, String> {
        self.require_active()?;
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or("Audio cursor epoch exhausted")?;
        let (source_frame, leading) = anchor(
            &self.timeline,
            target_ns,
            self.sample_rate,
            self.total_source_frames,
            self.first_source_timeline_ns,
        )?;
        self.epoch = epoch;
        self.output_frames = 0;
        self.source_frame = source_frame;
        self.leading_silence_remaining = leading;
        self.pending = None;
        Ok(self.snapshot())
    }

    pub fn cancel(&mut self) -> Result<CursorSnapshot, String> {
        if self.cancelled {
            return Ok(self.snapshot());
        }
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or("Audio cursor epoch exhausted")?;
        self.epoch = epoch;
        self.cancelled = true;
        self.paused = true;
        self.pending = None;
        Ok(self.snapshot())
    }

    fn require_active(&self) -> Result<(), String> {
        if self.cancelled {
            Err("Audio cursor is cancelled and cannot be resumed".into())
        } else {
            Ok(())
        }
    }
    fn require_running(&self) -> Result<(), String> {
        self.require_active()?;
        if self.paused {
            Err("Audio cursor is paused; reservations and acknowledgments are not allowed".into())
        } else {
            Ok(())
        }
    }
}

fn ticket_length(ticket: &Ticket) -> Result<u32, String> {
    ticket
        .span
        .leading_silence_frames
        .checked_add(ticket.span.source_frames)
        .and_then(|value| value.checked_add(ticket.span.trailing_silence_frames))
        .ok_or("Audio ticket length overflow".into())
}

fn anchor(
    timeline: &AudioTimeline,
    target_ns: u64,
    sample_rate: u32,
    total_frames: u64,
    first_source_ns: i128,
) -> Result<(u64, u64), String> {
    let source_frame = timeline.span(target_ns, 0)?.source_start_frame;
    let target = i128::from(target_ns);
    let leading = if total_frames != 0 && target < first_source_ns {
        let numerator = first_source_ns
            .checked_sub(target)
            .and_then(|value| value.checked_mul(i128::from(sample_rate)))
            .ok_or("Audio cursor leading-silence calculation overflow")?;
        let frames = numerator / 1_000_000_000 + i128::from(numerator % 1_000_000_000 != 0);
        u64::try_from(frames).map_err(|_| "Audio cursor leading-silence count exceeds its range")?
    } else {
        0
    };
    Ok((source_frame, leading))
}

#[cfg(test)]
mod tests {
    use super::super::audio_timeline::Timestamp;
    use super::*;

    fn timeline(offset_samples: i64, total: u64) -> AudioTimeline {
        AudioTimeline::new(
            Timestamp {
                pts: offset_samples,
                numerator: 1,
                denominator: 48000,
            },
            Timestamp {
                pts: 0,
                numerator: 1,
                denominator: 1,
            },
            48000,
            total,
        )
        .unwrap()
    }
    fn prefix(ticket: &Ticket, accepted: u32) -> Vec<Option<u64>> {
        let span = ticket.span;
        std::iter::repeat_n(None, span.leading_silence_frames as usize)
            .chain(
                (span.source_start_frame..span.source_start_frame + u64::from(span.source_frames))
                    .map(Some),
            )
            .chain(std::iter::repeat_n(
                None,
                span.trailing_silence_frames as usize,
            ))
            .take(accepted as usize)
            .collect()
    }

    #[test]
    fn one_pending_ticket_zero_acceptance_and_partial_commit_are_transactional() {
        let mut cursor = AudioCursor::new(timeline(0, 1000), 0).unwrap();
        let first = cursor.reserve(480).unwrap();
        assert_eq!(cursor.reserve(17).unwrap(), first);
        let before = cursor.snapshot();
        assert_eq!(cursor.commit(first.epoch, first.id, 0).unwrap(), before);
        let partial = cursor.commit(first.epoch, first.id, 1).unwrap();
        assert_eq!((partial.output_frames, partial.source_frame), (1, 1));
        let next = partial.pending.unwrap();
        assert_ne!(next.id, first.id);
        assert_eq!(next.output_start_frame, 1);
        assert_eq!(next.span.source_start_frame, 1);
        assert_eq!(next.span.source_frames, 479);
        assert!(cursor.commit(first.epoch, first.id, 1).is_err());
        assert_eq!(cursor.snapshot(), partial);
        let more = cursor.commit(next.epoch, next.id, 17).unwrap();
        assert_eq!((more.output_frames, more.source_frame), (18, 18));
        assert_eq!(more.pending.unwrap().span.source_frames, 462);
    }

    #[test]
    fn full_and_irregular_acceptance_emit_the_same_silence_source_and_tail_sequence() {
        let mut whole = AudioCursor::new(timeline(2, 5), 0).unwrap();
        let full = whole.reserve(10).unwrap();
        let expected = vec![
            None,
            None,
            Some(0),
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            None,
            None,
            None,
        ];
        assert_eq!(prefix(&full, 10), expected);
        whole.commit(full.epoch, full.id, 10).unwrap();
        let mut split = AudioCursor::new(timeline(2, 5), 0).unwrap();
        let mut emitted = Vec::new();
        for amount in [0, 1, 1, 3, 1, 4] {
            let ticket = split
                .reserve(if emitted.is_empty() { 10 } else { 1 })
                .unwrap();
            emitted.extend(prefix(&ticket, amount));
            split.commit(ticket.epoch, ticket.id, amount).unwrap();
        }
        assert_eq!(emitted, expected);
        assert_eq!(
            split.snapshot().output_frames,
            whole.snapshot().output_frames
        );
        assert_eq!(split.snapshot().source_frame, whole.snapshot().source_frame);
        assert!(split.snapshot().pending.is_none());
    }

    #[test]
    fn long_leading_gap_is_preserved_across_blocks_and_negative_origin_skips_source() {
        let mut positive = AudioCursor::new(timeline(96000, 48000), 0).unwrap();
        assert_eq!(positive.snapshot().leading_silence_remaining, 96000);
        for _ in 0..20 {
            let ticket = positive.reserve(4800).unwrap();
            assert_eq!(ticket.span.leading_silence_frames, 4800);
            positive.commit(ticket.epoch, ticket.id, 4800).unwrap();
        }
        let first_source = positive.reserve(17).unwrap();
        assert_eq!(
            (
                first_source.span.leading_silence_frames,
                first_source.span.source_start_frame,
                first_source.span.source_frames
            ),
            (0, 0, 17)
        );
        let mut negative = AudioCursor::new(timeline(-96000, 144000), 0).unwrap();
        assert_eq!(negative.reserve(17).unwrap().span.source_start_frame, 96000);
    }

    #[test]
    fn rounded_first_sample_anchor_never_duplicates_or_skips_over_ten_thousand_samples() {
        let mut cursor = AudioCursor::new(timeline(0, 20000), 20833).unwrap();
        let mut emitted = Vec::new();
        let mut cycle = [1_u32, 17, 479, 4800, 31].into_iter().cycle();
        while emitted.len() < 10000 {
            let count = cycle.next().unwrap().min(10000 - emitted.len() as u32);
            let ticket = cursor.reserve(count).unwrap();
            emitted.extend(prefix(&ticket, count));
            cursor.commit(ticket.epoch, ticket.id, count).unwrap();
        }
        assert_eq!(emitted, (1..=10000).map(Some).collect::<Vec<_>>());
        assert_eq!(
            (
                cursor.snapshot().output_frames,
                cursor.snapshot().source_frame
            ),
            (10000, 10001)
        );
    }

    #[test]
    fn pause_and_resume_preserve_pending_packet_and_reject_acks_while_paused() {
        let mut cursor = AudioCursor::new(timeline(0, 100), 0).unwrap();
        let first = cursor.reserve(10).unwrap();
        let paused = cursor.pause().unwrap();
        assert!(paused.paused);
        assert_eq!(paused.pending, Some(first));
        assert_eq!(cursor.pause().unwrap(), paused);
        assert!(cursor.reserve(10).is_err());
        assert!(cursor.commit(first.epoch, first.id, 1).is_err());
        assert_eq!(cursor.snapshot(), paused);
        assert_eq!(cursor.resume().unwrap().pending, Some(first));
        assert_eq!(
            cursor
                .commit(first.epoch, first.id, 10)
                .unwrap()
                .source_frame,
            10
        );
    }

    #[test]
    fn seek_reanchors_new_epoch_and_stale_ack_cannot_mutate_current_packet() {
        let mut cursor = AudioCursor::new(timeline(0, 100000), 0).unwrap();
        let old = cursor.reserve(480).unwrap();
        cursor.pause().unwrap();
        let sought = cursor.seek(1_000_000_000).unwrap();
        assert!(sought.epoch > old.epoch && sought.paused);
        assert_eq!((sought.output_frames, sought.source_frame), (0, 48000));
        assert!(sought.pending.is_none());
        cursor.resume().unwrap();
        let new = cursor.reserve(5).unwrap();
        assert_ne!(new.id, old.id);
        let before = cursor.snapshot();
        assert!(cursor.commit(old.epoch, old.id, 5).is_err());
        assert_eq!(cursor.snapshot(), before);
        cursor.commit(new.epoch, new.id, 5).unwrap();
        assert_eq!(cursor.snapshot().source_frame, 48005);
    }

    #[test]
    fn invalid_acceptance_eof_cancel_and_output_budget_do_not_silently_advance() {
        let mut cursor = AudioCursor::new(timeline(0, 2), 0).unwrap();
        assert!(cursor.reserve(0).is_err());
        assert!(cursor.reserve(4801).is_err());
        let ticket = cursor.reserve(5).unwrap();
        let before = cursor.snapshot();
        assert!(cursor.commit(ticket.epoch, ticket.id, 6).is_err());
        assert!(cursor.commit(ticket.epoch, ticket.id + 1, 1).is_err());
        assert_eq!(cursor.snapshot(), before);
        cursor.commit(ticket.epoch, ticket.id, 5).unwrap();
        let tail = cursor.reserve(5).unwrap();
        assert_eq!(tail.span.trailing_silence_frames, 5);
        assert_eq!(tail.span.source_start_frame, 2);
        let cancelled = cursor.cancel().unwrap();
        assert!(cancelled.cancelled && cancelled.pending.is_none());
        assert_eq!(cursor.cancel().unwrap(), cancelled);
        assert!(cursor.reserve(1).is_err());
        assert!(cursor.resume().is_err());
        assert!(cursor.seek(0).is_err());
        assert!(cursor.commit(tail.epoch, tail.id, 1).is_err());
        let mut empty = AudioCursor::new(timeline(0, 0), 0).unwrap();
        assert_eq!(empty.reserve(1).unwrap().span.trailing_silence_frames, 1);
        let mut exhausted = AudioCursor::new(timeline(0, 0), 0).unwrap();
        exhausted.output_frames = 48000 * 86400;
        assert!(exhausted.reserve(1).is_err());
    }

    #[test]
    fn epoch_and_ticket_id_exhaustion_preserve_existing_pending_work() {
        let mut cursor = AudioCursor::new(timeline(0, 100), 0).unwrap();
        let ticket = cursor.reserve(10).unwrap();
        cursor.epoch = u64::MAX;
        let before = cursor.snapshot();
        assert!(cursor.seek(0).is_err());
        assert!(cursor.cancel().is_err());
        assert_eq!(cursor.snapshot(), before);
        cursor.epoch = ticket.epoch;
        cursor.next_ticket_id = u64::MAX;
        let before = cursor.snapshot();
        assert!(cursor.commit(ticket.epoch, ticket.id, 1).is_err());
        assert_eq!(cursor.snapshot(), before);
        assert_eq!(cursor.commit(ticket.epoch, ticket.id, 0).unwrap(), before);
        cursor.commit(ticket.epoch, ticket.id, 10).unwrap();
        let before = cursor.snapshot();
        assert!(cursor.reserve(1).is_err());
        assert_eq!(cursor.snapshot(), before);
    }
}
