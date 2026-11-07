//! What stays on screen while recording: the red frame around the recorded area, and the
//! drawings made before the recording started. Click-through, without keyboard focus: the
//! user works normally underneath. The frame lies outside the recorded area; the drawings
//! inside it, so they are recorded.

use std::{cell::Cell, rc::Rc};

use gpui::{AnyWindowHandle, App};

use crate::drawings::Shared;

/// What to show for a recording.
#[derive(Clone)]
#[cfg_attr(
    not(all(target_os = "linux", feature = "wayland")),
    expect(dead_code, reason = "the frame needs layer-shell")
)]
pub struct Spec {
    pub on_screen: crate::record::OnScreen,
    pub drawings: Option<Shared>,
}

/// Without layer-shell there is no click-through window: no frame.
#[cfg(not(all(target_os = "linux", feature = "wayland")))]
pub fn show(_: &Spec, _: &mut App) -> Shown {
    Shown::default()
}

/// The windows on screen for one recording.
#[derive(Default)]
pub struct Shown {
    windows: Vec<AnyWindowHandle>,
    /// The controls drawer, while it is open.
    drawer: Rc<Cell<Option<AnyWindowHandle>>>,
}

pub fn hide(shown: Shown, cx: &mut App) {
    let drawer = shown.drawer.take();
    for window in shown.windows.into_iter().chain(drawer) {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    }
}

#[cfg(all(target_os = "linux", feature = "wayland"))]
pub use view::{display_of, layer_options, show};

#[cfg(all(target_os = "linux", feature = "wayland"))]
mod view {
    use gpui::{
        AnyView, AnyWindowHandle, App, Bounds, Context, DisplayId, Hsla, Point, Window,
        WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, prelude::*, px, size,
    };

    use super::{Shown, Spec};
    use crate::{
        capture::{MonitorInfo, Rect},
        overlay::{controls, layout},
        record::frame::{self, Surface},
    };

    const RED: Hsla = Hsla {
        h: 0.0,
        s: 0.9,
        l: 0.55,
        a: 0.95,
    };

    /// Open the frame (and drawings) for `spec`, and the strips that open the controls.
    pub fn show(spec: &Spec, cx: &mut App) -> Shown {
        let on_screen = &spec.on_screen;
        let extra = spec
            .drawings
            .as_ref()
            .map_or_else(Vec::new, |drawn| drawn.extents());
        let surfaces = frame::surfaces(&on_screen.areas, &extra, &on_screen.monitors);
        let mut shown = Shown::default();
        for surface in surfaces {
            match open_frame(&surface, &on_screen.monitors, spec, cx) {
                Ok(handle) => shown.windows.push(handle),
                Err(e) => eprintln!(
                    "screencapture: recording frame on {}: {e:#}",
                    surface.monitor.name
                ),
            }
        }
        let strips = controls::open_strips(on_screen, shown.drawer.clone(), cx);
        shown.windows.extend(strips);
        shown
    }

    pub fn display_of(screen: &MonitorInfo, monitors: &[MonitorInfo], cx: &App) -> Option<DisplayId> {
        let shows_screen = |display: &std::rc::Rc<dyn gpui::PlatformDisplay>| {
            let origin = display.bounds().origin;
            let origin = (f32::from(origin.x) as i32, f32::from(origin.y) as i32);
            layout::match_display(origin, monitors).is_some_and(|i| monitors[i].name == screen.name)
        };
        cx.displays().iter().find(|d| shows_screen(d)).map(|d| d.id())
    }

    /// A layer surface over everything at `bounds` (desktop px) on `screen`, without keyboard
    /// focus.
    pub fn layer_options(bounds: Rect, screen: Rect, display: DisplayId) -> WindowOptions {
        use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
        let kind = WindowKind::LayerShell(LayerShellOptions {
            namespace: "screencapture-recording".into(),
            layer: Layer::Overlay,
            anchor: Anchor::TOP | Anchor::LEFT,
            // Placed from the screen's corner, not pushed aside by panels.
            exclusive_zone: Some(px(-1.)),
            margin: Some((
                px((bounds.y - screen.y) as f32),
                px(0.),
                px(0.),
                px((bounds.x - screen.x) as f32),
            )),
            keyboard_interactivity: KeyboardInteractivity::None,
            ..LayerShellOptions::default()
        });
        let window_size = size(px(bounds.w as f32), px(bounds.h as f32));
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(Point::default(), window_size))),
            kind,
            focus: false,
            show: true,
            is_movable: false,
            is_resizable: false,
            display_id: Some(display),
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some("screencapture".into()),
            ..WindowOptions::default()
        }
    }

    fn open_frame(
        surface: &Surface,
        monitors: &[MonitorInfo],
        spec: &Spec,
        cx: &mut App,
    ) -> anyhow::Result<AnyWindowHandle> {
        let display = display_of(&surface.monitor, monitors, cx)
            .ok_or_else(|| anyhow::anyhow!("no display shows it"))?;
        let options = layer_options(surface.bounds, surface.monitor.rect, display);
        let surface = surface.clone();
        let drawings = spec.drawings.clone();
        let handle = cx.open_window(options, move |window, cx| {
            // Every click goes through to what is underneath.
            window.set_input_region(Some(&[]));
            let drawings = drawings.map(|drawn| drawn.view(surface.bounds, cx));
            cx.new(|_| FrameView { surface, drawings })
        })?;
        Ok(handle.into())
    }

    struct FrameView {
        surface: Surface,
        /// The drawings on this window, under the frame.
        drawings: Option<AnyView>,
    }

    impl Render for FrameView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let strips = self.surface.strips.iter().map(|strip| {
                div()
                    .absolute()
                    .left(px(strip.x as f32))
                    .top(px(strip.y as f32))
                    .w(px(strip.w as f32))
                    .h(px(strip.h as f32))
                    .bg(RED)
            });
            div()
                .size_full()
                .relative()
                .children(self.drawings.clone())
                .children(strips)
        }
    }
}
