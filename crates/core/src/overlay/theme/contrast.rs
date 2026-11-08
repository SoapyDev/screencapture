//! How readable one colour is on another (WCAG 2.x contrast ratio), and the fixes that make it so.

use gpui::{Hsla, Rgba, black, white};

/// WCAG's floor for normal-size text.
pub const READABLE: f32 = 4.5;

/// Contrast between `foreground` and `background`, from 1 (none) to 21 (black on white).
/// Transparency is ignored: both are judged as they would look opaque.
#[must_use]
pub fn ratio(foreground: Hsla, background: Hsla) -> f32 {
    let (a, b) = (luminance(foreground), luminance(background));
    let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Whether `colour` is on the dark side: white reads on it better than black does.
#[must_use]
pub fn is_dark(colour: Hsla) -> bool {
    ratio(white(), colour) > ratio(black(), colour)
}

/// White or black, whichever reads better on `background`.
#[must_use]
pub fn readable_on(background: Hsla) -> Hsla {
    if ratio(white(), background) >= ratio(black(), background) {
        white()
    } else {
        black()
    }
}

/// `foreground`, moved toward white or black (away from `background`) until it reads on it.
/// Gives up at the extreme, which is the best that can be done.
#[must_use]
pub fn made_readable_on(foreground: Hsla, background: Hsla) -> Hsla {
    let lighter = luminance(background) < 0.5;
    let target = if lighter { 1. } else { 0. };
    let mut fixed = foreground;
    for _ in 0..20 {
        if ratio(fixed, background) >= READABLE {
            break;
        }
        fixed.l = (target - fixed.l).mul_add(0.15, fixed.l);
    }
    fixed
}

fn luminance(color: Hsla) -> f32 {
    let Rgba { r, g, b, .. } = Rgba::from(color);
    0.0722f32.mul_add(linear(b), 0.2126f32.mul_add(linear(r), 0.7152 * linear(g)))
}

fn linear(channel: f32) -> f32 {
    if channel <= 0.03928 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use gpui::hsla;

    use super::*;

    #[test]
    fn black_on_white_is_the_widest_contrast() {
        assert!((ratio(black(), white()) - 21.).abs() < 0.01);
    }

    #[test]
    fn a_colour_on_itself_has_no_contrast() {
        let colour = hsla(0.3, 0.5, 0.5, 1.);
        assert!((ratio(colour, colour) - 1.).abs() < f32::EPSILON);
    }

    #[test]
    fn white_reads_best_on_a_dark_blue() {
        assert_eq!(readable_on(hsla(0.6, 0.8, 0.3, 1.)), white());
    }

    #[test]
    fn a_dark_blue_is_dark() {
        assert!(is_dark(hsla(0.6, 0.8, 0.2, 1.)));
    }

    #[test]
    fn a_pale_lavender_is_not_dark() {
        assert!(!is_dark(hsla(0.75, 0.8, 0.8, 1.)));
    }

    #[test]
    fn black_reads_best_on_a_pale_lavender() {
        assert_eq!(readable_on(hsla(0.75, 0.8, 0.8, 1.)), black());
    }

    #[test]
    fn a_dim_red_is_lightened_until_it_reads_on_black() {
        let fixed = made_readable_on(hsla(0., 0.9, 0.2, 1.), black());
        assert!(ratio(fixed, black()) >= READABLE);
    }

    #[test]
    fn a_colour_that_already_reads_is_left_alone() {
        let colour = hsla(0.1, 0.5, 0.8, 1.);
        assert_eq!(made_readable_on(colour, black()), colour);
    }

    #[test]
    fn a_pale_text_on_white_is_darkened_until_it_reads() {
        let fixed = made_readable_on(hsla(0.1, 0.5, 0.8, 1.), white());
        assert!(ratio(fixed, white()) >= READABLE);
    }
}
