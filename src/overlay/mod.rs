//! Selection overlay over *frozen* screens.
//!
//! Every monitor is captured the moment the overlay is requested; each overlay window shows its
//! monitor's frozen image, so what ends up in the file is exactly what was on screen at the
//! trigger, however long the selection takes (moving content, menus, tooltips…). The region
//! stays adjustable (drag edges/corners, drag inside to move, arrow keys). Whatever is being
//! captured (region, one screen, several, all) can be drawn on until Enter saves it.
//!
//! Latency: the capture runs on its own thread, overlapping gpui start-up when standalone; in
//! the daemon it is the only work done before the windows open.

mod actions;
#[cfg(unix)]
mod gate;
mod keys;
#[cfg(target_os = "linux")]
mod launch;
pub mod layout;
mod recording;
mod tools;

use std::{
    cell::OnceCell,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{
    AnyWindowHandle, App, Bounds, Context, CursorStyle, DispatchPhase, Div, Entity, FocusHandle, FontWeight,
    Hsla, ImageSource, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit,
    PathPromptOptions, Pixels, PlatformDisplay, Point, QuitMode, RenderImage, ScrollDelta, ScrollEvent,
    SharedString, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, canvas, div,
    hsla, img, prelude::*, px, rgba, size,
};
use smallvec::SmallVec;

#[cfg(unix)]
use crate::daemon;
use crate::{
    annotate::{self, Annotations, Color, Grab, Shape, Tool},
    capture::{MonitorInfo, Rect, Shot, Snapshot},
    job,
    save::Config,
    selection::{Handle, Hit, Mode, Request, Selection},
    trace,
};

use layout::Edge;

#[cfg(target_os = "linux")]
pub use launch::with_recording_frame;

enum Phase {
    Selecting,
    /// A recording is starting: the bars slide away, then the overlay closes.
    Leaving,
    /// Windows closed; composing and encoding in the background.
    Saving,
    /// Overlays closed while the folder picker is open.
    Browsing,
    /// Outcome reported; windows are going away.
    Done,
}

/// How long the daemon keeps its screen buffers (~15 MB per screen, ~20 ms off each capture)
/// without a request. Captures come in bursts; between them, the memory goes back.
#[cfg(unix)]
const RELEASE_BUFFERS_AFTER: std::time::Duration = std::time::Duration::from_mins(5);

/// The daemon's one-overlay-at-a-time rule (see [`gate`]).
#[cfg(unix)]
static GATE: gate::Gate = gate::Gate::new();
/// Standalone run result, for the process exit code.
static SAVED: AtomicBool = AtomicBool::new(false);

/// Every screen at the moment of the trigger, as gpui images. The captured BGRA buffers are
/// handed to gpui as they are (no conversion, no copy) and saving cuts from the same memory.
pub struct Frozen {
    screens: Vec<(MonitorInfo, Arc<RenderImage>)>,
}

impl Frozen {
    fn new(snapshot: Snapshot) -> Self {
        let screens = snapshot
            .shots
            .into_iter()
            .map(|shot| {
                let info = shot.info();
                let frame = image::Frame::new(shot.image);
                (info, Arc::new(render_image(frame)))
            })
            .collect();
        Self { screens }
    }
}

/// A gpui image owning `frame`'s pixels. (Not `SmallVec::from_elem`: that clones the frame,
/// a full extra copy of a screen.)
fn render_image(frame: image::Frame) -> RenderImage {
    let mut frames = SmallVec::new();
    frames.push(frame);
    RenderImage::new(frames)
}

/// The frozen screens as a [`Snapshot`] borrowing the display images' pixels.
fn borrowed(screens: &[(MonitorInfo, Arc<RenderImage>)]) -> anyhow::Result<Snapshot<&[u8]>> {
    let shots = screens
        .iter()
        .map(|(m, image)| {
            let size = image.size(0);
            let bytes = image.as_bytes(0).unwrap_or_default();
            let image = image::ImageBuffer::from_raw(size.width.0 as u32, size.height.0 as u32, bytes)
                .ok_or_else(|| anyhow::anyhow!("frozen image of {} has the wrong size", m.name))?;
            Ok(Shot {
                name: m.name.clone(),
                rect: m.rect,
                output_position: m.output_position,
                image,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    Snapshot::new(shots)
}

/// Capture every monitor.
pub fn freeze() -> anyhow::Result<Frozen> {
    let snapshot = Snapshot::take()?;
    trace("screens frozen");
    Ok(Frozen::new(snapshot))
}

pub struct Overlay {
    selection: Selection,
    annotations: Annotations,
    /// The frozen screens, in layout order.
    screens: Arc<[(MonitorInfo, Arc<RenderImage>)]>,
    /// Frozen image per monitor name.
    images: HashMap<String, Arc<RenderImage>>,
    phase: Phase,
    config: Config,
    windows: Vec<AnyWindowHandle>,
    /// Daemon client waiting for the outcome.
    #[cfg(unix)]
    client: Option<daemon::Pending>,
    /// Keep the process (and its warm GPU context) alive afterwards.
    resident: bool,
    /// Drawing colours, parsed from the config the first time the drawing row shows.
    palette: OnceCell<Vec<Color>>,
    /// Hue of the colour picker while it is open.
    picker: Option<f32>,
    /// Touchpad scrolling not yet turned into a width step.
    scroll: f32,
    /// The drawing toolbar row joins one frame after the overlay first shows: building it
    /// (symbol glyphs, ~30 elements) would otherwise make that first frame miss a vblank.
    tools_ready: bool,
    /// Per overlay, for `SCREENCAPTURE_TRACE`.
    traced: std::cell::Cell<bool>,
    /// Why the last action couldn't be done, shown above the action bar.
    notice: Option<SharedString>,
}

/// Open the overlay once `frozen` (started before gpui, to overlap its start-up) is ready, and
/// run until the capture is saved or cancelled. Returns whether a file was saved.
pub fn run(mode: Mode, frozen: std::thread::JoinHandle<anyhow::Result<Frozen>>, config: Config) -> bool {
    gpui_platform::application().run(move |cx: &mut App| {
        trace("gpui ready");
        // Overlays get closed and reopened around the folder picker; don't quit in between.
        cx.set_quit_mode(QuitMode::Explicit);
        match frozen.join().expect("capture thread panicked") {
            Ok(frozen) => start(mode, frozen, config, None, false, cx),
            Err(e) => {
                eprintln!("screencapture: {e:#}");
                cx.quit();
            }
        }
    });
    SAVED.load(Ordering::Relaxed)
}

/// Resident mode: initialise the GPU once, then open an overlay per client request.
/// The end of a recording started from a standalone overlay: like a saved capture, print it.
#[cfg(target_os = "linux")]
fn report_standalone_recording(outcome: anyhow::Result<crate::record::Outcome>) {
    match outcome {
        Ok(crate::record::Outcome::Saved(path)) => {
            SAVED.store(true, Ordering::Relaxed);
            println!("{}", path.display());
        }
        Ok(crate::record::Outcome::Failed(e)) => eprintln!("screencapture: {e}"),
        Ok(other) => eprintln!("screencapture: recording: {other:?}"),
        Err(e) => eprintln!("screencapture: {e:#}"),
    }
}

/// Work the daemon's request thread hands to the UI thread.
#[cfg(unix)]
enum ToUi {
    Overlay(daemon::Pending, anyhow::Result<Frozen>, Config),
    /// Show the frame for this recorded area on these screens, replacing any other; `None`
    /// removes it.
    #[cfg(target_os = "linux")]
    Frame(Option<(Vec<Rect>, Vec<MonitorInfo>)>),
}

/// How often the daemon checks on a running recorder (it may quit by itself).
#[cfg(target_os = "linux")]
const WATCH_RECORDER: std::time::Duration = std::time::Duration::from_millis(250);

#[cfg(unix)]
pub fn run_daemon(listener: std::os::unix::net::UnixListener) {
    use futures::StreamExt;

    gpui_platform::application().run(move |cx: &mut App| {
        cx.set_quit_mode(QuitMode::Explicit);
        warm_up(cx);
        // This process outlives every capture: it can keep the copied image on offer itself.
        crate::clipboard::serve();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        // Requests are served on this thread, and the screens frozen right here, the moment a
        // request arrives: the UI thread is only woken once there is something to show.
        std::thread::spawn(move || {
            // Used by both closures below, never at once.
            let session = std::cell::RefCell::new(crate::capture::Session::default());
            // Connect and fault in the screencopy buffers now, not on the first request.
            if let Err(e) = session.borrow_mut().take() {
                eprintln!("screencapture: warm-up capture failed ({e:#})");
            }
            trace("capture warmed up");
            let quiet = || {
                session.borrow_mut().release_buffers();
                crate::memory::release();
                trace("idle: screen buffers released");
            };
            // `SCREENCAPTURE_RELEASE_AFTER_SECS` overrides the delay.
            let release_after = std::env::var("SCREENCAPTURE_RELEASE_AFTER_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .map_or(RELEASE_BUFFERS_AFTER, std::time::Duration::from_secs);
            // Recordings run beside the overlay's machinery, on this thread.
            #[cfg(target_os = "linux")]
            let recorder = std::cell::RefCell::new(crate::record::Recorder::default());
            #[cfg(target_os = "linux")]
            let send_frame = |recorder: &crate::record::Recorder| {
                // Owned only here, to cross to the UI thread.
                let frame = recorder
                    .frame()
                    .map(|(areas, monitors)| (areas.to_vec(), monitors.to_vec()));
                let _ = tx.unbounded_send(ToUi::Frame(frame));
            };
            // While recording: notice a recorder that quit by itself, and take its frame down.
            #[cfg(target_os = "linux")]
            let watch = || {
                let mut recorder = recorder.borrow_mut();
                recorder.recording()?;
                if let Some(outcome) = recorder.ended_by_itself() {
                    eprintln!("screencapture: the recorder stopped by itself: {outcome:?}");
                    send_frame(&recorder);
                    return None;
                }
                Some(WATCH_RECORDER)
            };
            #[cfg(not(target_os = "linux"))]
            let watch = || None;
            daemon::serve(&listener, Some(release_after), quiet, watch, |mut pending| {
                crate::trace_restart();
                trace("request received");
                #[cfg(target_os = "linux")]
                {
                    let mut recorder = recorder.borrow_mut();
                    if let daemon::Request::Record(command) = pending.request.clone() {
                        // Never record the frozen overlay.
                        if command.starts() && recorder.recording().is_none() && GATE.is_open() {
                            pending.reply(&daemon::Reply::Busy);
                            return;
                        }
                        // A different recording (or none) now: its frame replaces the old one.
                        let before = recorder.recording().map(std::path::Path::to_path_buf);
                        let outcome = recorder.run(command, &mut crate::record::LiveEnv);
                        if recorder.recording() != before.as_deref() {
                            send_frame(&recorder);
                        }
                        let (reply, notes) = outcome.reply();
                        for note in &notes {
                            pending.note(note);
                        }
                        pending.reply(&reply);
                        return;
                    }
                    // Print while recording: say so, and leave the screen alone.
                    if let Some(path) = recorder.recording() {
                        pending.reply(&daemon::Reply::Recording(path.to_path_buf()));
                        return;
                    }
                }
                #[cfg(not(target_os = "linux"))]
                if matches!(pending.request, daemon::Request::Record(_)) {
                    pending.reply(&daemon::Reply::Error("recording needs Linux".into()));
                    return;
                }
                match GATE.enter(std::time::Instant::now()) {
                    gate::Entry::Busy => {
                        pending.reply(&daemon::Reply::Busy);
                        return;
                    }
                    // Pressed again just as the last overlay closed: let the screens repaint
                    // without it first.
                    gate::Entry::Go { wait } if !wait.is_zero() => {
                        trace("waiting for the last overlay to clear");
                        std::thread::sleep(wait);
                    }
                    gate::Entry::Go { .. } => {}
                }
                let frozen = session.borrow_mut().take().map(Frozen::new);
                trace("screens frozen");
                let _ = tx.unbounded_send(ToUi::Overlay(pending, frozen, Config::load()));
            });
        });
        cx.spawn(async move |cx| {
            let mut shown: Option<recording::Shown> = None;
            while let Some(work) = rx.next().await {
                match work {
                    ToUi::Overlay(pending, frozen, config) => {
                        cx.update(|cx| serve_request(pending, frozen, config, cx));
                    }
                    #[cfg(target_os = "linux")]
                    ToUi::Frame(frame) => cx.update(|cx| {
                        let spec = launch::spec_for(frame, cx);
                        launch::replace_frame(&mut shown, spec, cx);
                    }),
                }
            }
        })
        .detach();
    });
}

#[cfg(unix)]
fn serve_request(pending: daemon::Pending, frozen: anyhow::Result<Frozen>, config: Config, cx: &mut App) {
    match frozen {
        Ok(frozen) => {
            let daemon::Request::Overlay(mode) = pending.request else {
                unreachable!("only overlay requests reach the UI thread")
            };
            start(mode, frozen, config, Some(pending), true, cx);
        }
        Err(e) => {
            GATE.abort();
            pending.reply(&daemon::Reply::Error(format!("{e:#}")));
        }
    }
}

/// Create the GPU context ahead of time with an invisible 1x1 surface, so the first real
/// overlay opens as fast as later ones.
#[cfg(unix)]
fn warm_up(cx: &mut App) {
    struct Warmup;
    impl Render for Warmup {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            window.on_next_frame(|window, _| window.remove_window());
            div()
        }
    }
    let one = size(px(1.), px(1.));
    #[cfg(all(target_os = "linux", feature = "wayland"))]
    let kind = {
        use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
        WindowKind::LayerShell(LayerShellOptions {
            namespace: "screencapture-warmup".into(),
            layer: Layer::Background,
            anchor: Anchor::TOP | Anchor::LEFT,
            keyboard_interactivity: KeyboardInteractivity::None,
            ..LayerShellOptions::default()
        })
    };
    #[cfg(not(all(target_os = "linux", feature = "wayland")))]
    let kind = WindowKind::PopUp;
    let options = WindowOptions {
        titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Point::default(), one))),
        kind,
        focus: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..WindowOptions::default()
    };
    if let Err(e) = cx.open_window(options, |_, cx| cx.new(|_| Warmup)) {
        eprintln!("screencapture: warm-up window failed ({e:#}); first overlay will be slower");
    }
    trace("warmed up");
}

fn start(
    mode: Mode,
    frozen: Frozen,
    config: Config,
    #[cfg(unix)] client: Option<daemon::Pending>,
    #[cfg(not(unix))] _client: Option<()>,
    resident: bool,
    cx: &mut App,
) {
    let screens: Arc<[_]> = frozen.screens.into();
    let monitors: Vec<MonitorInfo> = screens.iter().map(|(m, _)| m.clone()).collect();
    let images = screens
        .iter()
        .map(|(m, image)| (m.name.clone(), image.clone()))
        .collect();
    let mut selection = Selection::new(mode, monitors);
    #[cfg(unix)]
    if let Some((x, y)) = crate::hyprland::cursor() {
        selection.motion(x, y);
    }
    let overlay = cx.new(|cx: &mut Context<Overlay>| {
        // However the overlay goes away (the compositor may close every window, e.g. on an
        // output change), the client gets an answer and the daemon takes new requests.
        cx.on_release(Overlay::released).detach();
        Overlay {
            selection,
            annotations: Annotations::default(),
            screens,
            images,
            phase: Phase::Selecting,
            config,
            windows: Vec::new(),
            #[cfg(unix)]
            client,
            resident,
            palette: OnceCell::new(),
            picker: None,
            scroll: 0.,
            tools_ready: false,
            traced: std::cell::Cell::new(false),
            notice: None,
        }
    });
    open_windows(&overlay, cx);

    // Test hook (debug builds): select a region by itself once the overlay is up, optionally
    // drawing a red line across its middle.
    #[cfg(debug_assertions)]
    if let Some(rect) = std::env::var("SCREENCAPTURE_E2E_SELECT")
        .ok()
        .and_then(|g| Rect::parse(&g).ok())
    {
        let line = std::env::var_os("SCREENCAPTURE_E2E_LINE").is_some();
        // Redact the left quarter, write "Hi" on a filled box in the top-right quarter.
        let extras = std::env::var_os("SCREENCAPTURE_E2E_EXTRAS").is_some();
        // Record the region instead of capturing it.
        let record = std::env::var_os("SCREENCAPTURE_E2E_RECORD").is_some();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            let () = overlay.update(cx, |o, cx| {
                if line {
                    let y = rect.y as f32 + rect.h as f32 / 2.;
                    o.annotations.tool = Some(Tool::Line);
                    o.annotations.press((rect.x as f32, y));
                    o.annotations.release((rect.right() as f32, y));
                }
                if extras {
                    let (x, y, w, h) = (rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32);
                    let a = &mut o.annotations;
                    a.tool = Some(Tool::Redact);
                    a.press((x, y));
                    a.release((x + w / 4., y + h));
                    a.tool = Some(Tool::Text);
                    a.fill_on = true;
                    a.press((x + w / 2., y));
                    a.release((x + w, y + h / 2.));
                    a.type_text("Hi");
                    a.stop_editing();
                }
                if record {
                    o.record_target(crate::record::plan::Target::Region(rect), cx);
                } else {
                    o.finish(Some(Request::Region(rect)), cx);
                }
            });
        })
        .detach();
    }
}

