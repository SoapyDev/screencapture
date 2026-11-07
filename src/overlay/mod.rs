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
mod capturing;
#[cfg(all(target_os = "linux", feature = "wayland"))]
mod controls;
#[cfg(all(target_os = "linux", feature = "wayland"))]
mod drawer;
mod drawn;
mod editing;
pub mod feature;
mod folder;
#[cfg(unix)]
mod gate;
mod keys;
#[cfg(target_os = "linux")]
mod launch;
pub mod layout;
mod recording;
#[cfg(target_os = "linux")]
mod recordings;
mod tools;

use std::{
    any::Any,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Context, CursorStyle, DispatchPhase, Div, Entity, FocusHandle,
    FontWeight, Hsla, ImageSource, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, PathPromptOptions, Pixels, PlatformDisplay, Point, QuitMode, RenderImage,
    ScrollDelta, ScrollEvent, SharedString, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, canvas, div, hsla, img, prelude::*, px, rgba, size,
};
use smallvec::SmallVec;

#[cfg(unix)]
use crate::daemon;
use crate::{
    capture::{MonitorInfo, Rect, Shot, Snapshot},
    config::Config,
    drawings::Shared,
    selection::{Handle, Hit, Mode, Request, Selection},
    trace,
};

use feature::{Act, Ctx, Effect, Feature, HandOver, Picked, SaveJob, Start};
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
    /// The frozen screens, in layout order.
    screens: Arc<[(MonitorInfo, Arc<RenderImage>)]>,
    /// Frozen image per monitor name.
    images: HashMap<String, Arc<RenderImage>>,
    phase: Phase,
    windows: Vec<AnyWindowHandle>,
    /// Daemon client waiting for the outcome.
    #[cfg(unix)]
    client: Option<daemon::Client>,
    /// Keep the process (and its warm GPU context) alive afterwards.
    resident: bool,
    /// What the features add, in the order they are asked.
    features: Vec<Box<dyn Feature>>,
    /// The features' toolbar rows join one frame after the overlay first shows: building them
    /// (symbol glyphs, ~30 elements) would otherwise make that first frame miss a vblank.
    tools_ready: bool,
    /// Per overlay, for `SCREENCAPTURE_TRACE`.
    traced: std::cell::Cell<bool>,
    /// Why the last action couldn't be done, shown above the action bar.
    notice: Option<SharedString>,
}

/// The features built into this program, with their remembered settings, in the order they are
/// asked (and their actions shown).
pub fn features() -> Vec<Box<dyn Feature>> {
    let config = Config::load();
    vec![
        Box::new(editing::Editing::new(config.section())),
        #[cfg(target_os = "linux")]
        Box::new(recordings::Recordings::new(config.section())),
        Box::new(capturing::Capturing::new(config.section())),
    ]
}

/// Open the overlay once `frozen` (started before gpui, to overlap its start-up) is ready, and
/// run until the capture is saved or cancelled. Returns whether a file was saved.
pub fn run(mode: Mode, frozen: std::thread::JoinHandle<anyhow::Result<Frozen>>) -> bool {
    gpui_platform::application().run(move |cx: &mut App| {
        trace("gpui ready");
        // Overlays get closed and reopened around the folder picker; don't quit in between.
        cx.set_quit_mode(QuitMode::Explicit);
        match frozen.join().expect("capture thread panicked") {
            Ok(frozen) => start(mode, frozen, None, false, cx),
            Err(e) => {
                eprintln!("screencapture: {e:#}");
                cx.quit();
            }
        }
    });
    SAVED.load(Ordering::Relaxed)
}

/// Run the recording `command` that `client` sent. The frame changes at once; the answer may
/// wait for the file to be finished (joining screens is slow), on its own thread, so other
/// requests don't.
#[cfg(target_os = "linux")]
fn record_for(
    client: daemon::Client,
    command: crate::record::command::Command,
    recorder: &mut crate::record::Recorder,
    send_frame: &impl Fn(&crate::record::Recorder),
) {
    // Never record the frozen overlay.
    if command.starts() && recorder.recording().is_none() && GATE.is_open() {
        client.reply(&daemon::Reply::Busy);
        return;
    }
    let before = recorder.recording().map(std::path::Path::to_path_buf);
    let outcome = recorder.run(command, &mut crate::record::LiveEnv);
    // A different recording (or none) now: its frame replaces the old one.
    if recorder.recording() != before.as_deref() {
        send_frame(recorder);
    }
    if !matches!(outcome, crate::record::Outcome::Finishing(_)) {
        outcome.answer(client);
        return;
    }
    std::thread::spawn(move || outcome.answer(client));
}

