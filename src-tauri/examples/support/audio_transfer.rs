//! Bounded PCM assembly for a single continuous cursor and decoder session.
#[path = "video_timeline.rs"]
mod video_timeline;
use super::{
    audio_cursor::{AudioCursor, CursorSnapshot, Ticket},
    audio_timeline::AudioTimeline,
    streaming_decoder::{AudioBlock, DecoderStats, StreamConfig, StreamingDecoder, VideoFrame},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Instant};
pub use video_timeline::VideoTimelineSnapshot;
use video_timeline::{TimedFrame, VideoTimeline};

struct Prepared {
    ticket: Ticket,
    samples: Vec<f32>,
    source_filled: u32,
    source_total: u32,
    leading: u32,
    offset: usize,
}
pub struct AudioTransfer {
    cursor: AudioCursor,
    decoder: Option<StreamingDecoder>,
    last_decoder_shutdown: Option<Result<Value, String>>,
    config: StreamConfig,
    input: Option<AudioBlock>,
    input_offset: usize,
    prepared: Option<Prepared>,
    video: Option<VideoTimeline<Arc<[u8]>>>,
    allow_audio_eof: bool,
    pub decoder_starts: u64,
}
impl AudioTransfer {
    pub fn new(
        timeline: AudioTimeline,
        target_ns: u64,
        config: StreamConfig,
    ) -> Result<Self, String> {
        let mut result = Self::configured(timeline, target_ns, config, false)?;
        result.open_decoder()?;
        Ok(result)
    }

    /// Finite source playback can seek into the last video frame after its
    /// audio samples have ended. AudioTimeline provides bounded tail silence;
    /// the source actor, not this transfer, owns the finite playback endpoint.
    pub fn new_av_source(
        timeline: AudioTimeline,
        target_ns: u64,
        config: StreamConfig,
    ) -> Result<Self, String> {
        let mut result = Self::configured(timeline, target_ns, config, false)?;
        result.allow_audio_eof = true;
        result.config.start_video_ns = target_ns;
        result.video = Some(VideoTimeline::new(result.cursor.snapshot().epoch)?);
        result.open_decoder()?;
        Ok(result)
    }

    /// Share one source epoch and StreamingDecoder for audio and video. Unlike
    /// new(), PCM preparation does not discard the sibling video queue. The
    /// coordinator must poll video_frame using project time from its clock.
    /// This first AV adapter requires active audio at the initial/seek target;
    /// audio-only trailing-silence behavior remains available through new().
    pub fn new_av(
        timeline: AudioTimeline,
        target_ns: u64,
        config: StreamConfig,
    ) -> Result<Self, String> {
        let mut result = Self::configured(timeline, target_ns, config, true)?;
        result.open_decoder()?;
        Ok(result)
    }

