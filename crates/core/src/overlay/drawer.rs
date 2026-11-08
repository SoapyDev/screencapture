//! A drawer that opens above a button: when it opens and closes, and where it goes.
//!
//! Resting the pointer on the button (or the drawer) opens it; leaving closes it after a short
//! linger, so a slip of the hand doesn't. Clicking inside pins it open until a click outside, the
//! Escape key, or the toggle key. Time is passed in, so nothing here waits.

use std::time::{Duration, Instant};

use crate::capture::Rect;

/// Pointer rest on the button before the drawer opens: passing over doesn't open it.
pub const DWELL: Duration = Duration::from_millis(120);
/// Time away from the button and the drawer before it closes.
pub const LINGER: Duration = Duration::from_millis(300);
/// Space between the button and its options bar.
pub const GAP: i32 = 4;

/// Whether the drawer is open, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drawer {
    Closed,
    /// The pointer rests on the button since then.
    Opening {
        since: Instant,
    },
    /// Open while the pointer stays on the button or the drawer.
    Open,
    /// The pointer left the button and the drawer then.
    Closing {
        since: Instant,
    },
    /// Open until a click outside, Escape or the toggle key.
    Pinned,
}

/// What happened that the drawer cares about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The pointer came onto the button or the drawer.
    PointerEntered,
    /// The pointer left both.
    PointerLeft,
    /// An option in the drawer was clicked.
    OptionClicked,
    ClickedOutside,
    Escape,
    /// The key that opens and closes the drawer.
    ToggleKey,
    /// Time passed: check the dwell and the linger.
    Tick,
}

impl Drawer {
    /// Whether the drawer is on screen.
    #[must_use]
    pub const fn is_shown(self) -> bool {
        matches!(self, Self::Open | Self::Closing { .. } | Self::Pinned)
    }

    /// The state after `event` at `now`.
    #[must_use]
    pub fn after(self, event: Event, now: Instant) -> Self {
        match event {
            Event::PointerEntered => self.pointer_entered(now),
            Event::PointerLeft => self.pointer_left(now),
            Event::OptionClicked => Self::Pinned,
            Event::ClickedOutside | Event::Escape => Self::Closed,
            Event::ToggleKey => self.toggled(),
            Event::Tick => self.ticked(now),
        }
    }

    /// When to send [`Event::Tick`] next, from `now`: `None` when nothing is waiting.
    #[must_use]
    pub fn next_tick(self, now: Instant) -> Option<Duration> {
        let (since, wait) = match self {
            Self::Opening { since } => (since, DWELL),
            Self::Closing { since } => (since, LINGER),
            _ => return None,
        };
        Some((since + wait).saturating_duration_since(now))
    }

    const fn pointer_entered(self, now: Instant) -> Self {
        match self {
            Self::Closed => Self::Opening { since: now },
            Self::Closing { .. } => Self::Open,
            other => other,
        }
    }

    const fn pointer_left(self, now: Instant) -> Self {
        match self {
            Self::Opening { .. } => Self::Closed,
            Self::Open => Self::Closing { since: now },
            other => other,
        }
    }

    const fn toggled(self) -> Self {
        match self {
            Self::Closed => Self::Pinned,
            _ => Self::Closed,
        }
    }

    fn ticked(self, now: Instant) -> Self {
        match self {
            Self::Opening { since } if now >= since + DWELL => Self::Open,
            Self::Closing { since } if now >= since + LINGER => Self::Closed,
            other => other,
        }
    }
}

/// Where an options bar goes: how wide, and how far to slide sideways to stay on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The most the bar may take; its content wraps beyond.
    pub width: i32,
    /// Added to the bar's centred position (negative: left).
    pub shift: i32,
}