/// Saves are made at once (~20 µs), so they land in the order the user made them; one that
/// failed is reported, naming `what` changed.
fn report_unremembered(saved: anyhow::Result<()>, what: &str) {
    if let Err(e) = saved {
        eprintln!("screencapture: could not remember {what}: {e:#}");
    }
}

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
    Overlay(daemon::Client, Mode, anyhow::Result<Frozen>),
    /// Show the frame for this recorded area on these screens, replacing any other; `None`
    /// removes it.
    #[cfg(target_os = "linux")]
    Frame(Option<crate::record::OnScreen>),
}

/// How often the daemon checks on a running recorder (it may quit by itself).
#[cfg(target_os = "linux")]
const WATCH_RECORDER: std::time::Duration = std::time::Duration::from_millis(250);

/// Resident mode: initialise the GPU once, then open an overlay per client request.
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
                let _ = tx.unbounded_send(ToUi::Frame(recorder.frame().cloned()));
            };
            // While recording: notice a recorder that quit by itself, and take its frame down.
            #[cfg(target_os = "linux")]
            let watch = || {
                let mut recorder = recorder.borrow_mut();
                recorder.recording()?;
                if let Some(outcome) = recorder.ended_by_itself() {
                    send_frame(&recorder);
                    std::thread::spawn(move || {
                        let outcome = outcome.finished();
                        eprintln!("screencapture: the recorder stopped by itself: {outcome:?}");
                    });
                    return None;
                }
                Some(WATCH_RECORDER)
            };
            #[cfg(not(target_os = "linux"))]
            let watch = || None;
            daemon::serve(
                &listener,
                Some(release_after),
                quiet,
                watch,
                |pending: crate::record::Pending| {
                    crate::trace_restart();
                    trace("request received");
                    let daemon::Pending { request, client } = pending;
                    let mode = match request {
                        daemon::Request::Overlay(mode) => mode,
                        #[cfg(target_os = "linux")]
                        daemon::Request::Feature(command) => {
                            record_for(client, command, &mut recorder.borrow_mut(), &send_frame);
                            return;
                        }
                        #[cfg(not(target_os = "linux"))]
                        daemon::Request::Feature(_) => {
                            client.reply(&daemon::Reply::Error("recording needs Linux".into()));
                            return;
                        }
                    };
                    // Print while recording: say so, and leave the screen alone.
                    #[cfg(target_os = "linux")]
                    if let Some(path) = recorder.borrow().recording() {
                        client.reply(&daemon::Reply::Recording(path.to_path_buf()));
                        return;
                    }
                    match GATE.enter(std::time::Instant::now()) {
                        gate::Entry::Busy => {
                            client.reply(&daemon::Reply::Busy);
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
                    let _ = tx.unbounded_send(ToUi::Overlay(client, mode, frozen));
                },
            );
        });
        cx.spawn(async move |cx| {
            let mut shown: Option<recording::Shown> = None;
            while let Some(work) = rx.next().await {
                match work {
                    ToUi::Overlay(client, mode, frozen) => {
                        cx.update(|cx| serve_request(client, mode, frozen, cx));
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
fn serve_request(client: daemon::Client, mode: Mode, frozen: anyhow::Result<Frozen>, cx: &mut App) {
    match frozen {
        Ok(frozen) => start(mode, frozen, Some(client), true, cx),
        Err(e) => {
            GATE.abort();
            client.reply(&daemon::Reply::Error(format!("{e:#}")));
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
    #[cfg(unix)] client: Option<daemon::Client>,
    #[cfg(not(unix))] _client: Option<()>,
    resident: bool,
    cx: &mut App,
) {
    let screens: Arc<[_]> = frozen.screens.into();
    let images = screens
        .iter()
        .map(|(m, image)| (m.name.clone(), image.clone()))
        .collect();
    let selection = selection_at_pointer(mode, &screens);
    let overlay = cx.new(|cx: &mut Context<Overlay>| {
        // However the overlay goes away (the compositor may close every window, e.g. on an
        // output change), the client gets an answer and the daemon takes new requests.
        cx.on_release(Overlay::released).detach();
        Overlay {
            selection,
            screens,
            images,
            phase: Phase::Selecting,
            windows: Vec::new(),
            #[cfg(unix)]
            client,
            resident,
            features: features(),
            tools_ready: false,
            traced: std::cell::Cell::new(false),
            notice: None,
        }
    });
    open_windows(&overlay, cx);
    #[cfg(debug_assertions)]
    run_test_hooks(&overlay, cx);
}

/// A selection over `screens`, told where the pointer already is.
fn selection_at_pointer(mode: Mode, screens: &[(MonitorInfo, Arc<RenderImage>)]) -> Selection {
    let monitors: Vec<MonitorInfo> = screens.iter().map(|(m, _)| m.clone()).collect();
    #[cfg_attr(
        not(unix),
        expect(unused_mut, reason = "only Hyprland says where the pointer is")
    )]
    let mut selection = Selection::new(mode, monitors);
    #[cfg(unix)]
    if let Some((x, y)) = crate::hyprland::cursor() {
        selection.motion(x, y);
    }
    selection
}

/// Test hook (debug builds): with `SCREENCAPTURE_E2E_SELECT`, once the overlay is up, let every
/// feature act on that region (draw, record, save) as the end-to-end tests ask.
#[cfg(debug_assertions)]
fn run_test_hooks(overlay: &Entity<Overlay>, cx: &App) {
    let Some(region) = std::env::var("SCREENCAPTURE_E2E_SELECT")
        .ok()
        .and_then(|g| Rect::parse(&g).ok())
    else {
        return;
    };
    let overlay = overlay.clone();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(500))
            .await;
        overlay.update(cx, |o, cx| {
            for index in 0..o.features.len() {
                if let Some(effect) = o.features[index].test_hook(region) {
                    o.apply(index, effect, cx);
                }
            }
            cx.notify();
        });
    })
    .detach();
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

    /// Run `act` on feature `index` (an `F`) and apply its effect, while selecting.
    fn act_on<F: Feature>(
        &mut self,
        index: usize,
        act: &impl Fn(&mut F, &Ctx) -> Effect,
        cx: &mut Context<Self>,
    ) {
        if !self.selecting() {
            return;
        }
        let ctx = Ctx {
            selection: &self.selection,
        };
        let feature: &mut dyn Any = &mut *self.features[index];
        let Some(feature) = feature.downcast_mut::<F>() else {
            return;
        };
        let effect = act(feature, &ctx);
        self.apply(index, effect, cx);
    }

    /// Do what feature `index` asked.
    fn apply(&mut self, index: usize, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::Repaint => cx.notify(),
            Effect::Save(request, job) => self.save(request, job, cx),
            Effect::HandOver(start) => self.hand_over(start, cx),
            Effect::Browse { prompt, picked } => self.browse(index, prompt, picked, cx),
            Effect::Notice(text) => {
                self.notice = Some(text);
                cx.notify();
            }
        }
    }

    /// What was drawn over the screens, if a feature draws.
    fn drawings(&self) -> Option<Shared> {
        self.features.iter().find_map(|feature| feature.drawings())
    }

    /// Dragging the selection, or something of a feature's.
    fn busy(&self) -> bool {
        self.selection.is_dragging() || self.features.iter().any(|feature| feature.busy())
    }

    fn holds_a_tool(&self) -> bool {
        self.features.iter().any(|feature| feature.holds_a_tool())
    }

    /// Close the overlay and save what `job` makes of `request`, off the UI thread.
    fn save(&mut self, request: Request, job: SaveJob, cx: &mut Context<Self>) {
        // E.g. Capture clicked while the bars slide away for a recording.
        if !self.selecting() {
            return;
        }
        // A request that can't produce an image would lose the selection and drawings: keep
        // the overlay up instead.
        if let Err(e) = borrowed(&self.screens).and_then(|snapshot| snapshot.placements(&request)) {
            eprintln!("screencapture: {e:#}");
            cx.notify();
            return;
        }
        trace("selection done");
        self.phase = Phase::Saving;
        let (drawings, screens) = (self.drawings(), self.screens.clone());
        // The pixels are already in memory: the overlay can go right away.
        Self::close(cx);
        // A strong handle: once the windows are gone, nothing else keeps the overlay alive.
        let entity = cx.entity();
        cx.spawn(async move |_, cx| {
            let made = async move { job(&borrowed(&screens)?, &request, drawings.as_deref()) };
            let saved = cx.background_spawn(made).await;
            trace("reported");
            entity.update(cx, |o, cx| o.end(&saved.map(Some), cx));
        })
        .detach();
    }

    /// Remove the windows once this update is over.
    fn close(cx: &mut Context<Self>) {
        let entity = cx.entity();
        cx.defer(move |cx| close_windows(&entity, cx));
    }

    /// Slide the bars away, then close the overlay and let `start` take over.
    fn hand_over(&mut self, start: Start, cx: &mut Context<Self>) {
        if !self.selecting() {
            return;
        }
        self.phase = Phase::Leaving;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(actions::SLIDE).await;
            let _ = this.update(cx, |o, cx| o.take_over(start, cx));
        })
        .detach();
    }

    fn take_over(&mut self, start: Start, cx: &mut Context<Self>) {
        // Something else ended the overlay while the bars slid away.
        if !matches!(self.phase, Phase::Leaving) {
            return;
        }
        let hand = HandOver {
            #[cfg(unix)]
            client: self.client.take(),
            resident: self.resident,
            drawings: self.drawings(),
        };
        self.phase = Phase::Done;
        #[cfg(unix)]
        GATE.leave(std::time::Instant::now());
        Self::close(cx);
        start(hand, cx);
    }

    /// Report the outcome (`Ok(None)` = cancelled), close the overlay, and quit unless resident.
    fn end(&mut self, outcome: &anyhow::Result<Option<PathBuf>>, cx: &mut Context<Self>) {
        #[cfg(unix)]
        if let Some(client) = self.client.take() {
            client.reply(&reply_for(outcome));
        }
        self.report(outcome);
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

    /// Standalone, the saved path goes to stdout; errors always go to stderr.
    fn report(&self, outcome: &anyhow::Result<Option<PathBuf>>) {
        match outcome {
            Ok(Some(path)) if !self.resident => {
                SAVED.store(true, Ordering::Relaxed);
                println!("{}", path.display());
            }
            Ok(_) => {}
            Err(e) => eprintln!("screencapture: {e:#}"),
        }
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

    /// Esc: a feature closes something of its own first, else give up.
    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.features.iter_mut().any(|feature| feature.escape()) {
            cx.notify();
        } else {
            self.cancel(cx);
        }
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.mode_changed();
        self.selection.select_all();
        cx.notify();
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode_changed();
        self.selection.set_mode(mode);
        cx.notify();
    }

    fn mode_changed(&mut self) {
        self.notice = None;
        for feature in &mut self.features {
            feature.mode_changed();
        }
    }

    /// Step aside for a folder picker asking `prompt`; feature `index` gets the folder picked.
    fn browse(&mut self, index: usize, prompt: String, picked: Picked, cx: &mut Context<Self>) {
        if !self.selecting() {
            return;
        }
        self.phase = Phase::Browsing;
        let entity = cx.entity();
        cx.defer(move |cx| {
            close_windows(&entity, cx);
            ask_for_folder(
                entity,
                prompt,
                Box::new(move |o, dir| o.browsed(index, dir, picked)),
                cx,
            );
        });
    }

    /// Back from the folder picker: hand `dir` to feature `index`, if one was picked.
    fn browsed(&mut self, index: usize, dir: Option<PathBuf>, picked: Picked) {
        self.phase = Phase::Selecting;
        let Some(dir) = dir else { return };
        let feature: &mut dyn Any = &mut *self.features[index];
        picked(feature, dir);
    }

    /// A key while selecting: the features first (a text box takes every key), then the
    /// overlay's own.
    fn key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        if !self.selecting() {
            return;
        }
        if let Some((index, effect)) = self.feature_key(keystroke) {
            self.apply(index, effect, cx);
            return;
        }
        self.own_key(keystroke, cx);
    }

    fn feature_key(&mut self, keystroke: &Keystroke) -> Option<(usize, Effect)> {
        let ctx = Ctx {
            selection: &self.selection,
        };
        let mut features = self.features.iter_mut().enumerate();
        features.find_map(|(index, feature)| Some((index, feature.key(keystroke, &ctx)?)))
    }

    fn own_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        match keystroke.key.as_str() {
            "escape" => self.escape(cx),
            "a" => self.select_all(cx),
            "r" => self.set_mode(Mode::Region, cx),
            "s" => self.set_mode(Mode::Screens, cx),
            key => {
                let Some((dx, dy)) = arrow(key) else { return };
                if keystroke.modifiers.shift {
                    self.selection.resize_by(dx, dy);
                } else {
                    self.selection.nudge(dx, dy);
                }
                cx.notify();
            }
        }
    }

    /// A left press: a feature's first (a shape, a tool in hand), else the selection's.
    fn press(&mut self, point: feature::Point, clicks: usize, cx: &mut Context<Self>) {
        if !self.selecting() {
            return;
        }
        if !self
            .features
            .iter_mut()
            .any(|feature| feature.press(point, clicks))
        {
            let (x, y) = rounded(point);
            self.selection.press(x, y);
        }
        cx.notify();
    }

    /// The pointer moved: repaint only when something visible changed.
    fn motion(&mut self, point: feature::Point, cx: &mut Context<Self>) {
        if !self.selecting() {
            return;
        }
        let before = self.selection_state();
        let changed = self
            .features
            .iter_mut()
            .fold(false, |changed, feature| feature.motion(point) | changed);
        let (x, y) = rounded(point);
        self.selection.motion(x, y);
        if changed || before != self.selection_state() {
            cx.notify();
        }
    }

    fn release(&mut self, point: feature::Point, cx: &mut Context<Self>) {
        if !self.selecting() {
            return;
        }
        if !self.features.iter_mut().any(|feature| feature.release(point)) {
            let (x, y) = rounded(point);
            self.selection.release(x, y);
        }
        cx.notify();
    }

    fn scroll(&mut self, lines: f32, cx: &mut Context<Self>) {
        if self.selecting() && self.features.iter_mut().any(|feature| feature.scroll(lines)) {
            cx.notify();
        }
    }

    /// Everything of the selection that changes how the pointer looks or what is highlighted.
    fn selection_state(&self) -> (Option<String>, Option<Rect>, Option<Hit>) {
        let selection = &self.selection;
        (
            selection.hovered().map(|m| m.name.clone()),
            selection.region(),
            selection.pointer().map(|(x, y)| selection.hit(x, y)),
        )
    }

    fn cursor(&self) -> CursorStyle {
        let pointer = self.selection.pointer();
        let point = pointer.map(|(x, y)| (x as f32, y as f32));
        let decided = point.and_then(|point| self.features.iter().find_map(|f| f.cursor(point)));
        if let Some(style) = decided {
            return style;
        }
        match (self.selection.mode, pointer) {
            (Mode::Region, Some((x, y))) => {
                cursor_for(self.selection.hit(x, y), self.selection.is_dragging())
            }
            _ => CursorStyle::Crosshair,
        }
    }

    /// Click handlers for feature `index`.
    fn act(overlay: &Entity<Self>, index: usize) -> Act {
        Act {
            overlay: overlay.clone(),
            index,
        }
    }
}

