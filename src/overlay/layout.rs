//! Pure geometry behind the overlay windows.

use crate::{
    capture::{MonitorInfo, Rect},
    selection::Request,
};

/// Areas of a monitor to darken so the selection (global coords) stays clear.
/// Returned rects are local to the monitor.
pub fn dim_rects(monitor: Rect, selection: Option<Rect>) -> Vec<Rect> {
    let local = Rect::new(0, 0, monitor.w, monitor.h);
    let Some(hole) = selection.and_then(|s| s.intersect(monitor)) else {
        return vec![local];
    };
    let hole = Rect::new(hole.x - monitor.x, hole.y - monitor.y, hole.w, hole.h);
    [
        Rect::new(0, 0, local.w, hole.y),
        Rect::new(0, hole.bottom(), local.w, local.h - hole.bottom()),
        Rect::new(0, hole.y, hole.x, hole.h),
        Rect::new(hole.right(), hole.y, local.w - hole.right(), hole.h),
    ]
    .into_iter()
    .filter(|r| !r.is_empty())
    .collect()
}

/// Which monitor a gpui display shows. gpui reports Wayland displays at their
/// `wl_output.geometry` position (possibly divided by an integer scale), so try that first,
/// then logical position; nearest wins.
pub fn match_display(origin: (i32, i32), monitors: &[MonitorInfo]) -> Option<usize> {
    let distance = |(x, y): (i32, i32)| (i64::from(x - origin.0)).abs() + (i64::from(y - origin.1)).abs();
    let exact = |m: &MonitorInfo| {
        (1..=3).any(|scale| (m.output_position.0 / scale, m.output_position.1 / scale) == origin)
    };
    monitors
        .iter()
        .position(exact)
        .or_else(|| monitors.iter().position(|m| (m.rect.x, m.rect.y) == origin))
        .or_else(|| (0..monitors.len()).min_by_key(|&i| distance(monitors[i].output_position)))
}

/// Whether capturing `request` needs this monitor; skipping the others saves copy time.
pub fn needs(request: &Request, monitor: &MonitorInfo) -> bool {
    match request {
        Request::Region(rect) => rect.intersect(monitor.rect).is_some(),
        Request::Screens(names) => {
            names.is_empty() || names.iter().any(|n| n.eq_ignore_ascii_case(&monitor.name))
        }
    }
}

/// Height of the band the toolbar occupies at the top or bottom of a monitor.
pub const TOOLBAR_BAND: i32 = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
}

/// Where the toolbar goes on `monitor` so it doesn't hide the region being adjusted: the top,
/// unless the region reaches into the top band and leaves the bottom one free.
pub fn toolbar_edge(monitor: Rect, region: Option<Rect>) -> Edge {
    let band = |y| Rect::new(monitor.x, y, monitor.w, TOOLBAR_BAND);
    let hits = |b: Rect| region.and_then(|r| r.intersect(b)).is_some();
    if hits(band(monitor.y)) && !hits(band(monitor.bottom() - TOOLBAR_BAND)) {
        Edge::Bottom
    } else {
        Edge::Top
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(name: &str, x: i32, output_x: i32) -> MonitorInfo {
        MonitorInfo {
            name: name.into(),
            rect: Rect::new(x, 0, 100, 50),
            output_position: (output_x, 0),
        }
    }

    #[test]
    fn no_selection_dims_whole_monitor() {
        assert_eq!(
            dim_rects(Rect::new(100, 0, 100, 50), None),
            vec![Rect::new(0, 0, 100, 50)]
        );
    }

    #[test]
    fn selection_elsewhere_dims_whole_monitor() {
        let sel = Some(Rect::new(0, 0, 10, 10));
        assert_eq!(
            dim_rects(Rect::new(100, 0, 100, 50), sel),
            vec![Rect::new(0, 0, 100, 50)]
        );
    }

    #[test]
    fn selection_inside_leaves_a_hole() {
        let rects = dim_rects(Rect::new(100, 0, 100, 50), Some(Rect::new(120, 10, 30, 20)));
        assert_eq!(
            rects,
            vec![
                Rect::new(0, 0, 100, 10),  // above
                Rect::new(0, 30, 100, 20), // below
                Rect::new(0, 10, 20, 20),  // left
                Rect::new(50, 10, 50, 20), // right
            ]
        );
        let dimmed: i32 = rects.iter().map(|r| r.w * r.h).sum();
        assert_eq!(dimmed, 100 * 50 - 30 * 20);
    }

    #[test]
    fn selection_crossing_edge_is_clipped() {
        let rects = dim_rects(Rect::new(100, 0, 100, 50), Some(Rect::new(50, 0, 100, 50)));
        assert_eq!(rects, vec![Rect::new(50, 0, 50, 50)]);
    }

    #[test]
    fn display_matches_output_position_first() {
        // Logical and output positions disagree: output position wins.
        let monitors = [mon("A", 0, 200), mon("B", 200, 0)];
        assert_eq!(match_display((0, 0), &monitors), Some(1));
        assert_eq!(match_display((200, 0), &monitors), Some(0));
    }

    #[test]
    fn display_falls_back_to_nearest() {
        let monitors = [mon("A", 0, 0), mon("B", 100, 100)];
        assert_eq!(match_display((98, 0), &monitors), Some(1));
        assert_eq!(match_display((0, 0), &[]), None);
    }

    #[test]
    fn region_needs_only_touched_monitors() {
        let req = Request::Region(Rect::new(90, 0, 20, 20));
        assert!(needs(&req, &mon("A", 0, 0)));
        assert!(needs(&req, &mon("B", 100, 0)));
        assert!(!needs(&req, &mon("C", 200, 0)));
    }

    #[test]
    fn screens_need_named_or_all_when_empty() {
        let req = Request::Screens(vec!["B".into()]);
        assert!(!needs(&req, &mon("A", 0, 0)));
        assert!(needs(&req, &mon("b", 100, 0)));
        assert!(needs(&Request::Screens(vec![]), &mon("A", 0, 0)));
    }

    #[test]
    fn toolbar_stays_on_top_unless_region_is_there() {
        let m = Rect::new(100, 0, 1000, 600);
        assert_eq!(toolbar_edge(m, None), Edge::Top);
        assert_eq!(toolbar_edge(m, Some(Rect::new(300, 200, 50, 50))), Edge::Top);
        assert_eq!(toolbar_edge(m, Some(Rect::new(300, 40, 50, 50))), Edge::Bottom);
        // On another monitor: irrelevant here.
        assert_eq!(toolbar_edge(m, Some(Rect::new(0, 0, 50, 50))), Edge::Top);
        // Covers both bands: top, over the region.
        assert_eq!(toolbar_edge(m, Some(Rect::new(300, 0, 50, 600))), Edge::Top);
    }
}
