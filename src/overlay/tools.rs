//! Drawing UI: the tool row, palette and colour picker, width stepper, and painting shapes.
//! Built only while the overlay is up; the picker only while it is open.

use std::sync::Arc;

use gpui::{
    App, Bounds, CursorStyle, Div, PathBuilder, PathStyle, Pixels, RenderImage, SharedString, Stateful,
    StrokeOptions, Window, div, hsla, point, prelude::*, px, size,
};

use super::{
    editing::Editing,
    feature::{Act, Effect},
    widgets::{button, color},
};
use crate::annotate::{self, Color, Shape, Target, Tool};

/// Everything a rendered text box depends on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextKey {
    text: String,
    size: u32,
    color: Color,
    fill: Option<Color>,
    w: u32,
    h: u32,
}

impl TextKey {
    /// For `shape` shown at `scale` device pixels per logical pixel.
    pub fn new(shape: &Shape, scale: f32) -> Self {
        let (l, t, r, b) = shape.bounds();
        Self {
            text: shape.text.clone(),
            size: (shape.width * scale).to_bits(),
            color: shape.color,
            fill: shape.fill,
            w: ((r - l) * scale).round() as u32,
            h: ((b - t) * scale).round() as u32,
        }
    }

    /// The box rasterised exactly as it will be saved, ready for gpui (BGRA).
    pub fn render(&self) -> Arc<RenderImage> {
        let mut image = crate::text::render(
            &self.text,
            f32::from_bits(self.size),
            self.color,
            self.fill,
            self.w,
            self.h,
        );
        for px in image.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        Arc::new(super::render_image(image::Frame::new(image)))
    }
}

/// What the drawing row shows, read from the overlay before building it.
pub struct DrawBar {
    pub tool: Option<Tool>,
    pub color: Color,
    pub width: f32,
    pub has_selection: bool,
    pub palette: Vec<Color>,
    pub custom: Option<Color>,
    /// Hue of the open colour picker.
    pub picker: Option<f32>,
    /// Text tool in hand or a text box selected: show the text controls.
    pub text: bool,
    pub fill: Option<Color>,
    pub target: Target,
}

/// Click handler changing the drawing tools with `change`.
fn edit(
    act: &Act,
    change: impl Fn(&mut Editing) + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    act.on(move |editing: &mut Editing, _| {
        change(editing);
        Effect::Repaint
    })
}

fn swatch(id: String, c: Color, selected: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .size(px(20.))
        .mx(px(2.))
        .rounded_full()
        .cursor(CursorStyle::PointingHand)
        .bg(color(c))
        .border_2()
        .border_color(if selected {
            gpui::white()
        } else {
            hsla(0., 0., 1., 0.15)
        })
}

pub fn drawing_row(act: &Act, bar: &DrawBar) -> Div {
    let mut row = super::panel();
    for (i, tool) in Tool::ALL.into_iter().enumerate() {
        let glyph = match tool {
            Tool::Rectangle => "□",
            Tool::Ellipse => "○",
            Tool::Line => "╱",
            Tool::Arrow => "↗",
            Tool::Freehand => "✎",
            Tool::Counter => "①",
            Tool::Text => "T",
            Tool::Redact => "▮",
        };
        row = row.child(
            button(
                format!("tool-{i}"),
                format!("{glyph} {}", i + 1),
                bar.tool == Some(tool),
            )
            .on_click(edit(act, move |e| e.toggle_tool(tool))),
        );
    }
    row = row.child(super::separator());
    for (i, &c) in bar.palette.iter().enumerate() {
        row = row.child(
            swatch(format!("color-{i}"), c, c == bar.color).on_click(edit(act, move |e| e.set_color(c))),
        );
    }
    if let Some(c) = bar.custom.filter(|c| !bar.palette.contains(c)) {
        row = row.child(
            swatch("color-custom".into(), c, c == bar.color).on_click(edit(act, move |e| e.set_color(c))),
        );
    }
    row = row
        .child(button("picker", "＋", bar.picker.is_some()).on_click(edit(act, Editing::toggle_picker)))
        .when(bar.text, |row| {
            // Which colour the palette sets, and whether the box has a background.
            row.child(super::separator())
                .child(
                    button("ink", "A text", bar.target == Target::Ink)
                        .on_click(edit(act, |e| e.set_target(Target::Ink))),
                )
                .child(
                    button("fill-target", "▇ fill", bar.target == Target::Fill)
                        .on_click(edit(act, |e| e.set_target(Target::Fill))),
                )
                .child(
                    button(
                        "fill",
                        if bar.fill.is_some() { "Fill on" } else { "Fill off" },
                        bar.fill.is_some(),
                    )
                    .on_click(edit(act, Editing::toggle_fill)),
                )
        })
        .child(super::separator())
        // Width (font size for text) stepper; the mouse wheel does the same.
        .child(button("thinner", "−", false).on_click(edit(act, |e| e.step_width(-1.))))
        .child(
            div()
                .w(px(44.))
                .flex()
                .justify_center()
                .child(format!("{} px", bar.width)),
        )
        .child(button("thicker", "+", false).on_click(edit(act, |e| e.step_width(1.))))
        .child(super::separator())
        .child(button("undo", "↶", false).on_click(edit(act, Editing::undo)));
    if bar.has_selection {
        row = row.child(button("delete", "🗑 Del", false).on_click(edit(act, Editing::delete_selected)));
    }
    row
}

