//! The overlay's look, shared by the features: buttons and colours.

use gpui::{
    App, Context, CursorStyle, Div, Entity, Hsla, Render, SharedString, Stateful, Window, div, prelude::*,
    px, rgba,
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
    div()
        .id(id.into())
        .px_3()
        .py_1()
        .rounded_md()
        .cursor(CursorStyle::PointingHand)
        .when_else(
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
    div()
        .id(id.into())
        .size(px(ROUND))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(fill)
        .border_2()
        .border_color(theme.border)
        .hover(|d| d.border_color(theme.accent))
        .cursor(CursorStyle::PointingHand)
        .tooltip(move |_, cx| cx.new(|_| Hint(hint.clone())).into())
        .child(picture(glyph, ink))
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
    let body = div()
        .absolute()
        .top(px(5.))
        .size(px(28.))
        .h(px(21.))
        .rounded_md()
        .border_2()
        .border_color(ink);
    let bump = div()
        .absolute()
        .top(px(1.))
        .left(px(9.))
        .w(px(10.))
        .h(px(5.))
        .rounded_t_sm()
        .bg(ink);
    let lens = div()
        .absolute()
        .top(px(10.))
        .left(px(9.))
        .size(px(10.))
        .rounded_full()
        .border_2()
        .border_color(ink);
    div()
        .relative()
        .w(px(28.))
        .h(px(26.))
        .child(body)
        .child(bump)
        .child(lens)
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
