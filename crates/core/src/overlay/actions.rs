//! The bottom action bar: what to do with the selection, side by side from every feature, then
//! each feature's settings. It slides away with the toolbar when a feature takes over.

use std::time::Duration;

use gpui::{Animation, AnimationExt, AnyElement, Div, SharedString, div, ease_in_out, prelude::*, px};

use super::{feature::Actions, layout::Edge, theme};

/// How long the bars take to slide off the screens when a recording starts.
pub const SLIDE: Duration = Duration::from_millis(150);
/// Distance of the bars from their screen edge.
pub const EDGE_GAP: f32 = 24.;
/// Room the action bar takes at the bottom: a toolbar at the bottom sits above it.
pub const ACTION_BAR_ROOM: f32 = 48.;
/// How far the bars travel when they slide away: past any screen edge.
const SLIDE_DISTANCE: f32 = 240.;

/// The bar for every feature's `actions`, with `notice` above it.
pub fn action_bar(actions: Vec<Actions>, notice: Option<&SharedString>, leaving: bool) -> AnyElement {
    let column = div()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .children(notice.map(notice_line))
        .child(row(actions));
    let place = div()
        .absolute()
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(column);
    slide(place, Edge::Bottom, EDGE_GAP, leaving, "action-bar")
}

/// The main actions together, then each feature's settings after a separator.
fn row(actions: Vec<Actions>) -> Div {
    let (main, settings): (Vec<_>, Vec<_>) = actions.into_iter().map(|a| (a.main, a.settings)).unzip();
    let groups = settings.into_iter().filter(|group| !group.is_empty());
    let separated =
        groups.flat_map(|group| std::iter::once(super::separator().into_any_element()).chain(group));
    super::panel()
        .children(main.into_iter().flatten())
        .children(separated)
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
