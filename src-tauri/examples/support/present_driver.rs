//! Unattached composition swap-chain submission evidence; never a display test.
use super::{
    composition_presenter::CompositionPresenter,
    frame_verification::{calibration_rgb, verify_readback},
    presentation_gate::{GatePhase, PresentationGate},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn need(ok: bool, error: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(error.into()) }
}
fn fingerprint(path: &Path) -> Result<Value, String> {
    use sha2::{Digest, Sha256};
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let m = f.metadata().map_err(|e| e.to_string())?;
    need(m.is_file(), "Fixture must be a regular file")?;
    let mut h = Sha256::new();
    let mut b = [0; 65536];
    loop {
        let n = f.read(&mut b).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(
        json!({"path":path,"size":m.len(),"modified":format!("{:?}",m.modified().map_err(|e|e.to_string())?),"sha256":format!("{:x}",h.finalize())}),
    )
}

struct Probe {
    presenter: CompositionPresenter,
    gate: PresentationGate,
    calls: u64,
    readbacks: u64,
    events: Vec<Value>,
}
impl Probe {
    fn new(width: u32, height: u32) -> Result<Self, String> {
        let presenter = CompositionPresenter::new(width, height)?;
        Ok(Self {
            presenter,
            gate: PresentationGate::new(width, height)?,
            calls: 0,
            readbacks: 0,
            events: Vec::new(),
        })
    }
    fn draw(&mut self, rgb: &[u8], label: &str, diagnostic: bool) -> Result<(), String> {
        let epoch = self.gate.epoch();
        self.gate.can_submit(epoch)?;
        let result = self
            .presenter
            .render_and_present(rgb, diagnostic)
            .map_err(|error| {
                let _ = self.gate.fail(error.clone());
                error
            })?;
        need(
            result.present_called,
            "A ready surface did not invoke Present",
        )?;
        self.calls += 1;
        let mut event = json!({"kind":"present","label":label,"epoch":epoch,"hresult":result.hresult,"disposition":result.disposition,
            "presentCalled":result.present_called,"requestedWidth":result.requested_width,"requestedHeight":result.requested_height,
            "uploadDrawMs":result.upload_draw_ms,"presentCallMs":result.present_call_ms,"diagnosticRequested":diagnostic,
            "compositionAttached":false,"screenDisplayed":false});
        match result.diagnostic {
            Some(data) => {
                need(
                    diagnostic,
                    "Unrequested CPU readback in normal present path",
                )?;
                self.readbacks += 1;
                let verification = verify_readback(
                    rgb,
                    &data.rgba,
                    self.presenter.info().width,
                    self.presenter.info().height,
                )?;
                event["readback"] = json!({"rowPitch":data.row_pitch,"readbackWaitMs":data.readback_wait_ms,
                    "inputRgbSha256":verification.input_rgb_sha256,"expectedRgbaSha256":verification.expected_rgba_sha256,"actualRgbaSha256":verification.actual_rgba_sha256,
                    "exact":verification.exact,"mismatchedPixels":verification.mismatched_pixels,"alphaErrors":verification.alpha_errors});
                self.events.push(event);
                need(
                    verification.exact,
                    "Swap-chain backbuffer pixels differ from source",
                )?;
            }
            None => {
                self.events.push(event);
                need(!diagnostic, "Requested diagnostic readback was omitted")?;
            }
        }
        Ok(())
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        let old = self.gate.epoch();
        let ticket = self.gate.begin_resize(width, height)?;
        need(
            self.gate.can_submit(old).is_err() && self.gate.can_submit(ticket.epoch).is_err(),
            "Frame accepted during resize",
        )?;
        let result = self.presenter.resize(width, height);
        self.events.push(json!({"kind":"resize","oldEpoch":old,"ticket":ticket,"backend":result.as_ref().ok(),"error":result.as_ref().err()}));
        match result {
            Ok(_) => self.gate.finish_resize(ticket, Ok(())),
            Err(error) => self.gate.finish_resize(ticket, Err(error)),
        }?;
        need(
            self.gate.can_submit(old).is_err(),
            "Old frame accepted after resize",
        )?;
        Ok(())
    }
    fn exercise(&mut self, fixture: &Path, report: &mut Value) -> Result<(), String> {
        report["initialSurface"] = json!(self.presenter.composition_info());
        let calibration = calibration_rgb(161, 91)?;
        self.draw(&calibration, "asymmetric-161x91", true)?;
        // Preserve stale-frame rejection before the backend boundary, never by
        // pretending the GPU can undo already submitted commands.
        let old = self.gate.epoch();
        let new = self.gate.advance_epoch()?;
        let before = self.calls;
        need(
            self.gate.can_submit(old).is_err(),
            "Stale frame was accepted after epoch change",
        )?;
        self.events.push(json!({"kind":"epoch","oldEpoch":old,"newEpoch":new,"oldRejected":true,"presentCallsBefore":before,"presentCallsAfter":self.calls}));
        self.draw(&calibration, "new-epoch-calibration", false)?;
        self.resize(160, 90)?;
        let original = fingerprint(fixture)?;
        let mut f = fs::File::open(fixture).map_err(|e| e.to_string())?;
        let stride = 160 * 90 * 3;
        need(
            original["size"] == 120 * stride,
            "Expected bounded four-second decoded RGB fixture",
        )?;
        let mut frame = vec![0; stride];
        for index in 0..30 {
            f.read_exact(&mut frame).map_err(|e| e.to_string())?;
            self.draw(
                &frame,
                &format!("decoded-frame-{index}"),
                index == 0 || index == 29,
            )?;
            // Slow diagnostics do not assert a display refresh or frame rate.
            thread::sleep(Duration::from_millis(2));
        }
        need(
            fingerprint(fixture)? == original,
            "Decoded source fixture changed",
        )?;
        report["decodedSource"] = original;
        self.resize(0, 0)?;
        let suspended = self.gate.snapshot();
        let before = self.calls;
        need(
            suspended.phase == GatePhase::Suspended
                && self.gate.can_submit(suspended.epoch).is_err(),
            "Zero-area surface allowed present",
        )?;
        let backend_suspended = self.presenter.render_and_present(&[], false)?;
        need(
            !backend_suspended.present_called
                && backend_suspended.hresult.is_none()
                && backend_suspended.diagnostic.is_none(),
            "Suspended backend called Present or forged success",
        )?;
        self.events.push(json!({"kind":"suspended","gate":suspended,"backendDisposition":backend_suspended.disposition,"presentCalled":backend_suspended.present_called,"hresult":backend_suspended.hresult,"presentCallsBefore":before,"presentCallsAfter":self.calls}));
        self.resize(320, 180)?;
        self.draw(&calibration_rgb(320, 180)?, "restored-320x180", true)?;
        self.resize(1920, 1080)?;
        let hd = calibration_rgb(1920, 1080)?;
        self.draw(&hd, "calibration-1080p", true)?;
        for index in 0..12 {
            self.draw(&hd, &format!("normal-1080p-{index}"), false)?;
        }
        self.resize(161, 91)?;
        self.draw(&calibration, "resized-back-161x91", true)?;
        let state = self.gate.snapshot();
        need(
            self.gate.begin_resize(1921, 1080).is_err() && self.gate.snapshot() == state,
            "Invalid resize changed gate state",
        )?;
        self.events
            .push(json!({"kind":"invalidExtentRejected","gate":state}));
        let closed = self.gate.close();
        need(
            self.gate.can_submit(closed.epoch).is_err() && self.gate.advance_epoch().is_err(),
            "Closed presentation gate revived",
        )?;
        self.events.push(json!({"kind":"closed","gate":closed}));
        report["finalSurface"] = json!(self.presenter.composition_info());
        Ok(())
    }
}

pub fn run() -> Result<(), String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut output = None;
    let mut fixture = manifest.join(
        "target/native-d3d-validation/run-ec927ae2-fcfe-4e7c-8ce0-475746a10284/decoded-video.rgb",
    );
    let mut args = std::env::args_os().skip(1);
    while let Some(key) = args.next() {
        let value = PathBuf::from(args.next().ok_or("Missing option value")?);
        match key.to_str() {
            Some("--output-dir") => output = Some(value),
            Some("--fixture") => fixture = value,
            _ => return Err("Expected --output-dir and optional --fixture".into()),
        }
    }
    let output = output.ok_or("Missing --output-dir")?;
    let lexical = manifest.join("target");
    let target = lexical.canonicalize().map_err(|e| e.to_string())?;
    need(
        output.is_absolute()
            && output.starts_with(&lexical)
            && !output
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir)),
        "Output must stay in checkout target",
    )?;
    let parent = output
        .ancestors()
        .find(|p| p.exists())
        .ok_or("Missing output ancestor")?;
    need(
        parent
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&target),
        "Output escaped via link",
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let output = output.canonicalize().map_err(|e| e.to_string())?;
    need(output.starts_with(&target), "Output escaped target")?;
    let run = output.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run).map_err(|e| e.to_string())?;
    let mut report = json!({"schemaVersion":1,"ok":false,"createdAt":chrono::Utc::now().to_rfc3339(),"buildMode":if cfg!(debug_assertions){"debug"}else{"release"},
        "scope":{"compositionAttached":false,"windowCreated":false,"visibleWindow":false,"screenDisplayed":false,"audioDevice":false,"applicationBackendChanged":false,
            "presentFlags":"DO_NOT_WAIT","syncInterval":0,"normalFramesReadBack":false,"diagnostics":"Explicit selected backbuffer readbacks occur before Present; raw status is not a display timestamp."}});
    let result = (|| {
        let started = Instant::now();
        let mut probe = Probe::new(161, 91)?;
        report["creationMs"] = (started.elapsed().as_secs_f64() * 1000.0).into();
        let result = probe.exercise(&fixture, &mut report);
        if let Err(error) = &result {
            let _ = probe.gate.fail(error.clone());
            report["failedGate"] = json!(probe.gate.snapshot());
        }
        report["events"] = json!(probe.events);
        report["presentCalls"] = probe.calls.into();
        report["readbackCalls"] = probe.readbacks.into();
        let drop_started = Instant::now();
        drop(probe);
        report["backendDropMs"] = (drop_started.elapsed().as_secs_f64() * 1000.0).into();
        report["resourcesDropped"] = true.into();
        result
    })();
    report["ok"] = result.is_ok().into();
    report["functionalPassed"] = result.is_ok().into();
    report["displayQualified"] = false.into();
    if let Err(error) = &result {
        report["error"] = error.clone().into();
    }
    let path = run.join("native-present-validation.json");
    let mut temp = tempfile::NamedTempFile::new_in(&run).map_err(|e| e.to_string())?;
    let written = serde_json::to_writer_pretty(&mut temp, &report)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            temp.write_all(b"\n")
                .and_then(|_| temp.as_file().sync_all())
                .map_err(|e| e.to_string())
        });
    if let Err(error) = written {
        let kept = temp.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{error}; retained {kept:?}"));
    }
    if let Err(error) = temp.persist_noclobber(&path) {
        let why = error.error.to_string();
        let kept = error.file.keep().map(|(_, p)| p);
        eprintln!("{report}");
        return Err(format!("{why}; retained {kept:?}"));
    }
    println!(
        "{}",
        json!({"ok":report["ok"],"report":path,"presentCalls":report.get("presentCalls"),"readbackCalls":report.get("readbackCalls"),"error":report.get("error")})
    );
    result
}