    fn configured(
        timeline: AudioTimeline,
        target_ns: u64,
        mut config: StreamConfig,
        av: bool,
    ) -> Result<Self, String> {
        let cursor = AudioCursor::new(timeline, target_ns)?;
        config.epoch = cursor.snapshot().epoch;
        let video = if av {
            if cursor.snapshot().source_frame >= cursor.snapshot().total_source_frames {
                return Err(
                    "AV transfer currently requires an active audio source at its target".into(),
                );
            }
            config.start_video_ns = target_ns;
            Some(VideoTimeline::new(cursor.snapshot().epoch)?)
        } else {
            None
        };
        Ok(Self {
            cursor,
            decoder: None,
            last_decoder_shutdown: None,
            config,
            input: None,
            input_offset: 0,
            prepared: None,
            video,
            allow_audio_eof: false,
            decoder_starts: 0,
        })
    }
    fn open_decoder(&mut self) -> Result<(), String> {
        let state = self.cursor.snapshot();
        self.config.epoch = state.epoch;
        self.config.start_audio_frame = state.source_frame;
        if self.video.is_some()
            && state.source_frame >= state.total_source_frames
            && !self.allow_audio_eof
        {
            return Err(
                "AV transfer currently requires an active audio source at its target".into(),
            );
        }
        if state.source_frame < state.total_source_frames || self.allow_audio_eof {
            self.decoder = Some(if self.allow_audio_eof {
                StreamingDecoder::start_source(self.config.clone())?
            } else {
                StreamingDecoder::start(self.config.clone())?
            });
            self.last_decoder_shutdown = None;
            self.decoder_starts += 1;
        }
        Ok(())
    }
    pub fn snapshot(&self) -> CursorSnapshot {
        self.cursor.snapshot()
    }
    pub fn video_snapshot(&self) -> Option<VideoTimelineSnapshot> {
        self.video.as_ref().map(VideoTimeline::snapshot)
    }
    /// Control-stage inspection only; process/queue statistics may acquire
    /// short CPU locks. No device call or decoder polling happens here.
    #[cfg(debug_assertions)]
    pub fn validation_video_failure(
        &self,
    ) -> Result<Box<dyn Fn() -> Result<Value, String> + Send>, String> {
        self.decoder
            .as_ref()
            .ok_or("Missing owned decoder")?
            .validation_video_failure()
    }
    pub fn decoder_stats(&self) -> Result<Option<DecoderStats>, String> {
        self.decoder
            .as_ref()
            .map(StreamingDecoder::stats)
            .transpose()
    }