/// Hue strip and a saturation × brightness grid for that hue.
pub fn picker(act: &Act, hue: f32, current: Color) -> Div {
    const HUES: usize = 36;
    const SATURATIONS: [f32; 10] = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.];
    const VALUES: [f32; 6] = [1., 0.9, 0.78, 0.65, 0.5, 0.35];
    let mut hues = div().flex();
    for i in 0..HUES {
        let h = i as f32 * 360. / HUES as f32;
        let chosen = (h - hue).abs() < 1.;
        hues = hues.child(
            div()
                .id(SharedString::from(format!("hue-{i}")))
                .w(px(9.))
                .h(px(18.))
                .cursor(CursorStyle::PointingHand)
                .bg(color(annotate::hsv(h, 1., 1.)))
                .when(chosen, |d| d.border_1().border_color(gpui::white()))
                .on_click(edit(act, move |e| e.set_picker_hue(h))),
        );
    }
    let mut grid = div().flex().flex_col();
    for (r, v) in VALUES.into_iter().enumerate() {
        let mut line = div().flex();
        for (c, s) in SATURATIONS.into_iter().enumerate() {
            let pick = annotate::hsv(hue, s, v);
            line = line.child(
                div()
                    .id(SharedString::from(format!("sv-{r}-{c}")))
                    .w(px(32.4))
                    .h(px(18.))
                    .cursor(CursorStyle::PointingHand)
                    .bg(color(pick))
                    .when(pick == current, |d| d.border_2().border_color(gpui::white()))
                    .on_click(edit(act, move |e| e.pick_custom(pick))),
            );
        }
        grid = grid.child(line);
    }
    super::panel()
        .flex_col()
        .items_start()
        .p_2()
        .child(hues)
        .child(grid)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .child(div().size(px(14.)).rounded_sm().bg(color(current)))
                .child(annotate::to_hex(current)),
        )
}

/// Paint `shapes` (global coordinates) into a window whose top-left is `origin`. Text boxes are
/// shown as images instead (see [`TextKey`]).
pub fn paint_shapes(shapes: &[Shape], origin: (i32, i32), bounds: Bounds<Pixels>, window: &mut Window) {
    use lyon::tessellation::{LineCap, LineJoin};
    let to_local = |(x, y): annotate::Point| {
        point(
            bounds.origin.x + px(x - origin.0 as f32),
            bounds.origin.y + px(y - origin.1 as f32),
        )
    };
    for shape in shapes {
        if shape.tool == Tool::Redact {
            let (l, t, r, b) = shape.bounds();
            let quad = Bounds::new(to_local((l, t)), size(px(r - l), px(b - t)));
            window.paint_quad(gpui::fill(quad, color(annotate::REDACTED)));
        }
        for stroke in shape.strokes() {
            for line in &stroke.lines {
                let Some((&first, rest)) = line.split_first() else {
                    continue;
                };
                let options = StrokeOptions::default()
                    .with_line_width(stroke.width)
                    .with_line_cap(LineCap::Round)
                    .with_line_join(LineJoin::Round);
                let mut path = PathBuilder::stroke(px(stroke.width)).with_style(PathStyle::Stroke(options));
                path.move_to(to_local(first));
                if rest.is_empty() {
                    // A dot: a zero-length segment would tessellate to nothing.
                    path.line_to(to_local((first.0 + 0.01, first.1)));
                }
                for &p in rest {
                    path.line_to(to_local(p));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color(stroke.color));
                }
            }
        }
    }
}
