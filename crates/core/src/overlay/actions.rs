//! The bottom action bar: what to do with the selection, side by side from every feature, then
//! each feature's settings. It slides away with the toolbar when a feature takes over.

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    Animation, AnimationExt, AnyElement, App, Bounds, Div, Entity, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, SharedString, Window, canvas, div, ease_in_out, prelude::*, px,
};

use super::{
    Overlay,
    drawer::{self, WantedSize},
    drawers::{Drawers, Part},
    feature::Actions,
    layout::Edge,
    theme,
};
use crate::{
    capture::Rect,
    units::{ToF32, saturating_i32},
};

/// How long the bars take to slide off the screens when a recording starts.
pub const SLIDE: Duration = Duration::from_millis(150);
/// Distance of the bars from their screen edge.
pub const EDGE_GAP: f32 = 24.;
/// Room the action bar takes at the bottom: a toolbar at the bottom sits above it.
pub const ACTION_BAR_ROOM: f32 = 48.;
/// How far the bars travel when they slide away: past any screen edge.
const SLIDE_DISTANCE: f32 = 240.;

/// How wide a drawer would like to be, and its room around the screen's edge.
const DRAWER_WIDTH: i32 = 780;
const DRAWER_HEIGHT: i32 = 160;

/// What the action bar is drawn from.
pub struct ActionBar<'a> {
    pub overlay: &'a Entity<Overlay>,
    pub drawers: &'a Drawers,
    /// Where the buttons rest, filled in as they are painted.
    pub anchor: &'a Rc<Cell<Option<Rect>>>,
    /// The screen, in the window's own coordinates.
    pub screen: Rect,
}

impl ActionBar<'_> {
    /// The bar for every feature's `actions`, with `notice` above it.
    pub fn render(&self, actions: Vec<Actions>, notice: Option<&SharedString>, leaving: bool) -> AnyElement {
        let (shift, width) = self.room_for_drawers();
        let column = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .children(notice.map(notice_line))
            .child(self.anchored(self.buttons(actions, width), shift));
        let place = div()
            .absolute()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(column);
        slide(place, Edge::Bottom, EDGE_GAP, leaving, "action-bar")
    }

    /// How far to move the buttons right so a drawer fits on their left, and how wide the
    /// drawer then is. The buttons rest there whether or not a drawer is open, so they never
    /// move away from under the pointer.
    fn room_for_drawers(&self) -> (i32, i32) {
        let Some(resting) = self.anchor.get() else {
            return (0, DRAWER_WIDTH);
        };
        let wanted = WantedSize {
            width: DRAWER_WIDTH,
            height: DRAWER_HEIGHT,
        };
        let placed = drawer::place(resting, resting, wanted, self.screen, saturating_i32(EDGE_GAP));
        (placed.column.x - resting.x, placed.drawer.w)
    }

    /// `buttons`, moved right by `shift`, with a measure of where they would rest unmoved.
    fn anchored(&self, buttons: Div, shift: i32) -> Div {
        let anchor = Rc::clone(self.anchor);
        let measure = canvas(
            move |bounds, window, _| remember_bounds(&anchor, bounds, window),
            |_, (), _, _| {},
        );
        div()
            .relative()
            .child(measure.absolute().size_full())
            .child(buttons.relative().left(px(shift.to_f32())))
    }

    /// The feature's round buttons, one over the other with a gap to see the screen through,
    /// each with its drawer.
    fn buttons(&self, actions: Vec<Actions>, drawer_width: i32) -> Div {
        let cells = actions
            .into_iter()
            .enumerate()
            .filter(|(_, actions)| !actions.main.is_empty());
        let cells = cells.map(|(index, actions)| self.cell(index, actions, drawer_width));
        div().flex().flex_col().gap_2().children(cells)
    }

    fn cell(&self, index: usize, actions: Actions, drawer_width: i32) -> Div {
        let shown = self.drawers.state(index).is_shown() && !actions.drawer.is_empty();
        let hovered = self.hover_listener(index, Part::Button);
        let button = div()
            .id(("action", index))
            .on_hover(hovered)
            .children(actions.main);
        let drawer = shown.then(|| self.drawer(index, actions.drawer, drawer_width));
        div().relative().child(button).children(drawer)
    }

    /// The drawer: a bar on the button's left, at its height, sliding out of it. Its options
    /// stay in one row while they fit in `width`, then wrap. The room around the bar is not
    /// part of it: only the bar itself catches the pointer.
    fn drawer(&self, index: usize, options: Vec<AnyElement>, width: i32) -> Div {
        let overlay = self.overlay.clone();
        let pinned = move |_: &MouseDownEvent, _: &mut Window, cx: &mut App| {
            overlay.update(cx, |overlay, cx| overlay.drawer_pressed(index, cx));
        };
        let options = options.into_iter().map(|option| div().flex_none().child(option));
        let bar = super::panel()
            .id(("drawer", index))
            .rounded_full()
            .px_2()
            .max_w_full()
            .flex_wrap();
        let bar = bar
            .on_hover(self.hover_listener(index, Part::Drawer))
            .on_mouse_move(self.moved_listener(index))
            .on_mouse_down(MouseButton::Left, pinned)
            .children(options)
            .with_animation(
                ("drawer-in", index),
                Animation::new(SLIDE).with_easing(ease_in_out),
                |bar, delta| bar.opacity(delta).mr(px(-24. * (1. - delta))),
            );
        let place = div().absolute().right_full().bottom_0().w(px(width.to_f32()));
        place
            .pr(px(drawer::GAP.to_f32()))
            .flex()
            .justify_end()
            .items_center()
            .child(bar)
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
    let animation = Animation::new(SLIDE).with_easing(ease_in_out);
    let animated = at(place, 0.).with_animation(id, animation, move |place, delta| {
        at(place, SLIDE_DISTANCE * delta)
    });
    animated.into_any_element()
}