/// The overlay's part once a folder was picked (or not).
type Reopened = Box<dyn FnOnce(&mut Overlay, Option<PathBuf>)>;

/// Ask for a folder asking `prompt`; then `picked` gets it on the overlay, which reopens.
fn ask_for_folder(entity: Entity<Overlay>, prompt: String, picked: Reopened, cx: &App) {
    let asked = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(prompt.into()),
    });
    cx.spawn(async move |cx| {
        let choice = asked.await.ok().and_then(Result::ok).flatten();
        let dir = choice.and_then(|paths| paths.into_iter().next());
        cx.update(|cx| {
            entity.update(cx, |o, _| picked(o, dir));
            open_windows(&entity, cx);
        });
    })
    .detach();
}

/// The daemon's answer for an overlay's outcome.
#[cfg(unix)]
fn reply_for(outcome: &anyhow::Result<Option<PathBuf>>) -> daemon::Reply {
    match outcome {
        Ok(Some(path)) => daemon::Reply::Saved(path.clone()),
        Ok(None) => daemon::Reply::Cancelled,
        Err(e) => daemon::Reply::Error(format!("{e:#}")),
    }
}

/// An arrow key, as a one-pixel step.
fn arrow(key: &str) -> Option<(i32, i32)> {
    match key {
        "left" => Some((-1, 0)),
        "right" => Some((1, 0)),
        "up" => Some((0, -1)),
        "down" => Some((0, 1)),
        _ => None,
    }
}

