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

pub mod actions;
pub mod drawer;
pub mod drawers;
pub mod feature;
pub mod folder;
#[cfg(unix)]
pub mod gate;
pub mod layout;
mod look;
pub mod motion;
pub mod navigation;
#[cfg(unix)]
pub mod service;
pub mod theme;
pub mod widgets;

use std::{
    any::Any,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Context, CursorStyle, DispatchPhase, Div, Entity, FocusHandle,
    FontWeight, Hsla, ImageSource, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, PathPromptOptions, Pixels, PlatformDisplay, Point, QuitMode, RenderImage,
    ScrollDelta, ScrollEvent, SharedString, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, canvas, div, hsla, img, prelude::*, px, size,
};
use smallvec::SmallVec;

use crate::units::{ToF32, saturating_i32, unsigned};

#[cfg(unix)]
use crate::daemon;
use crate::{
    capture::{MonitorInfo, Rect, Shot, Snapshot},
    drawings::Shared,
    selection::{Handle, Hit, Mode, Request, Selection},
    trace,
};

use feature::{Act, Ctx, Effect, Feature, HandOver, Picked, SaveJob, Start};
use layout::Edge;

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
#[must_use]
pub fn render_image(frame: image::Frame) -> RenderImage {
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
            let image = image::ImageBuffer::from_raw(unsigned(size.width.0), unsigned(size.height.0), bytes)
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
///
/// # Errors
///
/// When the display server refuses the capture, or there is no screen.
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
    /// The drawers beside the features' action buttons.
    drawers: drawers::Drawers,
    /// Where the action buttons rest, as last painted: the drawers need the room to their left.
    action_anchor: Rc<std::cell::Cell<Option<Rect>>>,
}

/// How the program makes its features, in the order they are asked (and their actions shown):
/// fresh for every overlay, with the settings remembered then.
pub type MakeFeatures = fn() -> Vec<Box<dyn Feature>>;

struct FeatureList(MakeFeatures);

impl gpui::Global for FeatureList {}

/// A standalone run saved something (recordings included): the process exits with success.
pub fn mark_saved() {
    SAVED.store(true, Ordering::Relaxed);
}

