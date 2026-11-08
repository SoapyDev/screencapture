//! What the user is selecting on the overlay, independent of the UI toolkit.
//! All points are global logical desktop coordinates.

use crate::capture::{MonitorInfo, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Drag a rectangle, adjust it, Enter to capture; a plain click captures the screen under it.
    Region,
    /// Click screens to toggle them, then confirm to stitch them together.
    Screens,
}

/// What to capture once the user is done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Region(Rect),
    /// Monitor names, in desktop layout order.
    Screens(Vec<String>),
}

/// Edge or corner of the region that can be dragged to resize it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

/// What lies under a point, relative to the current region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Handle(Handle),
    Inside,
    Outside,
}

/// Drags smaller than this (logical px) on either axis count as a click.
pub const CLICK_SLOP: i32 = 4;
/// How close to an edge (logical px, either side) a press grabs it.
pub const HANDLE_SLOP: i32 = 12;

#[derive(Clone, Copy, Debug)]
enum Drag {
    /// Drawing a new region from `start`; a click instead clears the old one (or picks a screen).
    New {
        start: (i32, i32),
        end: (i32, i32),
        had_region: bool,
    },
    /// Moving the region; `grab` is the pointer's offset from its top-left corner.
    Move { grab: (i32, i32), current: Rect },
    Resize {
        handle: Handle,
        press: (i32, i32),
        origin: Rect,
        current: Rect,
    },
}

pub struct Selection {
    pub mode: Mode,
    monitors: Vec<MonitorInfo>,
    /// The committed, adjustable region.
    region: Option<Rect>,
    drag: Option<Drag>,
    picked: Vec<String>,
    pointer: Option<(i32, i32)>,
}

impl Request {
    /// Whether capturing this needs `monitor`; skipping the others saves copy time.
    #[must_use]
    pub fn needs(&self, monitor: &MonitorInfo) -> bool {
        match self {
            Self::Region(rect) => rect.intersect(monitor.rect).is_some(),
            Self::Screens(names) => {
                names.is_empty() || names.iter().any(|n| n.eq_ignore_ascii_case(&monitor.name))
            }
        }
    }
}

impl Selection {
    #[must_use]
    pub fn new(mode: Mode, mut monitors: Vec<MonitorInfo>) -> Self {
        monitors.sort_by_key(|m| (m.rect.x, m.rect.y));
        Self {
            mode,
            monitors,
            region: None,
            drag: None,
            picked: Vec::new(),
            pointer: None,
        }
    }

    #[must_use]
    pub fn monitors(&self) -> &[MonitorInfo] {
        &self.monitors
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.region = None;
        self.drag = None;
        self.picked.clear();
    }

    pub fn press(&mut self, x: i32, y: i32) {
        self.pointer = Some((x, y));
        match self.mode {
            Mode::Region => self.drag = Some(self.drag_from(x, y)),
            Mode::Screens => self.toggle_screen_at(x, y),
        }
    }

    /// What a press at (`x`, `y`) starts: resizing, moving, or a new region.
    fn drag_from(&self, x: i32, y: i32) -> Drag {
        match (self.hit(x, y), self.region) {
            (Hit::Handle(handle), Some(r)) => Drag::Resize {
                handle,
                press: (x, y),
                origin: r,
                current: r,
            },
            (Hit::Inside, Some(r)) => Drag::Move {
                grab: (x - r.x, y - r.y),
                current: r,
            },
            _ => Drag::New {
                start: (x, y),
                end: (x, y),
                had_region: self.region.is_some(),
            },
        }
    }

    /// Pick the screen at (`x`, `y`), or drop it if picked.
    fn toggle_screen_at(&mut self, x: i32, y: i32) {
        let Some(name) = self.monitor_at(x, y).map(|m| m.name.clone()) else {
            return;
        };
        match self.picked.iter().position(|n| *n == name) {
            Some(i) => drop(self.picked.remove(i)),
            None => self.picked.push(name),
        }
    }
    pub fn motion(&mut self, x: i32, y: i32) {
        self.pointer = Some((x, y));
        let Some(drag) = self.drag else { return };
        self.drag = Some(self.dragged(drag, x, y));
    }

