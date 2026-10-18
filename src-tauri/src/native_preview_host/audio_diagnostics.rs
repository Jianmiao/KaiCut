//! Bounded observations around audio Start and pre-pump starvation checks.
//! This records device acceptance and scheduling intervals, not audible output.
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Instant;

const RETAINED_STARTS: usize = 16;
const RETAINED_UNDERRUNS: usize = 16;

pub struct StartObservation {
    pub source_epoch: u64,
    pub render_epoch: u64,
    pub accepted_frames: u64,
    pub output_frames: u64,
    pub pending_frames: Option<u32>,
    pub pre_start_padding: Result<u32, String>,
    pub capacity_frames: u32,
    pub sample_rate: u32,
}

pub struct PumpObservation {
    pub observed_at: Instant,
    pub source_epoch: u64,
    pub render_epoch: u64,
    pub accepted_frames: u64,
    pub output_frames: u64,
    pub pending_frames: Option<u32>,
    pub padding_frames: u32,
    pub remaining_frames: Option<u64>,
    pub since_last_pump_ms: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawClockSample {
    pub position: String,
    pub frequency: u64,
    pub qpc_100ns: String,
    pub reliable: bool,
    pub hresult: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClockObservation {
    pub sample: Option<RawClockSample>,
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartRecord {
    sequence: u64,
    source_epoch: u64,
    render_epoch: u64,
    accepted_frames: u64,
    output_frames: u64,
    pending_frames: Option<u32>,
    pre_start_padding_frames: Option<u32>,
    pre_start_padding_error: Option<String>,
    capacity_frames: u32,
    sample_rate: u32,
    call_began_at_segment_ms: f64,
    call_duration_ms: f64,
    succeeded: bool,
    error: Option<String>,
    first_pump: Option<PumpStamp>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PumpStamp {
    pump_index: u64,
    start_sequence: Option<u64>,
    first_pump: bool,
    source_epoch: u64,
    render_epoch: u64,
    accepted_frames: u64,
    output_frames: u64,
    pending_frames: Option<u32>,
    padding_frames: u32,
    remaining_frames: Option<u64>,
    at_segment_ms: f64,
    since_start_call_ms: Option<f64>,
    since_start_return_ms: Option<f64>,
    since_last_pump_ms: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnderrunRecord {
    #[serde(flatten)]
    pump: PumpStamp,
    clock: ClockObservation,
    clock_sampled_after_observation_ms: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsSummary {
    segment_sequence: u64,
    target_ns: String,
    start_count: u64,
    pump_count: u64,
    underrun_count: u64,
    retained_starts: usize,
    retained_underruns: usize,
    dropped_starts: u64,
    dropped_underruns: u64,
    complete: bool,
    max_starts: usize,
    max_underruns: usize,
    start_call_scope: &'static str,
}

struct ActiveStart {
    sequence: u64,
    call_began: Instant,
    returned: Instant,
    first_pump_pending: bool,
    retained_index: Option<usize>,
}

pub struct AudioDiagnostics {
    segment_sequence: u64,
    target: u64,
    origin: Instant,
    start_count: u64,
    pump_count: u64,
    underrun_count: u64,
    dropped_starts: u64,
    dropped_underruns: u64,
    active_start: Option<ActiveStart>,
    starts: Vec<StartRecord>,
    underruns: Vec<UnderrunRecord>,
}

impl AudioDiagnostics {
    pub fn new(segment_sequence: u64, target: u64) -> Self {
        Self {
            segment_sequence,
            target,
            origin: Instant::now(),
            start_count: 0,
            pump_count: 0,
            underrun_count: 0,
            dropped_starts: 0,
            dropped_underruns: 0,
            active_start: None,
            starts: Vec::new(),
            underruns: Vec::new(),
        }
    }

    pub fn record_start(
        &mut self,
        observation: StartObservation,
        call_began: Instant,
        returned: Instant,
        error: Option<&str>,
    ) {
        self.start_count += 1;
        let retained_index = if self.starts.len() < RETAINED_STARTS {
            let index = self.starts.len();
            let (padding, padding_error) = match observation.pre_start_padding {
                Ok(padding) => (Some(padding), None),
                Err(error) => (None, Some(bounded_error(&error))),
            };
            self.starts.push(StartRecord {
                sequence: self.start_count,
                source_epoch: observation.source_epoch,
                render_epoch: observation.render_epoch,
                accepted_frames: observation.accepted_frames,
                output_frames: observation.output_frames,
                pending_frames: observation.pending_frames,
                pre_start_padding_frames: padding,
                pre_start_padding_error: padding_error,
                capacity_frames: observation.capacity_frames,
                sample_rate: observation.sample_rate,
                call_began_at_segment_ms: elapsed_ms(call_began, self.origin),
                call_duration_ms: elapsed_ms(returned, call_began),
                succeeded: error.is_none(),
                error: error.map(bounded_error),
                first_pump: None,
            });
            Some(index)
        } else {
            self.dropped_starts += 1;
            None
        };
        self.active_start = error.is_none().then_some(ActiveStart {
            sequence: self.start_count,
            call_began,
            returned,
            first_pump_pending: true,
            retained_index,
        });
    }

    pub fn record_pump(
        &mut self,
        observation: PumpObservation,
        clock: impl FnOnce() -> ClockObservation,
    ) {
        self.pump_count += 1;
        let active = self.active_start.as_mut();
        let stamp = PumpStamp {
            pump_index: self.pump_count,
            start_sequence: active.as_ref().map(|start| start.sequence),
            first_pump: active
                .as_ref()
                .is_some_and(|start| start.first_pump_pending),
            source_epoch: observation.source_epoch,
            render_epoch: observation.render_epoch,
            accepted_frames: observation.accepted_frames,
            output_frames: observation.output_frames,
            pending_frames: observation.pending_frames,
            padding_frames: observation.padding_frames,
            remaining_frames: observation.remaining_frames,
            at_segment_ms: elapsed_ms(observation.observed_at, self.origin),
            since_start_call_ms: active
                .as_ref()
                .map(|start| elapsed_ms(observation.observed_at, start.call_began)),
            since_start_return_ms: active
                .as_ref()
                .map(|start| elapsed_ms(observation.observed_at, start.returned)),
            since_last_pump_ms: observation.since_last_pump_ms,
        };
        if let Some(active) = active {
            if active.first_pump_pending {
                if let Some(index) = active.retained_index {
                    self.starts[index].first_pump = Some(stamp.clone());
                }
                active.first_pump_pending = false;
            }
        }
        // Identical to the existing actor observation: an exhausted finite
        // endpoint is not starvation. Unknown remaining time stays unknown.
        if observation.padding_frames == 0 && observation.remaining_frames != Some(0) {
            self.underrun_count += 1;
            if self.underruns.len() < RETAINED_UNDERRUNS {
                let mut clock = clock();
                clock.error = clock.error.as_deref().map(bounded_error);
                self.underruns.push(UnderrunRecord {
                    pump: stamp,
                    clock,
                    clock_sampled_after_observation_ms: elapsed_ms(
                        Instant::now(),
                        observation.observed_at,
                    ),
                });
            } else {
                self.dropped_underruns += 1;
            }
        }
    }

    pub fn summary(&self) -> DiagnosticsSummary {
        DiagnosticsSummary {
            segment_sequence: self.segment_sequence,
            target_ns: self.target.to_string(),
            start_count: self.start_count,
            pump_count: self.pump_count,
            underrun_count: self.underrun_count,
            retained_starts: self.starts.len(),
            retained_underruns: self.underruns.len(),
            dropped_starts: self.dropped_starts,
            dropped_underruns: self.dropped_underruns,
            complete: self.dropped_starts == 0 && self.dropped_underruns == 0,
            max_starts: RETAINED_STARTS,
            max_underruns: RETAINED_UNDERRUNS,
            start_call_scope: "AudioPreviewAdapter::start",
        }
    }

    pub fn report(&self) -> Value {
        let mut report = json!(self.summary());
        report["starts"] = json!(self.starts);
        report["underruns"] = json!(self.underruns);
        report["decoderStats"] = json!({"sampled":false,"reason":"not sampled on pump path: decoder statistics require a blocking mutex"});
        report
    }
}

fn elapsed_ms(later: Instant, earlier: Instant) -> f64 {
    later.saturating_duration_since(earlier).as_secs_f64() * 1000.0
}

fn bounded_error(error: &str) -> String {
    error.chars().take(512).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, time::Duration};

    fn start(source_epoch: u64, render_epoch: u64) -> StartObservation {
        StartObservation {
            source_epoch,
            render_epoch,
            accepted_frames: 4800,
            output_frames: 4800,
            pending_frames: Some(480),
            pre_start_padding: Ok(4800),
            capacity_frames: 4800,
            sample_rate: 48000,
        }
    }

    fn pump(observed_at: Instant, remaining_frames: Option<u64>) -> PumpObservation {
        PumpObservation {
            observed_at,
            source_epoch: 1,
            render_epoch: 3,
            accepted_frames: 4800,
            output_frames: 4800,
            pending_frames: Some(480),
            padding_frames: 0,
            remaining_frames,
            since_last_pump_ms: 20.0,
        }
    }

    fn clock() -> ClockObservation {
        ClockObservation {
            sample: Some(RawClockSample {
                position: "9007199254740993".into(),
                frequency: 48000,
                qpc_100ns: "9007199254740999".into(),
                reliable: true,
                hresult: 0,
            }),
            error: None,
        }
    }

    #[test]
    fn first_pump_distinguishes_start_call_duration_from_post_return_latency() {
        let mut diagnostics = AudioDiagnostics::new(2, 3800000000);
        let began = diagnostics.origin;
        diagnostics.record_start(start(1, 3), began, began + Duration::from_millis(30), None);
        diagnostics.record_pump(pump(began + Duration::from_millis(50), Some(4800)), clock);
        let report = diagnostics.report();
        assert_eq!(report["startCount"], 1);
        assert_eq!(report["starts"][0]["callDurationMs"], 30.0);
        assert_eq!(report["starts"][0]["firstPump"]["sinceStartReturnMs"], 20.0);
        assert_eq!(report["starts"][0]["firstPump"]["sinceStartCallMs"], 50.0);
        assert_eq!(report["underruns"][0]["firstPump"], true);
        assert_eq!(report["underruns"][0]["remainingFrames"], 4800);
        assert_eq!(
            report["underruns"][0]["clock"]["sample"]["position"],
            "9007199254740993"
        );
    }

    #[test]
    fn exhausted_endpoint_is_not_underrun_but_unknown_remaining_is() {
        let mut diagnostics = AudioDiagnostics::new(1, 0);
        let now = diagnostics.origin;
        diagnostics.record_start(start(1, 1), now, now, None);
        diagnostics.record_pump(pump(now, Some(0)), || panic!("EOF does not sample clock"));
        let mut nonempty = pump(now, Some(4800));
        nonempty.padding_frames = 1;
        diagnostics.record_pump(nonempty, || panic!("Buffered audio does not sample clock"));
        diagnostics.record_pump(pump(now, None), clock);
        let report = diagnostics.report();
        assert_eq!(report["pumpCount"], 3);
        assert_eq!(report["underrunCount"], 1);
        assert_eq!(report["underruns"][0]["remainingFrames"], Value::Null);
        assert_eq!(report["underruns"][0]["firstPump"], false);
    }

    #[test]
    fn pause_resume_keeps_start_sequence_while_seek_resets_segment_identity() {
        let mut first = AudioDiagnostics::new(7, 0);
        let now = first.origin;
        first.record_start(start(1, 1), now, now, None);
        first.record_pump(pump(now, Some(1)), clock);
        first.record_start(start(1, 3), now, now, None);
        first.record_pump(pump(now, Some(1)), clock);
        let mut sought = AudioDiagnostics::new(8, 3800000000);
        sought.record_start(start(2, 4), now, now, None);
        let report = first.report();
        assert_eq!(report["underruns"][1]["startSequence"], 2);
        assert_eq!(report["underruns"][1]["firstPump"], true);
        assert_eq!(report["starts"][1]["sourceEpoch"], 1);
        assert_eq!(report["starts"][1]["renderEpoch"], 3);
        assert_eq!(sought.report()["segmentSequence"], 8);
        assert_eq!(sought.report()["targetNs"], "3800000000");
        assert_eq!(sought.report()["starts"][0]["sequence"], 1);
        assert_eq!(sought.report()["starts"][0]["sourceEpoch"], 2);
    }

    #[test]
    fn overflow_keeps_counters_and_declares_incomplete_without_extra_clock_reads() {
        let mut diagnostics = AudioDiagnostics::new(1, 0);
        let now = diagnostics.origin;
        let reads = Cell::new(0);
        for _ in 0..19 {
            diagnostics.record_start(start(1, 1), now, now, None);
            diagnostics.record_pump(pump(now, Some(1)), || {
                reads.set(reads.get() + 1);
                clock()
            });
        }
        let report = diagnostics.report();
        assert_eq!(report["startCount"], 19);
        assert_eq!(report["underrunCount"], 19);
        assert_eq!(report["retainedStarts"], 16);
        assert_eq!(report["retainedUnderruns"], 16);
        assert_eq!(report["droppedStarts"], 3);
        assert_eq!(report["droppedUnderruns"], 3);
        assert_eq!(report["complete"], false);
        assert_eq!(reads.get(), 16);
    }

    #[test]
    fn failed_start_and_unavailable_observations_are_retained_as_errors() {
        let mut diagnostics = AudioDiagnostics::new(1, 0);
        let now = diagnostics.origin;
        let mut observation = start(1, 1);
        observation.pre_start_padding = Err("GetCurrentPadding failed".into());
        diagnostics.record_start(observation, now, now, Some("Start failed"));
        diagnostics.record_pump(pump(now, Some(1)), || ClockObservation {
            sample: None,
            error: Some("GetPosition failed".into()),
        });
        let report = diagnostics.report();
        assert_eq!(report["starts"][0]["succeeded"], false);
        assert_eq!(report["starts"][0]["preStartPaddingFrames"], Value::Null);
        assert_eq!(
            report["starts"][0]["preStartPaddingError"],
            "GetCurrentPadding failed"
        );
        assert_eq!(report["underruns"][0]["startSequence"], Value::Null);
        assert_eq!(
            report["underruns"][0]["clock"]["error"],
            "GetPosition failed"
        );
    }
}