/// Open the overlay once `frozen` (started before gpui, to overlap its start-up) is ready, and
/// run until the capture is saved or cancelled. Returns whether a file was saved.
///
/// # Panics
///
/// When the capture thread behind `frozen` panicked: that panic is passed on.
pub fn run(
    mode: Mode,
    frozen: std::thread::JoinHandle<anyhow::Result<Frozen>>,
    features: MakeFeatures,
) -> bool {
    gpui_platform::application().run(move |cx: &mut App| {
        cx.set_global(FeatureList(features));
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

/// Saves are made at once (~20 µs), so they land in the order the user made them; one that
/// failed is reported, naming `what` changed.
pub fn report_unremembered(saved: anyhow::Result<()>, what: &str) {
    if let Err(e) = saved {
        eprintln!("screencapture: could not remember {what}: {e:#}");
    }
}

/// Work the daemon's request thread hands to the UI thread.
#[cfg(unix)]
enum ToUi {
    Overlay(daemon::Client, Mode, anyhow::Result<Frozen>),
    /// Something of a service's (the recording frame).
    Run(service::UiJob),
}

/// Resident mode: initialise the GPU once, then open an overlay per client request, and let
/// `service` answer requests of its own.
#[cfg(unix)]
pub fn run_daemon(
    listener: std::os::unix::net::UnixListener,
    service: impl service::Service,
    features: MakeFeatures,
) {
    gpui_platform::application().run(move |cx: &mut App| {
        cx.set_global(FeatureList(features));
        cx.set_quit_mode(QuitMode::Explicit);
        warm_up(cx);
        // This process outlives every capture: it can keep the copied image on offer itself.
        crate::clipboard::serve();
        let (tx, rx) = futures::channel::mpsc::unbounded();
        // Requests are served on their own thread, and the screens frozen right there, the
        // moment a request arrives: the UI thread is only woken once there is something to show.
        std::thread::spawn(move || serve_requests(&listener, service, &service::Ui(tx)));
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            let mut rx = rx;
            while let Some(work) = rx.next().await {
                cx.update(|cx| do_ui_work(work, cx));
            }
        })
        .detach();
    });
}

/// The UI thread's side of one piece of work: open an overlay, or run a service's job.
#[cfg(unix)]
fn do_ui_work(work: ToUi, cx: &mut App) {
    match work {
        ToUi::Overlay(client, mode, frozen) => serve_request(client, mode, frozen, cx),
        ToUi::Run(job) => job(cx),
    }
}

/// The daemon's request thread: keeps the screens' capture warm between requests (and gives
/// its memory back when quiet), and answers every request.
#[cfg(unix)]
fn serve_requests<S: service::Service>(
    listener: &std::os::unix::net::UnixListener,
    service: S,
    ui: &service::Ui,
) {
    // Used by the closures below, never at once.
    let session = std::cell::RefCell::new(crate::capture::Session::default());
    let service = std::cell::RefCell::new(service);
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
    let watch = || service.borrow_mut().watch(ui);
    let answer = |pending| answer_request(pending, &mut *service.borrow_mut(), &session, ui);
    daemon::serve(listener, Some(release_after()), quiet, watch, answer);
}

/// How long the screen buffers stay without a request (`SCREENCAPTURE_RELEASE_AFTER_SECS`
/// overrides it).
#[cfg(unix)]
fn release_after() -> std::time::Duration {
    let configured = std::env::var("SCREENCAPTURE_RELEASE_AFTER_SECS").ok();
    let seconds = configured.and_then(|v| v.parse().ok());
    seconds.map_or(RELEASE_BUFFERS_AFTER, std::time::Duration::from_secs)
}

/// One request: the service's, or an overlay to open over freshly frozen screens.
#[cfg(unix)]
fn answer_request<S: service::Service>(
    pending: daemon::Pending<S::Request>,
    service: &mut S,
    session: &std::cell::RefCell<crate::capture::Session>,
    ui: &service::Ui,
) {
    crate::trace_restart();
    trace("request received");
    let daemon::Pending { request, client } = pending;
    let mode = match request {
        daemon::Request::Overlay(mode) => mode,
        daemon::Request::Feature(request) => return service.handle(request, client, GATE.is_open(), ui),
    };
    if let Some(reply) = service.instead_of_overlay() {
        return client.reply(&reply);
    }
    if !wait_for_the_last_overlay() {
        return client.reply(&daemon::Reply::Busy);
    }
    let frozen = session.borrow_mut().take().map(Frozen::new);
    trace("screens frozen");
    let _ = ui.0.unbounded_send(ToUi::Overlay(client, mode, frozen));
}

/// One overlay at a time: `false` if one is up. Pressed again just as the last one closed, let
/// the screens repaint without it first.
#[cfg(unix)]
fn wait_for_the_last_overlay() -> bool {
    match GATE.enter(std::time::Instant::now()) {
        gate::Entry::Busy => false,
        gate::Entry::Go { wait } => {
            if !wait.is_zero() {
                trace("waiting for the last overlay to clear");
                std::thread::sleep(wait);
            }
            true
        }
    }
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
    let options = WindowOptions {
        titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(Point::default(), one))),
        kind: warm_up_kind(),
        focus: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..WindowOptions::default()
    };
    if let Err(e) = cx.open_window(options, |_, cx| cx.new(|_| Warmup)) {
        eprintln!("screencapture: warm-up window failed ({e:#}); first overlay will be slower");
    }
    trace("warmed up");
}

