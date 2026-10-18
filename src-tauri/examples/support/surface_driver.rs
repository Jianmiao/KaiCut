//! Owns a hidden HWND and exercises a real DirectComposition attachment.
use super::{
    composition_presenter::CompositionPresenter,
    frame_verification::{calibration_rgb, verify_readback},
    hidden_surface_host::HiddenSurfaceHost,
};
use kaicut_lib::preview_surface::{SurfaceLifecycle, SurfaceMetrics};
use serde_json::{Value, json};
use std::{fs, io::Write, path::PathBuf, time::Instant};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

fn need(ok: bool, error: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(error.into()) }
}
struct ComGuard;
impl ComGuard {
    fn enter() -> Result<Self, String> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| e.to_string())?;
        Ok(Self)
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
fn metrics(width: f64, height: f64, scale: f64, minimized: bool) -> SurfaceMetrics {
    SurfaceMetrics {
        logical_width: width,
        logical_height: height,
        scale_factor: scale,
        minimized,
    }
}

fn attach(
    host: &HiddenSurfaceHost,
    presenter: &mut CompositionPresenter,
    lifecycle: &mut SurfaceLifecycle,
    requested: SurfaceMetrics,
    events: &mut Vec<Value>,
) -> Result<(), String> {
    let ticket = lifecycle.begin_attach(requested)?;
    need(
        lifecycle.can_submit(ticket.epoch).is_err(),
        "Frames admitted before attachment commit",
    )?;
    let result = presenter.attach_to_window(host.handle()?);
    events.push(json!({"kind":"attach","ticket":ticket,"pending":lifecycle.snapshot(),"backend":result.as_ref().ok(),"error":result.as_ref().err(),"window":host.inspect()?}));
    lifecycle.finish(ticket, result.as_ref().map(|_| ()).map_err(Clone::clone))?;
    result?;
    need(
        presenter.binding_info().attached && presenter.composition_info().composition_attached,
        "Surface did not retain committed attachment",
    )
}

fn layout(
    host: &HiddenSurfaceHost,
    presenter: &mut CompositionPresenter,
    lifecycle: &mut SurfaceLifecycle,
    requested: SurfaceMetrics,
    events: &mut Vec<Value>,
) -> Result<(), String> {
    let old_epoch = lifecycle.epoch();
    let ticket = lifecycle.begin_layout(old_epoch, requested)?;
    need(
        lifecycle.can_submit(old_epoch).is_err() && lifecycle.can_submit(ticket.epoch).is_err(),
        "Layout transition admitted frames",
    )?;
    let validated = ticket.metrics.ok_or("Layout has no metrics")?;
    let extent = validated.physical;
    let result = (|| {
        let resized = if validated.suspended {
            presenter.resize(0, 0)?
        } else {
            presenter.resize(extent.width, extent.height)?
        };
        if !validated.suspended {
            host.resize(extent.width, extent.height)?;
        }
        Ok::<_, String>(resized)
    })();
    let completed = lifecycle.finish(ticket, result.as_ref().map(|_| ()).map_err(Clone::clone));
    events.push(json!({"kind":"layout","oldEpoch":old_epoch,"ticket":ticket,"backend":result.as_ref().ok(),"error":result.as_ref().err(),"confirmed":lifecycle.snapshot(),"window":host.inspect()?}));
    completed?;
    result?;
    need(
        lifecycle.can_submit(old_epoch).is_err(),
        "Old layout frame admitted",
    )?;
    if validated.suspended {
        need(
            lifecycle.can_submit(ticket.epoch).is_err(),
            "Suspended surface admitted frame",
        )?;
        let p = presenter.render_and_present(&[], false)?;
        need(
            !p.present_called && p.hresult.is_none() && p.diagnostic.is_none(),
            "Suspended surface invoked Present",
        )?;
        events.push(json!({"kind":"suspended","epoch":ticket.epoch,"simulatedMinimized":requested.minimized,"presentCalled":p.present_called,"hresult":p.hresult,"disposition":p.disposition,"window":host.inspect()?}));
    }
    Ok(())
}

