//! One overlay at a time in the daemon, and no capture of the last one on its way out.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

/// After the overlay ends, how long until the screens no longer show it: gpui still has to
/// destroy the surfaces, and the compositor to process that and repaint (a frame or two; more with a layer
/// fade-out animation, which the README's `no_anim` rule turns off).
pub const SETTLE: Duration = Duration::from_millis(100);

#[derive(Debug, PartialEq, Eq)]
pub enum Entry {
    /// An overlay is open: refuse.
    Busy,
    /// Capture after `wait`, so the previous overlay is not in the picture.
    Go { wait: Duration },
}

/// Shared by the socket thread (enters) and the UI thread (leaves).
pub struct Gate(Mutex<State>);

struct State {
    open: bool,
    settled_at: Option<Instant>,
}

impl Gate {
    pub const fn new() -> Self {
        Self(Mutex::new(State {
            open: false,
            settled_at: None,
        }))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A request arrived at `now`.
    pub fn enter(&self, now: Instant) -> Entry {
        let settled_at = {
            let mut state = self.state();
            if state.open {
                return Entry::Busy;
            }
            state.open = true;
            state.settled_at
        };
        Entry::Go {
            wait: settled_at.map_or(Duration::ZERO, |t| t.saturating_duration_since(now)),
        }
    }

    /// The overlay ended at `now`; its windows are being removed.
    pub fn leave(&self, now: Instant) {
        let mut state = self.state();
        state.open = false;
        state.settled_at = Some(now + SETTLE);
    }

    /// The request ended before any overlay showed.
    pub fn abort(&self) {
        self.state().open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_second_request_while_open_is_busy() {
        let gate = Gate::new();
        let now = Instant::now();
        assert_eq!(gate.enter(now), Entry::Go { wait: Duration::ZERO });
        assert_eq!(gate.enter(now), Entry::Busy);
    }

    #[test]
    fn right_after_closing_the_next_capture_waits_for_the_screens_to_settle() {
        let gate = Gate::new();
        let t = Instant::now();
        gate.enter(t);
        gate.leave(t);
        let soon = t + Duration::from_millis(30);
        assert_eq!(
            gate.enter(soon),
            Entry::Go {
                wait: SETTLE.saturating_sub(Duration::from_millis(30))
            }
        );
        gate.leave(soon);
        assert_eq!(gate.enter(soon + SETTLE * 2), Entry::Go { wait: Duration::ZERO });
    }

    #[test]
    fn a_request_that_never_showed_an_overlay_needs_no_settling() {
        let gate = Gate::new();
        let t = Instant::now();
        gate.enter(t);
        gate.abort();
        assert_eq!(gate.enter(t), Entry::Go { wait: Duration::ZERO });
    }
}