/// Where the options bar for a button centred at `center` goes on `screen`, `margin` from its
/// edges.
///
/// Centred over the button, slid sideways just enough to stay inside, and no wider than the
/// screen allows (its content wraps then).
#[must_use]
pub fn place_above(center: i32, wanted_width: i32, screen: Rect, margin: i32) -> Placement {
    let width = wanted_width.min(screen.w - 2 * margin).max(0);
    let left = center - width / 2;
    let leftmost = screen.x + margin;
    let rightmost = (screen.right() - margin - width).max(leftmost);
    Placement {
        width,
        shift: left.clamp(leftmost, rightmost) - left,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn later(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn resting_on_the_button_starts_opening() {
        let now = Instant::now();
        assert_eq!(
            Drawer::Closed.after(Event::PointerEntered, now),
            Drawer::Opening { since: now }
        );
    }

    #[test]
    fn the_dwell_opens_it() {
        let start = Instant::now();
        let opening = Drawer::Opening { since: start };
        assert_eq!(opening.after(Event::Tick, later(start, 120)), Drawer::Open);
    }

    #[test]
    fn before_the_dwell_it_keeps_waiting() {
        let start = Instant::now();
        let opening = Drawer::Opening { since: start };
        assert_eq!(opening.after(Event::Tick, later(start, 50)), opening);
    }

    #[test]
    fn passing_over_the_button_opens_nothing() {
        let start = Instant::now();
        let opening = Drawer::Opening { since: start };
        assert_eq!(
            opening.after(Event::PointerLeft, later(start, 50)),
            Drawer::Closed
        );
    }

    #[test]
    fn leaving_an_open_drawer_starts_closing() {
        let now = Instant::now();
        assert_eq!(
            Drawer::Open.after(Event::PointerLeft, now),
            Drawer::Closing { since: now }
        );
    }

    #[test]
    fn the_linger_closes_it() {
        let start = Instant::now();
        let closing = Drawer::Closing { since: start };
        assert_eq!(closing.after(Event::Tick, later(start, 300)), Drawer::Closed);
    }

    #[test]
    fn coming_back_during_the_linger_keeps_it_open() {
        let start = Instant::now();
        let closing = Drawer::Closing { since: start };
        assert_eq!(
            closing.after(Event::PointerEntered, later(start, 100)),
            Drawer::Open
        );
    }

    #[test]
    fn hovering_an_option_is_hovering_the_drawer() {
        let now = Instant::now();
        assert_eq!(Drawer::Open.after(Event::PointerEntered, now), Drawer::Open);
    }

    #[test]
    fn clicking_an_option_pins_it() {
        assert_eq!(
            Drawer::Open.after(Event::OptionClicked, Instant::now()),
            Drawer::Pinned
        );
    }

    #[test]
    fn clicking_an_option_while_closing_pins_it() {
        let now = Instant::now();
        assert_eq!(
            Drawer::Closing { since: now }.after(Event::OptionClicked, now),
            Drawer::Pinned
        );
    }

    #[test]
    fn a_pinned_drawer_stays_when_the_pointer_leaves() {
        assert_eq!(
            Drawer::Pinned.after(Event::PointerLeft, Instant::now()),
            Drawer::Pinned
        );
    }

    #[test]
    fn a_click_outside_closes_a_pinned_drawer() {
        assert_eq!(
            Drawer::Pinned.after(Event::ClickedOutside, Instant::now()),
            Drawer::Closed
        );
    }

    #[test]
    fn escape_closes_a_pinned_drawer() {
        assert_eq!(
            Drawer::Pinned.after(Event::Escape, Instant::now()),
            Drawer::Closed
        );
    }

    #[test]
    fn the_toggle_key_opens_a_closed_drawer_pinned() {
        assert_eq!(
            Drawer::Closed.after(Event::ToggleKey, Instant::now()),
            Drawer::Pinned
        );
    }

    #[test]
    fn the_toggle_key_closes_an_open_drawer() {
        assert_eq!(
            Drawer::Pinned.after(Event::ToggleKey, Instant::now()),
            Drawer::Closed
        );
    }

    #[test]
    fn only_open_states_are_shown() {
        let now = Instant::now();
        let shown = [
            Drawer::Closed,
            Drawer::Opening { since: now },
            Drawer::Open,
            Drawer::Pinned,
        ]
        .map(Drawer::is_shown);
        assert_eq!(shown, [false, false, true, true]);
    }

    #[test]
    fn a_closing_drawer_is_still_shown() {
        assert!(
            Drawer::Closing {
                since: Instant::now()
            }
            .is_shown()
        );
    }

    #[test]
    fn an_opening_drawer_asks_for_a_tick_at_the_end_of_the_dwell() {
        let start = Instant::now();
        assert_eq!(Drawer::Opening { since: start }.next_tick(start), Some(DWELL));
    }

    #[test]
    fn a_pinned_drawer_asks_for_no_tick() {
        assert_eq!(Drawer::Pinned.next_tick(Instant::now()), None);
    }

    const SCREEN: Rect = Rect::new(0, 0, 1000, 600);

    #[test]
    fn with_room_on_both_sides_the_bar_stays_centred() {
        assert_eq!(place_above(500, 300, SCREEN, 24).shift, 0);
    }

    #[test]
    fn the_bar_has_the_wanted_width_when_it_fits() {
        assert_eq!(place_above(500, 300, SCREEN, 24).width, 300);
    }

    #[test]
    fn near_the_left_edge_the_bar_slides_right() {
        assert_eq!(place_above(100, 300, SCREEN, 24).shift, 74);
    }

    #[test]
    fn near_the_right_edge_the_bar_slides_left() {
        assert_eq!(place_above(900, 300, SCREEN, 24).shift, -74);
    }

    #[test]
    fn a_bar_wider_than_the_screen_narrows_to_the_margins() {
        assert_eq!(place_above(500, 2000, SCREEN, 24).width, 952);
    }

    #[test]
    fn a_narrowed_bar_fills_the_space_between_the_margins() {
        let placed = place_above(300, 2000, SCREEN, 24);
        assert_eq!(300 - placed.width / 2 + placed.shift, 24);
    }
}
