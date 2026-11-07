//! The editing feature in the overlay: drawing tools, colours, widths, text boxes.

use std::{
    cell::{OnceCell, RefCell},
    collections::HashMap,
    sync::Arc,
};

use gpui::{
    AnyElement, CursorStyle, ImageSource, Keystroke, ObjectFit, RenderImage, Window, canvas, div, img,
    prelude::*, px,
};

use super::{
    ACCENT,
    feature::{Act, Ctx, Effect, Feature, Point},
    handle_div, keys, local, rect_div,
    tools::{self, TextKey},
    widgets,
};
use crate::{
    annotate::{self, Annotations, Color, Grab, Palette, Shape, Target, Tool},
    capture::{MonitorInfo, Rect},
    drawings::Shared,
};

pub struct Editing {
    annotations: Annotations,
    /// The colours offered, as remembered.
    palette: Palette,
    /// The palette parsed, the first time the drawing row shows.
    colors: OnceCell<Vec<Color>>,
    /// Hue of the colour picker while it is open.
    picker: Option<f32>,
    /// Touchpad scrolling not yet turned into a width step.
    scroll: f32,
    /// What the pointer was last over, to repaint when that changes.
    grab: Option<Grab>,
    /// Text boxes rendered for each screen's window, by what they show.
    texts: RefCell<HashMap<String, HashMap<TextKey, Arc<RenderImage>>>>,
}

impl Editing {
    pub fn new(palette: Palette) -> Self {
        Self {
            annotations: Annotations::default(),
            palette,
            colors: OnceCell::new(),
            picker: None,
            scroll: 0.,
            grab: None,
            texts: RefCell::default(),
        }
    }

    fn colors(&self) -> &[Color] {
        self.colors.get_or_init(|| self.palette.colors())
    }

    /// Pick a drawing tool, or put it down if it is already in hand.
    pub(super) fn toggle_tool(&mut self, tool: Tool) {
        self.annotations.stop_editing();
        self.annotations.tool = (self.annotations.tool != Some(tool)).then_some(tool);
        if self.annotations.tool == Some(Tool::Text) {
            crate::text::preload();
        }
    }

    pub(super) fn toggle_fill(&mut self) {
        self.annotations.toggle_fill();
    }

    pub(super) const fn set_target(&mut self, target: Target) {
        self.annotations.target = target;
    }

    pub(super) fn set_color(&mut self, color: Color) {
        self.annotations.set_color(color);
    }

    fn cycle_color(&mut self) {
        let colors = self.colors();
        let here = colors.iter().position(|c| *c == self.annotations.current_color());
        let next = colors[here.map_or(0, |i| (i + 1) % colors.len())];
        self.set_color(next);
    }

    pub(super) const fn toggle_picker(&mut self) {
        self.picker = if self.picker.is_some() { None } else { Some(0.) };
    }

    pub(super) const fn set_picker_hue(&mut self, hue: f32) {
        self.picker = Some(hue);
    }

    /// A colour from the picker: use it, and remember it as the custom swatch.
    pub(super) fn pick_custom(&mut self, color: Color) {
        self.picker = None;
        self.palette.custom_color = Some(annotate::to_hex(color));
        super::report_unremembered(crate::config::remember(&self.palette), "the colour");
        self.set_color(color);
    }

    pub(super) fn step_width(&mut self, delta: f32) {
        self.annotations.step_width(delta);
    }

    pub(super) fn undo(&mut self) {
        self.annotations.undo();
    }

    pub(super) fn delete_selected(&mut self) {
        self.annotations.delete_selected();
    }

    /// Keys while typing in a text box. Returns whether the key was used (always, while
    /// typing: shortcuts must not fire mid-sentence).
    fn text_key(&mut self, keystroke: &Keystroke) -> bool {
        use keys::TextEdit;
        if self.annotations.editing().is_none() {
            return false;
        }
        let modifiers = &keystroke.modifiers;
        let mods = keys::Mods {
            control: modifiers.control,
            alt: modifiers.alt,
            platform: modifiers.platform,
        };
        let typed = keystroke.key_char.as_deref();
        let annotations = &mut self.annotations;
        match keys::text_edit(&keystroke.key, typed, mods) {
            TextEdit::Done => annotations.stop_editing(),
            TextEdit::Newline => annotations.type_text("\n"),
            TextEdit::Backspace => annotations.backspace(),
            TextEdit::DeleteWord => annotations.delete_word(),
            TextEdit::Undo => {
                annotations.stop_editing();
                annotations.undo();
            }
            TextEdit::Type(text) => annotations.type_text(&text),
            TextEdit::Swallow => {}
        }
        true
    }

    /// A shortcut of the drawing tools: whether `key` is one.
    fn shortcut(&mut self, key: &str, control: bool) -> bool {
        match key {
            "z" if control => self.undo(),
            "delete" | "backspace" => self.delete_selected(),
            "c" => self.cycle_color(),
            "p" => self.toggle_picker(),
            "[" | "-" => self.step_width(-1.),
            "]" | "=" | "+" => self.step_width(1.),
            _ => return self.tool_shortcut(key),
        }
        true
    }

