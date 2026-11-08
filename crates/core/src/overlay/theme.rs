//! The overlay's chrome colours, by role: what the bars, buttons and marks are painted with.
//!
//! Only the interface lives here. What the user draws (the annotation colours) is theirs and
//! never follows the desktop.

use std::sync::RwLock;

use gpui::{Hsla, hsla, rgba, white};

/// Every colour the overlay's interface is painted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeTheme {
    /// Bars and popups.
    pub surface: Hsla,
    pub on_surface: Hsla,
    /// Secondary text on a surface.
    pub on_surface_muted: Hsla,
    pub border: Hsla,
    pub separator: Hsla,
    /// A button under the pointer.
    pub hover: Hsla,
    /// The selection and the active button.
    pub accent: Hsla,
    /// The accent, quieter: the screen a click would select.
    pub accent_hint: Hsla,
    pub on_accent: Hsla,
    /// Error text over a scrim.
    pub danger_text: Hsla,
    /// The recording dot.
    pub danger_fill: Hsla,
    /// Behind labels that sit over the screenshot.
    pub scrim: Hsla,
    pub scrim_soft: Hsla,
    pub on_scrim: Hsla,
    /// Resize handles.
    pub handle_fill: Hsla,
    pub swatch_ring_selected: Hsla,
    pub swatch_ring_idle: Hsla,
    /// A screen under the pointer in Screens mode.
    pub screen_hover_outline: Hsla,
}

impl ChromeTheme {
    /// The look the overlay always had: dark bars, blue accent.
    #[must_use]
    pub fn dark() -> Self {
        Self {
            surface: from_rgba(0x1e1e_22f0),
            on_surface: hsla(0., 0., 0.92, 1.),
            on_surface_muted: from_rgba(0xffff_ffa0),
            border: hsla(0., 0., 1., 0.12),
            separator: hsla(0., 0., 1., 0.15),
            hover: hsla(0., 0., 1., 0.12),
            accent: hsla(0.58, 0.85, 0.55, 1.),
            accent_hint: hsla(0.58, 0.85, 0.55, 0.5),
            on_accent: white(),
            danger_text: from_rgba(0xff8a_80ff),
            danger_fill: from_rgba(0xff4d_4dff),
            scrim: from_rgba(0x0000_00c0),
            scrim_soft: from_rgba(0x0000_00a0),
            on_scrim: white(),
            handle_fill: white(),
            swatch_ring_selected: white(),
            swatch_ring_idle: hsla(0., 0., 1., 0.15),
            screen_hover_outline: hsla(0., 0., 1., 0.5),
        }
    }
}

fn from_rgba(hex: u32) -> Hsla {
    Hsla::from(rgba(hex))
}

/// The theme in use, once one was chosen.
static CURRENT: RwLock<Option<ChromeTheme>> = RwLock::new(None);

/// The theme the overlay paints with now: the dark one until another is chosen.
#[must_use]
pub fn current() -> ChromeTheme {
    let chosen = CURRENT.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    chosen.unwrap_or_else(ChromeTheme::dark)
}

/// Paint the overlay with `theme` from now on.
pub fn set_current(theme: ChromeTheme) {
    let mut chosen = CURRENT.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    *chosen = Some(theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_surface_is_the_bars_colour_it_always_had() {
        assert_eq!(ChromeTheme::dark().surface, from_rgba(0x1e1e_22f0));
    }

    #[test]
    fn dark_accent_is_the_blue_it_always_had() {
        assert_eq!(ChromeTheme::dark().accent, hsla(0.58, 0.85, 0.55, 1.));
    }

    #[test]
    fn dark_error_text_is_the_salmon_it_always_had() {
        assert_eq!(ChromeTheme::dark().danger_text, from_rgba(0xff8a_80ff));
    }

    #[test]
    fn a_theme_set_is_the_one_in_use() {
        let light = ChromeTheme {
            accent: hsla(0.1, 0.5, 0.5, 1.),
            ..ChromeTheme::dark()
        };
        set_current(light);
        assert_eq!(current(), light);
    }
}