/// A desktop point to whole pixels, as the selection counts.
const fn rounded((x, y): feature::Point) -> (i32, i32) {
    (x.round() as i32, y.round() as i32)
}

/// One monitor's overlay window.
struct View {
    overlay: Entity<Overlay>,
    monitor: MonitorInfo,
    image: Option<Arc<RenderImage>>,
    focus: FocusHandle,
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
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.overlay.update(cx, |o, cx| o.key(&event.keystroke, cx));
    }

    fn on_press(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let point = to_global(self.monitor.rect, event.position);
        self.overlay
            .update(cx, |o, cx| o.press(point, event.click_count, cx));
    }

    fn on_scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            // Touchpads: one step per ~24 px.
            ScrollDelta::Pixels(p) => f32::from(p.y) / 24.,
        };
        self.overlay.update(cx, |o, cx| o.scroll(lines, cx));
    }

    /// Until the features' rows show: add them right after this frame (and trace the first).
    fn join_rows_after_this_frame(&self, o: &Overlay, window: &Window) {
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
                o.tools_ready = true;
                cx.notify();
            });
        });
    }

    /// The window taking keys, scrolls and presses.
    fn listening(&self, root: gpui::Stateful<Div>, o: &Overlay, cx: &Context<Self>) -> gpui::Stateful<Div> {
        root.cursor(o.cursor())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_scroll(cx.listener(Self::on_scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_press))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| this.overlay.update(cx, Overlay::cancel)),
            )
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
fn to_global(monitor: Rect, p: Point<Pixels>) -> feature::Point {
    (
        monitor.x as f32 + f32::from(p.x),
        monitor.y as f32 + f32::from(p.y),
    )
}