    /// `1`…`8`: pick (or put down) that tool.
    fn tool_shortcut(&mut self, key: &str) -> bool {
        let Some(tool) = key
            .parse::<usize>()
            .ok()
            .and_then(|n| Tool::ALL.get(n.wrapping_sub(1)))
        else {
            return false;
        };
        self.toggle_tool(*tool);
        true
    }

    /// What the drawing row shows.
    fn bar(&self) -> tools::DrawBar {
        let annotations = &self.annotations;
        tools::DrawBar {
            tool: annotations.tool,
            color: annotations.current_color(),
            width: annotations.current_width(),
            has_selection: annotations.selected().is_some(),
            palette: self.colors().to_vec(),
            custom: self.palette.custom_color(),
            picker: self.picker,
            text: annotations.sizing_text(),
            fill: annotations.current_fill(),
            target: annotations.target,
        }
    }

    /// A text box on `monitor`, as an image (rendered once per look, per window).
    fn text_box(
        &self,
        shape: &Shape,
        monitor: &MonitorInfo,
        scale: f32,
        kept: &mut HashMap<TextKey, Arc<RenderImage>>,
    ) -> Option<AnyElement> {
        let room = box_rect(shape);
        room.intersect(monitor.rect)?;
        let key = TextKey::new(shape, scale);
        let mut texts = self.texts.borrow_mut();
        let shown = texts.get_mut(&monitor.name).and_then(|shown| shown.remove(&key));
        let image = shown
            .or_else(|| kept.get(&key).cloned())
            .unwrap_or_else(|| key.render());
        kept.insert(key, image.clone());
        let placed = img(ImageSource::Render(image))
            .absolute()
            .left(px((room.x - monitor.rect.x) as f32))
            .top(px((room.y - monitor.rect.y) as f32))
            .w(px(room.w as f32))
            .h(px(room.h as f32))
            .object_fit(ObjectFit::Fill);
        Some(placed.into_any_element())
    }

    /// The box being typed in: its outline and caret.
    fn caret(&self, monitor: Rect) -> Vec<AnyElement> {
        let Some(shape) = self.annotations.editing() else {
            return Vec::new();
        };
        let outline = local(box_rect(shape), monitor);
        let (x, y, h) = crate::text::caret(&shape.text, shape.width, outline.w.max(1) as u32);
        let caret = div()
            .absolute()
            .left(px(outline.x as f32 + x))
            .top(px(outline.y as f32 + y))
            .w(px(2.))
            .h(px(h))
            .bg(widgets::color(shape.color));
        vec![
            rect_div(outline)
                .border_1()
                .border_color(ACCENT)
                .into_any_element(),
            caret.into_any_element(),
        ]
    }

    /// Debug builds: draw what the end-to-end tests ask for over `region`.
    #[cfg(debug_assertions)]
    fn draw_for_tests(&mut self, region: Rect) {
        let (x, y, w, h) = (region.x as f32, region.y as f32, region.w as f32, region.h as f32);
        let annotations = &mut self.annotations;
        // A line across the middle.
        if std::env::var_os("SCREENCAPTURE_E2E_LINE").is_some() {
            annotations.tool = Some(Tool::Line);
            annotations.press((x, y + h / 2.));
            annotations.release((x + w, y + h / 2.));
        }
        // The left quarter redacted, "Hi" on a filled box in the top-right quarter.
        if std::env::var_os("SCREENCAPTURE_E2E_EXTRAS").is_some() {
            annotations.tool = Some(Tool::Redact);
            annotations.press((x, y));
            annotations.release((x + w / 4., y + h));
            annotations.tool = Some(Tool::Text);
            annotations.fill_on = true;
            annotations.press((x + w / 2., y));
            annotations.release((x + w, y + h / 2.));
            annotations.type_text("Hi");
            annotations.stop_editing();
        }
    }
}

impl Feature for Editing {
    fn key(&mut self, key: &Keystroke, _: &Ctx) -> Option<Effect> {
        let used = self.text_key(key) || self.shortcut(&key.key, key.modifiers.control);
        used.then_some(Effect::Repaint)
    }

    fn escape(&mut self) -> bool {
        self.picker.take().is_some() || self.annotations.deselect()
    }

    fn press(&mut self, point: Point, clicks: usize) -> bool {
        self.picker = None;
        let taken = self.annotations.press(point);
        // Double-click a text box to type in it again.
        if taken && clicks >= 2 {
            self.annotations.start_editing();
        }
        taken
    }

    fn motion(&mut self, point: Point) -> bool {
        let busy = self.annotations.is_busy();
        if busy {
            self.annotations.motion(point);
        }
        let grab = self.annotations.grab_at(point);
        let moved = grab != std::mem::replace(&mut self.grab, grab);
        busy || moved
    }