fn open_windows(overlay: &Entity<Overlay>, cx: &mut App) {
    let monitors = overlay.read(cx).selection.monitors().to_vec();
    let pointer = overlay.read(cx).selection.hovered().map(|m| m.name.clone());
    let mut displays: Vec<_> = cx
        .displays()
        .into_iter()
        .filter_map(|d| {
            let o = d.bounds().origin;
            let i = layout::match_display((f32::from(o.x) as i32, f32::from(o.y) as i32), &monitors)?;
            Some((d, monitors[i].clone()))
        })
        .collect();
    // The pointer's monitor first: it is where the user looks.
    displays.sort_by_key(|(_, m)| Some(&m.name) != pointer.as_ref());
    let mut displays = displays.into_iter();
    // Open the first synchronously, the rest after yielding to the event loop, so the compositor
    // can configure and show the first surface without waiting for the others (~15 ms each).
    if let Some((display, monitor)) = displays.next() {
        open_window(overlay, &display, monitor, cx);
    }
    let rest: Vec<_> = displays.collect();
    let overlay = overlay.clone();
    cx.spawn(async move |cx| {
        for (display, monitor) in rest {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1))
                .await;
            // Finished or cancelled meanwhile: open nothing more.
            if !cx.update(|cx| overlay.read(cx).selecting()) {
                return;
            }
            cx.update(|cx| open_window(&overlay, &display, monitor, cx));
        }
        trace("overlay windows opened");
        // Only once every display was tried: did any window open at all?
        cx.update(|cx| {
            overlay.update(cx, |o, cx| {
                if o.selecting() && o.windows.is_empty() {
                    o.end(&Err(anyhow::anyhow!("could not open any overlay window")), cx);
                }
            });
        });
    })
    .detach();
}

