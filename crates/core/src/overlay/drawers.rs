//! The drawers of every feature's action button: which is open, where the pointer is, and the
//! rule that only one is open at a time.

use std::time::{Duration, Instant};

use super::drawer::{Drawer, Event};

/// Which part of a feature's button the pointer is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Button,
    Drawer,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    state: Drawer,
    over_button: bool,
    over_drawer: bool,
}

impl Slot {
    const fn new() -> Self {
        Self {
            state: Drawer::Closed,
            over_button: false,
            over_drawer: false,
        }
    }

    const fn over(&self) -> bool {
        self.over_button || self.over_drawer
    }
}

/// One drawer per feature, by feature index.
#[derive(Debug)]
pub struct Drawers {
    slots: Vec<Slot>,
}

impl Drawers {
    #[must_use]
    pub fn new(features: usize) -> Self {
        Self {
            slots: vec![Slot::new(); features],
        }
    }

    /// Where drawer `index` stands.
    #[must_use]
    pub fn state(&self, index: usize) -> Drawer {
        self.slots.get(index).map_or(Drawer::Closed, |slot| slot.state)
    }

    /// The pointer entered or left `part` of button `index`.
    pub fn hover(&mut self, index: usize, part: Part, hovered: bool, now: Instant) {
        let Some(slot) = self.slots.get_mut(index) else {
            return;
        };
        let was_over = slot.over();
        match part {
            Part::Button => slot.over_button = hovered,
            Part::Drawer => slot.over_drawer = hovered,
        }
        match (was_over, slot.over()) {
            (false, true) => self.send(index, Event::PointerEntered, now),
            (true, false) => self.send(index, Event::PointerLeft, now),
            _ => {}
        }
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
            self.send(index, Event::Tick, now);
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
        slot.state = slot.state.after(event, now);
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
        let mut drawers = Drawers::new(2);
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
        drawers.tick(start + DWELL + LINGER);
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
    fn escape_closes_an_open_drawer() {
        let start = Instant::now();
        let mut drawers = opened(start);
        assert!(drawers.escape(start));
    }

    #[test]
    fn escape_with_no_drawer_open_does_nothing() {
        assert!(!Drawers::new(2).escape(Instant::now()));
    }

    #[test]
    fn a_hover_on_a_feature_that_does_not_exist_is_ignored() {
        let mut drawers = Drawers::new(1);
        drawers.hover(5, Part::Button, true, Instant::now());
        assert_eq!(drawers.state(5), Drawer::Closed);
    }

    #[test]
    fn nothing_waits_when_every_drawer_is_closed() {
        assert_eq!(Drawers::new(2).next_tick(Instant::now()), None);
    }
}
