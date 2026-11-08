//! The bottom action bar: the features' round buttons side by side on one bar.
//!
//! Resting the pointer on a button opens a bar of its options above it. The bars slide away
//! with the toolbar when a feature takes over.

use std::{cell::Cell, rc::Rc};

use gpui::{
    Animation, AnimationExt, AnyElement, App, Bounds, Div, Entity, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, SharedString, Window, canvas, div, ease_in_out, prelude::*, px,
};

use super::{
    Overlay,
    drawer::{self, Placement},
    drawers::{Drawers, Part},
    feature::Actions,
    layout::Edge,
    theme,
};
use crate::{
    capture::Rect,
    units::{ToF32, saturating_i32},
};

/// Distance of the bars from their screen edge.
pub const EDGE_GAP: f32 = 24.;
/// Room the action bar takes at the bottom: a toolbar at the bottom sits above it.
pub const ACTION_BAR_ROOM: f32 = 72.;
/// How far the bars travel when they slide away: past any screen edge.
const SLIDE_DISTANCE: f32 = 240.;
/// How wide an options bar would like to be.
const OPTIONS_WIDTH: i32 = 780;
/// How far an options bar slides up as it opens.
const RISE: f32 = 12.;

/// What the action bar is drawn from.
pub struct ActionBar<'a> {
    pub overlay: &'a Entity<Overlay>,
    pub drawers: &'a Drawers,
    /// Where the buttons' bar is, filled in as it is painted.
    pub anchor: &'a Rc<Cell<Option<Rect>>>,
    /// The screen, in the window's own coordinates.
    pub screen: Rect,
}

impl ActionBar<'_> {
    /// The bar for every feature's `actions`, with `notice` above it.
    pub fn render(&self, actions: Vec<Actions>, notice: Option<&SharedString>, leaving: bool) -> AnyElement {
        let column = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .children(notice.map(notice_line))
            .child(self.measured(self.buttons(actions)));
        let place = div()
            .absolute()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(column);
        slide(place, Edge::Bottom, EDGE_GAP, leaving, "action-bar")
    }

    /// `bar`, with a note of where it is painted: the options bars centre on its buttons.
    fn measured(&self, bar: Div) -> Div {
        let anchor = Rc::clone(self.anchor);
        let measure = canvas(
            move |bounds, window, _| remember_bounds(&anchor, bounds, window),
            |_, (), _, _| {},
        );
        div().relative().child(measure.absolute().size_full()).child(bar)
    }

    /// The features' round buttons side by side on a pill-shaped bar, each with its options.
    fn buttons(&self, actions: Vec<Actions>) -> Div {
        let with_buttons: Vec<_> = actions
            .into_iter()
            .enumerate()
            .filter(|(_, actions)| !actions.main.is_empty())
            .collect();
        let count = with_buttons.len();
        let cells = with_buttons
            .into_iter()
            .enumerate()
            .map(|(position, (index, actions))| self.cell(index, actions, (position, count)));
        super::panel().rounded_full().gap_2().px_2().children(cells)
    }

    /// One button, with its options bar above it while open. `(position, count)` is its place among
    /// the buttons, to find where its centre is.
    fn cell(&self, index: usize, actions: Actions, (position, count): (usize, usize)) -> Div {
        let shown = self.drawers.state(index).is_shown() && !actions.drawer.is_empty();
        let hovered = self.hover_listener(index, Part::Button);
        let button = div()
            .id(("action", index))
            .on_hover(hovered)
            .children(actions.main);
        let options = shown.then(|| self.options(index, actions.drawer, self.placement(position, count)));
        div().relative().child(button).children(options)
    }

    /// Where the options bar of button `position` of `count` goes.
    fn placement(&self, position: usize, count: usize) -> Placement {
        let resting = self.anchor.get().unwrap_or(Rect::new(0, 0, 0, 0));
        let center =
            resting.x + resting.w * saturating_i32(position.to_f32() + 0.5) / saturating_i32(count.to_f32());
        drawer::place_above(center, OPTIONS_WIDTH, self.screen, saturating_i32(EDGE_GAP))
    }

    /// The options: a rounded bar above the button, rising and fading in. Its options stay in
    /// one row while they fit, then wrap. Only the bar catches the pointer, not the room
    /// around it.
    fn options(&self, index: usize, options: Vec<AnyElement>, placement: Placement) -> Div {
        let overlay = self.overlay.clone();
        let pinned = move |_: &MouseDownEvent, _: &mut Window, cx: &mut App| {
            overlay.update(cx, |overlay, cx| overlay.drawer_pressed(index, cx));
        };
        let options = options.into_iter().map(|option| div().flex_none().child(option));
        let bar = super::panel()
            .id(("drawer", index))
            .rounded_full()
            .px_2()
            .flex_none();
        let bar = bar
            .max_w(px(placement.width.to_f32()))
            .flex_wrap()
            .on_hover(self.hover_listener(index, Part::Drawer))
            .on_mouse_move(self.moved_listener(index))
            .on_mouse_down(MouseButton::Left, pinned)
            .children(options)
            .with_animation(
                ("drawer-in", index),
                Animation::new(super::motion::slide()).with_easing(ease_in_out),
                |bar, delta| bar.opacity(delta).relative().top(px(RISE * (1. - delta))),
            );
        let sideways = div().relative().left(px(placement.shift.to_f32())).child(bar);
        let above = div()
            .absolute()
            .bottom_full()
            .left_0()
            .right_0()
            .pb(px(drawer::GAP.to_f32()));
        above.flex().justify_center().child(sideways)
    }

    /// Moving over the bar says the pointer is on it, in case the enter was missed.
    fn moved_listener(&self, index: usize) -> impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static {
        let overlay = self.overlay.clone();
        move |_, _, cx| {
            overlay.update(cx, |overlay, cx| {
                overlay.drawer_hover(index, Part::Drawer, true, cx);
            });
        }
    }

    fn hover_listener(&self, index: usize, part: Part) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        let overlay = self.overlay.clone();
        move |hovered, _, cx| {
            overlay.update(cx, |overlay, cx| overlay.drawer_hover(index, part, *hovered, cx));
        }
    }
}

/// Note where the buttons are; when that changed, draw again with the room they need.
fn remember_bounds(anchor: &Cell<Option<Rect>>, bounds: Bounds<Pixels>, window: &mut Window) {
    let at = |value: Pixels| saturating_i32(f32::from(value));
    let found = Rect::new(
        at(bounds.origin.x),
        at(bounds.origin.y),
        at(bounds.size.width),
        at(bounds.size.height),
    );
    if anchor.replace(Some(found)) != Some(found) {
        window.refresh();
    }
}

/// Why the last action couldn't be done.
fn notice_line(text: &SharedString) -> Div {
    div()
        .px_2()
        .rounded_sm()
        .bg(theme::current().marks.scrim)
        .text_sm()
        .text_color(theme::current().marks.danger_text)
        .child(text.clone())
}

/// `place` at `offset` from its screen `edge`; while `leaving`, it slides off past that edge.
pub fn slide(place: Div, edge: Edge, offset: f32, leaving: bool, id: &'static str) -> AnyElement {
    let at = move |place: Div, travelled: f32| match edge {
        Edge::Top => place.top(px(offset - travelled)),
        Edge::Bottom => place.bottom(px(offset - travelled)),
    };
    if !leaving {
        return at(place, 0.).into_any_element();
    }
    let animation = Animation::new(super::motion::slide()).with_easing(ease_in_out);
    let animated = at(place, 0.).with_animation(id, animation, move |place, delta| {
        at(place, SLIDE_DISTANCE * delta)
    });
    animated.into_any_element()
}