/// `rect` relative to `monitor`'s top-left.
const fn local(rect: Rect, monitor: Rect) -> Rect {
    Rect {
        x: rect.x - monitor.x,
        y: rect.y - monitor.y,
        ..rect
    }
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
            self.join_rows_after_this_frame(o, window);
        }
        let root = frozen_screen(self.image.as_ref());
        if !o.shows_selection() {
            return root;
        }
        let monitor = &self.monitor;
        let under = o
            .features
            .iter()
            .flat_map(|f| f.under_dim(monitor, window))
            .collect::<Vec<_>>();
        let over = o.features.iter().flat_map(|f| f.over_dim(monitor.rect));
        self.listening(root, o, cx)
            .children(under)
            .children(dims(o, monitor))
            .children(selection_marks(o, monitor))
            .children(over)
            .children(bars(&self.overlay, o, monitor))
            .child(pointer_listeners(self.overlay.clone(), monitor.rect))
    }
}

/// The screen as it was at the trigger.
fn frozen_screen(image: Option<&Arc<RenderImage>>) -> gpui::Stateful<Div> {
    let root = div().id("overlay").size_full().relative().bg(gpui::black());
    root.children(image.map(|image| {
        img(ImageSource::Render(image.clone()))
            .absolute()
            .size_full()
            .object_fit(ObjectFit::Fill)
    }))
}

