//! Drawing UI: the tool row, palette and colour picker, width stepper, and painting shapes.
//! Built only while the overlay is up; the picker only while it is open.

use std::sync::Arc;

use gpui::{
    App, Bounds, CursorStyle, Div, PathBuilder, PathStyle, Pixels, RenderImage, SharedString, Stateful,
    StrokeOptions, Window, div, hsla, point, prelude::*, px, size,
};

use screencapture_core::overlay::{
    feature::{Act, Effect},
    panel, render_image, separator,
    widgets::{button, color},
};

use super::editing::Editing;
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
        Arc::new(render_image(image::Frame::new(image)))
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

/// The drawing tools' row: tools, colours, text controls, width, undo and delete.
pub fn drawing_row(act: &Act, bar: &DrawBar) -> Div {
    let delete = bar
        .has_selection
        .then(|| button("delete", "🗑 Del", false).on_click(edit(act, Editing::delete_selected)));
    panel()
        .children(tool_buttons(act, bar))
        .child(separator())
        .children(swatches(act, bar))
        .child(button("picker", "＋", bar.picker.is_some()).on_click(edit(act, Editing::toggle_picker)))
        .when(bar.text, |row| {
            row.child(separator()).children(text_controls(act, bar))
        })
        .child(separator())
        .children(width_stepper(act, bar.width))
        .child(separator())
        .child(button("undo", "↶", false).on_click(edit(act, Editing::undo)))
        .children(delete)
}

/// One button per tool, with its key.
fn tool_buttons(act: &Act, bar: &DrawBar) -> Vec<Stateful<Div>> {
    let button_for = |(i, tool): (usize, Tool)| {
        let label = format!("{} {}", glyph(tool), i + 1);
        let picked = button(format!("tool-{i}"), label, bar.tool == Some(tool));
        picked.on_click(edit(act, move |e| e.toggle_tool(tool)))
    };
    Tool::ALL.into_iter().enumerate().map(button_for).collect()
}

const fn glyph(tool: Tool) -> &'static str {
    match tool {
        Tool::Rectangle => "□",
        Tool::Ellipse => "○",
        Tool::Line => "╱",
        Tool::Arrow => "↗",
        Tool::Freehand => "✎",
        Tool::Counter => "①",
        Tool::Text => "T",
        Tool::Redact => "▮",
    }
}

/// The palette's colours, and the custom one if it isn't among them.
fn swatches(act: &Act, bar: &DrawBar) -> Vec<Stateful<Div>> {
    let custom = bar.custom.filter(|c| !bar.palette.contains(c));
    let ids = (0..bar.palette.len())
        .map(|i| format!("color-{i}"))
        .chain(["color-custom".into()]);
    let colours = bar.palette.iter().copied().chain(custom);
    let swatch_for =
        |(id, c): (String, Color)| swatch(id, c, c == bar.color).on_click(edit(act, move |e| e.set_color(c)));
    ids.zip(colours).map(swatch_for).collect()
}

/// Which colour the palette sets, and whether the box has a background.
fn text_controls(act: &Act, bar: &DrawBar) -> [Stateful<Div>; 3] {
    let filled = bar.fill.is_some();
    let ink =
        button("ink", "A text", bar.target == Target::Ink).on_click(edit(act, |e| e.set_target(Target::Ink)));
    let fill = button("fill-target", "▇ fill", bar.target == Target::Fill);
    let fill = fill.on_click(edit(act, |e| e.set_target(Target::Fill)));
    let toggle = button("fill", if filled { "Fill on" } else { "Fill off" }, filled);
    [ink, fill, toggle.on_click(edit(act, Editing::toggle_fill))]
}

/// Width (font size for text) stepper; the mouse wheel does the same.
fn width_stepper(act: &Act, width: f32) -> [gpui::AnyElement; 3] {
    let shown = div()
        .w(px(44.))
        .flex()
        .justify_center()
        .child(format!("{width} px"));
    [
        button("thinner", "−", false)
            .on_click(edit(act, |e| e.step_width(-1.)))
            .into_any_element(),
        shown.into_any_element(),
        button("thicker", "+", false)
            .on_click(edit(act, |e| e.step_width(1.)))
            .into_any_element(),
    ]
}