    fn release(&mut self, point: Point) -> bool {
        let busy = self.annotations.is_busy();
        if busy {
            self.annotations.release(point);
        }
        busy
    }

    fn busy(&self) -> bool {
        self.annotations.is_busy()
    }

    fn holds_a_tool(&self) -> bool {
        self.annotations.tool.is_some()
    }

    /// The selected shape grows or shrinks, else the width of the tool in hand.
    fn scroll(&mut self, lines: f32) -> bool {
        if self.annotations.selected().is_none() && self.annotations.tool.is_none() {
            return false;
        }
        self.scroll += lines;
        let steps = self.scroll.trunc();
        self.scroll -= steps;
        if steps != 0. {
            self.step_width(steps);
        }
        true
    }

    /// A new kind of capture starts with no tool in hand.
    fn mode_changed(&mut self) {
        self.annotations.stop_editing();
        self.annotations.tool = None;
    }

    fn cursor(&self, point: Point) -> Option<CursorStyle> {
        let busy = self.annotations.is_busy();
        match self.annotations.grab_at(point) {
            Some(Grab::Handle(_)) => Some(CursorStyle::Crosshair),
            Some(Grab::Shape(_)) if busy => Some(CursorStyle::ClosedHand),
            Some(Grab::Shape(_)) => Some(CursorStyle::OpenHand),
            None if busy || self.annotations.tool.is_some() => Some(CursorStyle::Crosshair),
            None => None,
        }
    }

    fn drawings(&self) -> Option<Shared> {
        super::drawn::Drawn::of(self.annotations.shapes())
    }

    /// The drawings, one element per shape so they stack as in the saved image, and the box
    /// being typed in.
    fn under_dim(&self, monitor: &MonitorInfo, window: &mut Window) -> Vec<AnyElement> {
        let scale = window.scale_factor();
        let mut kept = HashMap::new();
        let origin = (monitor.rect.x, monitor.rect.y);
        let mut layer: Vec<AnyElement> = self
            .annotations
            .shapes()
            .filter_map(|shape| match shape.tool {
                Tool::Text => self.text_box(shape, monitor, scale, &mut kept),
                _ => Some(painted(shape, origin)),
            })
            .collect();
        // Boxes no longer shown: free their textures.
        let replaced = self.texts.borrow_mut().insert(monitor.name.clone(), kept);
        for image in replaced.into_iter().flat_map(HashMap::into_values) {
            let _ = window.drop_image(image);
        }
        layer.extend(self.caret(monitor.rect));
        layer
    }

    /// The selected shape's handles (and a ring around a counter).
    fn over_dim(&self, monitor: Rect) -> Vec<AnyElement> {
        let Some(shape) = self
            .annotations
            .selected()
            .filter(|_| !self.annotations.is_busy())
        else {
            return Vec::new();
        };
        let to_local = |(x, y): Point| (x.round() as i32 - monitor.x, y.round() as i32 - monitor.y);
        let ring = (shape.tool == Tool::Counter).then(|| {
            let (x, y) = to_local(shape.points[0]);
            let r = shape.radius().round() as i32 + 4;
            let ring = rect_div(Rect::new(x - r, y - r, 2 * r, 2 * r)).rounded_full();
            ring.border_2().border_color(ACCENT).into_any_element()
        });
        let handles = shape.handles().into_iter().map(|handle| {
            let (x, y) = to_local(handle);
            handle_div(x, y).into_any_element()
        });
        ring.into_iter().chain(handles).collect()
    }

    fn rows(&self, act: &Act) -> Vec<AnyElement> {
        vec![tools::drawing_row(act, &self.bar()).into_any_element()]
    }

    fn popup(&self, act: &Act) -> Option<AnyElement> {
        let hue = self.picker?;
        let picker = tools::picker(act, hue, self.annotations.current_color());
        Some(picker.into_any_element())
    }

    #[cfg(debug_assertions)]
    fn test_hook(&mut self, region: Rect) -> Option<Effect> {
        self.draw_for_tests(region);
        None
    }
}

/// A shape other than text, painted over the whole window whose top-left is `origin`.
fn painted(shape: &Shape, origin: (i32, i32)) -> AnyElement {
    let shape = shape.clone();
    let paint = move |bounds, (), window: &mut Window, _: &mut gpui::App| {
        tools::paint_shapes(std::slice::from_ref(&shape), origin, bounds, window);
    };
    canvas(|_, _, _| (), paint)
        .absolute()
        .size_full()
        .into_any_element()
}

/// The box a shape's points span.
fn box_rect(shape: &Shape) -> Rect {
    let (left, top, right, bottom) = shape.bounds();
    Rect::new(
        left.round() as i32,
        top.round() as i32,
        (right - left).round() as i32,
        (bottom - top).round() as i32,
    )
}
