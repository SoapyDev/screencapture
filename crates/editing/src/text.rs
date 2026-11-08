//! Text boxes: layout and rasterising with cosmic-text.
//!
//! Shared by the overlay preview and the saved image, so both wrap and look the same. Fonts are only loaded when the Text tool is
//! first used ([`preload`]), never on the capture path.

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use image::RgbaImage;

use screencapture_core::units::{ToF32, saturating_i32, signed, unsigned};

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

/// `text` laid out for a box `width` pixels wide.
fn layout(system: &mut FontSystem, text: &str, size: f32, width: u32) -> Buffer {
    let size = size.max(1.);
    let mut buffer = Buffer::new(system, Metrics::new(size, size * LINE_HEIGHT));
    buffer.set_wrap(Wrap::WordOrGlyph);
    buffer.set_size(
        Some((2. * size).mul_add(-PADDING, width.to_f32()).max(size)),
        None,
    );
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

/// A `width`×`height` pixel image of `text` at `size` px in `color`, wrapped to the box, on
/// `fill` (transparent without).
#[must_use]
pub fn render(
    text: &str,
    size: f32,
    color: Color,
    fill: Option<Color>,
    (width, height): (u32, u32),
) -> RgbaImage {
    let background = image::Rgba(fill.unwrap_or([0; 4]));
    let mut image = RgbaImage::from_pixel(width.max(1), height.max(1), background);
    if text.is_empty() {
        return image;
    }
    let mut guard = fonts();
    let Fonts { system, cache } = &mut *guard;
    let mut buffer = layout(system, text, size, width);
    let pad = saturating_i32((size * PADDING).round());
    let [red, green, blue, alpha] = color;
    let ink = cosmic_text::Color::rgba(red, green, blue, alpha);
    buffer.draw(system, cache, ink, |x, y, width, height, glyph| {
        let shade = (
            [glyph.r(), glyph.g(), glyph.b(), 255],
            f32::from(glyph.a()) / 255.,
        );
        fill_rect(&mut image, (x + pad, y + pad, width, height), shade);
    });
    drop(guard);
    image
}

/// Blend `(colour, coverage)` over the `(left, top, width, height)` rectangle of `image`,
/// clipped to it.
fn fill_rect(
    image: &mut RgbaImage,
    (left, top, width, height): (i32, i32, u32, u32),
    (colour, coverage): (Color, f32),
) {
    let (image_width, image_height) = (signed(image.width()), signed(image.height()));
    for py in top.max(0)..(top + signed(height)).min(image_height) {
        for px in left.max(0)..(left + signed(width)).min(image_width) {
            let pixel = &mut image.get_pixel_mut(unsigned(px), unsigned(py)).0;
            *pixel = blend(*pixel, colour, coverage);
        }
    }
}

/// Caret after the last character, as (x, y, height) in pixels from the box's top-left.
#[must_use]
pub fn caret(text: &str, size: f32, width: u32) -> (f32, f32, f32) {
    let pad = size * PADDING;
    let buffer = layout(&mut fonts().system, text, size, width);
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
        let img = render("", 20., RED, Some([0, 0, 255, 255]), (30, 20));
        assert!(img.pixels().all(|p| p.0 == [0, 0, 255, 255]));
        assert!(render("", 20., RED, None, (30, 20)).pixels().all(|p| p[3] == 0));
    }

    #[test]
    fn text_is_drawn_in_its_colour_inside_the_padding() {
        let img = render("Hello", 24., RED, None, (200, 60));
        let ink = ink(&img);
        assert!(ink.len() > 50, "some ink");
        for &(x, y) in &ink {
            let p = img.get_pixel(x, y).0;
            assert_eq!((p[0], p[1], p[2]), (255, 0, 0));
            let edge = 24.0f32.mul_add(PADDING, -1.);
            assert!(x.to_f32() >= edge && y.to_f32() >= edge);
        }
        assert_eq!(img.get_pixel(199, 59)[3], 0);
    }

    #[test]
    fn text_blends_over_the_fill() {
        let img = render("Hi", 24., [255, 255, 255, 255], Some([0, 0, 0, 255]), (80, 40));
        assert!(img.pixels().all(|p| p[3] == 255), "opaque everywhere");
        assert!(img.pixels().any(|p| p.0 == [255, 255, 255, 255]));
        assert_eq!(img.get_pixel(79, 39).0, [0, 0, 0, 255]);
    }

    #[test]
    fn bigger_font_means_more_ink() {
        let small = ink(&render("Text", 12., RED, None, (300, 100))).len();
        let big = ink(&render("Text", 36., RED, None, (300, 100))).len();
        assert!(big > small * 4, "{small} vs {big}");
    }

    #[test]
    fn long_text_wraps_to_the_box() {
        let img = render("wrap wrap wrap wrap wrap wrap", 20., RED, None, (120, 200));
        let lowest = ink(&img).iter().map(|p| p.1).max().unwrap();
        assert!(
            lowest.to_f32() > 20. * LINE_HEIGHT * 2.,
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
