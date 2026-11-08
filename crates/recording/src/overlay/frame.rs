//! What stays on screen while recording: the red frame around the recorded area, and the
//! drawings made before the recording started. Click-through, without keyboard focus: the
//! user works normally underneath. The frame lies outside the recorded area; the drawings
//! inside it, so they are recorded.

use std::{cell::Cell, rc::Rc};

use gpui::{AnyWindowHandle, App};

use screencapture_core::drawings::Shared;

/// What to show for a recording.
#[derive(Clone)]
pub struct Spec {
    pub on_screen: crate::OnScreen,
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
        AnyView, AnyWindowHandle, App, Bounds, Context, DisplayId, Point, Window, WindowBackgroundAppearance,
        WindowBounds, WindowKind, WindowOptions, div, prelude::*, px, size,
    };

    use super::{Shown, Spec};
    use screencapture_core::{
        capture::{MonitorInfo, Rect},
        overlay::{layout, rect_div, theme},
        units::{ToF32, saturating_i32},
    };

    use crate::{
        frame::{self, Surface},
        overlay::controls,
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
            let origin = (
                saturating_i32(f32::from(origin.x)),
                saturating_i32(f32::from(origin.y)),
            );
            layout::match_display(origin, monitors).is_some_and(|i| monitors[i].name == screen.name)
        };
        cx.displays().iter().find(|d| shows_screen(d)).map(|d| d.id())
    }

    /// A layer surface over everything at `bounds` (desktop px) on `screen`, without keyboard
    /// focus.
    pub fn layer_options(bounds: Rect, screen: Rect, display: DisplayId) -> WindowOptions {
        let window_size = size(px(bounds.w.to_f32()), px(bounds.h.to_f32()));
        WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(Point::default(), window_size))),
            kind: layer_over_everything(bounds, screen),
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

    /// Top layer, placed from `screen`'s corner at `bounds`, not pushed aside by panels.
    fn layer_over_everything(bounds: Rect, screen: Rect) -> WindowKind {
        use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
        let (top, left) = (
            px((bounds.y - screen.y).to_f32()),
            px((bounds.x - screen.x).to_f32()),
        );
        WindowKind::LayerShell(LayerShellOptions {
            namespace: "screencapture-recording".into(),
            layer: Layer::Overlay,
            anchor: Anchor::TOP | Anchor::LEFT,
            exclusive_zone: Some(px(-1.)),
            margin: Some((top, px(0.), px(0.), left)),
            keyboard_interactivity: KeyboardInteractivity::None,
            ..LayerShellOptions::default()
        })
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
            let colour = theme::current().danger_fill;
            let strips = self
                .surface
                .strips
                .iter()
                .map(|strip| rect_div(*strip).bg(colour));
            div()
                .size_full()
                .relative()
                .children(self.drawings.clone())
                .children(strips)
        }
    }
}