fn open_window(
    overlay: &Entity<Overlay>,
    display: &Rc<dyn PlatformDisplay>,
    monitor: MonitorInfo,
    cx: &mut App,
) {
    let options = |kind, window_bounds| WindowOptions {
        titlebar: None,
        window_bounds: Some(window_bounds),
        kind,
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        display_id: Some(display.id()),
        window_background: WindowBackgroundAppearance::Opaque,
        app_id: Some("screencapture".into()),
        ..WindowOptions::default()
    };
    let build = |overlay: Entity<Overlay>, monitor: MonitorInfo| {
        move |window: &mut Window, cx: &mut App| cx.new(|cx| View::new(overlay, monitor, window, cx))
    };
    #[cfg(all(target_os = "linux", feature = "wayland"))]
    let mut handle = {
        use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
        let kind = WindowKind::LayerShell(LayerShellOptions {
            namespace: "screencapture".into(),
            layer: Layer::Overlay,
            anchor: Anchor::all(),
            // Cover panels too instead of being pushed aside by their exclusive zones.
            exclusive_zone: Some(px(-1.)),
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            ..LayerShellOptions::default()
        });
        let size = size(px(monitor.rect.w as f32), px(monitor.rect.h as f32));
        let bounds = WindowBounds::Windowed(Bounds::new(Point::default(), size));
        cx.open_window(options(kind, bounds), build(overlay.clone(), monitor.clone()))
            .ok()
    };
    #[cfg(not(all(target_os = "linux", feature = "wayland")))]
    let mut handle = None;
    // X11, other platforms, or a Wayland compositor without layer-shell.
    if handle.is_none() {
        let bounds = WindowBounds::Fullscreen(display.bounds());
        match cx.open_window(
            options(WindowKind::PopUp, bounds),
            build(overlay.clone(), monitor),
        ) {
            Ok(h) => handle = Some(h),
            Err(e) => eprintln!("overlay window: {e:#}"),
        }
    }
    if let Some(handle) = handle {
        overlay.update(cx, |o, _| o.windows.push(handle.into()));
    }
}