/// Everything on `monitor` but the selection, darkened.
fn dims(o: &Overlay, monitor: &MonitorInfo) -> impl Iterator<Item = Div> {
    let clear = match o.selection.mode {
        Mode::Region => o.selection.region(),
        Mode::Screens => o.selection.is_picked(&monitor.name).then_some(monitor.rect),
    };
    layout::dim_rects(monitor.rect, clear)
        .into_iter()
        .map(|r| rect_div(r).bg(DIM))
}

/// The selection's outline, handles and size on `monitor`, or the screen picked.
fn selection_marks(o: &Overlay, monitor: &MonitorInfo) -> Vec<AnyElement> {
    let hovered = o.selection.hovered().is_some_and(|m| m.name == monitor.name);
    match o.selection.mode {
        Mode::Region => region_marks(o, monitor.rect, hovered),
        Mode::Screens => {
            let picked = o.selection.is_picked(&monitor.name);
            vec![screen_mark(picked, hovered, monitor).into_any_element()]
        }
    }
}

fn region_marks(o: &Overlay, monitor: Rect, hovered: bool) -> Vec<AnyElement> {
    let region = o.selection.region();
    let Some(selected) = region.filter(|r| r.intersect(monitor).is_some()) else {
        // Nothing selected yet: the screen a click would select.
        let hint = (region.is_none() && hovered).then(|| {
            let outline = rect_div(local(monitor, monitor)).border_2();
            outline
                .border_color(hsla(0.58, 0.85, 0.55, 0.5))
                .into_any_element()
        });
        return hint.into_iter().collect();
    };
    let shown = local(selected, monitor);
    let handles = (!o.busy() && !o.holds_a_tool()).then(|| region_handles(shown));
    let outline = rect_div(shown).border_1().border_color(ACCENT).into_any_element();
    let label = size_label(selected, shown, monitor.h).into_any_element();
    std::iter::once(outline)
        .chain(handles.into_iter().flatten())
        .chain([label])
        .collect()
}

