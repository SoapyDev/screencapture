//! The overlay's look, shared by the features: buttons and colours.

use gpui::{App, CursorStyle, Div, Entity, SharedString, Stateful, Window, div, prelude::*, rgba};

use super::{Overlay, theme};

/// A colour as gpui takes it.
#[must_use]
pub fn color(c: [u8; 4]) -> gpui::Rgba {
    rgba(u32::from_be_bytes(c))
}

pub fn button(id: impl Into<SharedString>, label: impl Into<SharedString>, active: bool) -> Stateful<Div> {
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
        .child(label.into())
}

/// A button with a smaller line under its label: what it will do, without opening its drawer.
pub fn button_with_summary(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    summary: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    let summary = div().text_xs().opacity(0.7).child(summary.into());
    button(id, label, active)
        .flex()
        .flex_col()
        .items_center()
        .child(summary)
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