    /// `drag` with the pointer at (`x`, `y`).
    fn dragged(&self, drag: Drag, x: i32, y: i32) -> Drag {
        match drag {
            Drag::New {
                start, had_region, ..
            } => Drag::New {
                start,
                end: (x, y),
                had_region,
            },
            Drag::Move { grab, current } => Drag::Move {
                grab,
                current: self.moved(Rect::new(x - grab.0, y - grab.1, current.w, current.h), current),
            },
            Drag::Resize {
                handle,
                press,
                origin,
                ..
            } => Drag::Resize {
                handle,
                press,
                origin,
                current: self.resized(origin, handle, (x - press.0, y - press.1)),
            },
        }
    }
    /// Ends a drag. Nothing is captured until [`Self::confirm`]: a plain click on a screen
    /// (with no region yet) selects that whole screen as the region.
    pub fn release(&mut self, x: i32, y: i32) {
        self.motion(x, y);
        let Some(drag) = self.drag.take() else { return };
        self.region = match drag {
            Drag::New {
                start,
                end,
                had_region,
            } => self.drawn(start, end, had_region),
            Drag::Move { current, .. } | Drag::Resize { current, .. } => Some(current),
        };
    }

    /// The region a drag from `start` to `end` leaves. A click clears the region; with none
    /// (`had_region` false), it picks the screen under it.
    fn drawn(&self, start: (i32, i32), end: (i32, i32), had_region: bool) -> Option<Rect> {
        let click = (end.0 - start.0).abs() < CLICK_SLOP && (end.1 - start.1).abs() < CLICK_SLOP;
        match (click, had_region) {
            (false, _) => self.clamp(Rect::from_corners(start, end)),
            (true, true) => None,
            (true, false) => self.monitor_at(start.0, start.1).map(|m| m.rect),
        }
    }
    /// Every screen picked (what "All screens" does).
    #[must_use]
    pub fn is_all(&self) -> bool {
        self.mode == Mode::Screens && self.monitors.iter().all(|m| self.is_picked(&m.name))
    }

    /// Pick every screen (Screens mode).
    pub fn select_all(&mut self) {
        self.set_mode(Mode::Screens);
        self.picked = self.monitors.iter().map(|m| m.name.clone()).collect();
    }

    /// Enter / "Capture" button.
    pub fn confirm(&self) -> Option<Request> {
        match self.mode {
            Mode::Region => self.region().map(Request::Region),
            Mode::Screens => {
                let names: Vec<String> = self
                    .monitors
                    .iter()
                    .filter(|m| self.is_picked(&m.name))
                    .map(|m| m.name.clone())
                    .collect();
                (!names.is_empty()).then_some(Request::Screens(names))
            }
        }
    }

    /// The region as currently shown: live while dragging, else the committed one.
    #[must_use]
    pub fn region(&self) -> Option<Rect> {
        match self.drag {
            Some(Drag::New { start, end, .. }) => self.clamp(Rect::from_corners(start, end)),
            Some(Drag::Move { current, .. } | Drag::Resize { current, .. }) => Some(current),
            None => self.region,
        }
    }