/// The eight resize handles of `region` (window coordinates).
fn region_handles(region: Rect) -> impl Iterator<Item = AnyElement> {
    let (mid_x, mid_y) = (region.x + region.w / 2, region.y + region.h / 2);
    let (left, top, right, bottom) = (region.x, region.y, region.right(), region.bottom());
    [
        (left, top),
        (mid_x, top),
        (right, top),
        (right, mid_y),
        (right, bottom),
        (mid_x, bottom),
        (left, bottom),
        (left, mid_y),
    ]
    .into_iter()
    .map(|(x, y)| handle_div(x, y).into_any_element())
}

/// `selected`'s size, under it (shown at `shown`), or above it at the bottom of the screen.
fn size_label(selected: Rect, shown: Rect, screen_height: i32) -> Div {
    let top = if shown.bottom() + 28 < screen_height {
        shown.bottom() + 8
    } else {
        (shown.y - 30).max(0)
    };
    div()
        .absolute()
        .left(px(shown.x.max(0) as f32))
        .top(px(top as f32))
        .px_2()
        .rounded_sm()
        .bg(rgba(0x000000c0))
        .text_xs()
        .text_color(gpui::white())
        .child(format!("{} × {}", selected.w, selected.h))
}

/// A screen in Screens mode: its name, with a border when picked or under the pointer.
fn screen_mark(picked: bool, hovered: bool, monitor: &MonitorInfo) -> Div {
    let border = if picked {
        ACCENT
    } else if hovered {
        hsla(0., 0., 1., 0.5)
    } else {
        hsla(0., 0., 0., 0.)
    };
    let name = div()
        .px_4()
        .py_2()
        .rounded_lg()
        .bg(rgba(0x000000a0))
        .text_2xl()
        .font_weight(FontWeight::BOLD)
        .text_color(gpui::white())
        .child(format!("{}{}", if picked { "✓ " } else { "" }, monitor.name));
    let area = rect_div(local(monitor.rect, monitor.rect))
        .border_4()
        .border_color(border);
    area.flex().items_center().justify_center().child(name)
}