fn close_windows(overlay: &Entity<Overlay>, cx: &mut App) {
    let (windows, images): (Vec<_>, Vec<_>) = overlay.update(cx, |o, _| {
        (
            std::mem::take(&mut o.windows),
            o.images.values().cloned().collect(),
        )
    });
    if windows.is_empty() {
        return;
    }
    // Free the frozen screens' textures while the windows (and their atlases) still exist; the
    // daemon would otherwise keep them for good.
    for image in images {
        cx.drop_image(image, None);
    }
    for window in windows {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    }
}

impl Overlay {
    const fn selecting(&self) -> bool {
        matches!(self.phase, Phase::Selecting)
    }

    /// The selection and drawings are on screen (while selecting, and while the bars leave).
    const fn shows_selection(&self) -> bool {
        matches!(self.phase, Phase::Selecting | Phase::Leaving)
    }

    fn finish(&mut self, request: Option<Request>, cx: &mut Context<Self>) {
        let Some(request) = request else {
            cx.notify();
            return;
        };
        // A request that can't produce an image would lose the selection and drawings: keep
        // the overlay up instead.
        if let Err(e) = borrowed(&self.screens).and_then(|snapshot| snapshot.placements(&request)) {
            eprintln!("screencapture: {e:#}");
            cx.notify();
            return;
        }
        trace("selection done");
        self.phase = Phase::Saving;
        let shapes: Vec<Shape> = self.annotations.shapes().cloned().collect();
        let screens = self.screens.clone();
        let dir = self.config.output_dir(None);
        let entity = cx.entity();
        // The pixels are already in memory: the overlay can go right away.
        cx.defer({
            let entity = entity.clone();
            move |cx| close_windows(&entity, cx)
        });
        // A strong handle: once the windows are gone, nothing else keeps the overlay alive.
        cx.spawn(async move |_, cx| {
            let saved = cx
                .background_spawn(async move {
                    let image = job::compose(&borrowed(&screens)?, &request, &shapes)?;
                    trace("composed");
                    let saved = crate::save::save_png(&image, &dir, chrono::Local::now())?;
                    trace("saved");
                    crate::clipboard::copy_png(saved.png);
                    trace("copied");
                    Ok(saved.path)
                })
                .await;
            trace("reported");
            let () = entity.update(cx, |o, cx| o.end(&saved.map(Some), cx));
        })
        .detach();
    }

    /// Report the outcome (`Ok(None)` = cancelled), close the overlay, and quit unless resident.
    fn end(&mut self, outcome: &anyhow::Result<Option<PathBuf>>, cx: &mut Context<Self>) {
        #[cfg(unix)]
        if let Some(client) = self.client.take() {
            client.reply(&match outcome {
                Ok(Some(path)) => daemon::Reply::Saved(path.clone()),
                Ok(None) => daemon::Reply::Cancelled,
                Err(e) => daemon::Reply::Error(format!("{e:#}")),
            });
        }
        if !self.resident {
            match outcome {
                Ok(Some(path)) => {
                    SAVED.store(true, Ordering::Relaxed);
                    println!("{}", path.display());
                }
                Ok(None) => {}
                Err(e) => eprintln!("screencapture: {e:#}"),
            }
        } else if let Err(e) = outcome {
            eprintln!("screencapture: {e:#}");
        }
        self.phase = Phase::Done;
        // The client has its answer and may ask again at once: that request waits until the
        // windows removed just below are off the screens (see `gate::SETTLE`), not refused.
        #[cfg(unix)]
        GATE.leave(std::time::Instant::now());
        let entity = cx.entity();
        let resident = self.resident;
        cx.defer(move |cx| {
            close_windows(&entity, cx);
            if !resident {
                cx.quit();
            }
        });
    }

