//! The drawers of every feature's action button: which is open, where the pointer is, and the
//! rule that only one is open at a time.

use std::time::{Duration, Instant};

use super::drawer::{Drawer, Event, KEEP_ALIVE, LINGER};

/// Which part of a feature's button the pointer is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Button,
    Drawer,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    /// Whether the feature has a drawer at all.
    exists: bool,
    state: Drawer,
    /// When the drawer last opened by itself: it stays at least [`KEEP_ALIVE`] from then.
    opened: Option<Instant>,
    over_button: bool,
    over_drawer: bool,
}

impl Slot {
    const fn new(exists: bool) -> Self {
        Self {
            exists,
            state: Drawer::Closed,
            opened: None,
            over_button: false,
            over_drawer: false,
        }
    }

    const fn over(&self) -> bool {
        self.over_button || self.over_drawer
    }

    /// Remember when the drawer went from waiting to open.
    fn note_opening(&mut self, before: Drawer, now: Instant) {
        if matches!(before, Drawer::Opening { .. }) && self.state == Drawer::Open {
            self.opened = Some(now);
        }
    }

    /// Leaving soon after opening: the drawer closes no sooner than [`KEEP_ALIVE`] after it
    /// opened.
    fn keep_alive_on_leaving(&mut self, event: Event) {
        let (Event::PointerLeft, Drawer::Closing { since }, Some(opened)) = (event, self.state, self.opened)
        else {
            return;
        };
        let earliest = opened + KEEP_ALIVE.saturating_sub(LINGER);
        self.state = Drawer::Closing {
            since: since.max(earliest),
        };
    }
}

/// One drawer per feature, by feature index.
#[derive(Debug)]
pub struct Drawers {
    slots: Vec<Slot>,
}

impl Drawers {
    #[must_use]
    pub fn new(has_drawer: impl IntoIterator<Item = bool>) -> Self {
        Self {
            slots: has_drawer.into_iter().map(Slot::new).collect(),
        }
    }

    /// The toggle key: opens the first drawer pinned; with one open, moves on to the next, and
    /// past the last closes.
    pub fn toggle_next(&mut self, now: Instant) {
        let shown = (0..self.slots.len()).find(|index| self.state(*index).is_shown());
        let from = shown.map_or(0, |index| index + 1);
        let next = (from..self.slots.len()).find(|index| self.slots[*index].exists);
        if let Some(index) = shown {
            self.send(index, Event::ToggleKey, now);
        }
        if let Some(index) = next {
            self.send(index, Event::ToggleKey, now);
        }
    }

    /// Where drawer `index` stands.
    #[must_use]
    pub fn state(&self, index: usize) -> Drawer {
        self.slots.get(index).map_or(Drawer::Closed, |slot| slot.state)
    }

    /// The pointer entered or left `part` of button `index`. `true` if anything changed.
    pub fn hover(&mut self, index: usize, part: Part, hovered: bool, now: Instant) -> bool {
        let Some(slot) = self.slots.get_mut(index) else {
            return false;
        };
        let (was_over, was) = (slot.over(), slot.state);
        match part {
            Part::Button => slot.over_button = hovered,
            Part::Drawer => slot.over_drawer = hovered,
        }
        let event = match (was_over, slot.over()) {
            (false, true) => Some(Event::PointerEntered),
            (true, false) => Some(Event::PointerLeft),
            _ => None,
        };
        if let Some(event) = event {
            self.send(index, event, now);
        }
        event.is_some() || self.state(index) != was
    }

    /// An option of drawer `index` was pressed: it stays open.
    pub fn option_pressed(&mut self, index: usize, now: Instant) {
        self.send(index, Event::OptionClicked, now);
    }

    /// A press that hit neither a button nor a drawer: pinned drawers close.
    pub fn pressed_outside(&mut self, now: Instant) {
        for index in 0..self.slots.len() {
            if self.state(index) == Drawer::Pinned {
                self.send(index, Event::ClickedOutside, now);
            }
        }
    }

    /// Escape: closes the open drawers. `true` if any was.
    pub fn escape(&mut self, now: Instant) -> bool {
        let shown: Vec<_> = (0..self.slots.len())
            .filter(|index| self.state(*index).is_shown())
            .collect();
        for index in &shown {
            self.send(*index, Event::Escape, now);
        }
        !shown.is_empty()
    }

    /// Time passed: dwell and linger may be over.
    pub fn tick(&mut self, now: Instant) {
        for index in 0..self.slots.len() {
            self.keep_open_under_the_pointer(index);
            self.send(index, Event::Tick, now);
        }
    }

    /// A drawer never closes with the pointer on it, whatever the events said.
    fn keep_open_under_the_pointer(&mut self, index: usize) {
        let Some(slot) = self.slots.get_mut(index) else {
            return;
        };
        if slot.over() && matches!(slot.state, Drawer::Closing { .. }) {
            slot.state = Drawer::Open;
        }
    }

    /// When to tick next, from `now`.
    #[must_use]
    pub fn next_tick(&self, now: Instant) -> Option<Duration> {
        self.slots
            .iter()
            .filter_map(|slot| slot.state.next_tick(now))
            .min()
    }

    fn send(&mut self, index: usize, event: Event, now: Instant) {
        let Some(slot) = self.slots.get_mut(index) else {
            return;
        };
        let before = slot.state;
        slot.state = before.after(event, now);
        slot.note_opening(before, now);
        slot.keep_alive_on_leaving(event);
        if matches!(slot.state, Drawer::Open | Drawer::Pinned) {
            self.close_the_others(index);
        }
    }

