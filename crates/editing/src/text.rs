//! Text boxes: layout and rasterising with cosmic-text.
//!
//! Shared by the overlay preview and the saved image, so both wrap and look the same. Fonts are only loaded when the Text tool is
//! first used ([`preload`]), never on the capture path.

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use image::RgbaImage;

use crate::annotate::{Color, blend};

struct Fonts {
    system: FontSystem,
    cache: SwashCache,
}

fn fonts() -> MutexGuard<'static, Fonts> {
    static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let fonts = Fonts {
                system: FontSystem::new(),
                cache: SwashCache::new(),
            };
            screencapture_core::trace("fonts loaded");
            Mutex::new(fonts)
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// `text` laid out for a box `w` pixels wide.
fn layout(system: &mut FontSystem, text: &str, size: f32, w: u32) -> Buffer {
    let size = size.max(1.);
    let mut buffer = Buffer::new(system, Metrics::new(size, size * LINE_HEIGHT));
    buffer.set_wrap(Wrap::WordOrGlyph);
    buffer.set_size(Some((2. * size).mul_add(-PADDING, w as f32).max(size)), None);
    buffer.set_text(
        text,
        &Attrs::new().family(Family::SansSerif),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(system, false);
    buffer
}

/// Inner margin of a text box, as a fraction of the font size.
pub const PADDING: f32 = 0.3;
pub const LINE_HEIGHT: f32 = 1.25;

/// Start loading system fonts in the background.
pub fn preload() {
    std::thread::spawn(|| drop(fonts()));
}

/// A `w`×`h` pixel image of `text` at `size` px in `color`, wrapped to the box, on `fill`
/// (transparent without).
#[must_use]
pub fn render(text: &str, size: f32, color: Color, fill: Option<Color>, w: u32, h: u32) -> RgbaImage {
    let mut image = RgbaImage::from_pixel(w.max(1), h.max(1), image::Rgba(fill.unwrap_or([0; 4])));
    if text.is_empty() {
        return image;
    }
    let mut guard = fonts();
    let Fonts { system, cache } = &mut *guard;
    let mut buffer = layout(system, text, size, w);
    let pad = (size * PADDING).round() as i32;
    let [r, g, b, a] = color;
    buffer.draw(
        system,
        cache,
        cosmic_text::Color::rgba(r, g, b, a),
        |x, y, gw, gh, c| {
            for py in y..y + gh as i32 {
                for px in x..x + gw as i32 {
                    let (ix, iy) = (px + pad, py + pad);
                    if ix < 0 || iy < 0 || ix >= image.width() as i32 || iy >= image.height() as i32 {
                        continue;
                    }
                    let dst = &mut image.get_pixel_mut(ix as u32, iy as u32).0;
                    *dst = blend(*dst, [c.r(), c.g(), c.b(), 255], f32::from(c.a()) / 255.);
                }
            }
        },
    );
    drop(guard);
    image
}

/// Caret after the last character, as (x, y, height) in pixels from the box's top-left.
#[must_use]
pub fn caret(text: &str, size: f32, w: u32) -> (f32, f32, f32) {
    let pad = size * PADDING;
    let buffer = layout(&mut fonts().system, text, size, w);
    let (x, y, h) = buffer
        .layout_runs()
        .last()
        .map_or((0., 0., size * LINE_HEIGHT), |run| {
            let x = run.glyphs.last().map_or(0., |g| g.x + g.w);
            (x, run.line_top, run.line_height)
        });
    (x + pad, y + pad, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = [255, 0, 0, 255];

    fn ink(img: &RgbaImage) -> Vec<(u32, u32)> {
        img.enumerate_pixels()
            .filter(|(_, _, p)| p[3] > 128)
            .map(|(x, y, _)| (x, y))
            .collect()
    }

    #[test]
    fn empty_box_is_its_fill_or_nothing() {
        let img = render("", 20., RED, Some([0, 0, 255, 255]), 30, 20);
        assert!(img.pixels().all(|p| p.0 == [0, 0, 255, 255]));
        assert!(render("", 20., RED, None, 30, 20).pixels().all(|p| p[3] == 0));
    }

    #[test]
    fn text_is_drawn_in_its_colour_inside_the_padding() {
        let img = render("Hello", 24., RED, None, 200, 60);
        let ink = ink(&img);
        assert!(ink.len() > 50, "some ink");
        for &(x, y) in &ink {
            let p = img.get_pixel(x, y).0;
            assert_eq!((p[0], p[1], p[2]), (255, 0, 0));
            assert!(x as f32 >= 24.0f32.mul_add(PADDING, -1.) && y as f32 >= 24.0f32.mul_add(PADDING, -1.));
        }
        assert_eq!(img.get_pixel(199, 59)[3], 0);
    }

    #[test]
    fn text_blends_over_the_fill() {
        let img = render("Hi", 24., [255, 255, 255, 255], Some([0, 0, 0, 255]), 80, 40);
        assert!(img.pixels().all(|p| p[3] == 255), "opaque everywhere");
        assert!(img.pixels().any(|p| p.0 == [255, 255, 255, 255]));
        assert_eq!(img.get_pixel(79, 39).0, [0, 0, 0, 255]);
    }

    #[test]
    fn bigger_font_means_more_ink() {
        let small = ink(&render("Text", 12., RED, None, 300, 100)).len();
        let big = ink(&render("Text", 36., RED, None, 300, 100)).len();
        assert!(big > small * 4, "{small} vs {big}");
    }

    #[test]
    fn long_text_wraps_to_the_box() {
        let img = render("wrap wrap wrap wrap wrap wrap", 20., RED, None, 120, 200);
        let lowest = ink(&img).iter().map(|p| p.1).max().unwrap();
        assert!(
            lowest as f32 > 20. * LINE_HEIGHT * 2.,
            "several lines, lowest ink at {lowest}"
        );
    }

    #[test]
    fn caret_follows_the_text() {
        let pad = 20. * PADDING;
        let (x0, y0, h) = caret("", 20., 200);
        assert!((x0 - pad).abs() < 0.5 && (y0 - pad).abs() < 0.5);
        assert!(20.0f32.mul_add(-LINE_HEIGHT, h).abs() < 0.5);
        let (x1, y1, _) = caret("abc", 20., 200);
        assert!(x1 > x0 + 10. && (y1 - y0).abs() < 0.5);
        let (x2, y2, _) = caret("abc\n", 20., 200);
        assert!((x2 - pad).abs() < 0.5 && y2 > y0 + 20.);
    }
}