    /// Return the latest observed due video frame, retaining one future frame.
    /// The returned Arc shares bounded decoded RGB data; this does not render,
    /// read the device clock or establish that no newer due frame is delayed.
    pub fn video_frame(&mut self, project_time_ns: u64) -> Result<Option<VideoFrame>, String> {
        let state = self.cursor.snapshot();
        if state.cancelled {
            return Err("Cannot select video from a cancelled transfer".into());
        }
        let selector = self
            .video
            .as_mut()
            .ok_or("Video selection requires new_av transfer mode")?;
        if selector.snapshot().epoch != state.epoch {
            return Err("Audio/video transfer epochs diverged".into());
        }
        let decoder = self
            .decoder
            .as_mut()
            .ok_or("Shared AV decoder is unavailable")?;
        let selected = selector.poll_at(project_time_ns, || {
            decoder.poll_video().map(|frame| {
                frame.map(|frame| TimedFrame {
                    epoch: frame.epoch,
                    index: frame.index,
                    pts_ns: frame.pts_ns,
                    payload: frame.rgb,
                })
            })
        })?;
        Ok(selected.map(|frame| VideoFrame {
            epoch: frame.epoch,
            index: frame.index,
            pts_ns: frame.pts_ns,
            rgb: Arc::clone(&frame.payload),
        }))
    }
    pub fn prepare(&mut self, request: u32) -> Result<bool, String> {
        let state = self.cursor.snapshot();
        if state.paused || state.cancelled {
            return Err("Cannot prepare output while paused or cancelled".into());
        }
        if !(1..=4800).contains(&request) {
            return Err("Audio transfer request must be 1..=4800 frames".into());
        }
        if self.prepared.is_none() {
            let ticket = self.cursor.reserve(request)?;
            let span = ticket.span;
            let frames =
                span.leading_silence_frames + span.source_frames + span.trailing_silence_frames;
            self.prepared = Some(Prepared {
                ticket,
                samples: vec![0.0; frames as usize * 2],
                source_filled: 0,
                source_total: span.source_frames,
                leading: span.leading_silence_frames,
                offset: 0,
            });
        }
        // Legacy audio-only transfers drain the unused sibling stream. AV
        // transfers keep it exclusively for the project-time video selector.
        if self.video.is_none() {
            if let Some(decoder) = self.decoder.as_mut() {
                let _ = decoder.poll_video()?;
            }
        }
        let prepared = self
            .prepared
            .as_mut()
            .ok_or("Missing reserved audio packet")?;
        while prepared.source_filled < prepared.source_total {
            if self.input.is_none() {
                self.input = self
                    .decoder
                    .as_mut()
                    .ok_or("Missing source decoder")?
                    .poll_audio()?;
                self.input_offset = 0;
            }
            let Some(block) = self.input.as_ref() else {
                return Ok(false);
            };
            let expected =
                prepared.ticket.span.source_start_frame + u64::from(prepared.source_filled);
            if block.epoch != prepared.ticket.epoch
                || block.source_start_frame + (self.input_offset / 2) as u64 != expected
            {
                return Err("Source block epoch or sample continuity mismatch".into());
            }
            if block.samples.len() % 2 != 0 || block.samples.len() > 960 || block.samples.is_empty()
            {
                return Err("Invalid bounded source PCM block".into());
            }
            let count = ((block.samples.len() - self.input_offset) / 2)
                .min((prepared.source_total - prepared.source_filled) as usize);
            let out = (prepared.leading + prepared.source_filled) as usize * 2;
            prepared.samples[out..out + count * 2]
                .copy_from_slice(&block.samples[self.input_offset..self.input_offset + count * 2]);
            self.input_offset += count * 2;
            prepared.source_filled += count as u32;
            if self.input_offset == block.samples.len() {
                self.input = None;
            }
        }
        Ok(true)
    }
    pub fn packet(&self) -> Result<(&[f32], Ticket), String> {
        let p = self.prepared.as_ref().ok_or("No prepared audio packet")?;
        if p.source_filled != p.source_total {
            return Err("Audio packet is still waiting for decoded samples".into());
        }
        Ok((&p.samples[p.offset..], p.ticket))
    }
    pub fn commit(&mut self, accepted: u32) -> Result<CursorSnapshot, String> {
        let p = self.prepared.as_mut().ok_or("No packet to commit")?;
        if p.source_filled != p.source_total {
            return Err("Cannot commit an incomplete packet".into());
        }
        let snapshot = self.cursor.commit(p.ticket.epoch, p.ticket.id, accepted)?;
        p.offset += accepted as usize * 2;
        if let Some(ticket) = snapshot.pending {
            p.ticket = ticket;
        } else {
            self.prepared = None;
        }
        Ok(snapshot)
    }
    pub fn reject_replay(&mut self, ticket: Ticket) -> bool {
        let before = self.cursor.snapshot();
        let rejected = self.cursor.commit(ticket.epoch, ticket.id, 1).is_err();
        rejected && self.cursor.snapshot() == before
    }
    /// Diagnostic-only stale acknowledgment check. A current ticket is never
    /// submitted, and zero accepted frames cannot advance a source cursor.
    pub fn reject_stale_ack_zero(&mut self, ticket: Ticket) -> bool {
        let before = self.cursor.snapshot();
        if before.epoch == ticket.epoch
            && before
                .pending
                .is_some_and(|pending| pending.id == ticket.id)
        {
            return false;
        }
        let rejected = self.cursor.commit(ticket.epoch, ticket.id, 0).is_err();
        rejected && self.cursor.snapshot() == before
    }
    pub fn pause(&mut self) -> Result<CursorSnapshot, String> {
        self.cursor.pause()
    }
    pub fn resume(&mut self) -> Result<CursorSnapshot, String> {
        self.cursor.resume()
    }
    pub fn paused_commit_rejected(&mut self) -> bool {
        let before = self.cursor.snapshot();
        let result = self.commit(1);
        result.is_err() && before == self.cursor.snapshot()
    }
    pub fn seek(&mut self, target: u64) -> Result<Value, String> {
        let start = Instant::now();
        let (old, after) = self.reset_for_seek(target)?;
        let shutdown = self.close_decoder()?;
        self.open_decoder()?;
        let stale_rejected = old.pending.is_none_or(|ticket| self.reject_replay(ticket));
        if !stale_rejected {
            return Err("Old output ticket remained valid after seek".into());
        }
        Ok(
            json!({"before":old,"after":after,"oldDecoderShutdown":shutdown,"staleTicketRejected":stale_rejected,"elapsedMs":start.elapsed().as_secs_f64()*1000.0,"decoderStarts":self.decoder_starts}),
        )
    }