/// The toolbar and the action bar, on the screen under the pointer (all of them until the
/// pointer is seen), hidden while dragging so they never get in the way.
fn bars(overlay: &Entity<Overlay>, o: &Overlay, monitor: &MonitorInfo) -> Vec<AnyElement> {
    let elsewhere = o.selection.hovered().is_some_and(|m| m.name != monitor.name);
    if o.busy() || elsewhere {
        return Vec::new();
    }
    let leaving = matches!(o.phase, Phase::Leaving);
    let edge = layout::toolbar_edge(monitor.rect, o.selection.region());
    vec![
        toolbar(overlay, o, edge, leaving),
        action_bar(overlay, o, leaving),
    ]
}

/// The toolbar: the overlay's row, then the features' rows (and popup), growing away from
/// its screen edge.
fn toolbar(overlay: &Entity<Overlay>, o: &Overlay, edge: Edge, leaving: bool) -> AnyElement {
    let acts = (0..o.features.len()).map(|index| Overlay::act(overlay, index));
    let features = o.features.iter().zip(acts).filter(|_| o.tools_ready);
    let (rows, popups): (Vec<_>, Vec<_>) = features.map(|(f, act)| (f.rows(&act), f.popup(&act))).unzip();
    let rows = std::iter::once(main_row(overlay, o).into_any_element()).chain(rows.into_iter().flatten());
    let column = stack(rows.collect(), popups.into_iter().flatten().next(), edge);
    let place = div()
        .absolute()
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(column);
    let offset = match edge {
        Edge::Top => actions::EDGE_GAP,
        Edge::Bottom => actions::EDGE_GAP + actions::ACTION_BAR_ROOM,
    };
    actions::slide(place, edge, offset, leaving, "toolbar")
}

/// `rows` (and `popup`) stacked away from the screen `edge`.
fn stack(rows: Vec<AnyElement>, popup: Option<AnyElement>, edge: Edge) -> Div {
    let column = div().flex().flex_col().items_center().gap_1();
    match edge {
        Edge::Top => column.children(rows).children(popup),
        Edge::Bottom => column.children(popup).children(rows.into_iter().rev()),
    }
}

/// The overlay's own row: what to select, the features' buttons, and close.
fn main_row(overlay: &Entity<Overlay>, o: &Overlay) -> Div {
    let act = |f: fn(&mut Overlay, &mut Context<Overlay>)| tools::on(overlay, f);
    let (mode, all) = (o.selection.mode, o.selection.is_all());
    let region = tools::button("region", "Region  R", mode == Mode::Region);
    let screens = tools::button("screens", "Screens  S", mode == Mode::Screens && !all);
    let feature_buttons = o.features.iter().enumerate();
    let feature_buttons =
        feature_buttons.flat_map(|(index, f)| f.main_buttons(&Overlay::act(overlay, index)));
    panel()
        .child(region.on_click(act(|o, cx| o.set_mode(Mode::Region, cx))))
        .child(screens.on_click(act(|o, cx| o.set_mode(Mode::Screens, cx))))
        .child(tools::button("all", "All screens  A", all).on_click(act(Overlay::select_all)))
        .child(separator())
        .children(feature_buttons)
        .child(tools::button("close", "✕", false).on_click(act(Overlay::cancel)))
}

fn action_bar(overlay: &Entity<Overlay>, o: &Overlay, leaving: bool) -> AnyElement {
    let ctx = Ctx {
        selection: &o.selection,
    };
    let features = o.features.iter().enumerate();
    let actions = features.map(|(index, f)| f.actions(&Overlay::act(overlay, index), &ctx));
    actions::action_bar(actions.collect(), o.notice.as_ref(), leaving)
}

/// Window-wide listeners: during a drag the compositor keeps sending pointer events to the
/// surface where it started, even past its edge, so selections can span monitors.
fn pointer_listeners(overlay: Entity<Overlay>, monitor: Rect) -> impl IntoElement {
    let listen = move |_, (), window: &mut Window, _: &mut App| {
        let moving = overlay.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                let point = to_global(monitor, e.position);
                moving.update(cx, |o, cx| o.motion(point, cx));
            }
        });
        let releasing = overlay.clone();
        window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                let point = to_global(monitor, e.position);
                releasing.update(cx, |o, cx| o.release(point, cx));
            }
        });
    };
    canvas(|_, _, _| (), listen).absolute().size_full()
}
