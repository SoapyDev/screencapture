//! Shapes drawn over the frozen screens, editing them, and burning them into the saved image.
//! Points are global logical desktop coordinates, as `f32` so freehand strokes stay smooth.
//!
//! Nothing here runs until the user first draws: an empty [`Annotations`] is a few words.

use image::RgbaImage;

pub type Point = (f32, f32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Freehand,
    /// Numbered step marker: a filled disc with a white number, counting up per click.
    Counter,
    /// A box to type in, optionally on a filled background.
    Text,
    /// Destructive: the saved pixels under the box become opaque black.
    Redact,
}

impl Tool {
    pub const ALL: [Self; 8] = [
        Self::Rectangle,
        Self::Ellipse,
        Self::Line,
        Self::Arrow,
        Self::Freehand,
        Self::Counter,
        Self::Text,
        Self::Redact,
    ];

    /// Shapes drawn from a box (two corners) that are hit anywhere inside.
    pub const fn is_box(self) -> bool {
        matches!(self, Self::Text | Self::Redact)
    }
}

/// Which colour the palette sets for text: the letters or the box behind them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Target {
    #[default]
    Ink,
    Fill,
}

/// Straight-alpha RGBA.
pub type Color = [u8; 4];

/// Built-in palette (pastels, then white and black), used unless the config file names its own.
pub const PASTELS: [Color; 8] = [
    [0xFF, 0x8A, 0x80, 0xFF], // soft red
    [0xFF, 0xB8, 0x6C, 0xFF], // orange
    [0xFF, 0xE0, 0x82, 0xFF], // yellow
    [0x8E, 0xE4, 0xA1, 0xFF], // green
    [0x8A, 0xB8, 0xFF, 0xFF], // blue
    [0xC5, 0xA3, 0xFF, 0xFF], // purple
    [0xFF, 0xFF, 0xFF, 0xFF], // white
    [0x00, 0x00, 0x00, 0xFF], // black
];

pub const MIN_WIDTH: f32 = 1.;
pub const MAX_WIDTH: f32 = 40.;
pub const DEFAULT_WIDTH: f32 = 4.;
pub const MIN_TEXT_SIZE: f32 = 8.;
pub const MAX_TEXT_SIZE: f32 = 200.;
pub const DEFAULT_TEXT_SIZE: f32 = 24.;
/// Width of the box a click with the Text tool makes.
pub const DEFAULT_TEXT_BOX: f32 = 300.;
/// Opaque redaction colour.
pub const REDACTED: Color = [0, 0, 0, 0xFF];
/// How far (logical px) beyond a stroke or handle a press still grabs it.
pub const GRAB_SLOP: f32 = 6.;

