//! The overlay's look, shared by the features: buttons and colours.

use gpui::{App, CursorStyle, Div, Entity, SharedString, Stateful, Window, div, hsla, prelude::*, rgba};

use super::{ACCENT, Overlay};

/// A colour as gpui takes it.
pub fn color(c: [u8; 4]) -> gpui::Rgba {
    rgba(u32::from_be_bytes(c))
}

pub fn button(id: impl Into<SharedString>, label: impl Into<SharedString>, active: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .px_3()
        .py_1()
        .rounded_md()
        .cursor(CursorStyle::PointingHand)
        .when_else(
            active,
            |d| d.bg(ACCENT).text_color(gpui::white()),
            |d| d.hover(|d| d.bg(hsla(0., 0., 1., 0.12))),
        )
        .child(label.into())
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