fn draw(
    host: &HiddenSurfaceHost,
    presenter: &mut CompositionPresenter,
    lifecycle: &SurfaceLifecycle,
    label: &str,
    diagnostic: bool,
    events: &mut Vec<Value>,
) -> Result<(), String> {
    lifecycle.can_submit(lifecycle.epoch())?;
    need(
        presenter.binding_info().attached,
        "Drawing requires committed window attachment",
    )?;
    let extent = lifecycle
        .snapshot()
        .confirmed
        .ok_or("No confirmed surface extent")?
        .physical;
    let rgb = calibration_rgb(extent.width, extent.height)?;
    let p = presenter.render_and_present(&rgb, diagnostic)?;
    need(
        p.present_called && p.hresult.is_some() && p.diagnostic.is_some() == diagnostic,
        "Unexpected Present/readback result",
    )?;
    let mut event = json!({"kind":"present","label":label,"epoch":lifecycle.epoch(),"width":extent.width,"height":extent.height,"diagnosticRequested":diagnostic,"presentCalled":p.present_called,"hresult":p.hresult,"disposition":p.disposition,"uploadDrawMs":p.upload_draw_ms,"presentCallMs":p.present_call_ms,"binding":presenter.binding_info(),"window":host.inspect()?});
    let exact = if let Some(data) = &p.diagnostic {
        let v = verify_readback(&rgb, &data.rgba, extent.width, extent.height)?;
        event["readback"] = json!({"inputRgbSha256":v.input_rgb_sha256,"expectedRgbaSha256":v.expected_rgba_sha256,"actualRgbaSha256":v.actual_rgba_sha256,"exact":v.exact,"mismatchedPixels":v.mismatched_pixels,"alphaErrors":v.alpha_errors,"rowPitch":data.row_pitch,"readbackWaitMs":data.readback_wait_ms});
        v.exact
    } else {
        true
    };
    events.push(event);
    need(exact, "Attached backbuffer differs from calibration source")?;
    host.pump()
}
fn draw_group(
    host: &HiddenSurfaceHost,
    presenter: &mut CompositionPresenter,
    lifecycle: &SurfaceLifecycle,
    label: &str,
    events: &mut Vec<Value>,
) -> Result<(), String> {
    draw(host, presenter, lifecycle, label, true, events)?;
    for i in 0..3 {
        draw(
            host,
            presenter,
            lifecycle,
            &format!("{label}-normal-{i}"),
            false,
            events,
        )?;
    }
    Ok(())
}
fn detach(
    host: &HiddenSurfaceHost,
    presenter: &mut CompositionPresenter,
    lifecycle: &mut SurfaceLifecycle,
    events: &mut Vec<Value>,
) -> Result<(), String> {
    let old = lifecycle.epoch();
    let ticket = lifecycle.begin_detach(old)?;
    let result = presenter.detach_from_window();
    events.push(json!({"kind":"detach","oldEpoch":old,"ticket":ticket,"backend":result.as_ref().ok(),"error":result.as_ref().err(),"binding":presenter.binding_info(),"window":host.inspect()?}));
    lifecycle.finish(ticket, result.as_ref().map(|_| ()).map_err(Clone::clone))?;
    result?;
    need(
        !presenter.binding_info().attached
            && lifecycle.can_submit(old).is_err()
            && lifecycle.can_submit(lifecycle.epoch()).is_err(),
        "Detached surface permits frames",
    )
}
fn exercise(
    host: &HiddenSurfaceHost,
    presenter: &mut CompositionPresenter,
    lifecycle: &mut SurfaceLifecycle,
    events: &mut Vec<Value>,
) -> Result<(), String> {
    let initial = metrics(161.0, 91.0, 1.0, false);
    attach(host, presenter, lifecycle, initial, events)?;
    let binding = presenter.binding_info();
    need(
        presenter.attach_to_window(host.handle()?).is_err() && presenter.binding_info() == binding,
        "Duplicate attachment changed binding",
    )?;
    events.push(
        json!({"kind":"duplicateAttachRejected","binding":binding,"epoch":lifecycle.epoch()}),
    );
    draw_group(host, presenter, lifecycle, "initial", events)?;
    for (scale, label) in [(1.0, "base"), (1.5, "scale-150"), (2.0, "scale-200")] {
        layout(
            host,
            presenter,
            lifecycle,
            metrics(320.0, 180.0, scale, false),
            events,
        )?;
        draw_group(host, presenter, lifecycle, label, events)?;
    }
    layout(
        host,
        presenter,
        lifecycle,
        metrics(320.0, 180.0, 2.0, true),
        events,
    )?;
    layout(
        host,
        presenter,
        lifecycle,
        metrics(320.0, 180.0, 2.0, false),
        events,
    )?;
    draw_group(host, presenter, lifecycle, "restored", events)?;
    layout(
        host,
        presenter,
        lifecycle,
        metrics(0.0, 180.0, 2.0, false),
        events,
    )?;
    layout(
        host,
        presenter,
        lifecycle,
        metrics(960.0, 540.0, 2.0, false),
        events,
    )?;
    draw_group(host, presenter, lifecycle, "1080p", events)?;
    let state = lifecycle.snapshot();
    for invalid in [
        metrics(f64::NAN, 90.0, 1.0, false),
        metrics(1921.0, 1080.0, 1.0, false),
        metrics(320.0, 180.0, 0.0, false),
    ] {
        need(
            lifecycle.begin_layout(state.epoch, invalid).is_err() && lifecycle.snapshot() == state,
            "Invalid layout changed state",
        )?;
    }
    need(
        lifecycle.begin_layout(state.epoch - 1, initial).is_err() && lifecycle.snapshot() == state,
        "Old layout callback changed state",
    )?;
    events.push(json!({"kind":"invalidLayoutRejected","invalidCount":3,"staleEpochRejected":true,"state":state}));
    detach(host, presenter, lifecycle, events)?;
    let again = presenter.detach_from_window()?;
    need(
        !again.was_attached && !again.commit_submitted && again.commit_hresult.is_none(),
        "Repeated detach forged a commit",
    )?;
    events.push(json!({"kind":"repeatDetach","backend":again,"epoch":lifecycle.epoch()}));
    attach(
        host,
        presenter,
        lifecycle,
        metrics(960.0, 540.0, 2.0, false),
        events,
    )?;
    draw_group(host, presenter, lifecycle, "reattached", events)?;
    detach(host, presenter, lifecycle, events)?;
    let closed = lifecycle.close();
    need(
        lifecycle.can_submit(closed.epoch).is_err() && lifecycle.begin_attach(initial).is_err(),
        "Closed surface revived",
    )?;
    events.push(json!({"kind":"closed","state":closed,"binding":presenter.binding_info(),"window":host.inspect()?}));
    Ok(())
}