/// Hues across the picker's strip.
const HUES: usize = 36;
const SATURATIONS: [f32; 10] = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.];
const VALUES: [f32; 6] = [1., 0.9, 0.78, 0.65, 0.5, 0.35];

/// Hue strip and a saturation × brightness grid for that hue.
pub fn picker(act: &Act, hue: f32, current: Color) -> Div {
    let current_shown = div()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .child(div().size(px(14.)).rounded_sm().bg(color(current)))
        .child(annotate::to_hex(current));
    panel()
        .flex_col()
        .items_start()
        .p_2()
        .child(hue_strip(act, hue))
        .child(shade_grid(act, hue, current))
        .child(current_shown)
}

/// Every hue; a click shows its shades.
fn hue_strip(act: &Act, hue: f32) -> Div {
    let cell = |i: usize| {
        let h = i as f32 * 360. / HUES as f32;
        div()
            .id(SharedString::from(format!("hue-{i}")))
            .w(px(9.))
            .h(px(18.))
            .cursor(CursorStyle::PointingHand)
            .bg(color(annotate::hsv(h, 1., 1.)))
            .when((h - hue).abs() < 1., |d| d.border_1().border_color(gpui::white()))
            .on_click(edit(act, move |e| e.set_picker_hue(h)))
    };
    div().flex().children((0..HUES).map(cell))
}

/// `hue`'s shades: brightness down, saturation across; a click picks one.
fn shade_grid(act: &Act, hue: f32, current: Color) -> Div {
    let cell = |r: usize, c: usize, pick: Color| {
        div()
            .id(SharedString::from(format!("sv-{r}-{c}")))
            .w(px(32.4))
            .h(px(18.))
            .cursor(CursorStyle::PointingHand)
            .bg(color(pick))
            .when(pick == current, |d| d.border_2().border_color(gpui::white()))
            .on_click(edit(act, move |e| e.pick_custom(pick)))
    };
    let line = |(r, v): (usize, f32)| {
        let shades = SATURATIONS.into_iter().enumerate();
        div()
            .flex()
            .children(shades.map(|(c, s)| cell(r, c, annotate::hsv(hue, s, v))))
    };
    div()
        .flex()
        .flex_col()
        .children(VALUES.into_iter().enumerate().map(line))
}

/// Paint `shapes` (global coordinates) into a window whose top-left is `origin`. Text boxes are
/// shown as images instead (see [`TextKey`]).
pub fn paint_shapes(shapes: &[Shape], origin: (i32, i32), bounds: Bounds<Pixels>, window: &mut Window) {
    let place = Place { origin, bounds };
    for shape in shapes {
        if shape.tool == Tool::Redact {
            let (l, t, r, b) = shape.bounds();
            let quad = Bounds::new(place.at((l, t)), size(px(r - l), px(b - t)));
            window.paint_quad(gpui::fill(quad, color(annotate::REDACTED)));
        }
        for stroke in shape.strokes() {
            for line in &stroke.lines {
                paint_line(line, &stroke, place, window);
            }
        }
    }
}

/// Where desktop points land in a window whose top-left is `origin`, painting in `bounds`.
#[derive(Clone, Copy)]
struct Place {
    origin: (i32, i32),
    bounds: Bounds<Pixels>,
}

impl Place {
    fn at(self, (x, y): annotate::Point) -> gpui::Point<Pixels> {
        let (left, top) = (x - self.origin.0 as f32, y - self.origin.1 as f32);
        point(self.bounds.origin.x + px(left), self.bounds.origin.y + px(top))
    }
}

/// One polyline of `stroke`, round-capped; a single point is a dot.
fn paint_line(line: &[annotate::Point], stroke: &annotate::Stroke, place: Place, window: &mut Window) {
    use lyon::tessellation::{LineCap, LineJoin};
    let Some((&first, rest)) = line.split_first() else {
        return;
    };
    let options = StrokeOptions::default()
        .with_line_width(stroke.width)
        .with_line_cap(LineCap::Round)
        .with_line_join(LineJoin::Round);
    let mut path = PathBuilder::stroke(px(stroke.width)).with_style(PathStyle::Stroke(options));
    path.move_to(place.at(first));
    if rest.is_empty() {
        // A dot: a zero-length segment would tessellate to nothing.
        path.line_to(place.at((first.0 + 0.01, first.1)));
    }
    for &p in rest {
        path.line_to(place.at(p));
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color(stroke.color));
    }
}
