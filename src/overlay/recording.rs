//! What stays on screen while recording: the red frame around the recorded area, and the
//! drawings made before the recording started. Click-through, without keyboard focus: the
//! user works normally underneath. The frame lies outside the recorded area; the drawings
//! inside it, so they are recorded.

use gpui::{AnyWindowHandle, App};

use crate::{
    annotate::Shape,
    capture::{MonitorInfo, Rect},
};

/// What to show for a recording.
#[derive(Clone, Debug)]
#[cfg_attr(
    not(all(target_os = "linux", feature = "wayland")),
    expect(dead_code, reason = "the frame needs layer-shell")
)]
pub struct Spec {
    pub area: Rect,
    pub monitors: Vec<MonitorInfo>,
    pub shapes: Vec<Shape>,
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
}

pub fn hide(shown: Shown, cx: &mut App) {
    for window in shown.windows {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    }
}

#[cfg(all(target_os = "linux", feature = "wayland"))]
pub use view::show;

#[cfg(all(target_os = "linux", feature = "wayland"))]
mod view {
    use std::{collections::HashMap, sync::Arc};

    use gpui::{
        AnyWindowHandle, App, Bounds, Context, Hsla, ImageSource, ObjectFit, Point, RenderImage, Window,
        WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, canvas, div, img, prelude::*,
        px, size,
    };

    use super::{Shown, Spec};
    use crate::{
        annotate::{Shape, Tool},
        capture::Rect,
        overlay::{layout, tools},
        record::frame::{self, Surface},
    };

    const RED: Hsla = Hsla {
        h: 0.0,
        s: 0.9,
        l: 0.55,
        a: 0.95,
    };

    /// Room a drawing takes on screen: its points, plus what its stroke, arrow head or disc adds.
    fn shape_rect(shape: &Shape) -> Rect {
        let (l, t, r, b) = shape.bounds();
        let pad = if shape.tool == Tool::Text {
            0.
        } else {
            shape.width.mul_add(4., 4.)
        };
        Rect::new(
            (l - pad).floor() as i32,
            (t - pad).floor() as i32,
            2.0f32.mul_add(pad, r - l).ceil() as i32,
            2.0f32.mul_add(pad, b - t).ceil() as i32,
        )
    }

    /// Open the frame (and drawings) for `spec`.
    pub fn show(spec: &Spec, cx: &mut App) -> Shown {
        let extra: Vec<Rect> = spec.shapes.iter().map(shape_rect).collect();
        let surfaces = frame::surfaces(spec.area, &extra, &spec.monitors);
        let shapes: Arc<[Shape]> = spec.shapes.clone().into();
        let displays = cx.displays();
        let mut shown = Shown::default();
        for surface in surfaces {
            let display = displays.iter().find(|d| {
                let o = d.bounds().origin;
                layout::match_display((f32::from(o.x) as i32, f32::from(o.y) as i32), &spec.monitors)
                    .is_some_and(|i| spec.monitors[i].name == surface.monitor.name)
            });
            let Some(display) = display else { continue };
            match open(&surface, display.id(), shapes.clone(), cx) {
                Ok(handle) => shown.windows.push(handle),
                Err(e) => eprintln!(
                    "screencapture: recording frame on {}: {e:#}",
                    surface.monitor.name
                ),
            }
        }
        shown
    }

    fn open(
        surface: &Surface,
        display: gpui::DisplayId,
        shapes: Arc<[Shape]>,
        cx: &mut App,
    ) -> anyhow::Result<AnyWindowHandle> {
        use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
        let m = surface.monitor.rect;
        let b = surface.bounds;
        let kind = WindowKind::LayerShell(LayerShellOptions {
            namespace: "screencapture-recording".into(),
            layer: Layer::Overlay,
            anchor: Anchor::TOP | Anchor::LEFT,
            // Placed from the screen's corner, not pushed aside by panels.
            exclusive_zone: Some(px(-1.)),
            margin: Some((px((b.y - m.y) as f32), px(0.), px(0.), px((b.x - m.x) as f32))),
            keyboard_interactivity: KeyboardInteractivity::None,
            ..LayerShellOptions::default()
        });
        let options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                Point::default(),
                size(px(b.w as f32), px(b.h as f32)),
            ))),
            kind,
            focus: false,
            show: true,
            is_movable: false,
            is_resizable: false,
            display_id: Some(display),
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some("screencapture".into()),
            ..WindowOptions::default()
        };
        let surface = surface.clone();
        let handle = cx.open_window(options, move |window, cx| {
            // Every click goes through to what is underneath.
            window.set_input_region(Some(&[]));
            cx.new(|_| FrameView {
                surface,
                shapes,
                texts: HashMap::new(),
            })
        })?;
        Ok(handle.into())
    }

    struct FrameView {
        surface: Surface,
        shapes: Arc<[Shape]>,
        texts: HashMap<tools::TextKey, Arc<RenderImage>>,
    }

    impl Render for FrameView {
        fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let b = self.surface.bounds;
            let mut root = div().size_full().relative();
            // Drawings first, the frame on top.
            let origin = (b.x, b.y);
            let scale = window.scale_factor();
            for shape in self.shapes.iter() {
                if shape.tool == Tool::Text {
                    let r = shape_rect(shape);
                    if r.intersect(b).is_none() {
                        continue;
                    }
                    let key = tools::TextKey::new(shape, scale);
                    let image = self
                        .texts
                        .entry(key)
                        .or_insert_with_key(tools::TextKey::render)
                        .clone();
                    root = root.child(
                        img(ImageSource::Render(image))
                            .absolute()
                            .left(px((r.x - b.x) as f32))
                            .top(px((r.y - b.y) as f32))
                            .w(px(r.w as f32))
                            .h(px(r.h as f32))
                            .object_fit(ObjectFit::Fill),
                    );
                } else {
                    let shape = shape.clone();
                    root = root.child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, (), window, _| {
                                tools::paint_shapes(std::slice::from_ref(&shape), origin, bounds, window);
                            },
                        )
                        .absolute()
                        .size_full(),
                    );
                }
            }
            for strip in &self.surface.strips {
                root = root.child(
                    div()
                        .absolute()
                        .left(px(strip.x as f32))
                        .top(px(strip.y as f32))
                        .w(px(strip.w as f32))
                        .h(px(strip.h as f32))
                        .bg(RED),
                );
            }
            root
        }
    }
}