pub fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    need(
        args.next().as_deref() == Some(std::ffi::OsStr::new("--output-dir")),
        "Expected --output-dir",
    )?;
    let output = PathBuf::from(args.next().ok_or("Missing output directory")?);
    need(args.next().is_none(), "Unexpected arguments")?;
    let lexical = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let target = lexical.canonicalize().map_err(|e| e.to_string())?;
    need(
        output.is_absolute()
            && output.starts_with(&lexical)
            && !output
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir)),
        "Output must stay inside checkout target",
    )?;
    let ancestor = output
        .ancestors()
        .find(|p| p.exists())
        .ok_or("Missing output ancestor")?;
    need(
        ancestor
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&target),
        "Output escaped through link",
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let output = output.canonicalize().map_err(|e| e.to_string())?;
    need(output.starts_with(&target), "Output escaped target")?;
    let run = output.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&run).map_err(|e| e.to_string())?;
    let mut report = json!({"schemaVersion":1,"createdAt":chrono::Utc::now().to_rfc3339(),"processId":std::process::id(),"buildMode":if cfg!(debug_assertions){"debug"}else{"release"},"ok":false,"displayQualified":false,"scope":{"hiddenHost":true,"windowCreated":false,"audioDevice":false,"applicationBackendChanged":false,"osMinimizeTested":false,"osDpiTransitionTested":false,"screenDisplayed":false},"events":[]});
    let result = (|| {
        let _com = ComGuard::enter()?;
        // Reverse local drop order also preserves GPU-before-HWND on errors.
        let mut host = HiddenSurfaceHost::new(161, 91)?;
        report["scope"]["windowCreated"] = true.into();
        report["initialWindow"] = json!(host.inspect()?);
        let mut presenter = CompositionPresenter::new(161, 91)?;
        report["renderer"] = json!(presenter.info());
        let mut lifecycle = SurfaceLifecycle::new();
        let mut events = Vec::new();
        let work = exercise(&host, &mut presenter, &mut lifecycle, &mut events);
        if let Err(error) = &work {
            let _ = lifecycle.fail(lifecycle.epoch(), error.clone());
        }
        report["events"] = json!(events);
        report["finalState"] = json!(lifecycle.close());
        let detached = presenter.detach_from_window();
        report["cleanupDetach"] = json!({"result":detached.as_ref().ok(),"error":detached.as_ref().err(),"binding":presenter.binding_info()});
        let started = Instant::now();
        drop(presenter);
        report["presenterDropMs"] = (started.elapsed().as_secs_f64() * 1000.0).into();
        report["graphicsDroppedBeforeWindowDestroy"] = true.into();
        let closed = host.close();
        report["windowClose"] =
            json!({"result":closed.as_ref().ok(),"error":closed.as_ref().err()});
        work?;
        detached?;
        closed?;
        Ok::<(), String>(())
    })();
    report["ok"] = result.is_ok().into();
    report["functionalPassed"] = result.is_ok().into();
    if let Err(error) = &result {
        report["error"] = error.clone().into();
    }
    let path = run.join("native-surface-validation.json");
    let mut temp = tempfile::NamedTempFile::new_in(&run).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut temp, &report).map_err(|e| e.to_string())?;
    temp.write_all(b"\n")
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    temp.persist_noclobber(&path).map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({"ok":report["ok"],"report":path,"error":report.get("error")})
    );
    result
}