    /// Invalidate all local output/selection state before stopping the old
    /// decoder or opening a replacement. Decoder ownership is handled by seek.
    fn reset_for_seek(&mut self, target: u64) -> Result<(CursorSnapshot, CursorSnapshot), String> {
        let old = self.cursor.snapshot();
        let after = self.cursor.seek(target)?;
        if let Some(selector) = &mut self.video {
            selector.reset(after.epoch)?;
            self.config.start_video_ns = target;
        }
        self.prepared = None;
        self.input = None;
        self.input_offset = 0;
        Ok((old, after))
    }
    fn close_decoder(&mut self) -> Result<Value, String> {
        if let Some(mut decoder) = self.decoder.take() {
            let result = decoder.close().and_then(|report| {
                // Decode failure and resource cleanup are independent facts.
                // Preserve both the original failure and the full process ledger
                // when teardown succeeded, so the adapter can report safe close.
                let clean =
                    report.joined && report.processes_reaped && report.cleanup_errors.is_empty();
                let value = json!(report);
                if clean {
                    Ok(value)
                } else {
                    Err(format!("Continuous decoder did not close cleanly: {value}"))
                }
            });
            // Taking the decoder must not make a retry erase a failed cleanup.
            self.last_decoder_shutdown = Some(result);
        }
        self.last_decoder_shutdown
            .clone()
            .unwrap_or(Ok(Value::Null))
    }
    pub fn close(&mut self) -> Result<Value, String> {
        let cancelled = self.cursor.cancel()?;
        if let Some(selector) = &mut self.video {
            // The cancelled cursor invalidates the retained frames as well.
            // A second close is idempotent and does not reset the same epoch.
            if selector.snapshot().epoch < cancelled.epoch {
                selector.reset(cancelled.epoch)?;
            }
        }
        self.prepared = None;
        self.input = None;
        let decoder = self.close_decoder()?;
        let state = self.cursor.snapshot();
        let rejected = self.cursor.reserve(1).is_err() && self.cursor.snapshot() == state;
        if !rejected {
            return Err("Cancelled transport accepted new output".into());
        }
        Ok(
            json!({"cursor":cancelled,"decoder":decoder,"decoderStarts":self.decoder_starts,"cancelRejectsOutput":rejected}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::{audio_timeline::Timestamp, streaming_decoder::SeekMode};
    use super::*;

    fn config() -> StreamConfig {
        StreamConfig {
            ffmpeg: "unused-ffmpeg".into(),
            ffprobe: "unused-ffprobe".into(),
            video: "unused.mp4".into(),
            audio: "unused.wav".into(),
            sample_rate: 48000,
            channels: 2,
            video_width: 160,
            video_height: 90,
            seek_mode: SeekMode::IndexedPts,
            epoch: 77,
            start_audio_frame: 0,
            start_video_ns: 0,
        }
    }
    fn timeline(total: u64) -> AudioTimeline {
        AudioTimeline::new(
            Timestamp {
                pts: 2,
                numerator: 1,
                denominator: 1,
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

    #[test]
    fn decoder_source_failure_preserves_successful_shutdown_evidence() {
        let mut transfer = AudioTransfer::configured(timeline(480000), 0, config(), true).unwrap();
        transfer.decoder = Some(super::super::streaming_decoder::failed_decoder_fixture(
            "controlled source decode failure",
            false,
        ));
        let report = transfer
            .close()
            .expect("a decoded source error does not undo successful cleanup");
        assert_eq!(report["decoder"]["joined"], true);
        assert_eq!(report["decoder"]["processesReaped"], true);
        assert_eq!(
            report["decoder"]["error"],
            "controlled source decode failure"
        );
        assert_eq!(
            report["decoder"]["stats"]["error"],
            "controlled source decode failure"
        );
        assert_eq!(report["decoder"]["cleanupErrors"], json!([]));
        assert_eq!(transfer.close().unwrap()["decoder"], report["decoder"]);
    }

    #[test]
    fn decoder_cleanup_failure_cannot_turn_into_empty_success_on_retry() {
        let mut transfer = AudioTransfer::configured(timeline(480000), 0, config(), true).unwrap();
        transfer.decoder = Some(super::super::streaming_decoder::failed_decoder_fixture(
            "controlled source decode failure",
            true,
        ));
        let first = transfer
            .close()
            .expect_err("a reader panic is a cleanup failure");
        assert!(first.contains("reader thread panicked"));
        let second = transfer
            .close()
            .expect_err("retry must retain the failed cleanup ledger");
        assert_eq!(first, second);
    }

    #[test]
    fn av_and_audio_only_initialization_keep_their_explicit_video_seek_contract() {
        let av =
            AudioTransfer::configured(timeline(480000), 1_500_000_000, config(), true).unwrap();
        assert_eq!(av.config.start_video_ns, 1_500_000_000);
        assert_eq!(av.video_snapshot().unwrap().epoch, av.snapshot().epoch);
        let audio =
            AudioTransfer::configured(timeline(480000), 1_500_000_000, config(), false).unwrap();
        assert_eq!(audio.config.start_video_ns, 0);
        assert!(audio.video_snapshot().is_none());
        assert!(AudioTransfer::configured(timeline(0), 0, config(), true).is_err());
        assert!(AudioTransfer::configured(timeline(48000), 3_000_000_000, config(), true).is_err());
    }

    #[test]
    fn av_audio_preparation_keeps_future_video_and_seek_invalidates_all_retained_frames() {
        let mut transfer = AudioTransfer::configured(timeline(480000), 0, config(), true).unwrap();
        let epoch = transfer.snapshot().epoch;
        let mut source = std::collections::VecDeque::from([
            TimedFrame {
                epoch,
                index: 0,
                pts_ns: 0,
                payload: Arc::from([1_u8, 2, 3]),
            },
            TimedFrame {
                epoch,
                index: 1,
                pts_ns: 70_000_000,
                payload: Arc::from([4_u8, 5, 6]),
            },
        ]);
        transfer
            .video
            .as_mut()
            .unwrap()
            .poll_at(0, || Ok(source.pop_front()))
            .unwrap();
        let selected = transfer.video_snapshot().unwrap();
        // Audio begins at 2s, so this bounded packet is real leading silence and
        // needs no decoder. Its preparation must not drain the future frame.
        assert!(transfer.prepare(480).unwrap());
        assert_eq!(transfer.video_snapshot(), Some(selected));
        let (old, after) = transfer.reset_for_seek(50_000_000).unwrap();
        assert!(after.epoch > old.epoch);
        assert!(after.pending.is_none());
        assert!(transfer.prepared.is_none());
        assert_eq!(transfer.config.start_video_ns, 50_000_000);
        let state = transfer.video_snapshot().unwrap();
        assert_eq!(state.epoch, after.epoch);
        assert_eq!(state.buffered_frames, 0);
        assert_eq!(state.last_query_ns, None);
    }

    #[test]
    fn audio_only_seek_and_silence_behavior_remain_usable_without_a_video_consumer() {
        let mut transfer = AudioTransfer::new(timeline(0), 0, config()).unwrap();
        assert!(transfer.prepare(17).unwrap());
        let (samples, ticket) = transfer.packet().unwrap();
        assert_eq!(samples, &[0.0; 34]);
        assert_eq!(ticket.span.trailing_silence_frames, 17);
        transfer.commit(17).unwrap();
        transfer.seek(1_000_000_000).unwrap();
        assert_eq!(transfer.config.start_video_ns, 0);
        assert!(transfer.video_frame(1_000_000_000).is_err());
        assert!(transfer.decoder_stats().unwrap().is_none());
        transfer.close().unwrap();
        assert!(transfer.snapshot().cancelled);
    }
}
