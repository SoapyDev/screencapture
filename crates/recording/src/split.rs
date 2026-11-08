//! A recording across several screens is one recording per screen, joined at stop. This is
//! where each screen's part is, and where it lands in the joined video.

use anyhow::Result;

use super::plan;
use screencapture_core::capture::{MonitorInfo, Rect};

/// One screen's share of a recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Part {
    /// What is recorded, in desktop coordinates (logical px).
    pub area: Rect,
    /// Its top-left in the joined video (logical px).
    pub slot: (i32, i32),
}

/// The parts of `area` on `screens`, with room for the frame where no screen is beyond, even
/// sizes, and slots that close the gaps between screens (like stitched screenshots). Also the
/// joined video's size.
///
/// # Errors
///
/// When a part is too small to record once the frame has room.
pub fn split(
    area: Rect,
    screens: &[&MonitorInfo],
    monitors: &[MonitorInfo],
) -> Result<(Vec<Part>, (i32, i32))> {
    let pieces: Vec<Rect> = screens
        .iter()
        .filter_map(|screen| screen.rect.intersect(area))
        .map(|piece| plan::room_for_border(piece, monitors))
        .collect();
    let areas = pieces
        .iter()
        .map(|&piece| plan::ensure_recordable(even_out(piece, &pieces)))
        .collect::<Result<Vec<Rect>>>()?;
    let xs = Gaps::closed(areas.iter().map(|a| (a.x, a.right())));
    let ys = Gaps::closed(areas.iter().map(|a| (a.y, a.bottom())));
    let parts = areas
        .iter()
        .map(|&area| Part {
            area,
            slot: (xs.place(area.x), ys.place(area.y)),
        })
        .collect();
    Ok((parts, (xs.extent(), ys.extent())))
}

/// `piece` cut to even sizes on the side that has no neighbour among `pieces`, so neighbouring
/// parts still meet without a seam.
fn even_out(piece: Rect, pieces: &[Rect]) -> Rect {
    let neighbour_right = pieces
        .iter()
        .any(|p| p.x == piece.right() && p.y < piece.bottom() && piece.y < p.bottom());
    let neighbour_below = pieces
        .iter()
        .any(|p| p.y == piece.bottom() && p.x < piece.right() && piece.x < p.right());
    let (odd_w, odd_h) = (piece.w % 2, piece.h % 2);
    let x = if neighbour_right { piece.x + odd_w } else { piece.x };
    let y = if neighbour_below { piece.y + odd_h } else { piece.y };
    Rect::new(x, y, piece.w - odd_w, piece.h - odd_h)
}

/// One axis of the joined video: the covered spans, in order, with the gaps between them
/// taken out.
struct Gaps {
    /// Merged spans as (start, end, where the span starts in the joined video).
    spans: Vec<(i32, i32, i32)>,
}

impl Gaps {
    fn closed(spans: impl Iterator<Item = (i32, i32)>) -> Self {
        let mut sorted: Vec<(i32, i32)> = spans.collect();
        sorted.sort_unstable();
        let mut merged: Vec<(i32, i32)> = Vec::with_capacity(sorted.len());
        for (start, end) in sorted {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        let mut placed = 0;
        let spans = merged
            .into_iter()
            .map(|(start, end)| {
                let span = (start, end, placed);
                placed += end - start;
                span
            })
            .collect();
        Self { spans }
    }

    /// Where desktop coordinate `at` (the start of a part) lands.
    fn place(&self, at: i32) -> i32 {
        self.spans
            .iter()
            .find(|&&(start, end, _)| (start..end).contains(&at))
            .map_or(0, |&(start, _, placed)| placed + at - start)
    }

    fn extent(&self) -> i32 {
        self.spans
            .last()
            .map_or(0, |&(start, end, placed)| placed + end - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::BORDER;

    fn monitor(name: &str, x: i32) -> MonitorInfo {
        MonitorInfo {
            name: name.into(),
            rect: Rect::new(x, 0, 2560, 1440),
            output_position: (x, 0),
            refresh_mhz: None,
        }
    }

    fn three() -> Vec<MonitorInfo> {
        vec![
            monitor("DP-1", 0),
            monitor("HDMI-A-1", 2560),
            monitor("DP-2", 5120),
        ]
    }

    fn all_of(monitors: &[MonitorInfo]) -> Vec<&MonitorInfo> {
        monitors.iter().collect()
    }

    fn desktop(monitors: &[MonitorInfo]) -> Rect {
        monitors
            .iter()
            .map(|m| m.rect)
            .reduce(Rect::union)
            .expect("tests have screens")
    }

    #[test]
    fn one_screen_is_one_part() {
        let m = three();
        let (parts, _) = split(m[1].rect, &[&m[1]], &m).expect("a screen can be split");
        assert_eq!(parts.len(), 1);
    }

    #[test]
    fn every_screen_gets_a_part() {
        let m = three();
        let (parts, _) = split(desktop(&m), &all_of(&m), &m).expect("the desktop can be split");
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn outer_edges_leave_room_for_the_frame() {
        let m = three();
        let (parts, _) = split(desktop(&m), &all_of(&m), &m).expect("the desktop can be split");
        assert_eq!(parts[0].area.y, BORDER);
    }

    #[test]
    fn parts_are_even_sized() {
        let m = three();
        let (parts, _) = split(desktop(&m), &all_of(&m), &m).expect("the desktop can be split");
        assert!(
            parts.iter().all(|p| p.area.w % 2 == 0 && p.area.h % 2 == 0),
            "{parts:?}"
        );
    }

    #[test]
    fn evening_out_trims_the_side_away_from_a_neighbour() {
        let m = three();
        let (parts, _) = split(desktop(&m), &all_of(&m), &m).expect("the desktop can be split");
        // DP-1's part ends where HDMI-A-1's begins: no seam in the joined video.
        assert_eq!(parts[0].area.right(), parts[1].area.x);
    }

    #[test]
    fn neighbouring_parts_sit_side_by_side() {
        let m = three();
        let (parts, _) = split(desktop(&m), &all_of(&m), &m).expect("the desktop can be split");
        assert_eq!(parts[1].slot, (parts[0].area.w, 0));
    }

    #[test]
    fn the_joined_size_covers_every_part() {
        let m = three();
        let (parts, size) = split(desktop(&m), &all_of(&m), &m).expect("the desktop can be split");
        let width: i32 = parts.iter().map(|p| p.area.w).sum();
        assert_eq!(size, (width, parts[0].area.h));
    }

    #[test]
    fn a_screen_left_out_leaves_no_gap() {
        let m = three();
        let picked = [&m[0], &m[2]];
        let (parts, _) = split(desktop(&m), &picked, &m).expect("two screens can be split");
        assert_eq!(parts[1].slot.0, parts[0].area.w);
    }

    #[test]
    fn a_region_across_two_screens_is_cut_at_the_seam() {
        let m = three();
        let region = Rect::new(2000, 300, 1000, 500);
        let touched = [&m[0], &m[1]];
        let (parts, _) = split(region, &touched, &m).expect("a region can be split");
        assert_eq!((parts[0].area.right(), parts[1].area.x), (2560, 2560));
    }

    #[test]
    fn a_part_too_small_to_record_is_an_error() {
        let m = three();
        // 2 px on DP-1, the rest on HDMI-A-1.
        let region = Rect::new(2558, 300, 500, 500);
        assert!(split(region, &[&m[0], &m[1]], &m).is_err());
    }
}