    /// The overlay is gone. If that happened without [`Self::end`] (all windows closed under us),
    /// still answer the client, free the daemon and, standalone, quit.
    fn released(&mut self, cx: &mut App) {
        if self.resident {
            // Once this overlay's screens and drawings are actually freed, give the memory back.
            cx.defer(|cx| cx.background_spawn(async { crate::memory::release() }).detach());
        }
        if matches!(self.phase, Phase::Done) {
            return;
        }
        #[cfg(unix)]
        if let Some(client) = self.client.take() {
            client.reply(&daemon::Reply::Error("the overlay was closed".into()));
        }
        #[cfg(unix)]
        GATE.leave(std::time::Instant::now());
        if !self.resident {
            eprintln!("screencapture: the overlay was closed");
            cx.quit();
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.selecting() {
            self.end(&Ok(None), cx);
        }
    }

    /// Esc: close the picker, else drop the shape selection, else give up.
    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.picker.take().is_some() || self.annotations.deselect() {
            cx.notify();
        } else {
            self.cancel(cx);
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let request = self.selection.confirm();
        self.finish(request, cx);
    }

    /// A new kind of capture starts with no tool in hand.
    fn put_tool_down(&mut self) {
        self.annotations.stop_editing();
        self.annotations.tool = None;
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.put_tool_down();
        self.selection.select_all();
        cx.notify();
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.notice = None;
        self.put_tool_down();
        self.selection.set_mode(mode);
        cx.notify();
    }

    /// Pick a drawing tool, or put it down if it is already in hand.
    fn toggle_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        self.annotations.stop_editing();
        self.annotations.tool = (self.annotations.tool != Some(tool)).then_some(tool);
        if self.annotations.tool == Some(Tool::Text) {
            crate::text::preload();
        }
        cx.notify();
    }

    fn toggle_fill(&mut self, cx: &mut Context<Self>) {
        self.annotations.toggle_fill();
        cx.notify();
    }

    fn set_target(&mut self, target: annotate::Target, cx: &mut Context<Self>) {
        self.annotations.target = target;
        cx.notify();
    }

    /// Keys while typing in a text box. Returns whether the key was used (always, while
    /// typing: shortcuts must not fire mid-sentence).
    fn text_key(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        use keys::TextEdit;
        if self.annotations.editing().is_none() {
            return false;
        }
        let m = &keystroke.modifiers;
        let mods = keys::Mods {
            control: m.control,
            alt: m.alt,
            platform: m.platform,
        };
        let a = &mut self.annotations;
        match keys::text_edit(&keystroke.key, keystroke.key_char.as_deref(), mods) {
            TextEdit::Done => a.stop_editing(),
            TextEdit::Newline => a.type_text("\n"),
            TextEdit::Backspace => a.backspace(),
            TextEdit::DeleteWord => a.delete_word(),
            TextEdit::Undo => {
                a.stop_editing();
                a.undo();
            }
            TextEdit::Type(text) => a.type_text(&text),
            TextEdit::Swallow => return true,
        }
        cx.notify();
        true
    }

    fn palette(&self) -> &[Color] {
        self.palette.get_or_init(|| self.config.colors())
    }

    fn set_color(&mut self, color: Color, cx: &mut Context<Self>) {
        self.annotations.set_color(color);
        cx.notify();
    }

    fn cycle_color(&mut self, cx: &mut Context<Self>) {
        let palette = self.palette().to_vec();
        let i = palette
            .iter()
            .position(|c| *c == self.annotations.current_color());
        let next = palette[i.map_or(0, |i| (i + 1) % palette.len())];
        self.set_color(next, cx);
    }

    fn toggle_picker(&mut self, cx: &mut Context<Self>) {
        self.picker = if self.picker.is_some() { None } else { Some(0.) };
        cx.notify();
    }

    fn set_picker_hue(&mut self, hue: f32, cx: &mut Context<Self>) {
        self.picker = Some(hue);
        cx.notify();
    }

    /// A colour from the picker: use it, and remember it as the custom swatch.
    fn pick_custom(&mut self, color: Color, cx: &mut Context<Self>) {
        self.picker = None;
        self.config.custom_color = Some(annotate::to_hex(color));
        let config = self.config.clone();
        cx.background_spawn(async move {
            if let Err(e) = config.save() {
                eprintln!("screencapture: could not remember colour: {e:#}");
            }
        })
        .detach();
        self.set_color(color, cx);
    }

    fn step_width(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.annotations.step_width(delta);
        cx.notify();
    }

    /// Mouse wheel: resize the selected shape, or the width of the tool in hand.
    fn scroll(&mut self, lines: f32, cx: &mut Context<Self>) {
        if self.annotations.selected().is_none() && self.annotations.tool.is_none() {
            return;
        }
        self.scroll += lines;
        let steps = self.scroll.trunc();
        if steps != 0. {
            self.scroll -= steps;
            self.step_width(steps, cx);
        }
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        self.annotations.undo();
        cx.notify();
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        self.annotations.delete_selected();
        cx.notify();
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        self.phase = Phase::Browsing;
        let entity = cx.entity();
        let start = self.config.output_dir(None);
        cx.defer(move |cx| {
            close_windows(&entity, cx);
            let picked = cx.prompt_for_paths(PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some(format!("Save screenshots in (now {})", start.display()).into()),
            });
            cx.spawn(async move |cx| {
                let choice = picked
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten()
                    .and_then(|p| p.into_iter().next());
                cx.update(|cx| {
                    entity.update(cx, |o, _| {
                        if let Some(dir) = choice {
                            o.config.output_dir = Some(dir);
                            if let Err(e) = o.config.save() {
                                eprintln!("could not remember folder: {e:#}");
                            }
                        }
                        o.phase = Phase::Selecting;
                    });
                    open_windows(&entity, cx);
                });
            })
            .detach();
        });
    }

    fn output_label(&self) -> SharedString {
        let dir: PathBuf = self.config.output_dir(None);
        let text = dirs::home_dir()
            .and_then(|home| {
                dir.strip_prefix(&home)
                    .ok()
                    .map(|rest| format!("~/{}", rest.display()))
            })
            .unwrap_or_else(|| dir.display().to_string());
        text.into()
    }

    fn pointer(&self) -> Option<annotate::Point> {
        self.selection.pointer().map(|(x, y)| (x as f32, y as f32))
    }

    /// Everything that can change how the pointer looks or what is highlighted.
    fn pointer_state(&self) -> (Option<String>, Option<Rect>, Option<Hit>, Option<Grab>) {
        (
            self.selection.hovered().map(|m| m.name.clone()),
            self.selection.region(),
            self.selection.pointer().map(|(x, y)| self.selection.hit(x, y)),
            self.pointer().and_then(|p| self.annotations.grab_at(p)),
        )
    }

    fn cursor(&self) -> CursorStyle {
        let busy = self.annotations.is_busy();
        match self.pointer().and_then(|p| self.annotations.grab_at(p)) {
            Some(Grab::Handle(_)) => return CursorStyle::Crosshair,
            Some(Grab::Shape(_)) if busy => return CursorStyle::ClosedHand,
            Some(Grab::Shape(_)) => return CursorStyle::OpenHand,
            None if busy || self.annotations.tool.is_some() => return CursorStyle::Crosshair,
            None => {}
        }
        match (self.selection.mode, self.selection.pointer()) {
            (Mode::Region, Some((x, y))) => {
                cursor_for(self.selection.hit(x, y), self.selection.is_dragging())
            }
            _ => CursorStyle::Crosshair,
        }
    }
}

