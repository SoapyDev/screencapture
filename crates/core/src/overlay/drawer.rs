//! A drawer that opens beside a button: when it opens and closes, and where it goes.
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
/// Space between the button and its drawer.
pub const GAP: i32 = 8;

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

/// How big the drawer would like to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WantedSize {
    pub width: i32,
    pub height: i32,
}

/// Where everything goes while the drawer is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The column of buttons, moved right if the drawer needed the room.
    pub column: Rect,
    /// The button the drawer belongs to.
    pub button: Rect,
    /// The drawer: left of the button, its bottom on the button's bottom; the content wraps in
    /// this width.
    pub drawer: Rect,
}

/// Where a drawer for `button` (in `column`) opens on `screen`, `margin` from its edges.
///
/// To the left of the button, over whatever is there. With too little room the column moves
/// right as far as the margin allows, and then the drawer narrows (its content wraps).
#[must_use]
pub fn place(column: Rect, button: Rect, wanted: WantedSize, screen: Rect, margin: i32) -> Placement {
    let room_left = |from: i32| from - GAP - (screen.x + margin);
    let missing = (wanted.width - room_left(column.x)).max(0);
    let room_right = (screen.right() - margin - column.right()).max(0);
    let shift = missing.min(room_right);
    let width = wanted.width.min(room_left(column.x + shift)).max(0);
    let moved = |rect: Rect| Rect {
        x: rect.x + shift,
        ..rect
    };
    let button = moved(button);
    let top = (button.bottom() - wanted.height).max(screen.y + margin);
    Placement {
        column: moved(column),
        button,
        drawer: Rect::new(button.x - GAP - width, top, width, button.bottom() - top),
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
    const WANTED: WantedSize = WantedSize {
        width: 300,
        height: 120,
    };

    fn column_at(x: i32) -> Rect {
        Rect::new(x, 500, 100, 70)
    }

    fn button_in(column: Rect) -> Rect {
        Rect::new(column.x, column.y + 35, column.w, 35)
    }

    fn placed(x: i32) -> Placement {
        let column = column_at(x);
        place(column, button_in(column), WANTED, SCREEN, 24)
    }

    #[test]
    fn with_room_on_the_left_nothing_moves() {
        assert_eq!(placed(450).column, column_at(450));
    }

    #[test]
    fn the_drawer_sits_left_of_its_button_with_a_gap() {
        assert_eq!(placed(450).drawer.right(), 450 - GAP);
    }

    #[test]
    fn the_drawer_has_the_wanted_width_when_there_is_room() {
        assert_eq!(placed(450).drawer.w, 300);
    }

    #[test]
    fn the_drawer_grows_up_from_its_buttons_bottom() {
        assert_eq!(placed(450).drawer.bottom(), 570);
    }

    #[test]
    fn with_too_little_room_the_column_moves_right() {
        assert!(placed(200).column.x > 200);
    }

    #[test]
    fn the_button_moves_with_the_column() {
        let placement = placed(200);
        assert_eq!(placement.button.x, placement.column.x);
    }

    #[test]
    fn once_moved_the_drawer_has_the_wanted_width() {
        assert_eq!(placed(200).drawer.w, 300);
    }

    #[test]
    fn the_column_never_passes_the_right_margin() {
        assert!(placed(10).column.right() <= SCREEN.right() - 24);
    }

    #[test]
    fn on_a_small_screen_the_drawer_narrows_to_fit() {
        let screen = Rect::new(0, 0, 300, 600);
        let column = Rect::new(150, 500, 100, 70);
        let drawer = place(column, button_in(column), WANTED, screen, 24).drawer;
        assert!(drawer.x >= 24);
    }

    #[test]
    fn a_drawer_taller_than_the_room_above_stops_at_the_top_margin() {
        let tall = WantedSize {
            width: 300,
            height: 900,
        };
        let column = column_at(450);
        assert_eq!(place(column, button_in(column), tall, SCREEN, 24).drawer.y, 24);
    }
}