/// `#RRGGBB` or `#RRGGBBAA` (the `#` is optional).
pub fn parse_color(text: &str) -> Option<Color> {
    let hex = text.trim();
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if !hex.is_ascii() || !matches!(hex.len(), 6 | 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    let alpha = if hex.len() == 8 { byte(6)? } else { 0xFF };
    Some([byte(0)?, byte(2)?, byte(4)?, alpha])
}

/// Opaque colour from hue (degrees), saturation and value (0..=1), for the colour picker.
pub fn hsv(h: f32, s: f32, v: f32) -> Color {
    let channel = |n: f32| {
        let k = (n + h / 60.) % 6.;
        let c = (v * s).mul_add(-k.min(4. - k).clamp(0., 1.), v);
        (c * 255.).round() as u8
    };
    [channel(5.), channel(3.), channel(1.), 0xFF]
}

pub fn to_hex(c: Color) -> String {
    let [r, g, b, a] = c;
    if a == 0xFF {
        format!("#{r:02X}{g:02X}{b:02X}")
    } else {
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shape {
    pub tool: Tool,
    /// Start and end for the two-point tools; every sample for freehand; the centre for a counter.
    pub points: Vec<Point>,
    pub color: Color,
    /// Stroke width in logical pixels; sets a counter's size.
    pub width: f32,
    /// A counter's number.
    pub number: u32,
    /// A text box's contents; its font size (px) is `width`.
    pub text: String,
    /// A text box's background.
    pub fill: Option<Color>,
}

/// Lines stroked together in one colour and width (round caps and joins). A one-point line is
/// a dot, which is how filled discs are drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub lines: Vec<Vec<Point>>,
    pub width: f32,
    pub color: Color,
}

impl Shape {
    /// Outline geometry of the drawing tools (a counter has none: see [`Self::strokes`]).
    pub fn polylines(&self) -> Vec<Vec<Point>> {
        let (a, b) = match self.points[..] {
            [] => return Vec::new(),
            [only] => (only, only),
            [a, .., b] => (a, b),
        };
        match self.tool {
            Tool::Counter | Tool::Text | Tool::Redact => Vec::new(),
            Tool::Freehand => vec![self.points.clone()],
            Tool::Line => vec![vec![a, b]],
            Tool::Rectangle => {
                let (l, t, r, bt) = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
                vec![vec![(l, t), (r, t), (r, bt), (l, bt), (l, t)]]
            }
            Tool::Ellipse => {
                let (cx, cy) = (f32::midpoint(a.0, b.0), f32::midpoint(a.1, b.1));
                let (rx, ry) = ((a.0 - b.0).abs() / 2., (a.1 - b.1).abs() / 2.);
                vec![ellipse((cx, cy), rx, ry)]
            }
            Tool::Arrow => {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let len = dx.hypot(dy);
                if len < f32::EPSILON {
                    return vec![vec![a, b]];
                }
                let head = 4.0_f32.mul_add(self.width, 6.).min(len);
                let (ux, uy) = (dx / len, dy / len);
                let (sin, cos) = 28.0_f32.to_radians().sin_cos();
                let barb = |side: f32| {
                    // Unit vector rotated by ±28°, pointing back from the tip.
                    let (rx, ry) = (
                        (side * uy).mul_add(-sin, ux * cos),
                        f32::mul_add(uy, cos, side * ux * sin),
                    );
                    (head.mul_add(-rx, b.0), head.mul_add(-ry, b.1))
                };
                vec![vec![a, b], vec![barb(1.), b, barb(-1.)]]
            }
        }
    }

    /// Radius of a counter's disc.
    pub const fn radius(&self) -> f32 {
        1.5_f32.mul_add(self.width, 8.)
    }

    /// Everything to draw, in order. The overlay and the saved image both draw from this, so
    /// they match.
    pub fn strokes(&self) -> Vec<Stroke> {
        if self.tool.is_box() {
            return Vec::new();
        }
        if self.tool != Tool::Counter {
            return vec![Stroke {
                lines: self.polylines(),
                width: self.width,
                color: self.color,
            }];
        }
        let Some(&centre) = self.points.first() else {
            return Vec::new();
        };
        let r = self.radius();
        let (lines, width) = number_strokes(self.number, centre, r);
        vec![
            Stroke {
                lines: vec![vec![centre]],
                width: r * 2.,
                color: self.color,
            },
            Stroke {
                lines,
                width,
                color: [0xFF; 4],
            },
        ]
    }

    /// Distance from `p` to the drawn shape's centre line (0 inside a counter's disc).
    pub fn distance(&self, p: Point) -> f32 {
        if self.tool == Tool::Counter {
            return self
                .points
                .first()
                .map_or(f32::MAX, |&c| (dist(p, c) - self.radius()).max(0.));
        }
        if self.tool.is_box() {
            let (l, t, r, b) = self.bounds();
            return (l - p.0)
                .max(p.0 - r)
                .max(0.)
                .hypot((t - p.1).max(p.1 - b).max(0.));
        }
        self.polylines()
            .iter()
            .flat_map(|line| {
                let dot = (line.len() == 1).then(|| (line[0], line[0]));
                line.windows(2).map(|s| (s[0], s[1])).chain(dot)
            })
            .map(|(a, b)| segment_distance(p, a, b))
            .fold(f32::MAX, f32::min)
    }

    /// Points that can be dragged to reshape: line/arrow ends, box corners (rectangle, ellipse,
    /// freehand: top-left, top-right, bottom-right, bottom-left). Counters only move.
    pub fn handles(&self) -> Vec<Point> {
        match self.tool {
            Tool::Counter => Vec::new(),
            Tool::Line | Tool::Arrow => self.points.clone(),
            Tool::Rectangle | Tool::Ellipse | Tool::Freehand | Tool::Text | Tool::Redact => {
                let (l, t, r, b) = self.bounds();
                vec![(l, t), (r, t), (r, b), (l, b)]
            }
        }
    }

    /// This shape with handle `i` dragged to `p`.
    pub fn with_handle(&self, i: usize, p: Point) -> Self {
        let mut shape = self.clone();
        match self.tool {
            Tool::Counter => {}
            Tool::Line | Tool::Arrow => shape.points[i] = p,
            Tool::Rectangle | Tool::Ellipse | Tool::Text | Tool::Redact => {
                shape.points = vec![self.handles()[(i + 2) % 4], p];
            }
            Tool::Freehand => {
                // Scale about the opposite corner so it stays put.
                let corners = self.handles();
                let (o, c) = (corners[(i + 2) % 4], corners[i]);
                let factor = |to: f32, from: f32, fixed: f32| {
                    if (from - fixed).abs() < f32::EPSILON {
                        1.
                    } else {
                        (to - fixed) / (from - fixed)
                    }
                };
                let (fx, fy) = (factor(p.0, c.0, o.0), factor(p.1, c.1, o.1));
                for q in &mut shape.points {
                    *q = ((q.0 - o.0).mul_add(fx, o.0), (q.1 - o.1).mul_add(fy, o.1));
                }
            }
        }
        shape
    }

    /// Bounding box (left, top, right, bottom) of the points.
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        bounds(&self.points)
    }

    pub fn translated(&self, dx: f32, dy: f32) -> Self {
        let mut shape = self.clone();
        for q in &mut shape.points {
            *q = (q.0 + dx, q.1 + dy);
        }
        shape
    }
}

fn dist(a: Point, b: Point) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Bounding box (left, top, right, bottom).
fn bounds(points: &[Point]) -> (f32, f32, f32, f32) {
    points.iter().fold(
        (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
        |(l, t, r, b), &(x, y)| (l.min(x), t.min(y), r.max(x), b.max(y)),
    )
}

/// Stroke-font digits in a unit box (x and y in 0..=1, y down), so the number looks the same
/// on screen and in the file without a font rasteriser.
fn digit(d: u32) -> Vec<Vec<Point>> {
    let poly = |pts: &[Point]| pts.to_vec();
    match d {
        0 => vec![ellipse((0.5, 0.5), 0.5, 0.5)],
        1 => vec![poly(&[(0.2, 0.25), (0.6, 0.), (0.6, 1.)])],
        2 => vec![poly(&[
            (0.05, 0.25),
            (0.2, 0.06),
            (0.5, 0.),
            (0.8, 0.06),
            (0.95, 0.27),
            (0.85, 0.48),
            (0.05, 1.),
            (1., 1.),
        ])],
        3 => vec![poly(&[
            (0.05, 0.1),
            (0.35, 0.),
            (0.7, 0.02),
            (0.92, 0.2),
            (0.85, 0.4),
            (0.45, 0.48),
            (0.85, 0.56),
            (0.97, 0.76),
            (0.8, 0.96),
            (0.45, 1.),
            (0.05, 0.9),
        ])],
        4 => vec![poly(&[(0.75, 1.), (0.75, 0.), (0., 0.7), (1., 0.7)])],
        5 => vec![poly(&[
            (0.9, 0.),
            (0.15, 0.),
            (0.08, 0.45),
            (0.45, 0.38),
            (0.8, 0.46),
            (0.97, 0.7),
            (0.85, 0.92),
            (0.5, 1.),
            (0.05, 0.9),
        ])],
        6 => vec![poly(&[
            (0.85, 0.04),
            (0.55, 0.),
            (0.25, 0.12),
            (0.05, 0.45),
            (0.05, 0.75),
            (0.25, 0.98),
            (0.6, 1.),
            (0.9, 0.85),
            (0.95, 0.62),
            (0.75, 0.45),
            (0.45, 0.42),
            (0.15, 0.55),
        ])],
        7 => vec![poly(&[(0., 0.), (1., 0.), (0.35, 1.)])],
        8 => vec![ellipse((0.5, 0.25), 0.4, 0.25), ellipse((0.5, 0.73), 0.5, 0.27)],
        _ => digit(6)
            .into_iter()
            .map(|l| l.into_iter().map(|(x, y)| (1. - x, 1. - y)).collect())
            .collect(),
    }
}

/// The number's strokes centred on `centre`, sized to sit inside a disc of radius `r`; returns
/// them with their stroke width.
fn number_strokes(number: u32, centre: Point, r: f32) -> (Vec<Vec<Point>>, f32) {
    const ASPECT: f32 = 0.6; // digit width / height
    const GAP: f32 = 0.2; // between digits, in heights
    let digits: Vec<u32> = number
        .to_string()
        .chars()
        .filter_map(|c| c.to_digit(10))
        .collect();
    let n = digits.len() as f32;
    let total = n.mul_add(ASPECT, (n - 1.) * GAP); // in heights
    // As tall as looks right, but with the corners (plus the stroke) inside the disc.
    let height = (0.9 * r).min(0.86 * r / (total / 2.).hypot(0.5));
    let width = height * 0.2;
    let left = total.mul_add(-height / 2., centre.0);
    let top = centre.1 - height / 2.;
    let lines = digits
        .iter()
        .enumerate()
        .flat_map(|(i, &d)| {
            let x0 = (i as f32 * (ASPECT + GAP)).mul_add(height, left);
            digit(d).into_iter().map(move |line| {
                line.into_iter()
                    .map(|(x, y)| ((x * ASPECT).mul_add(height, x0), y.mul_add(height, top)))
                    .collect()
            })
        })
        .collect();
    (lines, width)
}

/// Closed ring through `n` points; about one segment per 4 px keeps it smooth.
fn ellipse((cx, cy): Point, rx: f32, ry: f32) -> Vec<Point> {
    let n = ((std::f32::consts::TAU * rx.max(ry) / 4.) as usize).clamp(32, 256);
    let mut ring: Vec<Point> = (0..n)
        .map(|i| {
            let t = std::f32::consts::TAU * i as f32 / n as f32;
            (rx.mul_add(t.cos(), cx), ry.mul_add(t.sin(), cy))
        })
        .collect();
    ring.push(ring[0]);
    ring
}

/// What a press would grab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    /// A handle of the selected shape.
    Handle(usize),
    /// A shape's body (index into the shapes).
    Shape(usize),
}

#[derive(Clone, Debug)]
enum Edit {
    Move {
        start: Point,
        origin: Shape,
    },
    Handle {
        index: usize,
        start: Point,
        origin: Shape,
    },
}

/// The shapes of one capture, the one being drawn, and the one selected for editing.
pub struct Annotations {
    shapes: Vec<Shape>,
    current: Option<Shape>,
    selected: Option<usize>,
    edit: Option<Edit>,
    /// Earlier versions of `shapes`, for undo.
    history: Vec<Vec<Shape>>,
    /// Width changes on the same shape (scroll steps) share one undo step.
    coalescing_width: Option<usize>,
    /// Tool for new shapes; `None`: presses off every shape adjust the region instead.
    pub tool: Option<Tool>,
    /// Colour and width for new shapes.
    pub color: Color,
    pub width: f32,
    /// Text box being typed into.
    editing: Option<usize>,
    /// Font size and background for new text boxes.
    pub text_size: f32,
    pub fill_color: Color,
    pub fill_on: bool,
    /// What the palette colours for text.
    pub target: Target,
}

impl Default for Annotations {
    fn default() -> Self {
        Self {
            shapes: Vec::new(),
            current: None,
            selected: None,
            edit: None,
            history: Vec::new(),
            coalescing_width: None,
            tool: None,
            color: PASTELS[0],
            width: DEFAULT_WIDTH,
            editing: None,
            text_size: DEFAULT_TEXT_SIZE,
            fill_color: [0x1E, 0x1E, 0x22, 0xFF],
            fill_on: false,
            target: Target::Ink,
        }
    }
}

impl Annotations {
    /// Grab a handle or shape, or start a new shape if a tool is in hand. Returns whether the
    /// press was taken (otherwise it belongs to the region).
    pub fn press(&mut self, p: Point) -> bool {
        self.stop_editing();
        if let Some(grab) = self.grab_at(p) {
            let index = match grab {
                Grab::Handle(_) => self.selected.unwrap_or_default(),
                Grab::Shape(i) => i,
            };
            self.selected = Some(index);
            let origin = self.shapes[index].clone();
            self.edit = Some(match grab {
                Grab::Handle(handle) => Edit::Handle {
                    index: handle,
                    start: p,
                    origin,
                },
                Grab::Shape(_) => Edit::Move { start: p, origin },
            });
            self.checkpoint();
            return true;
        }
        self.selected = None;
        let Some(tool) = self.tool else { return false };
        let mut shape = Shape {
            tool,
            points: vec![p, p],
            color: self.color,
            width: self.width,
            ..Shape::default()
        };
        match tool {
            Tool::Freehand => shape.points = vec![p],
            Tool::Counter => (shape.points, shape.number) = (vec![p], self.next_number()),
            Tool::Text => {
                shape.width = self.text_size;
                shape.fill = self.fill_on.then_some(self.fill_color);
            }
            Tool::Redact => shape.color = REDACTED,
            _ => {}
        }
        self.current = Some(shape);
        true
    }

    pub fn motion(&mut self, p: Point) {
        if let Some(shape) = &mut self.current {
            match shape.tool {
                Tool::Freehand if shape.points.last() != Some(&p) => shape.points.push(p),
                Tool::Freehand => {}
                Tool::Counter => shape.points[0] = p,
                _ => shape.points[1] = p,
            }
        }
        if let (Some(edit), Some(i)) = (&self.edit, self.selected) {
            self.shapes[i] = match edit {
                Edit::Move { start, origin } => origin.translated(p.0 - start.0, p.1 - start.1),
                Edit::Handle { index, start, origin } => {
                    let h = origin.handles()[*index];
                    origin.with_handle(*index, (h.0 + p.0 - start.0, h.1 + p.1 - start.1))
                }
            };
        }
    }

    pub fn release(&mut self, p: Point) {
        self.motion(p);
        if self.edit.take().is_some() && self.history.last() == Some(&self.shapes) {
            // Only selected: nothing to undo.
            self.history.pop();
        }
        let Some(mut shape) = self.current.take() else {
            return;
        };
        // A click with a two-point tool is a slip, not a shape; with the Text tool, a box of the
        // default size.
        let tiny = match shape.points[..] {
            [a, b] => (a.0 - b.0).abs() < 2. && (a.1 - b.1).abs() < 2.,
            _ => false,
        };
        if tiny && shape.tool == Tool::Text {
            let a = shape.points[0];
            let h = shape
                .width
                .mul_add(crate::text::LINE_HEIGHT, 2. * shape.width * crate::text::PADDING);
            shape.points = vec![a, (a.0 + DEFAULT_TEXT_BOX, a.1 + h)];
        } else if tiny {
            return;
        }
        let text = shape.tool == Tool::Text;
        self.checkpoint();
        self.shapes.push(shape);
        self.selected = Some(self.shapes.len() - 1);
        if text {
            self.editing = self.selected;
        }
    }

    /// What a press at `p` would grab (for the pointer's look).
    pub fn grab_at(&self, p: Point) -> Option<Grab> {
        if let Some(shape) = self.selected() {
            let handle = shape.handles().iter().position(|&h| dist(h, p) <= GRAB_SLOP + 2.);
            if let Some(i) = handle {
                return Some(Grab::Handle(i));
            }
        }
        // Topmost first.
        (0..self.shapes.len())
            .rev()
            .find(|&i| {
                let s = &self.shapes[i];
                let reach = if s.tool.is_box() {
                    GRAB_SLOP
                } else {
                    s.width / 2. + GRAB_SLOP
                };
                s.distance(p) <= reach
            })
            .map(Grab::Shape)
    }

    /// Remember the shapes before a change, for undo.
    fn checkpoint(&mut self) {
        self.history.push(self.shapes.clone());
        self.coalescing_width = None;
    }

    fn next_number(&self) -> u32 {
        self.shapes
            .iter()
            .filter(|s| s.tool == Tool::Counter)
            .map(|s| s.number)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Drawing or editing in progress.
    pub const fn is_busy(&self) -> bool {
        self.current.is_some() || self.edit.is_some()
    }

    pub fn selected(&self) -> Option<&Shape> {
        self.selected.and_then(|i| self.shapes.get(i))
    }

    /// Returns whether something was selected.
    pub const fn deselect(&mut self) -> bool {
        self.selected.take().is_some()
    }

    pub fn delete_selected(&mut self) {
        if let Some(i) = self.selected.take() {
            self.editing = None;
            self.checkpoint();
            self.shapes.remove(i);
        }
    }

    /// Width shown in the stepper: the selected shape's, else the one for new shapes.
    pub fn current_width(&self) -> f32 {
        match self.selected() {
            Some(s) => s.width,
            None if self.tool == Some(Tool::Text) => self.text_size,
            None => self.width,
        }
    }

    /// Whether width controls currently mean font size.
    pub fn sizing_text(&self) -> bool {
        self.selected()
            .map_or_else(|| self.tool == Some(Tool::Text), |s| s.tool == Tool::Text)
    }

    /// Set the width for new shapes and the selected one, within [`MIN_WIDTH`, `MAX_WIDTH`].
    pub fn set_width(&mut self, width: f32) {
        let width = if self.sizing_text() {
            let size = width.clamp(MIN_TEXT_SIZE, MAX_TEXT_SIZE);
            self.text_size = size;
            size
        } else {
            let width = width.clamp(MIN_WIDTH, MAX_WIDTH);
            self.width = width;
            width
        };
        if let Some(i) = self.selected {
            if (self.shapes[i].width - width).abs() > f32::EPSILON && self.coalescing_width != Some(i) {
                self.checkpoint();
                self.coalescing_width = Some(i);
            }
            self.shapes[i].width = width;
        }
    }

    pub fn step_width(&mut self, delta: f32) {
        self.set_width(self.current_width() + delta);
    }

    /// Colour shown as chosen: the selected shape's, else the one for new shapes.
    pub fn current_color(&self) -> Color {
        if self.coloring_fill() {
            return self.current_fill().unwrap_or(self.fill_color);
        }
        self.selected().map_or(self.color, |s| s.color)
    }

    /// Whether the palette currently sets a text background.
    fn coloring_fill(&self) -> bool {
        self.target == Target::Fill && self.sizing_text()
    }

    /// Set the colour for new shapes and the selected one.
    pub fn set_color(&mut self, color: Color) {
        if self.coloring_fill() {
            self.fill_color = color;
            self.fill_on = true;
            if let Some(i) = self.selected
                && self.shapes[i].fill != Some(color)
            {
                self.checkpoint();
                self.shapes[i].fill = Some(color);
            }
            return;
        }
        self.color = color;
        if let Some(i) = self.selected
            && self.shapes[i].color != color
        {
            self.checkpoint();
            self.shapes[i].color = color;
        }
    }

    pub fn editing(&self) -> Option<&Shape> {
        self.editing.and_then(|i| self.shapes.get(i))
    }

    /// Type into the text box being edited.
    pub fn type_text(&mut self, text: &str) {
        if let Some(i) = self.editing {
            self.shapes[i].text.push_str(text);
        }
    }

    pub fn backspace(&mut self) {
        if let Some(i) = self.editing {
            self.shapes[i].text.pop();
        }
    }

    /// Remove the last word (and the spaces after it), like Ctrl+Backspace in an editor.
    pub fn delete_word(&mut self) {
        if let Some(i) = self.editing {
            let text = &mut self.shapes[i].text;
            let kept = text
                .trim_end()
                .trim_end_matches(|c: char| !c.is_whitespace())
                .len();
            text.truncate(kept);
        }
    }

    /// Type into the selected text box again. Returns whether it is now being edited.
    pub fn start_editing(&mut self) -> bool {
        match self.selected {
            Some(i) if self.shapes[i].tool == Tool::Text => {
                if self.editing != Some(i) {
                    self.checkpoint();
                    self.editing = Some(i);
                }
                true
            }
            _ => false,
        }
    }

    /// Done typing; an empty box goes away (with no undo step left behind).
    pub fn stop_editing(&mut self) {
        let Some(i) = self.editing.take() else { return };
        if self.shapes[i].text.is_empty() {
            self.shapes.remove(i);
            self.selected = None;
        }
        if self.history.last() == Some(&self.shapes) {
            self.history.pop();
        }
    }

    /// Background shown as chosen: the selected text box's, else the one for new boxes.
    pub fn current_fill(&self) -> Option<Color> {
        match self.selected() {
            Some(s) if s.tool == Tool::Text => s.fill,
            _ => self.fill_on.then_some(self.fill_color),
        }
    }

    /// Background on or off, for new text boxes and the selected one.
    pub fn toggle_fill(&mut self) {
        self.fill_on = self.current_fill().is_none();
        let fill = self.fill_on.then_some(self.fill_color);
        if let Some(i) = self.selected
            && self.shapes[i].tool == Tool::Text
        {
            self.checkpoint();
            self.shapes[i].fill = fill;
        }
    }

    pub fn undo(&mut self) {
        if self.current.take().is_some() {
            return;
        }
        self.edit = None;
        self.editing = None;
        self.selected = None;
        self.coalescing_width = None;
        if let Some(previous) = self.history.pop() {
            self.shapes = previous;
        }
    }

    /// Finished shapes, then the one in progress.
    pub fn shapes(&self) -> impl Iterator<Item = &Shape> {
        self.shapes.iter().chain(&self.current)
    }

    #[cfg(test)]
    pub const fn is_empty(&self) -> bool {
        self.shapes.is_empty() && self.current.is_none()
    }
}

/// Draw `shapes` onto `image`, whose top-left is the logical point `origin` and which has
/// `scale` pixels per logical pixel.
#[cfg(test)]
pub fn rasterize(image: &mut RgbaImage, shapes: &[Shape], origin: Point, scale: f32) {
    let clip = [0, 0, image.width(), image.height()];
    rasterize_clipped(image, shapes, origin, scale, clip);
}

/// [`rasterize`], touching only pixels in `clip` (`[x0, y0, x1, y1)`).
pub fn rasterize_clipped(image: &mut RgbaImage, shapes: &[Shape], origin: Point, scale: f32, clip: [u32; 4]) {
    let clip = [
        clip[0],
        clip[1],
        clip[2].min(image.width()),
        clip[3].min(image.height()),
    ];
    let to_pixels = |(x, y): Point| ((x - origin.0) * scale, (y - origin.1) * scale);
    for shape in shapes {
        match shape.tool {
            Tool::Redact => redact(image, shape, origin, scale, clip),
            Tool::Text => burn_text(image, shape, origin, scale, clip),
            _ => {}
        }
        for group in shape.strokes() {
            let lines: Vec<Vec<Point>> = group
                .lines
                .into_iter()
                .map(|l| l.into_iter().map(to_pixels).collect())
                .collect();
            stroke(image, &lines, group.width * scale / 2., group.color, clip);
        }
    }
}

/// A box shape's pixel rectangle `[x0, y0, x1, y1)`: every pixel it overlaps, within `clip`.
fn box_pixels(shape: &Shape, origin: Point, scale: f32, clip: [u32; 4]) -> [u32; 4] {
    let (l, t, r, b) = shape.bounds();
    let at = |v: f32, o: f32, lo: u32, hi: u32| ((v - o) * scale).clamp(lo as f32, hi as f32);
    [
        at(l, origin.0, clip[0], clip[2]).floor() as u32,
        at(t, origin.1, clip[1], clip[3]).floor() as u32,
        at(r, origin.0, clip[0], clip[2]).ceil() as u32,
        at(b, origin.1, clip[1], clip[3]).ceil() as u32,
    ]
}

/// Destructive: replace the pixels, no blending.
fn redact(image: &mut RgbaImage, shape: &Shape, origin: Point, scale: f32, clip: [u32; 4]) {
    let [x0, y0, x1, y1] = box_pixels(shape, origin, scale, clip);
    for y in y0..y1 {
        for x in x0..x1 {
            image.put_pixel(x, y, image::Rgba(REDACTED));
        }
    }
}

fn burn_text(image: &mut RgbaImage, shape: &Shape, origin: Point, scale: f32, clip: [u32; 4]) {
    let (l, t, r, b) = shape.bounds();
    let (x0, y0) = (((l - origin.0) * scale).round(), ((t - origin.1) * scale).round());
    let (w, h) = (((r - l) * scale).round() as u32, ((b - t) * scale).round() as u32);
    let text = crate::text::render(&shape.text, shape.width * scale, shape.color, shape.fill, w, h);
    for (dx, dy, p) in text.enumerate_pixels() {
        let (x, y) = (x0 + dx as f32, y0 + dy as f32);
        let inside = x >= clip[0] as f32 && y >= clip[1] as f32 && x < clip[2] as f32 && y < clip[3] as f32;
        if inside && p[3] > 0 {
            let dst = &mut image.get_pixel_mut(x as u32, y as u32).0;
            *dst = blend(*dst, [p[0], p[1], p[2], 255], f32::from(p[3]) / 255.);
        }
    }
}

/// Integer pixel box `[x0, x1) x [y0, y1)` around `points`, grown by `pad`, clipped to the image.
fn pixel_box(points: impl Iterator<Item = Point>, pad: f32, clip: [u32; 4]) -> Option<[u32; 4]> {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (x, y) in points {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let at = |v: f32, min: u32, max: u32| v.clamp(min as f32, max as f32) as u32;
    let b = [
        at((x0 - pad).floor(), clip[0], clip[2]),
        at((y0 - pad).floor(), clip[1], clip[3]),
        at((x1 + pad).ceil(), clip[0], clip[2]),
        at((y1 + pad).ceil(), clip[1], clip[3]),
    ];
    (b[0] < b[2] && b[1] < b[3]).then_some(b)
}

/// Anti-aliased stroke with round caps and joins: a pixel's coverage comes from the distance of
/// its centre to the nearest segment. Coverage is merged per shape before blending, so a
/// translucent stroke crossing itself doesn't get darker.
fn stroke(image: &mut RgbaImage, lines: &[Vec<Point>], half: f32, color: Color, size: [u32; 4]) {
    let pad = half + 1.;
    let Some([bx0, by0, bx1, by1]) = pixel_box(lines.iter().flatten().copied(), pad, size) else {
        return;
    };
    let bw = (bx1 - bx0) as usize;
    let mut mask = vec![0_f32; bw * (by1 - by0) as usize];
    for line in lines {
        let segments = line.windows(2).map(|s| (s[0], s[1]));
        let dot = (line.len() == 1).then(|| (line[0], line[0]));
        for (a, b) in segments.chain(dot) {
            let Some([x0, y0, x1, y1]) = pixel_box([a, b].into_iter(), pad, size) else {
                continue;
            };
            for py in y0.max(by0)..y1.min(by1) {
                for px in x0.max(bx0)..x1.min(bx1) {
                    let d = segment_distance((px as f32 + 0.5, py as f32 + 0.5), a, b);
                    let coverage = (half + 0.5 - d).clamp(0., 1.);
                    let m = &mut mask[(py - by0) as usize * bw + (px - bx0) as usize];
                    *m = m.max(coverage);
                }
            }
        }
    }
    for (i, &coverage) in mask.iter().enumerate() {
        if coverage > 0. {
            let (px, py) = (bx0 + (i % bw) as u32, by0 + (i / bw) as u32);
            let dst = &mut image.get_pixel_mut(px, py).0;
            *dst = blend(*dst, color, coverage);
        }
    }
}

fn segment_distance(p: Point, a: Point, b: Point) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let (apx, apy) = (p.0 - a.0, p.1 - a.1);
    let len2 = abx.mul_add(abx, aby * aby);
    let t = if len2 > 0. {
        (apx.mul_add(abx, apy * aby) / len2).clamp(0., 1.)
    } else {
        0.
    };
    f32::mul_add(t, -abx, apx).hypot(f32::mul_add(t, -aby, apy))
}

/// Straight-alpha source-over.
pub fn blend(dst: [u8; 4], src: Color, coverage: f32) -> [u8; 4] {
    let a = f32::from(src[3]) / 255. * coverage;
    let da = f32::from(dst[3]) / 255.;
    let out_a = da.mul_add(1. - a, a);
    if out_a <= 0. {
        return [0; 4];
    }
    let channel =
        |s: u8, d: u8| (f32::from(s).mul_add(a, f32::from(d) * da * (1. - a)) / out_a).round() as u8;
    [
        channel(src[0], dst[0]),
        channel(src[1], dst[1]),
        channel(src[2], dst[2]),
        (out_a * 255.).round() as u8,
    ]
}

/// The drawings' section of the config: the colours offered.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Palette {
    /// Drawing colours as `#RRGGBB` / `#RRGGBBAA`; the built-in pastels when absent.
    #[serde(default)]
    pub palette: Option<Vec<String>>,
    /// Last colour chosen in the picker.
    #[serde(default)]
    pub custom_color: Option<String>,
}

impl crate::config::Section for Palette {}

impl Palette {
    /// The configured colours that parse, else the built-in pastels.
    pub fn colors(&self) -> Vec<Color> {
        let parsed: Vec<Color> = self
            .palette
            .iter()
            .flatten()
            .filter_map(|c| parse_color(c))
            .collect();
        if parsed.is_empty() {
            PASTELS.to_vec()
        } else {
            parsed
        }
    }

    pub fn custom_color(&self) -> Option<Color> {
        self.custom_color.as_deref().and_then(parse_color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    const RED: Color = [255, 0, 0, 255];

    fn shape(tool: Tool, points: &[Point]) -> Shape {
        Shape {
            tool,
            points: points.to_vec(),
            color: RED,
            width: 2.,
            ..Shape::default()
        }
    }

    fn close(a: Point, b: Point) -> bool {
        (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01
    }

    #[test]
    fn rectangle_is_one_closed_outline_in_any_drag_direction() {
        let lines = shape(Tool::Rectangle, &[(30., 40.), (10., 20.)]).polylines();
        assert_eq!(
            lines,
            vec![vec![(10., 20.), (30., 20.), (30., 40.), (10., 40.), (10., 20.)]]
        );
    }

    #[test]
    fn ellipse_is_closed_and_fits_its_box() {
        let lines = shape(Tool::Ellipse, &[(0., 0.), (40., 20.)]).polylines();
        assert_eq!(lines.len(), 1);
        let ring = &lines[0];
        assert!(ring.len() > 16);
        assert!(close(ring[0], *ring.last().unwrap()));
        for &(x, y) in ring {
            let (nx, ny) = ((x - 20.) / 20., (y - 10.) / 10.);
            assert!(
                (f32::mul_add(ny, ny, nx * nx) - 1.).abs() < 0.01,
                "({x}, {y}) is off the ellipse"
            );
        }
    }

    #[test]
    fn line_is_its_two_points() {
        let lines = shape(Tool::Line, &[(1., 2.), (3., 4.)]).polylines();
        assert_eq!(lines, vec![vec![(1., 2.), (3., 4.)]]);
    }

    #[test]
    fn arrow_has_shaft_and_symmetric_head_at_the_end() {
        let lines = shape(Tool::Arrow, &[(0., 0.), (100., 0.)]).polylines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], vec![(0., 0.), (100., 0.)]);
        let head = &lines[1];
        assert_eq!(head.len(), 3);
        assert!(close(head[1], (100., 0.)), "head meets at the tip");
        assert!(head[0].0 < 100. && head[2].0 < 100., "barbs point back");
        assert!(close((head[0].0, -head[0].1), head[2]), "barbs mirror each other");
    }

    #[test]
    fn short_arrow_head_never_exceeds_shaft() {
        let lines = shape(Tool::Arrow, &[(0., 0.), (6., 0.)]).polylines();
        assert!(lines[1][0].0 >= 0. && lines[1][2].0 >= 0.);
    }

    #[test]
    fn freehand_is_its_samples() {
        let pts = [(0., 0.), (1., 1.), (2., 0.)];
        assert_eq!(shape(Tool::Freehand, &pts).polylines(), vec![pts.to_vec()]);
    }

    #[test]
    fn without_tool_presses_are_not_taken() {
        let mut a = Annotations::default();
        assert!(!a.press((1., 1.)));
        assert!(a.is_empty());
    }

    #[test]
    fn dragging_with_a_tool_adds_a_shape_with_current_style() {
        let mut a = Annotations {
            tool: Some(Tool::Rectangle),
            color: PASTELS[3],
            width: 8.,
            ..Default::default()
        };
        assert!(a.press((10., 10.)));
        a.motion((20., 15.));
        assert!(a.is_busy());
        assert_eq!(a.shapes().next().unwrap().points, vec![(10., 10.), (20., 15.)]);
        a.release((30., 40.));
        assert!(!a.is_busy());
        let shapes: Vec<_> = a.shapes().cloned().collect();
        assert_eq!(
            shapes,
            vec![Shape {
                tool: Tool::Rectangle,
                points: vec![(10., 10.), (30., 40.)],
                color: PASTELS[3],
                width: 8.,
                ..Shape::default()
            }]
        );
    }

    #[test]
    fn freehand_collects_every_sample() {
        let mut a = Annotations {
            tool: Some(Tool::Freehand),
            ..Default::default()
        };
        a.press((0., 0.));
        a.motion((1., 0.));
        a.motion((2., 1.));
        a.release((3., 1.));
        assert_eq!(
            a.shapes().next().unwrap().points,
            vec![(0., 0.), (1., 0.), (2., 1.), (3., 1.)]
        );
    }

    #[test]
    fn a_click_with_a_two_point_tool_draws_nothing_but_freehand_leaves_a_dot() {
        let mut a = Annotations {
            tool: Some(Tool::Ellipse),
            ..Default::default()
        };
        a.press((5., 5.));
        a.release((5.5, 5.));
        assert!(a.is_empty());
        a.tool = Some(Tool::Freehand);
        a.press((5., 5.));
        a.release((5., 5.));
        assert_eq!(a.shapes().count(), 1);
    }

    #[test]
    fn undo_removes_last_shape() {
        let mut a = Annotations {
            tool: Some(Tool::Line),
            ..Default::default()
        };
        for (x, y) in [(10., 0.), (20., 30.)] {
            a.press((0., y));
            a.release((x, y));
        }
        a.undo();
        assert_eq!(a.shapes().map(|s| s.points[1].0).collect::<Vec<_>>(), vec![10.]);
        a.undo();
        a.undo();
        assert!(a.is_empty());
    }

    fn white(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([255; 4]))
    }

    #[test]
    fn line_is_burnt_in_with_its_width() {
        let mut img = white(20, 20);
        let line = Shape {
            width: 4.,
            ..shape(Tool::Line, &[(2., 10.), (18., 10.)])
        };
        rasterize(&mut img, &[line], (0., 0.), 1.);
        // Pixel rows 8..12 have their centres within 2 px of y = 10.
        for y in 8..12 {
            assert_eq!(img.get_pixel(10, y).0, RED, "row {y}");
        }
        assert_eq!(img.get_pixel(10, 5).0, [255; 4]);
        assert_eq!(img.get_pixel(10, 14).0, [255; 4]);
    }

    #[test]
    fn edges_are_antialiased() {
        let mut img = white(20, 20);
        // Edge at y = 10 ± 1.5: row 11 (centre 11.5) is half covered.
        let line = Shape {
            width: 3.,
            ..shape(Tool::Line, &[(2., 10.), (18., 10.)])
        };
        rasterize(&mut img, &[line], (0., 0.), 1.);
        let [r, g, b, a] = img.get_pixel(10, 11).0;
        assert_eq!((r, a), (255, 255));
        assert!((100..160).contains(&g) && g == b, "partial coverage, got {g}");
    }

    #[test]
    fn origin_and_scale_map_logical_points_to_pixels() {
        // Image covers logical (100,50) at 2 px per logical px.
        let mut img = white(40, 40);
        let line = shape(Tool::Line, &[(105., 60.), (115., 60.)]); // 2 logical = 4 px wide
        rasterize(&mut img, &[line], (100., 50.), 2.);
        assert_eq!(img.get_pixel(20, 20).0, RED);
        assert_eq!(img.get_pixel(20, 18).0, RED);
        assert_eq!(img.get_pixel(20, 21).0, RED);
        assert_eq!(img.get_pixel(20, 24).0, [255; 4]);
        assert_eq!(img.get_pixel(5, 20).0, [255; 4], "line starts at x = 10 px");
    }

    #[test]
    fn shapes_outside_the_image_are_clipped() {
        let mut img = white(10, 10);
        rasterize(
            &mut img,
            &[shape(Tool::Line, &[(-50., -50.), (-40., 100.)])],
            (0., 0.),
            1.,
        );
        assert!(img.pixels().all(|p| p.0 == [255; 4]));
    }

    #[test]
    fn transparent_background_takes_the_stroke_colour() {
        let mut img = RgbaImage::new(10, 10);
        rasterize(
            &mut img,
            &[shape(Tool::Line, &[(0., 5.), (10., 5.)])],
            (0., 0.),
            1.,
        );
        assert_eq!(img.get_pixel(5, 5).0, RED);
    }

    #[test]
    fn overlapping_segments_of_one_shape_do_not_darken_joins() {
        let half = [255, 0, 0, 128];
        let mut img = white(20, 20);
        let mut zigzag = shape(Tool::Freehand, &[(2., 10.), (10., 10.), (2., 10.)]);
        zigzag.color = half;
        rasterize(&mut img, &[zigzag.clone()], (0., 0.), 1.);
        let once = img.get_pixel(6, 10).0;
        let mut img2 = white(20, 20);
        zigzag.points.truncate(2);
        rasterize(&mut img2, &[zigzag], (0., 0.), 1.);
        assert_eq!(once, img2.get_pixel(6, 10).0);
    }

    #[test]
    fn colors_parse_from_hex() {
        assert_eq!(parse_color("#FF8A80"), Some([0xFF, 0x8A, 0x80, 0xFF]));
        assert_eq!(parse_color("ff8a8080"), Some([0xFF, 0x8A, 0x80, 0x80]));
        assert_eq!(parse_color(" #abc123 "), Some([0xAB, 0xC1, 0x23, 0xFF]));
        assert_eq!(parse_color("#FF8A8"), None);
        assert_eq!(parse_color("#GG0000"), None);
    }

    /// Draw a shape with the tool in hand, from `a` to `b`.
    fn draw(a: &mut Annotations, tool: Tool, from: Point, to: Point) {
        a.tool = Some(tool);
        assert!(a.press(from));
        a.motion(to);
        a.release(to);
    }

    fn drag(a: &mut Annotations, from: Point, to: Point) -> bool {
        let taken = a.press(from);
        a.motion(to);
        a.release(to);
        taken
    }

    #[test]
    fn counters_number_themselves_and_are_discs_with_white_digits() {
        let mut a = Annotations {
            tool: Some(Tool::Counter),
            ..Default::default()
        };
        for x in [10., 60., 110.] {
            a.press((x, 10.));
            a.release((x, 10.));
        }
        let numbers: Vec<u32> = a.shapes().map(|s| s.number).collect();
        assert_eq!(numbers, vec![1, 2, 3]);

        let counter = a.shapes().next().unwrap();
        let strokes = counter.strokes();
        let disc = &strokes[0];
        assert_eq!(disc.lines, vec![vec![(10., 10.)]], "a dot at the centre");
        assert_eq!(disc.width, counter.radius() * 2.);
        assert_eq!(disc.color, counter.color);
        let digits = &strokes[1];
        assert_eq!(digits.color, [255; 4]);
        for &(x, y) in digits.lines.iter().flatten() {
            assert!(
                (x - 10.).hypot(y - 10.) + digits.width / 2. < counter.radius(),
                "digit leaves the disc"
            );
        }
    }

    #[test]
    fn counter_numbering_continues_after_the_highest_left() {
        let mut a = Annotations {
            tool: Some(Tool::Counter),
            ..Default::default()
        };
        for x in [10., 60.] {
            a.press((x, 10.));
            a.release((x, 10.));
        }
        a.undo();
        a.press((100., 10.));
        a.release((100., 10.));
        assert_eq!(a.shapes().map(|s| s.number).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn two_digit_counters_fit_too() {
        let c = Shape {
            number: 12,
            ..shape(Tool::Counter, &[(0., 0.)])
        };
        let strokes = c.strokes();
        let xs: Vec<f32> = strokes[1].lines.iter().flatten().map(|p| p.0).collect();
        assert!(
            xs.iter().any(|&x| x < -2.) && xs.iter().any(|&x| x > 2.),
            "two digits side by side"
        );
        for &(x, y) in strokes[1].lines.iter().flatten() {
            assert!(x.hypot(y) < c.radius());
        }
    }

    #[test]
    fn distance_measures_to_the_outline() {
        let rect = shape(Tool::Rectangle, &[(0., 0.), (100., 50.)]);
        assert!(rect.distance((50., 0.)) < 0.01);
        assert!(
            (rect.distance((50., 20.)) - 20.).abs() < 0.01,
            "the inside is not the shape"
        );
        let counter = shape(Tool::Counter, &[(0., 0.)]);
        assert_eq!(counter.distance((3., 0.)), 0.);
        assert!((counter.distance((counter.radius() + 5., 0.)) - 5.).abs() < 0.01);
    }

    #[test]
    fn handles_are_ends_or_box_corners() {
        assert_eq!(
            shape(Tool::Arrow, &[(1., 2.), (3., 4.)]).handles(),
            vec![(1., 2.), (3., 4.)]
        );
        assert_eq!(
            shape(Tool::Ellipse, &[(30., 40.), (10., 20.)]).handles(),
            vec![(10., 20.), (30., 20.), (30., 40.), (10., 40.)]
        );
        assert_eq!(
            shape(Tool::Freehand, &[(0., 5.), (10., 0.), (4., 8.)]).handles(),
            vec![(0., 0.), (10., 0.), (10., 8.), (0., 8.)]
        );
        assert_eq!(shape(Tool::Counter, &[(0., 0.)]).handles(), [] as [(f32, f32); 0]);
    }

    #[test]
    fn dragging_a_handle_reshapes() {
        let line = shape(Tool::Line, &[(0., 0.), (10., 0.)]);
        assert_eq!(
            line.with_handle(1, (30., 5.)).points,
            vec![(0., 0.), (30., 5.)],
            "longer"
        );
        let rect = shape(Tool::Rectangle, &[(0., 0.), (10., 10.)]);
        // Top-right corner dragged: bottom-left stays.
        let r = rect.with_handle(1, (20., -5.));
        assert_eq!(r.handles(), vec![(0., -5.), (20., -5.), (20., 10.), (0., 10.)]);
        let free = shape(Tool::Freehand, &[(0., 0.), (10., 10.)]);
        // Bottom-right corner to (20, 30): scaled from the top-left.
        assert_eq!(free.with_handle(2, (20., 30.)).points, vec![(0., 0.), (20., 30.)]);
    }

    #[test]
    fn translation_moves_every_point() {
        let s = shape(Tool::Freehand, &[(0., 0.), (1., 2.)]).translated(5., -1.);
        assert_eq!(s.points, vec![(5., -1.), (6., 1.)]);
    }

    #[test]
    fn a_new_shape_is_selected_and_its_style_editable() {
        let mut a = Annotations::default();
        draw(&mut a, Tool::Line, (0., 0.), (50., 0.));
        assert!(a.selected().is_some());
        a.set_width(10.);
        a.set_color(PASTELS[4]);
        let s = a.selected().unwrap();
        assert_eq!((s.width, s.color), (10., PASTELS[4]));
        assert_eq!(
            (a.width, a.color),
            (10., PASTELS[4]),
            "and the next shapes follow"
        );
    }

    #[test]
    fn width_is_clamped_and_shown_for_the_selection() {
        let mut a = Annotations::default();
        a.set_width(500.);
        assert_eq!(a.current_width(), MAX_WIDTH);
        a.step_width(-100.);
        assert_eq!(a.current_width(), MIN_WIDTH);
        draw(&mut a, Tool::Line, (0., 0.), (50., 0.));
        a.deselect();
        a.width = 6.;
        a.press((25., 0.));
        a.release((25., 0.));
        assert_eq!(a.current_width(), MIN_WIDTH, "selected shape's width");
    }

    #[test]
    fn clicking_a_stroke_selects_it_even_with_a_tool_in_hand() {
        let mut a = Annotations::default();
        draw(&mut a, Tool::Rectangle, (0., 0.), (100., 100.));
        draw(&mut a, Tool::Line, (200., 0.), (300., 0.));
        a.tool = Some(Tool::Rectangle);
        assert!(drag(&mut a, (2., 50.), (2., 50.)));
        assert_eq!(a.selected().unwrap().tool, Tool::Rectangle);
        assert_eq!(a.shapes().count(), 2, "no new shape");
        assert_eq!(a.grab_at((250., 3.)), Some(Grab::Shape(1)));
        assert_eq!(a.grab_at((250., 30.)), None);
    }

    #[test]
    fn without_a_tool_a_press_off_every_shape_is_for_the_region_and_deselects() {
        let mut a = Annotations::default();
        draw(&mut a, Tool::Line, (0., 0.), (50., 0.));
        a.tool = None;
        assert!(!a.press((25., 40.)));
        assert!(a.selected().is_none());
    }

    #[test]
    fn dragging_a_shape_moves_it_and_its_handles_reshape_it() {
        let mut a = Annotations::default();
        draw(&mut a, Tool::Arrow, (0., 0.), (50., 0.));
        a.tool = None;
        assert!(drag(&mut a, (25., 1.), (35., 11.)));
        assert_eq!(a.selected().unwrap().points, vec![(10., 10.), (60., 10.)]);
        assert_eq!(a.grab_at((61., 9.)), Some(Grab::Handle(1)));
        assert!(drag(&mut a, (61., 9.), (101., 9.)));
        assert_eq!(a.selected().unwrap().points, vec![(10., 10.), (100., 10.)]);
    }

    #[test]
    fn delete_removes_the_selection() {
        let mut a = Annotations::default();
        draw(&mut a, Tool::Line, (0., 0.), (50., 0.));
        draw(&mut a, Tool::Line, (0., 20.), (50., 20.));
        a.press((25., 0.));
        a.release((25., 0.));
        a.delete_selected();
        assert_eq!(a.shapes().map(|s| s.points[0].1).collect::<Vec<_>>(), vec![20.]);
        assert!(a.selected().is_none());
        a.delete_selected(); // nothing selected: no-op
        assert_eq!(a.shapes().count(), 1);
    }

    #[test]
    fn undo_reverts_every_kind_of_edit_one_step_at_a_time() {
        let mut a = Annotations::default();
        draw(&mut a, Tool::Line, (0., 0.), (50., 0.));
        let drawn: Vec<Shape> = a.shapes().cloned().collect();
        a.tool = None;
        drag(&mut a, (25., 0.), (25., 30.)); // move
        let moved: Vec<Shape> = a.shapes().cloned().collect();
        for _ in 0..5 {
            a.step_width(1.); // scrolling: one undo step
        }
        a.set_color(PASTELS[2]);
        a.delete_selected();
        assert!(a.is_empty());

        a.undo();
        assert_eq!(a.shapes().next().unwrap().color, PASTELS[2]);
        a.undo();
        assert_eq!(a.shapes().next().unwrap().width, DEFAULT_WIDTH + 5.);
        a.undo();
        assert_eq!(a.shapes().cloned().collect::<Vec<_>>(), moved);
        a.undo();
        assert_eq!(a.shapes().cloned().collect::<Vec<_>>(), drawn);
        a.undo();
        assert!(a.is_empty());
    }

    #[test]
    fn selecting_without_moving_adds_no_undo_step() {
        let mut a = Annotations::default();
        draw(&mut a, Tool::Line, (0., 0.), (50., 0.));
        draw(&mut a, Tool::Line, (0., 20.), (50., 20.));
        a.tool = None;
        drag(&mut a, (25., 0.), (25., 0.));
        a.undo();
        assert_eq!(a.shapes().count(), 1);
    }

    #[test]
    fn counters_are_burnt_in_white_on_colour() {
        let mut img = white(40, 40);
        let c = Shape {
            color: [0, 0, 255, 255],
            number: 1,
            ..shape(Tool::Counter, &[(20., 20.)])
        };
        assert!(Tool::Text.is_box() && !Tool::Rectangle.is_box());
        rasterize(&mut img, std::slice::from_ref(&c), (0., 0.), 1.);
        // Inside the disc near its rim: fill colour.
        let r = c.radius() as u32;
        assert_eq!(img.get_pixel(20 - r + 2, 20).0, [0, 0, 255, 255]);
        // Some pixels of the number are white.
        let whites = (10..30)
            .flat_map(|y| (10..30).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                img.get_pixel(x, y).0 == [255; 4] && (x as f32 - 19.5).hypot(y as f32 - 19.5) < 8.
            })
            .count();
        assert!(whites > 3, "number drawn");
    }

    #[test]
    fn hsv_covers_the_wheel() {
        assert_eq!(hsv(0., 1., 1.), [255, 0, 0, 255]);
        assert_eq!(hsv(120., 1., 1.), [0, 255, 0, 255]);
        assert_eq!(hsv(240., 1., 1.), [0, 0, 255, 255]);
        assert_eq!(hsv(360., 1., 1.), [255, 0, 0, 255]);
        assert_eq!(hsv(60., 0.5, 1.), [255, 255, 128, 255]);
        assert_eq!(hsv(200., 0., 0.), [0, 0, 0, 255]);
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(to_hex([0xAB, 0x01, 0xFF, 0xFF]), "#AB01FF");
        assert_eq!(to_hex([1, 2, 3, 4]), "#01020304");
        for c in PASTELS {
            assert_eq!(parse_color(&to_hex(c)), Some(c));
        }
    }

    fn text_box(a: &mut Annotations, from: Point, to: Point) {
        draw(a, Tool::Text, from, to);
    }

    #[test]
    fn a_text_box_is_drawn_then_typed_into() {
        let mut a = Annotations::default();
        text_box(&mut a, (10., 10.), (200., 60.));
        assert!(a.editing().is_some());
        a.type_text("Hi");
        a.backspace();
        a.type_text("ey");
        let t = a.editing().unwrap();
        assert_eq!(t.text, "Hey");
        assert_eq!((t.width, t.color, t.fill), (DEFAULT_TEXT_SIZE, PASTELS[0], None));
        assert_eq!(
            t.handles(),
            vec![(10., 10.), (200., 10.), (200., 60.), (10., 60.)]
        );
        a.type_text("\nyou");
        assert_eq!(a.editing().unwrap().text, "Hey\nyou");
    }

    #[test]
    fn a_click_with_the_text_tool_makes_a_default_box() {
        let mut a = Annotations {
            tool: Some(Tool::Text),
            ..Default::default()
        };
        a.press((10., 10.));
        a.release((10., 10.));
        let t = a.editing().unwrap();
        let h = DEFAULT_TEXT_SIZE.mul_add(
            crate::text::LINE_HEIGHT,
            2. * DEFAULT_TEXT_SIZE * crate::text::PADDING,
        );
        assert_eq!(t.points, vec![(10., 10.), (10. + DEFAULT_TEXT_BOX, 10. + h)]);
    }

    #[test]
    fn an_empty_text_box_vanishes_without_an_undo_step() {
        let mut a = Annotations::default();
        draw(&mut a, Tool::Line, (0., 0.), (50., 0.));
        text_box(&mut a, (10., 100.), (200., 160.));
        a.stop_editing();
        assert_eq!(a.shapes().count(), 1);
        assert!(a.editing().is_none());
        a.undo();
        assert!(a.is_empty(), "the undo removed the line, not a ghost box");
    }

    #[test]
    fn pressing_elsewhere_ends_typing() {
        let mut a = Annotations::default();
        text_box(&mut a, (10., 100.), (200., 160.));
        a.type_text("x");
        a.tool = None;
        a.press((500., 500.));
        a.release((500., 500.));
        assert!(a.editing().is_none());
        assert_eq!(a.shapes().next().unwrap().text, "x");
    }

    #[test]
    fn typing_a_box_is_one_undo_step_and_reediting_another() {
        let mut a = Annotations::default();
        draw(&mut a, Tool::Line, (0., 0.), (50., 0.));
        text_box(&mut a, (10., 100.), (200., 160.));
        a.type_text("abc");
        a.stop_editing();
        // Select it again by clicking inside, edit, finish.
        a.tool = None;
        a.press((100., 130.));
        a.release((100., 130.));
        assert!(a.start_editing());
        a.type_text("d");
        a.stop_editing();
        a.undo();
        assert_eq!(a.shapes().nth(1).unwrap().text, "abc");
        a.undo();
        assert_eq!(a.shapes().count(), 1, "box and its typing went together");
    }

    #[test]
    fn start_editing_needs_a_selected_text_box() {
        let mut a = Annotations::default();
        assert!(!a.start_editing());
        draw(&mut a, Tool::Line, (0., 0.), (50., 0.));
        assert!(!a.start_editing());
    }

    #[test]
    fn wheel_and_stepper_size_text_separately_from_strokes() {
        let mut a = Annotations {
            tool: Some(Tool::Text),
            ..Default::default()
        };
        a.step_width(2.);
        assert_eq!((a.text_size, a.width), (DEFAULT_TEXT_SIZE + 2., DEFAULT_WIDTH));
        assert_eq!(a.current_width(), DEFAULT_TEXT_SIZE + 2.);
        a.set_width(1000.);
        assert_eq!(a.text_size, MAX_TEXT_SIZE);
        a.set_width(1.);
        assert_eq!(a.text_size, MIN_TEXT_SIZE);
        text_box(&mut a, (0., 0.), (100., 50.));
        a.set_width(40.);
        assert_eq!(a.selected().unwrap().width, 40.);
        a.tool = Some(Tool::Line);
        a.deselect();
        assert_eq!(a.current_width(), DEFAULT_WIDTH, "strokes keep their own width");
    }

    #[test]
    fn fill_and_colour_targets_for_text() {
        let mut a = Annotations::default();
        text_box(&mut a, (0., 0.), (100., 50.));
        assert_eq!(a.current_fill(), None);
        a.toggle_fill();
        assert_eq!(a.selected().unwrap().fill, Some(a.fill_color));
        a.target = Target::Fill;
        a.set_color(PASTELS[2]);
        assert_eq!(a.selected().unwrap().fill, Some(PASTELS[2]));
        assert_eq!(a.current_color(), PASTELS[2]);
        assert_eq!(a.selected().unwrap().color, PASTELS[0], "ink unchanged");
        a.target = Target::Ink;
        a.set_color(PASTELS[4]);
        assert_eq!(a.selected().unwrap().color, PASTELS[4]);
        a.toggle_fill();
        assert_eq!(a.selected().unwrap().fill, None);
        assert!(!a.fill_on);
        // The next box follows.
        a.toggle_fill();
        text_box(&mut a, (0., 200.), (100., 250.));
        assert_eq!(a.selected().unwrap().fill, Some(PASTELS[2]));
    }

    #[test]
    fn boxes_are_grabbed_anywhere_inside() {
        let t = shape(Tool::Text, &[(0., 0.), (100., 50.)]);
        assert_eq!(t.distance((50., 25.)), 0.);
        assert!((t.distance((110., 25.)) - 10.).abs() < 0.01);
        let r = shape(Tool::Redact, &[(100., 50.), (0., 0.)]);
        assert_eq!(r.distance((50., 25.)), 0.);
        assert_eq!(r.handles().len(), 4);
        assert_eq!(r.with_handle(2, (120., 60.)).handles()[2], (120., 60.));
    }

    #[test]
    fn redaction_blacks_out_whole_pixels_whatever_the_colour() {
        let mut img = white(20, 20);
        img.put_pixel(15, 15, Rgba([0; 4]));
        let r = Shape {
            color: PASTELS[1],
            ..shape(Tool::Redact, &[(4.5, 4.5), (16., 16.)])
        };
        rasterize(&mut img, &[r], (0., 0.), 1.);
        assert_eq!(img.get_pixel(5, 5).0, REDACTED);
        assert_eq!(img.get_pixel(15, 15).0, REDACTED, "transparent too");
        assert_eq!(img.get_pixel(4, 4).0, REDACTED, "partly covered pixels go too");
        assert_eq!(img.get_pixel(3, 3).0, [255; 4]);
        assert_eq!(img.get_pixel(16, 10).0, [255; 4]);
    }

    #[test]
    fn redaction_maps_through_origin_and_scale() {
        let mut img = white(40, 40);
        rasterize(
            &mut img,
            &[shape(Tool::Redact, &[(105., 55.), (110., 60.)])],
            (100., 50.),
            2.,
        );
        assert_eq!(img.get_pixel(10, 10).0, REDACTED);
        assert_eq!(img.get_pixel(19, 19).0, REDACTED);
        assert_eq!(img.get_pixel(20, 20).0, [255; 4]);
    }

    #[test]
    fn text_is_burnt_in_inside_its_box() {
        let mut img = white(200, 100);
        let t = Shape {
            text: "Hello".into(),
            width: 12.,
            fill: Some([0, 0, 0, 255]),
            ..shape(Tool::Text, &[(110., 60.), (160., 80.)])
        };
        rasterize(&mut img, &[t], (100., 50.), 2.);
        // Box is pixels (20,20)..(120,60): filled black with red letters.
        assert_eq!(img.get_pixel(119, 59).0, [0, 0, 0, 255]);
        assert!((20..120).any(|x| (20..60).any(|y| img.get_pixel(x, y).0 == RED)));
        assert_eq!(img.get_pixel(125, 30).0, [255; 4]);
        assert_eq!(img.get_pixel(19, 30).0, [255; 4]);
    }

    #[test]
    fn the_fill_target_only_applies_to_text() {
        let mut a = Annotations::default();
        text_box(&mut a, (0., 0.), (100., 50.));
        a.type_text("x");
        a.target = Target::Fill;
        a.stop_editing();
        a.deselect();
        a.tool = Some(Tool::Rectangle);
        a.set_color(PASTELS[3]);
        assert_eq!(a.color, PASTELS[3], "the rectangle gets the colour");
        assert_eq!(a.current_color(), PASTELS[3]);
        assert!(!a.fill_on, "and text boxes don't silently get a background");
    }

    #[test]
    fn deleting_words() {
        let mut a = Annotations::default();
        text_box(&mut a, (0., 0.), (100., 50.));
        a.type_text("hello big world");
        a.delete_word();
        assert_eq!(a.editing().unwrap().text, "hello big ");
        a.delete_word();
        assert_eq!(a.editing().unwrap().text, "hello ");
        a.type_text("x\n");
        a.delete_word();
        assert_eq!(a.editing().unwrap().text, "hello ");
        a.delete_word();
        a.delete_word();
        assert_eq!(a.editing().unwrap().text, "");
    }

    #[test]
    fn built_in_palette_is_the_pastels_plus_white_and_black() {
        assert_eq!(PASTELS.len(), 8);
        assert_eq!(PASTELS[..6].iter().filter(|c| c[3] == 0xFF).count(), 6);
        assert_eq!(&PASTELS[6..], &[[0xFF; 4], [0, 0, 0, 0xFF]]);
    }

    fn palette(colours: &[&str]) -> Palette {
        Palette {
            palette: Some(colours.iter().map(ToString::to_string).collect()),
            ..Palette::default()
        }
    }

    #[test]
    fn no_palette_offers_the_pastels() {
        assert_eq!(Palette::default().colors(), PASTELS.to_vec());
    }

    #[test]
    fn the_palette_keeps_the_colours_that_parse() {
        let colours = palette(&["#112233", "nonsense", "44556680"]).colors();
        assert_eq!(colours, vec![[0x11, 0x22, 0x33, 0xFF], [0x44, 0x55, 0x66, 0x80]]);
    }

    #[test]
    fn a_palette_of_nothing_that_parses_offers_the_pastels() {
        assert_eq!(palette(&["nope"]).colors(), PASTELS.to_vec());
    }

    #[test]
    fn the_palette_reads_from_the_config_file() {
        let path = tempfile::tempdir().expect("the system has a temporary folder");
        let path = path.path().join("config.json");
        let json = r##"{"output_dir":null,"palette":["#ff0000"],"custom_color":"#00ff00"}"##;
        std::fs::write(&path, json).expect("the folder is writable");
        let palette: Palette = crate::config::Config::load_from(&path).section();
        assert_eq!(palette.custom_color(), Some([0, 255, 0, 255]));
    }
}