/// What the main toolbar row shows, read from the overlay before building it.
struct Bar {
    mode: Mode,
    /// Sliding away: a recording is starting.
    leaving: bool,
    output: SharedString,
    edge: Edge,
    /// Every screen picked: "All screens" is the active choice.
    all: bool,
    draw: tools::DrawBar,
    /// Show the drawing row (from the second frame on).
    tools: bool,
}

/// One monitor's overlay window.
struct View {
    overlay: Entity<Overlay>,
    monitor: MonitorInfo,
    image: Option<Arc<RenderImage>>,
    focus: FocusHandle,
    /// Text boxes rendered for this window, by what they show.
    texts: HashMap<tools::TextKey, Arc<RenderImage>>,
}

/// Dark rounded strip the toolbar rows are made of.
fn panel() -> Div {
    div()
        .occlude()
        .flex()
        .items_center()
        .gap_1()
        .p_1()
        .rounded_lg()
        .bg(rgba(0x1e1e22f0))
        .border_1()
        .border_color(hsla(0., 0., 1., 0.12))
        .text_sm()
        .text_color(hsla(0., 0., 0.92, 1.))
}

fn separator() -> Div {
    div().w(px(1.)).h(px(20.)).mx_1().bg(hsla(0., 0., 1., 0.15))
}

impl View {
    fn new(
        overlay: Entity<Overlay>,
        monitor: MonitorInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&overlay, |_, _, cx| cx.notify()).detach();
        let image = overlay.read(cx).images.get(&monitor.name).cloned();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            overlay,
            monitor,
            image,
            focus,
            texts: HashMap::new(),
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let (shift, control) = (keystroke.modifiers.shift, keystroke.modifiers.control);
        self.overlay.update(cx, |o, cx| {
            if !o.selecting() || o.text_key(keystroke, cx) {
                return;
            }
            match keystroke.key.as_str() {
                "escape" => o.escape(cx),
                "enter" => o.confirm(cx),
                "z" if control => o.undo(cx),
                "delete" | "backspace" => o.delete_selected(cx),
                "a" => o.select_all(cx),
                "r" => o.set_mode(Mode::Region, cx),
                "s" => o.set_mode(Mode::Screens, cx),
                "o" => o.browse(cx),
                "v" => o.record(shift, cx),
                "c" => o.cycle_color(cx),
                "p" => o.toggle_picker(cx),
                "[" | "-" => o.step_width(-1., cx),
                "]" | "=" | "+" => o.step_width(1., cx),
                key @ ("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8") => {
                    let i = key.parse::<usize>().map_or(0, |n| n - 1);
                    o.toggle_tool(Tool::ALL[i], cx);
                }
                key @ ("left" | "right" | "up" | "down") => {
                    let (dx, dy) = match key {
                        "left" => (-1, 0),
                        "right" => (1, 0),
                        "up" => (0, -1),
                        _ => (0, 1),
                    };
                    if shift {
                        o.selection.resize_by(dx, dy);
                    } else {
                        o.selection.nudge(dx, dy);
                    }
                    cx.notify();
                }
                _ => {}
            }
        });
    }

    fn on_press(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let p = to_global(self.monitor.rect, event.position);
        self.overlay.update(cx, |o, cx| {
            if !o.selecting() {
                return;
            }
            o.picker = None;
            // Shapes and the tool in hand first; otherwise the press is for the selection.
            if !o.annotations.press(p) {
                o.selection.press(p.0.round() as i32, p.1.round() as i32);
            } else if event.click_count >= 2 {
                // Double-click a text box to type in it again.
                o.annotations.start_editing();
            }
            cx.notify();
        });
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            // Touchpads: one step per ~24 px.
            ScrollDelta::Pixels(p) => f32::from(p.y) / 24.,
        };
        self.overlay.update(cx, |o, cx| {
            if o.selecting() {
                o.scroll(lines, cx);
            }
        });
    }

    fn toolbar(&self, bar: &Bar, cx: &Context<Self>) -> impl IntoElement {
        let overlay = self.overlay.clone();
        let act = |f: fn(&mut Overlay, &mut Context<Overlay>)| {
            let overlay = overlay.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| overlay.update(cx, f)
        };
        let main = panel()
            .child(
                tools::button("region", "Region  R", bar.mode == Mode::Region)
                    .on_click(act(|o, cx| o.set_mode(Mode::Region, cx))),
            )
            .child(
                tools::button("screens", "Screens  S", bar.mode == Mode::Screens && !bar.all)
                    .on_click(act(|o, cx| o.set_mode(Mode::Screens, cx))),
            )
            .child(tools::button("all", "All screens  A", bar.all).on_click(act(Overlay::select_all)))
            .child(separator())
            .child(
                tools::button("folder", format!("Save in {}  O", bar.output), false)
                    .on_click(cx.listener(|this, _, _, cx| this.overlay.update(cx, Overlay::browse))),
            )
            .child(tools::button("close", "✕", false).on_click(act(Overlay::cancel)));

        let mut column = div().flex().flex_col().items_center().gap_1();
        let rows = [
            Some(main.into_any_element()),
            bar.tools
                .then(|| tools::drawing_row(&self.overlay, &bar.draw).into_any_element()),
        ];
        let rows = rows.into_iter().flatten();
        // Rows grow away from the screen edge.
        match bar.edge {
            Edge::Top => column = column.children(rows),
            Edge::Bottom => column = column.children(rows.rev()),
        }
        if let Some(hue) = bar.draw.picker.filter(|_| bar.tools) {
            let picker = tools::picker(&self.overlay, hue, bar.draw.color);
            column = match bar.edge {
                Edge::Top => column.child(picker),
                Edge::Bottom => div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(picker)
                    .child(column),
            };
        }
        let place = div()
            .absolute()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(column);
        let offset = match bar.edge {
            Edge::Top => actions::EDGE_GAP,
            Edge::Bottom => actions::EDGE_GAP + actions::ACTION_BAR_ROOM,
        };
        actions::slide(place, bar.edge, offset, bar.leaving, "toolbar")
    }
}