/// Out of sight: under everything, no keyboard.
#[cfg(unix)]
#[cfg_attr(
    not(all(target_os = "linux", feature = "wayland")),
    expect(clippy::missing_const_for_fn, reason = "the layer surface is not const")
)]
fn warm_up_kind() -> WindowKind {
    #[cfg(all(target_os = "linux", feature = "wayland"))]
    {
        use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
        WindowKind::LayerShell(LayerShellOptions {
            namespace: "screencapture-warmup".into(),
            layer: Layer::Background,
            anchor: Anchor::TOP | Anchor::LEFT,
            keyboard_interactivity: KeyboardInteractivity::None,
            ..LayerShellOptions::default()
        })
    }
    #[cfg(not(all(target_os = "linux", feature = "wayland")))]
    WindowKind::PopUp
}

fn start(mode: Mode, frozen: Frozen, client: Waiting, resident: bool, cx: &mut App) {
    let features = (cx.global::<FeatureList>().0)();
    let overlay = cx.new(|cx: &mut Context<Overlay>| {
        // However the overlay goes away (the compositor may close every window, e.g. on an
        // output change), the client gets an answer and the daemon takes new requests.
        cx.on_release(Overlay::released).detach();
        Overlay::new(mode, frozen, client, resident, features)
    });
    open_windows(&overlay, cx);
    #[cfg(debug_assertions)]
    run_test_hooks(&overlay, cx);
}

/// The daemon client waiting for the overlay's outcome (none standalone).
#[cfg(unix)]
type Waiting = Option<daemon::Client>;
#[cfg(not(unix))]
type Waiting = Option<()>;

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
    let selected = std::env::var("SCREENCAPTURE_E2E_SELECT").ok();
    let Some(region) = selected.and_then(|g| Rect::parse(&g).ok()) else {
        return;
    };
    let overlay = overlay.clone();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(std::time::Duration::from_millis(500))
            .await;
        overlay.update(cx, |o, cx| o.test_hooks_on(region, cx));
    })
    .detach();
}

fn open_windows(overlay: &Entity<Overlay>, cx: &mut App) {
    look::follow_desktop(overlay, cx);
    let mut displays = displays_by_pointer(overlay.read(cx), cx).into_iter();
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
        cx.update(|cx| overlay.update(cx, Overlay::end_without_windows));
    })
    .detach();
}

/// Each display with the screen it shows, the pointer's first: it is where the user looks.
fn displays_by_pointer(o: &Overlay, cx: &App) -> Vec<(Rc<dyn PlatformDisplay>, MonitorInfo)> {
    let monitors = o.selection.monitors();
    let pointer = o.selection.hovered().map(|m| m.name.as_str());
    let shown = |display: Rc<dyn PlatformDisplay>| {
        let origin = display.bounds().origin;
        let i = layout::match_display(
            (
                saturating_i32(f32::from(origin.x)),
                saturating_i32(f32::from(origin.y)),
            ),
            monitors,
        )?;
        Some((display, monitors[i].clone()))
    };
    let mut displays: Vec<_> = cx.displays().into_iter().filter_map(shown).collect();
    displays.sort_by_key(|(_, m)| Some(m.name.as_str()) != pointer);
    displays
}

fn open_window(
    overlay: &Entity<Overlay>,
    display: &Rc<dyn PlatformDisplay>,
    monitor: MonitorInfo,
    cx: &mut App,
) {
    let build = |overlay: Entity<Overlay>, monitor: MonitorInfo| {
        move |window: &mut Window, cx: &mut App| cx.new(|cx| View::new(overlay, monitor, window, cx))
    };
    #[cfg(all(target_os = "linux", feature = "wayland"))]
    let layer = {
        let options = window_options(display, cover_everything(), windowed(monitor.rect));
        cx.open_window(options, build(overlay.clone(), monitor.clone()))
            .ok()
    };
    #[cfg(not(all(target_os = "linux", feature = "wayland")))]
    let layer = None;
    // X11, other platforms, or a Wayland compositor without layer-shell.
    let handle = layer.or_else(|| {
        let options = window_options(
            display,
            WindowKind::PopUp,
            WindowBounds::Fullscreen(display.bounds()),
        );
        cx.open_window(options, build(overlay.clone(), monitor))
            .inspect_err(|e| eprintln!("overlay window: {e:#}"))
            .ok()
    });
    if let Some(handle) = handle {
        overlay.update(cx, |o, _| o.windows.push(handle.into()));
    }
}