    fn close_the_others(&mut self, kept: usize) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if index != kept && slot.state.is_shown() {
                slot.state = Drawer::Closed;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::drawer::{DWELL, LINGER};

    fn opened(start: Instant) -> Drawers {
        let mut drawers = Drawers::new([true, true]);
        drawers.hover(0, Part::Button, true, start);
        drawers.tick(start + DWELL);
        drawers
    }

    #[test]
    fn resting_on_a_button_opens_its_drawer() {
        assert_eq!(opened(Instant::now()).state(0), Drawer::Open);
    }

    #[test]
    fn the_other_drawer_stays_closed() {
        assert_eq!(opened(Instant::now()).state(1), Drawer::Closed);
    }

    #[test]
    fn moving_from_the_button_onto_the_drawer_keeps_it_open() {
        let start = Instant::now();
        let mut drawers = opened(start);
        drawers.hover(0, Part::Drawer, true, start + DWELL);
        drawers.hover(0, Part::Button, false, start + DWELL);
        drawers.tick(start + DWELL + LINGER);
        assert_eq!(drawers.state(0), Drawer::Open);
    }

    #[test]
    fn leaving_both_closes_it_after_the_linger() {
        let start = Instant::now();
        let mut drawers = opened(start);
        drawers.hover(0, Part::Button, false, start + DWELL);
        drawers.tick(start + DWELL + KEEP_ALIVE);
        assert_eq!(drawers.state(0), Drawer::Closed);
    }

    #[test]
    fn leaving_right_after_opening_does_not_close_it_within_the_keep_alive() {
        let start = Instant::now();
        let mut drawers = opened(start);
        drawers.hover(0, Part::Button, false, start + DWELL);
        drawers.tick(start + DWELL + KEEP_ALIVE / 2);
        assert!(drawers.state(0).is_shown());
    }

    #[test]
    fn leaving_long_after_opening_closes_after_the_linger_only() {
        let start = Instant::now();
        let mut drawers = opened(start);
        let later = start + DWELL + KEEP_ALIVE * 3;
        drawers.hover(0, Part::Button, false, later);
        drawers.tick(later + LINGER);
        assert_eq!(drawers.state(0), Drawer::Closed);
    }

    #[test]
    fn pressing_an_option_pins_the_drawer() {
        let start = Instant::now();
        let mut drawers = opened(start);
        drawers.option_pressed(0, start + DWELL);
        assert_eq!(drawers.state(0), Drawer::Pinned);
    }

    #[test]
    fn a_pinned_drawer_survives_the_pointer_leaving() {
        let start = Instant::now();
        let mut drawers = opened(start);
        drawers.option_pressed(0, start);
        drawers.hover(0, Part::Button, false, start);
        drawers.tick(start + LINGER * 2);
        assert_eq!(drawers.state(0), Drawer::Pinned);
    }

    #[test]
    fn a_press_outside_closes_a_pinned_drawer() {
        let start = Instant::now();
        let mut drawers = opened(start);
        drawers.option_pressed(0, start);
        drawers.pressed_outside(start);
        assert_eq!(drawers.state(0), Drawer::Closed);
    }

    #[test]
    fn opening_another_drawer_closes_the_first() {
        let start = Instant::now();
        let mut drawers = opened(start);
        drawers.hover(1, Part::Button, true, start);
        drawers.tick(start + DWELL * 2);
        assert_eq!(drawers.state(0), Drawer::Closed);
    }

    #[test]
    fn the_toggle_key_pins_the_first_drawer() {
        let mut drawers = Drawers::new([false, true, true]);
        drawers.toggle_next(Instant::now());
        assert_eq!(drawers.state(1), Drawer::Pinned);
    }

    #[test]
    fn the_toggle_key_moves_on_to_the_next_drawer() {
        let mut drawers = Drawers::new([true, true]);
        drawers.toggle_next(Instant::now());
        drawers.toggle_next(Instant::now());
        assert_eq!(drawers.state(1), Drawer::Pinned);
    }

    #[test]
    fn the_toggle_key_past_the_last_drawer_closes_it() {
        let mut drawers = Drawers::new([true, true]);
        for _ in 0..3 {
            drawers.toggle_next(Instant::now());
        }
        assert_eq!(drawers.state(1), Drawer::Closed);
    }

    #[test]
    fn the_toggle_key_with_no_drawers_does_nothing() {
        let mut drawers = Drawers::new([false]);
        drawers.toggle_next(Instant::now());
        assert_eq!(drawers.state(0), Drawer::Closed);
    }

    #[test]
    fn a_drawer_closing_under_the_pointer_opens_again() {
        let start = Instant::now();
        let mut drawers = opened(start);
        drawers.hover(0, Part::Drawer, true, start);
        drawers.slots[0].state = Drawer::Closing { since: start };
        drawers.tick(start + LINGER * 2);
        assert_eq!(drawers.state(0), Drawer::Open);
    }

    #[test]
    fn escape_closes_an_open_drawer() {
        let start = Instant::now();
        let mut drawers = opened(start);
        assert!(drawers.escape(start));
    }

    #[test]
    fn escape_with_no_drawer_open_does_nothing() {
        assert!(!Drawers::new([true, true]).escape(Instant::now()));
    }

    #[test]
    fn a_hover_on_a_feature_that_does_not_exist_is_ignored() {
        let mut drawers = Drawers::new([true]);
        drawers.hover(5, Part::Button, true, Instant::now());
        assert_eq!(drawers.state(5), Drawer::Closed);
    }

    #[test]
    fn nothing_waits_when_every_drawer_is_closed() {
        assert_eq!(Drawers::new([true, true]).next_tick(Instant::now()), None);
    }
}
