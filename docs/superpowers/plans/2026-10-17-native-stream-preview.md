# Native streaming preview implementation plan

**Goal:** Extend the approved independent native probe to decode during playback with bounded queues and an explicit preview lifecycle.

**Architecture:** Keep the existing WASAPI owner and asynchronous D3D worker. A streaming decoder owns audited FFmpeg video/audio children and an ffprobe timestamp child; pipes pair exact RGB frames with actual integer PTS and parse PCM WAV incrementally. A small session policy validates preparation, playback, pause, seek epochs, EOF and terminal cancellation.

**Tech stack:** Existing Rust/windows/serde/sha2 dependencies and the unchanged audited LGPL Windows sidecars.

**Spec:** `docs/NATIVE-QUEUE-PROBE.md` approved next step; detailed scope and acceptance in `docs/NATIVE-STREAM-PROBE.md`.

## Constraints

- Independent `native-stream-probe` feature/example; no app backend switch, UI, project schema, MCP or encoder change.
- No visible windows, computer control, recording or endpoint-volume change. Actual PCM uses a uniquely owned muted WASAPI session.
- Audio queue: 8 blocks of at most 480 sample frames. Video queue: 4 RGB24 frames at 160×90. Reader scratch buffers and stderr also bounded; no complete video/PCM or PTS table in playback memory.
- Video uses decoded integer timestamps, never frame-index/fps invented timing. WAV pipes must handle unknown data length and reject partial sample frames.
- Backpressure waits on decoder threads; playback polls without waiting on decoder/GPU work. Failure remains independently observable.
- Seek restarts only owned subprocesses and sequentially skips to the exact source position in this stage; do not claim fast random access or production VFR readiness.
- Close wakes queues, kills/reaps owned children and joins readers. Never terminate unrelated FFmpeg processes.
- Preserve the pre-existing dirty checkout. No blanket commit/reset or unrelated refactor.

## Deliverables and acceptance

1. `support/preview_session.rs`: `NativePreviewSession` with epoch-checked readiness/EOF callbacks. Play requires audio and video readiness; EOF requires source EOF, output drain and video EOF. Seek/cancel reject old callbacks; close/failure cannot resurrect. Verify meaningful state transitions and overflow before integration.
2. `support/stream_process.rs` and `support/streaming_decoder.rs`: bounded streaming transport, actual timestamp pairing, exact sample conversion and ordered cleanup. Test short reads, truncated WAV/RGB, malformed/missing PTS, queue pressure and cancellation; inspect packaged binary paths/hashes before launch.
3. `native_stream_probe.rs` and a focused driver: actual muted WASAPI/D3D run for four-second controlled media, pause/resume, seek to 1.5 seconds and cancel. Compare incrementally submitted PCM and consumed frame hashes against previous independently decoded fixtures without preloading those fixtures into playback.
4. Generate separate 20-second controlled copies under a unique target directory. Confirm playback starts before complete decode, queues remain capped, cancellation reaps children. This is a bounded short-duration stress check, not long-project qualification.
5. Preserve raw reports, source/binary hashes and test results under target; update README, architecture/tasks/contract and the streaming probe document with actual limits.

## Verification

- Run focused parser/queue/state tests before device runs. Include success and failure/cleanup paths.
- Real hardware and decoded source evidence must pass independently of fake-backend tests.
- Run `pnpm lint`, `pnpm test`, `pnpm build`, default `cargo test --locked`, streaming example tests and `cargo fmt --check`.
- Explicitly distinguish native offscreen computation from Present/display/audible synchronization. Existing WebView startup acceptance remains open.

## Execution result

Implemented all five deliverables and verified the independent device path. Final source/frame/PCM/epoch checks and owned-process cleanup evidence are in `src-tauri/target/native-stream-validation/run-5f1da80e-6826-4c4f-b0fa-ef4065e25762/`. The streaming example has 63 passing ordinary tests plus one explicitly executed real-sidecar test; default Rust has 100 passing and six existing export-device tests ignored. Frontend 231 tests, lint/build, Rust format and diff checks pass.

Functional acceptance passed; realtime qualification remains false. The initial run recorded a 154ms audio supply gap against a 100ms endpoint buffer and 2.75s GPU shutdown. Follow-up diagnostics found large variation in requested 2ms sleeps and idle GPU teardown. Retain all three reports and prioritize event-driven audio/driver-lifecycle investigation next; no main-app backend replacement is authorized by these results alone.