const ACCENT: Hsla = Hsla {
    h: 0.58,
    s: 0.85,
    l: 0.55,
    a: 1.,
};
const DIM: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.,
    a: 0.45,
};
/// Side of the square resize handles, in logical px.
const HANDLE: i32 = 8;

fn rect_div(r: Rect) -> Div {
    div()
        .absolute()
        .left(px(r.x as f32))
        .top(px(r.y as f32))
        .w(px(r.w as f32))
        .h(px(r.h as f32))
}

fn handle_div(x: i32, y: i32) -> Div {
    rect_div(Rect::new(x - HANDLE / 2, y - HANDLE / 2, HANDLE, HANDLE))
        .bg(gpui::white())
        .border_1()
        .border_color(ACCENT)
}

/// Window-local position to global logical desktop coordinates.
fn to_global(monitor: Rect, p: Point<Pixels>) -> annotate::Point {
    (
        monitor.x as f32 + f32::from(p.x),
        monitor.y as f32 + f32::from(p.y),
    )
}

const fn cursor_for(hit: Hit, dragging: bool) -> CursorStyle {
    match hit {
        Hit::Handle(Handle::N | Handle::S) => CursorStyle::ResizeUpDown,
        Hit::Handle(Handle::E | Handle::W) => CursorStyle::ResizeLeftRight,
        Hit::Handle(Handle::NW | Handle::SE) => CursorStyle::ResizeUpLeftDownRight,
        Hit::Handle(Handle::NE | Handle::SW) => CursorStyle::ResizeUpRightDownLeft,
        Hit::Inside if dragging => CursorStyle::ClosedHand,
        Hit::Inside => CursorStyle::OpenHand,
        Hit::Outside => CursorStyle::Crosshair,
    }
}

impl Render for View {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let o = self.overlay.read(cx);
        if !o.tools_ready {
            let first = !o.traced.replace(true);
            if first {
                trace("first overlay frame built");
            }
            let overlay = self.overlay.clone();
            window.on_next_frame(move |_, cx| {
                if first {
                    trace("first overlay frame presented");
                }
                overlay.update(cx, |o, cx| {
                    if !o.tools_ready {
                        o.tools_ready = true;
                        cx.notify();
                    }
                });
            });
        }
        let mut root = div().id("overlay").size_full().relative().bg(gpui::black());
        if let Some(image) = &self.image {
            root = root.child(
                img(ImageSource::Render(image.clone()))
                    .absolute()
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            );
        }
        if !o.shows_selection() {
            return root;
        }

        let monitor = self.monitor.rect;
        let local = |r: Rect| Rect::new(r.x - monitor.x, r.y - monitor.y, r.w, r.h);
        let region = o.selection.region();
        let hovered = o.selection.hovered().map(|m| m.name == self.monitor.name);
        let mode = o.selection.mode;
        let busy = o.selection.is_dragging() || o.annotations.is_busy();