    #[must_use]
    pub const fn pointer(&self) -> Option<(i32, i32)> {
        self.pointer
    }

    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// What a press at this point would grab.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Hit {
        let Some(r) = self.region else { return Hit::Outside };
        let in_span = |v: i32, lo: i32, hi: i32| v >= lo - HANDLE_SLOP && v <= hi + HANDLE_SLOP;
        if !(in_span(x, r.x, r.right()) && in_span(y, r.y, r.bottom())) {
            return Hit::Outside;
        }
        let sides = (Side::of(x, r.x, r.right()), Side::of(y, r.y, r.bottom()));
        match handle_at(sides) {
            Some(handle) => Hit::Handle(handle),
            None if r.contains(x, y) => Hit::Inside,
            None => Hit::Outside,
        }
    }
    /// Move the region by whole pixels (arrow keys), staying on the desktop.
    pub fn nudge(&mut self, dx: i32, dy: i32) {
        if let Some(r) = self.region {
            self.region = Some(self.moved(Rect::new(r.x + dx, r.y + dy, r.w, r.h), r));
        }
    }

    /// Grow or shrink the region from its bottom-right corner (Shift+arrows).
    pub fn resize_by(&mut self, dw: i32, dh: i32) {
        if let Some(r) = self.region {
            let grown = Rect::new(r.x, r.y, (r.w + dw).max(1), (r.h + dh).max(1));
            self.region = Some(self.clamp(grown).unwrap_or(r));
        }
    }

    #[must_use]
    pub fn is_picked(&self, name: &str) -> bool {
        self.picked.iter().any(|n| n == name)
    }

    /// Monitor under the pointer, for hover highlight and toolbar placement.
    #[must_use]
    pub fn hovered(&self) -> Option<&MonitorInfo> {
        let (x, y) = self.pointer?;
        self.monitor_at(x, y)
    }

    fn monitor_at(&self, x: i32, y: i32) -> Option<&MonitorInfo> {
        self.monitors.iter().find(|m| m.rect.contains(x, y))
    }

    fn desktop(&self) -> Option<Rect> {
        self.monitors.iter().map(|m| m.rect).reduce(Rect::union)
    }

    /// The part of `r` on the desktop's bounding box.
    fn clamp(&self, r: Rect) -> Option<Rect> {
        r.intersect(self.desktop()?).filter(|&r| self.on_a_monitor(r))
    }

    /// The desktop's bounding box has holes where monitors of different sizes meet; a region
    /// entirely in one would capture nothing.
    fn on_a_monitor(&self, r: Rect) -> bool {
        self.monitors.iter().any(|m| m.rect.intersect(r).is_some())
    }

    /// `r` slid onto the desktop, unless that leaves it on no monitor: then `current`.
    fn moved(&self, r: Rect, current: Rect) -> Rect {
        Some(self.keep_on_desktop(r))
            .filter(|&r| self.on_a_monitor(r))
            .unwrap_or(current)
    }

    /// Slide `r` (without resizing) so it lies within the desktop's bounding box.
    fn keep_on_desktop(&self, r: Rect) -> Rect {
        let Some(d) = self.desktop() else { return r };
        let x = r.x.min(d.right() - r.w).max(d.x);
        let y = r.y.min(d.bottom() - r.h).max(d.y);
        Rect::new(x, y, r.w, r.h)
    }

    /// `origin` with the grabbed edges moved by `delta`; flips past the opposite edge.
    fn resized(&self, origin: Rect, handle: Handle, (dx, dy): (i32, i32)) -> Rect {
        let (mut left, mut top, mut right, mut bottom) =
            (origin.x, origin.y, origin.right(), origin.bottom());
        match handle {
            Handle::N | Handle::NE | Handle::NW => top += dy,
            Handle::S | Handle::SE | Handle::SW => bottom += dy,
            Handle::E | Handle::W => {}
        }
        match handle {
            Handle::W | Handle::NW | Handle::SW => left += dx,
            Handle::E | Handle::NE | Handle::SE => right += dx,
            Handle::N | Handle::S => {}
        }
        let mut r = Rect::from_corners((left, top), (right, bottom));
        r.w = r.w.max(1);
        r.h = r.h.max(1);
        self.clamp(r).unwrap_or(origin)
    }
}

/// Where a coordinate is along one side of the region: near its start or end edge, or between.
#[derive(Clone, Copy)]
enum Side {
    Start,
    Between,
    End,
}

impl Side {
    /// Where `v` is between edges `lo` and `hi`; the closer edge wins when they are less than
    /// two slops apart.
    const fn of(v: i32, lo: i32, hi: i32) -> Self {
        let (to_lo, to_hi) = ((v - lo).abs(), (v - hi).abs());
        if to_lo <= Self::reach(v < lo, hi - lo) && to_lo <= to_hi {
            Self::Start
        } else if to_hi <= Self::reach(v > hi, hi - lo) {
            Self::End
        } else {
            Self::Between
        }
    }

    /// How far from an edge a press grabs it: the whole slop outside the region; inside, never
    /// more than a third of the `span`, so a small region keeps a middle to grab.
    const fn reach(outside: bool, span: i32) -> i32 {
        if outside || span / 3 >= HANDLE_SLOP {
            HANDLE_SLOP
        } else {
            span / 3
        }
    }
}

