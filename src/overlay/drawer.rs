//! The controls drawer while recording: a thin strip at the top of each screen; resting the
//! pointer there slides down Stop / Discard and the time recorded. No keyboard, no pause.

use std::time::{Duration, Instant};

use crate::capture::Rect;

/// Pointer rest on the strip before the drawer opens: passing through doesn't open it.
pub const DWELL: Duration = Duration::from_millis(150);
/// Time away from the drawer before it closes: a slip of the hand doesn't close it.
pub const LINGER: Duration = Duration::from_millis(300);
/// The strip's size: narrow, so clicks on a top bar beside it still reach the bar.
pub const STRIP: (i32, i32) = (320, 2);
/// The drawer's size.
pub const DRAWER: (i32, i32) = (600, 56);

/// The strip that opens the drawer on `screen`: centred on its top edge.
pub fn hot_strip(screen: Rect) -> Rect {
    centred_on_top(screen, STRIP)
}

/// Where the drawer opens on `screen`: centred below its top edge.
pub fn drawer_place(screen: Rect) -> Rect {
    centred_on_top(screen, DRAWER)
}

fn centred_on_top(screen: Rect, (width, height): (i32, i32)) -> Rect {
    let width = width.min(screen.w);
    Rect::new(screen.x + (screen.w - width) / 2, screen.y, width, height)
}

/// `elapsed` as a clock: `0:07`, `12:34`, `1:02:03`.
pub fn clock(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours == 0 {
        return format!("{minutes}:{seconds:02}");
    }
    format!("{hours}:{minutes:02}:{seconds:02}")
}

/// Whether the drawer is open, and what the pointer is doing about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drawer {
    Closed,
    /// The pointer rests on the strip since then.
    Opening {
        since: Instant,
    },
    Open,
    /// The pointer left the drawer then.
    Closing {
        since: Instant,
    },
}

/// What to do on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Nothing,
    Open,
    Close,
}

impl Drawer {
    /// The pointer reached the strip or the drawer.
    pub const fn pointer_entered(self, now: Instant) -> Self {
        match self {
            Self::Closed => Self::Opening { since: now },
            Self::Closing { .. } => Self::Open,
            same @ (Self::Opening { .. } | Self::Open) => same,
        }
    }

    /// The pointer left the strip or the drawer.
    pub const fn pointer_left(self, now: Instant) -> Self {
        match self {
            Self::Opening { .. } => Self::Closed,
            Self::Open => Self::Closing { since: now },
            same @ (Self::Closed | Self::Closing { .. }) => same,
        }
    }

    /// Where things stand at `now`, and what that changes on screen.
    pub fn at(self, now: Instant) -> (Self, Change) {
        match self {
            Self::Opening { since } if now.saturating_duration_since(since) >= DWELL => {
                (Self::Open, Change::Open)
            }
            Self::Closing { since } if now.saturating_duration_since(since) >= LINGER => {
                (Self::Closed, Change::Close)
            }
            same => (same, Change::Nothing),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(2560, 0, 2560, 1440);

    fn later(start: Instant, by: Duration) -> Instant {
        start.checked_add(by).expect("a test instant fits")
    }

    #[test]
    fn the_strip_is_centred_on_the_top_edge() {
        assert_eq!(
            hot_strip(SCREEN),
            Rect::new(2560 + (2560 - STRIP.0) / 2, 0, STRIP.0, STRIP.1)
        );
    }

    #[test]
    fn the_drawer_opens_centred_under_the_top_edge() {
        assert_eq!(
            drawer_place(SCREEN),
            Rect::new(2560 + (2560 - DRAWER.0) / 2, 0, DRAWER.0, DRAWER.1)
        );
    }

    #[test]
    fn short_times_read_as_minutes_and_seconds() {
        assert_eq!(clock(Duration::from_secs(7)), "0:07");
    }

    #[test]
    fn long_times_read_with_hours() {
        assert_eq!(clock(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn resting_on_the_strip_starts_opening() {
        let now = Instant::now();
        assert_eq!(
            Drawer::Closed.pointer_entered(now),
            Drawer::Opening { since: now }
        );
    }

    #[test]
    fn passing_through_the_strip_opens_nothing() {
        let now = Instant::now();
        assert_eq!(Drawer::Opening { since: now }.pointer_left(now), Drawer::Closed);
    }

    #[test]
    fn a_short_rest_is_not_enough() {
        let now = Instant::now();
        let (_, change) = Drawer::Opening { since: now }.at(later(now, DWELL / 2));
        assert_eq!(change, Change::Nothing);
    }

    #[test]
    fn resting_long_enough_opens_it() {
        let now = Instant::now();
        assert_eq!(
            Drawer::Opening { since: now }.at(later(now, DWELL)),
            (Drawer::Open, Change::Open)
        );
    }

    #[test]
    fn leaving_the_open_drawer_starts_closing() {
        let now = Instant::now();
        assert_eq!(Drawer::Open.pointer_left(now), Drawer::Closing { since: now });
    }

    #[test]
    fn coming_back_in_time_keeps_it_open() {
        let now = Instant::now();
        assert_eq!(Drawer::Closing { since: now }.pointer_entered(now), Drawer::Open);
    }

    #[test]
    fn staying_away_closes_it() {
        let now = Instant::now();
        assert_eq!(
            Drawer::Closing { since: now }.at(later(now, LINGER)),
            (Drawer::Closed, Change::Close)
        );
    }

    #[test]
    fn an_open_drawer_stays_open_by_itself() {
        assert_eq!(Drawer::Open.at(Instant::now()), (Drawer::Open, Change::Nothing));
    }
}
