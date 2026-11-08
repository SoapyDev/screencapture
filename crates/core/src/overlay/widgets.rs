//! The overlay's look, shared by the features: buttons and colours.

use gpui::{
    App, Context, CursorStyle, Div, Entity, Hsla, MouseButton, Render, SharedString, Stateful, Window, div,
    prelude::*, px, rgba,
};

use super::{Overlay, theme};

/// A colour as gpui takes it.
#[must_use]
pub fn color(c: [u8; 4]) -> gpui::Rgba {
    rgba(u32::from_be_bytes(c))
}

pub fn button(id: impl Into<SharedString>, label: impl Into<SharedString>, active: bool) -> Stateful<Div> {
    shell(id, active).child(label.into())
}

/// A button whose size is that of `widest` (the longest text it will ever show), so changing
/// its `label` never moves what is beside it.
pub fn steady_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    widest: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    let sizer = div().invisible().child(widest.into());
    let shown = div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(label.into());
    shell(id, active).relative().child(sizer).child(shown)
}

/// The look of a button, without its text.
fn shell(id: impl Into<SharedString>, active: bool) -> Stateful<Div> {
    let theme = theme::current();
    let button = div()
        .id(id.into())
        .px_3()
        .py_1()
        .rounded_md()
        .border_2()
        .border_color(gpui::transparent_black());
    keyboard_focusable(button).when_else(
        active,
        |d| d.bg(theme.accent).text_color(theme.on_accent),
        |d| d.hover(|d| d.bg(theme.hover)),
    )
}

/// Diameter of the big round action buttons.
const ROUND: f32 = 56.;

/// The picture on a round button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Camera,
    RecordDot,
}

/// A big round button showing `glyph`; `active` (ready to act) fills it with the accent. Its
/// `hint` names it for the pointer's rest, and for anyone who can't read the picture.
pub fn round_button(
    id: impl Into<SharedString>,
    glyph: Glyph,
    hint: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    let theme = theme::current();
    let (fill, ink) = if active {
        (theme.accent, theme.on_accent)
    } else {
        (theme.surface, theme.on_surface)
    };
    let hint = hint.into();
    let button = div()
        .id(id.into())
        .size(px(ROUND))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(fill)
        .border_2()
        .border_color(theme.border);
    keyboard_focusable(button)
        .hover(|d| d.border_color(theme.accent))
        .tooltip(move |_, cx| cx.new(|_| Hint(hint.clone())).into())
        .child(picture(glyph, ink))
}

/// `button`, reached with Tab and ringed while it has the keyboard's focus. A click doesn't
/// take the focus (gpui gives it to what is pressed): Enter and Space keep acting on the
/// overlay after a click, and only Tab hands them to a button.
fn keyboard_focusable(button: Stateful<Div>) -> Stateful<Div> {
    let ring = theme::current().on_surface;
    button
        .focusable()
        .tab_stop(true)
        .focus_visible(move |style| style.border_color(ring))
        .cursor(CursorStyle::PointingHand)
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
}

fn picture(glyph: Glyph, ink: Hsla) -> Div {
    match glyph {
        Glyph::Camera => camera(ink),
        Glyph::RecordDot => div()
            .size(px(24.))
            .rounded_full()
            .bg(theme::current().danger_fill),
    }
}

/// A camera drawn from boxes: body, viewfinder bump and lens.
fn camera(ink: Hsla) -> Div {
    div()
        .relative()
        .w(px(28.))
        .h(px(26.))
        .child(camera_body(ink))
        .child(camera_bump(ink))
        .child(camera_lens(ink))
}

fn camera_body(ink: Hsla) -> Div {
    let body = div().absolute().top(px(5.)).size(px(28.)).h(px(21.));
    body.rounded_md().border_2().border_color(ink)
}

/// The viewfinder on top of the body.
fn camera_bump(ink: Hsla) -> Div {
    let bump = div().absolute().top(px(1.)).left(px(9.));
    bump.w(px(10.)).h(px(5.)).rounded_t_sm().bg(ink)
}

fn camera_lens(ink: Hsla) -> Div {
    let lens = div().absolute().top(px(10.)).left(px(9.)).size(px(10.));
    lens.rounded_full().border_2().border_color(ink)
}

/// The text shown when the pointer rests on a round button.
struct Hint(SharedString);

impl Render for Hint {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::current();
        let text = self.0.clone();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme.surface)
            .text_color(theme.on_surface)
            .text_sm()
            .child(text)
    }
}

/// Click handler running `f` on the overlay.
pub fn on(
    overlay: &Entity<Overlay>,
    f: impl Fn(&mut Overlay, &mut gpui::Context<Overlay>) + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let overlay = overlay.clone();
    // Buttons stay drawn while the bars slide away, but they no longer act.
    move |_, _, cx| {
        overlay.update(cx, |o, cx| {
            if o.selecting() {
                f(o, cx);
            }
        });
    }
}