/// The handle at these sides (across, down), if any.
const fn handle_at(sides: (Side, Side)) -> Option<Handle> {
    match sides {
        (Side::Start, Side::Start) => Some(Handle::NW),
        (Side::End, Side::Start) => Some(Handle::NE),
        (Side::Start, Side::End) => Some(Handle::SW),
        (Side::End, Side::End) => Some(Handle::SE),
        (Side::Start, Side::Between) => Some(Handle::W),
        (Side::End, Side::Between) => Some(Handle::E),
        (Side::Between, Side::Start) => Some(Handle::N),
        (Side::Between, Side::End) => Some(Handle::S),
        (Side::Between, Side::Between) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::PixelSize;

    fn monitors() -> Vec<MonitorInfo> {
        ["DP-1", "HDMI-A-1", "DP-2"]
            .iter()
            .zip((0..).step_by(100))
            .map(|(name, left)| MonitorInfo {
                name: (*name).into(),
                rect: Rect::new(left, 0, 100, 50),
                output_position: (left, 0),
                refresh_mhz: None,
                pixels: PixelSize::new(100, 50),
            })
            .collect()
    }

    /// Region mode with a committed region at (20,10) 40x20.
    fn with_region() -> Selection {
        let mut s = Selection::new(Mode::Region, monitors());
        s.press(20, 10);
        s.motion(40, 20);
        s.release(60, 30);
        assert_eq!(s.region(), Some(Rect::new(20, 10, 40, 20)));
        s
    }

    fn drag(s: &mut Selection, from: (i32, i32), to: (i32, i32)) {
        s.press(from.0, from.1);
        s.motion(to.0, to.1);
        s.release(to.0, to.1);
    }

    #[test]
    fn drag_draws_region_in_any_direction_and_waits_for_enter() {
        let mut s = Selection::new(Mode::Region, monitors());
        assert_eq!(s.confirm(), None);
        s.press(150, 40);
        s.motion(120, 10);
        assert!(s.is_dragging());
        assert_eq!(s.region(), Some(Rect::new(120, 10, 30, 30)));
        s.release(90, 5);
        assert_eq!(s.region(), Some(Rect::new(90, 5, 60, 35)));
        assert_eq!(s.confirm(), Some(Request::Region(Rect::new(90, 5, 60, 35))));
    }

    #[test]
    fn region_is_clamped_to_desktop() {
        let mut s = Selection::new(Mode::Region, monitors());
        drag(&mut s, (-20, -20), (30, 30));
        assert_eq!(s.region(), Some(Rect::new(0, 0, 30, 30)));
    }

    #[test]
    fn click_without_region_selects_that_screen_for_editing() {
        let mut s = Selection::new(Mode::Region, monitors());
        s.press(150, 20);
        s.release(151, 21);
        assert_eq!(s.region(), Some(Rect::new(100, 0, 100, 50)));
        assert_eq!(s.confirm(), Some(Request::Region(Rect::new(100, 0, 100, 50))));
    }

    #[test]
    fn click_outside_region_clears_it() {
        let mut s = with_region();
        s.press(90, 45);
        s.release(90, 45);
        assert_eq!(s.region(), None);
        assert_eq!(s.confirm(), None);
    }

    #[test]
    fn click_outside_every_screen_does_nothing() {
        let mut s = Selection::new(Mode::Region, vec![monitors().remove(0)]);
        s.press(500, 500);
        s.release(500, 500);
        assert_eq!(s.region(), None);
    }

    #[test]
    fn hit_finds_handles_edges_inside_and_outside() {
        let s = with_region(); // x 20..60, y 10..30
        assert_eq!(s.hit(20, 10), Hit::Handle(Handle::NW));
        assert_eq!(s.hit(61, 31), Hit::Handle(Handle::SE));
        assert_eq!(s.hit(58, 12), Hit::Handle(Handle::NE));
        assert_eq!(s.hit(40, 10), Hit::Handle(Handle::N));
        assert_eq!(s.hit(14, 20), Hit::Handle(Handle::W));
        assert_eq!(s.hit(40, 20), Hit::Inside);
        assert_eq!(s.hit(90, 45), Hit::Outside);
        assert_eq!(Selection::new(Mode::Region, monitors()).hit(40, 20), Hit::Outside);
    }

    #[test]
    fn a_big_region_has_handles_a_finger_wide() {
        let mut s = Selection::new(Mode::Region, monitors());
        s.press(10, 5);
        s.release(190, 45);
        // 12 px outside the left edge: still the west handle (a 24 px target).
        assert_eq!(s.hit(-2, 25), Hit::Handle(Handle::W));
    }

    #[test]
    fn a_small_region_keeps_its_handles_full_size_outside() {
        let mut s = Selection::new(Mode::Region, monitors());
        s.press(10, 10);
        s.release(19, 40);
        // 6 px outside a 9 px wide region: nothing there to keep a middle for.
        assert_eq!(s.hit(4, 25), Hit::Handle(Handle::W));
    }

    #[test]
    fn dragging_corner_resizes() {
        let mut s = with_region();
        drag(&mut s, (60, 30), (80, 45));
        assert_eq!(s.region(), Some(Rect::new(20, 10, 60, 35)));
    }

    #[test]
    fn dragging_edge_resizes_one_axis() {
        let mut s = with_region();
        drag(&mut s, (20, 20), (5, 40));
        assert_eq!(s.region(), Some(Rect::new(5, 10, 55, 20)));
    }

    #[test]
    fn resizing_past_opposite_edge_flips() {
        let mut s = with_region();
        drag(&mut s, (20, 20), (70, 20)); // west edge dragged beyond east edge at 60
        assert_eq!(s.region(), Some(Rect::new(60, 10, 10, 20)));
    }

    #[test]
    fn region_is_live_while_resizing() {
        let mut s = with_region();
        s.press(60, 30);
        s.motion(70, 40);
        assert_eq!(s.region(), Some(Rect::new(20, 10, 50, 30)));
    }

    #[test]
    fn dragging_inside_moves_and_stays_on_desktop() {
        let mut s = with_region();
        drag(&mut s, (40, 20), (50, 25));
        assert_eq!(s.region(), Some(Rect::new(30, 15, 40, 20)));
        drag(&mut s, (50, 25), (-100, -100));
        assert_eq!(s.region(), Some(Rect::new(0, 0, 40, 20)));
        drag(&mut s, (20, 10), (1000, 1000)); // grabbed at its centre
        assert_eq!(s.region(), Some(Rect::new(260, 30, 40, 20)));
    }

    #[test]
    fn pressing_outside_region_draws_a_new_one() {
        let mut s = with_region();
        drag(&mut s, (150, 5), (190, 45));
        assert_eq!(s.region(), Some(Rect::new(150, 5, 40, 40)));
    }

    #[test]
    fn keyboard_nudges_and_resizes() {
        let mut s = with_region();
        s.nudge(1, -1);
        assert_eq!(s.region(), Some(Rect::new(21, 9, 40, 20)));
        s.resize_by(5, -3);
        assert_eq!(s.region(), Some(Rect::new(21, 9, 45, 17)));
        s.resize_by(-100, 0); // never below one pixel
        assert_eq!(s.region(), Some(Rect::new(21, 9, 1, 17)));
        s.nudge(-1000, 0);
        assert_eq!(s.region(), Some(Rect::new(0, 9, 1, 17)));
    }

    #[test]
    fn screens_mode_toggles_and_confirms_in_layout_order() {
        let mut s = Selection::new(Mode::Screens, monitors());
        assert_eq!(s.confirm(), None);
        for x in [250, 50, 150] {
            drag(&mut s, (x, 10), (x, 10));
        }
        drag(&mut s, (150, 10), (150, 10)); // untoggle HDMI-A-1
        assert!(s.is_picked("DP-1") && !s.is_picked("HDMI-A-1"));
        assert_eq!(
            s.confirm(),
            Some(Request::Screens(vec!["DP-1".into(), "DP-2".into()]))
        );
    }

    #[test]
    fn switching_mode_clears_selection() {
        let mut s = with_region();
        s.set_mode(Mode::Screens);
        drag(&mut s, (50, 10), (50, 10));
        s.set_mode(Mode::Region);
        assert_eq!(s.confirm(), None);
        s.set_mode(Mode::Screens);
        assert_eq!(s.confirm(), None);
    }

    #[test]
    fn select_all_picks_every_screen_and_waits_for_enter() {
        let mut s = with_region();
        assert!(!s.is_all());
        s.select_all();
        assert!(s.is_all());
        assert_eq!(s.mode, Mode::Screens);
        assert_eq!(
            s.confirm(),
            Some(Request::Screens(vec![
                "DP-1".into(),
                "HDMI-A-1".into(),
                "DP-2".into()
            ]))
        );
    }

    #[test]
    fn hover_tracks_pointer() {
        let mut s = Selection::new(Mode::Region, monitors());
        assert!(s.hovered().is_none());
        s.motion(260, 10);
        assert_eq!(s.hovered().unwrap().name, "DP-2");
    }

    #[test]
    fn unpicking_one_screen_is_no_longer_all() {
        let mut s = Selection::new(Mode::Screens, monitors());
        s.select_all();
        drag(&mut s, (50, 10), (50, 10));
        assert!(!s.is_all());
        drag(&mut s, (50, 10), (50, 10));
        assert!(s.is_all());
        s.set_mode(Mode::Region);
        assert!(!s.is_all());
    }

    /// 100x50 next to a shorter 100x30: the band x 100..200, y 30..50 is on no monitor.
    fn uneven() -> Selection {
        let mon = |name: &str, rect| MonitorInfo {
            name: name.into(),
            rect,
            output_position: (rect.x, rect.y),
            refresh_mhz: None,
            pixels: PixelSize::of(rect),
        };
        Selection::new(
            Mode::Region,
            vec![
                mon("TALL", Rect::new(0, 0, 100, 50)),
                mon("SHORT", Rect::new(100, 0, 100, 30)),
            ],
        )
    }

    #[test]
    fn a_region_drawn_where_no_monitor_is_does_not_exist() {
        let mut s = uneven();
        drag(&mut s, (150, 35), (190, 48));
        assert_eq!(s.region(), None);
        assert_eq!(s.confirm(), None);
    }

    #[test]
    fn a_region_cannot_be_moved_or_resized_off_every_monitor() {
        let mut s = uneven();
        drag(&mut s, (150, 2), (170, 20)); // on SHORT, 20x18
        drag(&mut s, (160, 11), (160, 41)); // would land at y 32..50: nowhere
        assert_eq!(s.region(), Some(Rect::new(150, 2, 20, 18)), "move refused");
        s.nudge(0, 29); // y 31..49
        assert_eq!(s.region(), Some(Rect::new(150, 2, 20, 18)), "nudge refused");
        s.nudge(0, 11); // y 13..31 still touches SHORT
        assert_eq!(s.region(), Some(Rect::new(150, 13, 20, 18)));
        drag(&mut s, (150, 13), (180, 40)); // NW corner past SE: (170,31) 10x9, nowhere
        assert_eq!(s.region(), Some(Rect::new(150, 13, 20, 18)), "resize refused");
        // Partly over the gap is fine: still on a monitor.
        drag(&mut s, (170, 31), (170, 45));
        assert_eq!(s.region(), Some(Rect::new(150, 13, 20, 32)));
    }

    fn mon(name: &str, x: i32) -> MonitorInfo {
        MonitorInfo {
            name: name.into(),
            rect: Rect::new(x, 0, 100, 50),
            output_position: (x, 0),
            refresh_mhz: None,
            pixels: PixelSize::new(100, 50),
        }
    }

    #[test]
    fn region_needs_only_touched_monitors() {
        let req = Request::Region(Rect::new(90, 0, 20, 20));
        assert!(req.needs(&mon("A", 0)));
        assert!(req.needs(&mon("B", 100)));
        assert!(!req.needs(&mon("C", 200)));
    }

    #[test]
    fn screens_need_named_or_all_when_empty() {
        let req = Request::Screens(vec!["B".into()]);
        assert!(!req.needs(&mon("A", 0)));
        assert!(req.needs(&mon("b", 100)));
        assert!((Request::Screens(vec![])).needs(&mon("A", 0)));
    }
}
