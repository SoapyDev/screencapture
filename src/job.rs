//! Turning a [`Request`] into a saved file.

use std::path::Path;

use anyhow::Result;
use image::RgbaImage;

use crate::{annotate::Shape, capture::Snapshot, overlay::layout::needs, selection::Request};

/// Capture what `request` asks for. Only the monitors involved are copied.
pub fn grab(request: &Request) -> Result<RgbaImage> {
    compose(&Snapshot::take_where(|m| needs(request, m))?, request, &[])
}

/// The final image for an overlay selection: cut from the frozen `snapshot`, with the drawn
/// `shapes` burnt in at the output's pixel density.
pub fn compose<C: std::ops::Deref<Target = [u8]>>(
    snapshot: &Snapshot<C>,
    request: &Request,
    shapes: &[Shape],
) -> Result<RgbaImage> {
    let mut image = snapshot.grab(request)?;
    // Screens are BGRA; the PNG (and the drawing colours) are RGBA.
    crate::capture::swap_red_blue(&mut image);
    if shapes.is_empty() {
        return Ok(image);
    }
    // Each screen's part of the drawing goes where that screen landed, cut at its edges.
    let (placements, scale) = snapshot.placements(request)?;
    let px = |v: i32| (f64::from(v) * scale).round().max(0.) as u32;
    for p in placements {
        let origin = ((p.src.x - p.dst.0) as f32, (p.src.y - p.dst.1) as f32);
        let clip = [
            px(p.dst.0),
            px(p.dst.1),
            px(p.dst.0 + p.src.w),
            px(p.dst.1 + p.src.h),
        ];
        crate::annotate::rasterize_clipped(&mut image, shapes, origin, scale as f32, clip);
    }
    Ok(image)
}

/// Capture and save.
pub fn run(request: &Request, dir: &Path) -> Result<crate::save::Saved> {
    let image = grab(request)?;
    crate::save::save_png(&image, dir, chrono::Local::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        annotate::Tool,
        capture::{self, Rect, Shot},
    };
    use image::Rgba;

    /// One white monitor at (0,0) 100x50 logical, 2 px per logical px.
    fn hidpi_white() -> Snapshot {
        Snapshot::new(vec![Shot {
            name: "A".into(),
            rect: Rect::new(0, 0, 100, 50),
            output_position: (0, 0),
            image: RgbaImage::from_pixel(200, 100, Rgba([255; 4])),
        }])
        .unwrap()
    }

    fn red_line(from: (f32, f32), to: (f32, f32)) -> Shape {
        Shape {
            tool: Tool::Line,
            points: vec![from, to],
            color: [255, 0, 0, 255],
            width: 2.,
            ..Shape::default()
        }
    }

    #[test]
    fn region_gets_shapes_at_output_density() {
        let shapes = [red_line((10., 20.), (30., 20.))];
        let img = compose(
            &hidpi_white(),
            &Request::Region(Rect::new(10, 10, 20, 20)),
            &shapes,
        )
        .unwrap();
        assert_eq!(img.dimensions(), (40, 40));
        // Logical y = 20 is pixel row 20 of the cut; the 2-logical-px stroke is 4 px thick.
        for y in [18, 19, 20, 21] {
            assert_eq!(img.get_pixel(20, y).0, [255, 0, 0, 255], "row {y}");
        }
        assert_eq!(img.get_pixel(20, 23).0, [255; 4]);
    }

    #[test]
    fn stitched_screens_get_each_screens_shapes_at_their_place() {
        let white = |name: &str, x: i32| Shot {
            name: name.into(),
            rect: Rect::new(x, 0, 100, 50),
            output_position: (x, 0),
            image: RgbaImage::from_pixel(100, 50, Rgba([255; 4])),
        };
        let snap = Snapshot::new(vec![white("A", 0), white("B", 100), white("C", 200)]).unwrap();
        let shapes = [
            red_line((210., 25.), (290., 25.)), // on C
            red_line((150., 10.), (160., 10.)), // on B, not picked
            red_line((90., 40.), (130., 40.)),  // from A into B
        ];
        let img = compose(&snap, &Request::Screens(vec!["A".into(), "C".into()]), &shapes).unwrap();
        assert_eq!(img.dimensions(), (200, 50));
        let red = [255, 0, 0, 255];
        assert_eq!(img.get_pixel(150, 25).0, red, "C's line lands in C's half");
        assert_eq!(img.get_pixel(105, 25).0, [255; 4]);
        assert!(
            (0..200).all(|x| img.get_pixel(x, 10).0 == [255; 4]),
            "B is not in the image"
        );
        assert_eq!(img.get_pixel(95, 40).0, red, "A's part of the line");
        assert_eq!(
            img.get_pixel(103, 40).0,
            [255; 4],
            "B's part is cut, not drawn over C"
        );
    }

    #[test]
    fn screens_are_bgra_but_the_saved_image_is_rgba() {
        let snap = Snapshot::new(vec![Shot {
            name: "A".into(),
            rect: Rect::new(0, 0, 2, 1),
            output_position: (0, 0),
            image: RgbaImage::from_raw(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]).unwrap(),
        }])
        .unwrap();
        for request in [Request::Region(Rect::new(0, 0, 2, 1)), Request::Screens(vec![])] {
            let img = compose(&snap, &request, &[]).unwrap();
            assert_eq!(img.into_raw(), vec![3, 2, 1, 255, 6, 5, 4, 255]);
        }
    }

    #[test]
    fn without_shapes_it_is_the_plain_cut() {
        let snap = hidpi_white();
        let request = Request::Region(Rect::new(10, 10, 20, 20));
        let mut cut = snap.grab(&request).unwrap();
        crate::capture::swap_red_blue(&mut cut);
        assert_eq!(compose(&snap, &request, &[]).unwrap(), cut);
    }

    #[test]
    #[ignore = "needs a graphical session"]
    fn all_screens_match_desktop_size() {
        let monitors = capture::monitors().unwrap();
        let width: i32 = monitors.iter().map(|m| m.rect.w).sum();
        let image = grab(&Request::Screens(Vec::new())).unwrap();
        assert!(image.width() >= width as u32);
    }

    #[test]
    #[ignore = "needs a graphical session"]
    fn region_is_cut_to_size_and_saved() {
        let first = capture::monitors().unwrap().remove(0);
        let rect = Rect::new(first.rect.x + 10, first.rect.y + 10, 64, 32);
        let dir = tempfile::tempdir().unwrap();
        let path = run(&Request::Region(rect), dir.path()).unwrap().path;
        let saved = image::open(&path).unwrap();
        assert_eq!((saved.width(), saved.height()), (64, 32));
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("screenshot_")
        );
    }
}
