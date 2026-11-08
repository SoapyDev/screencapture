//! Turning a [`Request`] into a saved file.

use std::path::Path;

use anyhow::Result;
use image::RgbaImage;

use screencapture_core::{
    capture::Snapshot,
    drawings::Drawings,
    selection::Request,
    units::{ToF32, saturating_u32},
};

/// Capture what `request` asks for. Only the monitors involved are copied.
///
/// # Errors
///
/// When the display server refuses the capture, or `request` names no screen there is.
pub fn grab(request: &Request) -> Result<RgbaImage> {
    compose(&Snapshot::take_where(|m| request.needs(m))?, request, None)
}

/// The final image for an overlay selection: cut from the frozen `snapshot`, with any
/// `drawings` burnt in at the output's pixel density.
///
/// # Errors
///
/// When the region touches no screen, or a named screen doesn't exist.
pub fn compose<C: std::ops::Deref<Target = [u8]>>(
    snapshot: &Snapshot<C>,
    request: &Request,
    drawings: Option<&dyn Drawings>,
) -> Result<RgbaImage> {
    let mut image = snapshot.grab(request)?;
    // Screens are BGRA; the PNG (and the drawing colours) are RGBA.
    screencapture_core::capture::swap_red_blue(&mut image);
    let Some(drawings) = drawings else {
        return Ok(image);
    };
    // Each screen's part of the drawing goes where that screen landed, cut at its edges.
    let (placements, scale) = snapshot.placements(request)?;
    let px = |v: i32| saturating_u32((f64::from(v) * scale).round());
    for p in placements {
        let origin = ((p.src.x - p.dst.0).to_f32(), (p.src.y - p.dst.1).to_f32());
        let clip = [
            px(p.dst.0),
            px(p.dst.1),
            px(p.dst.0 + p.src.w),
            px(p.dst.1 + p.src.h),
        ];
        drawings.rasterize(&mut image, origin, scale.to_f32(), clip);
    }
    Ok(image)
}

/// Capture and save.
///
/// # Errors
///
/// When capturing fails, or the file can't be written.
pub fn run(request: &Request, dir: &Path) -> Result<crate::save::Saved> {
    let image = grab(request)?;
    crate::save::save_png(&image, dir, chrono::Local::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;
    use screencapture_core::capture::{self, Rect, Shot};

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

    /// Stand-in drawings: red desktop rectangles.
    struct Fill(Vec<Rect>);

    impl Fill {
        fn covers(&self, (x, y): (f32, f32)) -> bool {
            let inside = |r: &Rect| {
                (r.x.to_f32()..r.right().to_f32()).contains(&x)
                    && (r.y.to_f32()..r.bottom().to_f32()).contains(&y)
            };
            self.0.iter().any(inside)
        }
    }

    impl Drawings for Fill {
        fn rasterize(&self, image: &mut RgbaImage, origin: (f32, f32), scale: f32, clip: [u32; 4]) {
            for py in clip[1]..clip[3].min(image.height()) {
                for px in clip[0]..clip[2].min(image.width()) {
                    let desktop = (px.to_f32() / scale + origin.0, py.to_f32() / scale + origin.1);
                    if self.covers(desktop) {
                        image.put_pixel(px, py, Rgba([255, 0, 0, 255]));
                    }
                }
            }
        }

        fn extents(&self) -> Vec<Rect> {
            self.0.clone()
        }

        fn view(&self, _: Rect, _: &mut gpui::App) -> gpui::AnyView {
            unreachable!("composing an image never asks for a view")
        }
    }

    #[test]
    fn region_gets_shapes_at_output_density() {
        let line = Fill(vec![Rect::new(10, 19, 20, 2)]);
        let img = compose(
            &hidpi_white(),
            &Request::Region(Rect::new(10, 10, 20, 20)),
            Some(&line),
        )
        .unwrap();
        assert_eq!(img.dimensions(), (40, 40));
        // Logical y = 19..21 is pixel rows 18..22 of the cut: 2 logical px are 4 px.
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
        let lines = Fill(vec![
            Rect::new(210, 24, 80, 2), // on C
            Rect::new(150, 9, 10, 2),  // on B, not picked
            Rect::new(90, 39, 40, 2),  // from A into B
        ]);
        let img = compose(
            &snap,
            &Request::Screens(vec!["A".into(), "C".into()]),
            Some(&lines),
        )
        .unwrap();
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
            let img = compose(&snap, &request, None).unwrap();
            assert_eq!(img.into_raw(), vec![3, 2, 1, 255, 6, 5, 4, 255]);
        }
    }

    #[test]
    fn without_shapes_it_is_the_plain_cut() {
        let snap = hidpi_white();
        let request = Request::Region(Rect::new(10, 10, 20, 20));
        let mut cut = snap.grab(&request).unwrap();
        screencapture_core::capture::swap_red_blue(&mut cut);
        assert_eq!(compose(&snap, &request, None).unwrap(), cut);
    }

    #[test]
    #[ignore = "needs a graphical session"]
    fn all_screens_match_desktop_size() {
        let monitors = capture::monitors().unwrap();
        let width: i32 = monitors.iter().map(|m| m.rect.w).sum();
        let image = grab(&Request::Screens(Vec::new())).unwrap();
        assert!(image.width() >= screencapture_core::units::unsigned(width));
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
