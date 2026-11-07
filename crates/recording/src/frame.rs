//! Where the red recording frame (and drawings kept on screen while recording) go: one
//! click-through surface per monitor, only as big as what it shows.

use super::plan::BORDER;
use screencapture_core::capture::{MonitorInfo, Rect};

/// The four strips of the frame around `area`, in desktop coordinates. They lie outside it,
/// so the frame is never recorded.
#[must_use]
pub const fn strips(area: Rect) -> [Rect; 4] {
    let (l, t, r, b) = (area.x - BORDER, area.y - BORDER, area.right(), area.bottom());
    let across = area.w + 2 * BORDER;
    [
        Rect::new(l, t, across, BORDER),
        Rect::new(l, b, across, BORDER),
        Rect::new(l, area.y, BORDER, area.h),
        Rect::new(r, area.y, BORDER, area.h),
    ]
}

/// The strips around `areas` that lie outside all of them (between two parts there is none).
fn outline(areas: &[Rect]) -> Vec<Rect> {
    let outside = |strip: &Rect| areas.iter().all(|area| strip.intersect(*area).is_none());
    areas
        .iter()
        .flat_map(|&area| strips(area))
        .filter(outside)
        .collect()
}

/// One monitor's part of the frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    pub monitor: MonitorInfo,
    /// The surface, in desktop coordinates.
    pub bounds: Rect,
    /// Frame strips, relative to `bounds`.
    pub strips: Vec<Rect>,
}

/// Surfaces for the frame around `areas` (a recording's parts, one per screen), plus whatever
/// `extra` (drawings' bounds) needs room on screen; monitors with nothing to show get none.
#[must_use]
pub fn surfaces(areas: &[Rect], extra: &[Rect], monitors: &[MonitorInfo]) -> Vec<Surface> {
    let frame = outline(areas);
    monitors
        .iter()
        .filter_map(|monitor| {
            let on = |r: &Rect| r.intersect(monitor.rect);
            let pieces: Vec<Rect> = frame.iter().filter_map(on).collect();
            let drawings = extra.iter().filter_map(on);
            let bounds = pieces.iter().copied().chain(drawings).reduce(Rect::union)?;
            let strips = pieces
                .into_iter()
                .map(|p| Rect::new(p.x - bounds.x, p.y - bounds.y, p.w, p.h))
                .collect();
            Some(Surface {
                monitor: monitor.clone(),
                bounds,
                strips,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, x: i32) -> MonitorInfo {
        MonitorInfo {
            name: name.into(),
            rect: Rect::new(x, 0, 2560, 1440),
            output_position: (x, 0),
            refresh_mhz: None,
        }
    }

    #[test]
    fn the_frame_hugs_the_area_from_outside() {
        let area = Rect::new(100, 200, 640, 480);
        let [top, bottom, left, right] = strips(area);
        assert_eq!(
            top,
            Rect::new(100 - BORDER, 200 - BORDER, 640 + 2 * BORDER, BORDER)
        );
        assert_eq!(bottom, Rect::new(100 - BORDER, 680, 640 + 2 * BORDER, BORDER));
        assert_eq!(left, Rect::new(100 - BORDER, 200, BORDER, 480));
        assert_eq!(right, Rect::new(740, 200, BORDER, 480));
        for strip in strips(area) {
            assert_eq!(strip.intersect(area), None, "never over the recording");
        }
    }

    #[test]
    fn one_small_surface_around_a_region() {
        let m = [monitor("DP-1", 0), monitor("DP-2", 2560)];
        let area = Rect::new(100, 200, 640, 480);
        let s = surfaces(&[area], &[], &m);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].monitor.name, "DP-1");
        assert_eq!(
            s[0].bounds,
            Rect::new(100 - BORDER, 200 - BORDER, 640 + 2 * BORDER, 480 + 2 * BORDER)
        );
        assert_eq!(
            s[0].strips[0],
            Rect::new(0, 0, 640 + 2 * BORDER, BORDER),
            "relative to the surface"
        );
    }

    #[test]
    fn a_frame_across_a_screen_edge_is_split_between_the_screens() {
        let m = [monitor("DP-1", 0), monitor("DP-2", 2560)];
        // A full DP-2 after the inset: its left strip lands on DP-1's right edge.
        let area = Rect::new(2560, BORDER, 2560 - BORDER, 1440 - 2 * BORDER);
        let s = surfaces(&[area], &[], &m);
        let names: Vec<&str> = s.iter().map(|s| s.monitor.name.as_str()).collect();
        assert_eq!(names, ["DP-1", "DP-2"]);
        assert_eq!(
            s[0].bounds,
            Rect::new(2560 - BORDER, 0, BORDER, 1440),
            "a sliver of DP-1"
        );
        assert_eq!(
            s[0].strips.len(),
            3,
            "the left strip and the ends of top and bottom"
        );
        assert_eq!(s[1].bounds, m[1].rect);
        for surface in &s {
            assert_eq!(
                surface.monitor.rect.intersect(surface.bounds),
                Some(surface.bounds)
            );
            for strip in &surface.strips {
                assert!(!strip.is_empty());
            }
        }
    }

    #[test]
    fn drawings_widen_the_surface() {
        let m = [monitor("DP-1", 0)];
        let area = Rect::new(100, 200, 640, 480);
        let s = surfaces(&[area], &[Rect::new(50, 900, 100, 100)], &m);
        assert_eq!(
            s[0].bounds,
            Rect::new(50, 200 - BORDER, 640 + BORDER + 50, 800 + BORDER)
        );
        // Strips stay where the area is.
        assert_eq!(s[0].strips[0].x, 100 - BORDER - 50);
    }

    #[test]
    fn no_strip_runs_between_two_parts() {
        let m = [monitor("DP-1", 0), monitor("DP-2", 2560)];
        let left = Rect::new(BORDER, BORDER, 2560 - BORDER, 1000);
        let right = Rect::new(2560, BORDER, 1000, 1000);
        let parts = [left, right];
        let all: Vec<Rect> = surfaces(&parts, &[], &m)
            .iter()
            .flat_map(|s| {
                s.strips
                    .iter()
                    .map(|r| Rect::new(r.x + s.bounds.x, r.y + s.bounds.y, r.w, r.h))
            })
            .collect();
        assert!(
            all.iter()
                .all(|strip| parts.iter().all(|part| strip.intersect(*part).is_none())),
            "{all:?}"
        );
    }

    #[test]
    fn nothing_on_a_screen_means_no_surface_there() {
        let m = [
            monitor("DP-1", 0),
            monitor("DP-2", 2560),
            monitor("HDMI-A-1", 5120),
        ];
        let s = surfaces(&[Rect::new(100, 200, 640, 480)], &[], &m);
        assert_eq!(s.len(), 1);
    }
}