/// An overlay window of `kind` on `display`.
fn window_options(
    display: &Rc<dyn PlatformDisplay>,
    kind: WindowKind,
    bounds: WindowBounds,
) -> WindowOptions {
    WindowOptions {
        titlebar: None,
        window_bounds: Some(bounds),
        kind,
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        display_id: Some(display.id()),
        window_background: WindowBackgroundAppearance::Opaque,
        app_id: Some("screencapture".into()),
        ..WindowOptions::default()
    }
}

/// A window the size of `screen`.
#[cfg(all(target_os = "linux", feature = "wayland"))]
fn windowed(screen: Rect) -> WindowBounds {
    let size = size(px(screen.w.to_f32()), px(screen.h.to_f32()));
    WindowBounds::Windowed(Bounds::new(Point::default(), size))
}

/// The top layer over the whole screen, with the keyboard: panels too are covered instead of
/// pushing it aside with their exclusive zones.
#[cfg(all(target_os = "linux", feature = "wayland"))]
fn cover_everything() -> WindowKind {
    use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
    WindowKind::LayerShell(LayerShellOptions {
        namespace: "screencapture".into(),
        layer: Layer::Overlay,
        anchor: Anchor::all(),
        exclusive_zone: Some(px(-1.)),
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        ..LayerShellOptions::default()
    })
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
    fn new(
        mode: Mode,
        frozen: Frozen,
        client: Waiting,
        resident: bool,
        features: Vec<Box<dyn Feature>>,
    ) -> Self {
        let drawers = drawers::Drawers::new(features.iter().map(|feature| feature.has_drawer()));
        let screens: Arc<[_]> = frozen.screens.into();
        let images = screens
            .iter()
            .map(|(m, image)| (m.name.clone(), image.clone()))
            .collect();
        #[cfg(not(unix))]
        let _ = client;
        Self {
            selection: selection_at_pointer(mode, &screens),
            screens,
            images,
            phase: Phase::Selecting,
            windows: Vec::new(),
            #[cfg(unix)]
            client,
            resident,
            features,
            tools_ready: false,
            traced: std::cell::Cell::new(false),
            notice: None,
            drawers,
            action_anchor: Rc::default(),
        }
    }

    /// Debug builds: every feature acts on `region` as the end-to-end tests ask.
    #[cfg(debug_assertions)]
    fn test_hooks_on(&mut self, region: Rect, cx: &mut Context<Self>) {
        if std::env::var_os("SCREENCAPTURE_E2E_DRAWER").is_some() {
            self.drawers.toggle_next(Instant::now());
            self.drawers_changed(cx);
        }
        // The first feature (the drawings) acts first, so what is drawn is kept; then the last
        // first: a recording the test starts wins over a capture "clicked" while the bars slide
        // away, whatever order the buttons are shown in.
        let count = self.features.len();
        for index in std::iter::once(0).chain((1..count).rev()).take(count) {
            if let Some(effect) = self.features[index].test_hook(region) {
                self.apply(index, effect, cx);
            }
        }
        cx.notify();
    }

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
        if !self.selecting() || !self.can_make(&request) {
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

    /// Whether `request` can produce an image: if not, the overlay stays up, keeping the
    /// selection and drawings.
    fn can_make(&self, request: &Request) -> bool {
        let placed = borrowed(&self.screens).and_then(|snapshot| snapshot.placements(request));
        placed.inspect_err(|e| eprintln!("screencapture: {e:#}")).is_ok()
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
            cx.background_executor().timer(motion::slide()).await;
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

    /// Every display was tried: if no window opened at all, give up.
    fn end_without_windows(&mut self, cx: &mut Context<Self>) {
        if self.selecting() && self.windows.is_empty() {
            self.end(&Err(anyhow::anyhow!("could not open any overlay window")), cx);
        }
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
                mark_saved();
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
        let closed_a_drawer = self.drawers.escape(Instant::now());
        if closed_a_drawer || self.features.iter_mut().any(|feature| feature.escape()) {
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
    /// What is left for the window to do (move the focus), if the key asked for it.
    fn key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> Option<navigation::Navigation> {
        if !self.selecting() {
            return None;
        }
        if let Some((index, effect)) = self.feature_key(keystroke) {
            self.apply(index, effect, cx);
            return None;
        }
        let asked = navigation::navigation(keystroke);
        match asked {
            Some(navigation::Navigation::ToggleOptions) => self.toggle_next_drawer(cx),
            Some(_) => return asked,
            None => self.own_key(keystroke, cx),
        }
        None
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
        self.drawers.pressed_outside(Instant::now());
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
        let point = pointer.map(|(x, y)| (x.to_f32(), y.to_f32()));
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
    /// The pointer entered or left a part of feature `index`'s action button.
    pub(crate) fn drawer_hover(
        &mut self,
        index: usize,
        part: drawers::Part,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if self.drawers.hover(index, part, hovered, Instant::now()) {
            self.drawers_changed(cx);
        }
    }

    /// Tab: open the drawers one after the other, pinned.
    fn toggle_next_drawer(&mut self, cx: &mut Context<Self>) {
        self.drawers.toggle_next(Instant::now());
        self.drawers_changed(cx);
    }

    /// A press inside feature `index`'s drawer: it stays open.
    pub(crate) fn drawer_pressed(&mut self, index: usize, cx: &mut Context<Self>) {
        self.drawers.option_pressed(index, Instant::now());
        self.drawers_changed(cx);
    }

    /// Show the drawers as they are now, and come back when one is waiting on the clock.
    fn drawers_changed(&self, cx: &mut Context<Self>) {
        cx.notify();
        let Some(wait) = self.drawers.next_tick(Instant::now()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |overlay, cx| {
                overlay.drawers.tick(Instant::now());
                overlay.drawers_changed(cx);
            });
        })
        .detach();
    }

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
fn rounded((x, y): feature::Point) -> (i32, i32) {
    (saturating_i32(x.round()), saturating_i32(y.round()))
}

/// One monitor's overlay window.
struct View {
    overlay: Entity<Overlay>,
    monitor: MonitorInfo,
    image: Option<Arc<RenderImage>>,
    focus: FocusHandle,
}

/// Dark rounded strip the toolbar rows are made of.
#[must_use]
pub fn panel() -> Div {
    let theme = theme::current();
    div()
        .occlude()
        .flex()
        .items_center()
        .gap_1()
        .p_1()
        .rounded_lg()
        .bg(theme.surface)
        .border_1()
        .border_color(theme.border)
        .text_sm()
        .text_color(theme.on_surface)
}

#[must_use]
pub fn separator() -> Div {
    div().w(px(1.)).h(px(20.)).mx_1().bg(theme::current().separator)
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

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if navigation::presses_focused_button(key) && self.button_has_focus(window, cx) {
            return;
        }
        match self.overlay.update(cx, |o, cx| o.key(key, cx)) {
            Some(navigation::Navigation::FocusNext) => window.focus_next(cx),
            Some(navigation::Navigation::FocusPrevious) => window.focus_prev(cx),
            _ => {}
        }
    }

    /// A button, not the overlay itself, has the keyboard focus: it takes Enter and Space.
    fn button_has_focus(&self, window: &Window, cx: &App) -> bool {
        window.focused(cx).is_some_and(|focused| focused != self.focus)
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

const DIM: Hsla = Hsla {
    h: 0.,
    s: 0.,
    l: 0.,
    a: 0.45,
};
/// Side of the square resize handles, in logical px.
const HANDLE: i32 = 10;

#[must_use]
pub fn rect_div(r: Rect) -> Div {
    div()
        .absolute()
        .left(px(r.x.to_f32()))
        .top(px(r.y.to_f32()))
        .w(px(r.w.to_f32()))
        .h(px(r.h.to_f32()))
}

#[must_use]
pub fn handle_div(x: i32, y: i32) -> Div {
    rect_div(Rect::new(x - HANDLE / 2, y - HANDLE / 2, HANDLE, HANDLE))
        .bg(theme::current().marks.handle_fill)
        .border_1()
        .border_color(theme::current().accent)
}

/// Window-local position to global logical desktop coordinates.
fn to_global(monitor: Rect, p: Point<Pixels>) -> feature::Point {
    (
        monitor.x.to_f32() + f32::from(p.x),
        monitor.y.to_f32() + f32::from(p.y),
    )
}

/// `rect` relative to `monitor`'s top-left.
#[must_use]
pub const fn local(rect: Rect, monitor: Rect) -> Rect {
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
                .border_color(theme::current().accent_hint)
                .into_any_element()
        });
        return hint.into_iter().collect();
    };
    let shown = local(selected, monitor);
    let handles = (!o.busy() && !o.holds_a_tool()).then(|| region_handles(shown));
    let outline = rect_div(shown)
        .border_1()
        .border_color(theme::current().accent)
        .into_any_element();
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
        .left(px(shown.x.max(0).to_f32()))
        .top(px(top.to_f32()))
        .px_2()
        .rounded_sm()
        .bg(theme::current().marks.scrim)
        .text_xs()
        .text_color(theme::current().marks.on_scrim)
        .child(format!("{} × {}", selected.w, selected.h))
}

/// A screen in Screens mode: its name, with a border when picked or under the pointer.
fn screen_mark(picked: bool, hovered: bool, monitor: &MonitorInfo) -> Div {
    let theme = theme::current();
    let border = if picked {
        theme.accent
    } else if hovered {
        theme.marks.screen_hover_outline
    } else {
        hsla(0., 0., 0., 0.)
    };
    let name = div()
        .px_4()
        .py_2()
        .rounded_lg()
        .bg(theme.marks.scrim_soft)
        .text_2xl()
        .font_weight(FontWeight::BOLD)
        .text_color(theme.marks.on_scrim)
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
        action_bar(overlay, o, monitor.rect, leaving),
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
    let act = |f: fn(&mut Overlay, &mut Context<Overlay>)| widgets::on(overlay, f);
    let (mode, all) = (o.selection.mode, o.selection.is_all());
    let region = widgets::button("region", "Region  R", mode == Mode::Region);
    let screens = widgets::button("screens", "Screens  S", mode == Mode::Screens && !all);
    panel()
        .child(region.on_click(act(|o, cx| o.set_mode(Mode::Region, cx))))
        .child(screens.on_click(act(|o, cx| o.set_mode(Mode::Screens, cx))))
        .child(widgets::button("all", "All screens  A", all).on_click(act(Overlay::select_all)))
        .child(separator())
        .child(widgets::button("close", "✕", false).on_click(act(Overlay::cancel)))
}

fn action_bar(overlay: &Entity<Overlay>, o: &Overlay, screen: Rect, leaving: bool) -> AnyElement {
    let ctx = Ctx {
        selection: &o.selection,
    };
    let features = o.features.iter().enumerate();
    let actions = features.map(|(index, f)| f.actions(&Overlay::act(overlay, index), &ctx));
    let bar = actions::ActionBar {
        overlay,
        drawers: &o.drawers,
        anchor: &o.action_anchor,
        screen: local(screen, screen),
    };
    bar.render(actions.collect(), o.notice.as_ref(), leaving)
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