        let mut layer = root
            .cursor(o.cursor())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_press))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| this.overlay.update(cx, Overlay::cancel)),
            );

        // Drawings sit on the frozen image, under the dimming, like the screen content, one
        // element per shape so they stack as in the saved image.
        let scale = window.scale_factor();
        let mut texts = HashMap::new();
        let origin = (monitor.x, monitor.y);
        let box_rect = |s: &Shape| {
            let (l, t, r, b) = s.bounds();
            Rect::new(
                l.round() as i32,
                t.round() as i32,
                (r - l).round() as i32,
                (b - t).round() as i32,
            )
        };
        for shape in o.annotations.shapes() {
            if shape.tool != Tool::Text {
                let shape = shape.clone();
                layer = layer.child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, (), window, _| {
                            tools::paint_shapes(std::slice::from_ref(&shape), origin, bounds, window);
                        },
                    )
                    .absolute()
                    .size_full(),
                );
                continue;
            }
            let rect = box_rect(shape);
            if rect.intersect(monitor).is_none() {
                continue;
            }
            let key = tools::TextKey::new(shape, scale);
            let image = self
                .texts
                .remove(&key)
                .unwrap_or_else(|| texts.get(&key).cloned().unwrap_or_else(|| key.render()));
            texts.insert(key, image.clone());
            layer = layer.child(
                img(ImageSource::Render(image))
                    .absolute()
                    .left(px((rect.x - monitor.x) as f32))
                    .top(px((rect.y - monitor.y) as f32))
                    .w(px(rect.w as f32))
                    .h(px(rect.h as f32))
                    .object_fit(ObjectFit::Fill),
            );
        }
        // Boxes no longer shown: free their textures.
        for (_, stale) in std::mem::replace(&mut self.texts, texts) {
            let _ = window.drop_image(stale);
        }
        // The box being typed in: outline and caret.
        if let Some(shape) = o.annotations.editing() {
            let l = local(box_rect(shape));
            let (x, y, h) = crate::text::caret(&shape.text, shape.width, l.w.max(1) as u32);
            layer = layer.child(rect_div(l).border_1().border_color(ACCENT)).child(
                div()
                    .absolute()
                    .left(px(l.x as f32 + x))
                    .top(px(l.y as f32 + y))
                    .w(px(2.))
                    .h(px(h))
                    .bg(tools::color(shape.color)),
            );
        }

        // Dim everything but the selection.
        let clear = match mode {
            Mode::Region => region,
            Mode::Screens => o.selection.is_picked(&self.monitor.name).then_some(monitor),
        };
        for r in layout::dim_rects(monitor, clear) {
            layer = layer.child(rect_div(r).bg(DIM));
        }

        match mode {
            Mode::Region => {
                if let Some(sel) = region.filter(|r| r.intersect(monitor).is_some()) {
                    let l = local(sel);
                    layer = layer.child(rect_div(l).border_1().border_color(ACCENT));
                    if !busy && o.annotations.tool.is_none() {
                        let (mx, my) = (l.x + l.w / 2, l.y + l.h / 2);
                        for (hx, hy) in [
                            (l.x, l.y),
                            (mx, l.y),
                            (l.right(), l.y),
                            (l.right(), my),
                            (l.right(), l.bottom()),
                            (mx, l.bottom()),
                            (l.x, l.bottom()),
                            (l.x, my),
                        ] {
                            layer = layer.child(handle_div(hx, hy));
                        }
                    }
                    let label_y = if l.bottom() + 28 < monitor.h {
                        l.bottom() + 8
                    } else {
                        (l.y - 30).max(0)
                    };
                    layer = layer.child(
                        div()
                            .absolute()
                            .left(px(l.x.max(0) as f32))
                            .top(px(label_y as f32))
                            .px_2()
                            .rounded_sm()
                            .bg(rgba(0x000000c0))
                            .text_xs()
                            .text_color(gpui::white())
                            .child(format!("{} × {}", sel.w, sel.h)),
                    );
                } else if region.is_none() && hovered == Some(true) {
                    layer = layer.child(
                        rect_div(local(monitor))
                            .border_2()
                            .border_color(hsla(0.58, 0.85, 0.55, 0.5)),
                    );
                }
            }
            Mode::Screens => {
                let picked = o.selection.is_picked(&self.monitor.name);
                let border = if picked {
                    ACCENT
                } else if hovered == Some(true) {
                    hsla(0., 0., 1., 0.5)
                } else {
                    hsla(0., 0., 0., 0.)
                };
                layer = layer.child(
                    rect_div(local(monitor))
                        .border_4()
                        .border_color(border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .px_4()
                                .py_2()
                                .rounded_lg()
                                .bg(rgba(0x000000a0))
                                .text_2xl()
                                .font_weight(FontWeight::BOLD)
                                .text_color(gpui::white())
                                .child(format!("{}{}", if picked { "✓ " } else { "" }, self.monitor.name)),
                        ),
                );
            }
        }

        // The selected shape's handles (a ring around a counter).
        if let Some(shape) = o.annotations.selected().filter(|_| !busy) {
            let to_local =
                |(x, y): annotate::Point| (x.round() as i32 - monitor.x, y.round() as i32 - monitor.y);
            if shape.tool == Tool::Counter {
                let (cx_, cy_) = to_local(shape.points[0]);
                let r = shape.radius().round() as i32 + 4;
                layer = layer.child(
                    rect_div(Rect::new(cx_ - r, cy_ - r, 2 * r, 2 * r))
                        .rounded_full()
                        .border_2()
                        .border_color(ACCENT),
                );
            }
            for h in shape.handles() {
                let (hx, hy) = to_local(h);
                layer = layer.child(handle_div(hx, hy));
            }
        }

        // Toolbar on the monitor under the pointer (all of them until the pointer is seen),
        // hidden while dragging so it never gets in the way.
        if !busy && hovered != Some(false) {
            let ready = match o.selection.confirm() {
                Some(Request::Screens(n)) => n.len(),
                Some(Request::Region(_)) => 1,
                None => 0,
            };
            let leaving = matches!(o.phase, Phase::Leaving);
            let bar = Bar {
                mode,
                leaving,
                output: o.output_label(),
                edge: layout::toolbar_edge(monitor, region),
                all: o.selection.is_all(),
                tools: o.tools_ready,
                draw: tools::DrawBar {
                    tool: o.annotations.tool,
                    color: o.annotations.current_color(),
                    width: o.annotations.current_width(),
                    has_selection: o.annotations.selected().is_some(),
                    palette: o.palette().to_vec(),
                    custom: o.config.custom_color(),
                    picker: o.picker,
                    text: o.annotations.sizing_text(),
                    fill: o.annotations.current_fill(),
                    target: o.annotations.target,
                },
            };
            let actions = actions::ActionBar {
                mode,
                ready,
                settings: o.config.record,
                notice: o.notice.as_ref(),
                leaving,
            };
            layer = layer
                .child(self.toolbar(&bar, cx))
                .child(actions::action_bar(&self.overlay, &actions));
        }

        // Window-wide listeners: during a drag the compositor keeps sending pointer events to
        // the surface where it started, even past its edge, so selections can span monitors.
        let overlay = self.overlay.clone();
        layer.child(
            canvas(
                |_, _, _| (),
                move |_, (), window, _| {
                    let moving = overlay.clone();
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let p = to_global(monitor, e.position);
                        moving.update(cx, |o, cx| {
                            if !o.selecting() {
                                return;
                            }
                            let before = o.pointer_state();
                            if o.annotations.is_busy() {
                                o.annotations.motion(p);
                                cx.notify();
                            }
                            o.selection.motion(p.0.round() as i32, p.1.round() as i32);
                            // Repaint only when something visible changed.
                            if before != o.pointer_state() {
                                cx.notify();
                            }
                        });
                    });
                    window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                        if phase != DispatchPhase::Bubble || e.button != MouseButton::Left {
                            return;
                        }
                        let p = to_global(monitor, e.position);
                        overlay.update(cx, |o, cx| {
                            if !o.selecting() {
                                return;
                            }
                            if o.annotations.is_busy() {
                                o.annotations.release(p);
                            } else {
                                o.selection.release(p.0.round() as i32, p.1.round() as i32);
                            }
                            cx.notify();
                        });
                    });
                },
            )
            .absolute()
            .size_full(),
        )
    }
}
