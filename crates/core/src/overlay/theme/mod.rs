//! The overlay's chrome colours, by role: what the bars, buttons and marks are painted with.
//!
//! Only the interface lives here. What the user draws (the annotation colours) is theirs and
//! never follows the desktop.

use std::sync::RwLock;

use gpui::{Hsla, WindowAppearance, black, hsla, rgba, white};

pub mod choice;
pub mod contrast;
pub mod css;
pub mod desktop;
pub mod gtk;
pub mod kde;
#[cfg(target_os = "linux")]
pub mod portal;
pub mod roles;

use contrast::{made_readable_on, readable_on};

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
    /// The recording dot.
    pub danger_fill: Hsla,
    pub swatch_ring_selected: Hsla,
    pub swatch_ring_idle: Hsla,
    /// What is painted straight over the screenshot, whatever the desktop's colours.
    pub marks: ContentMarks,
}

/// Labels and handles over the screenshot: dark scrims with light text, so they read on any
/// content, light or dark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentMarks {
    pub scrim: Hsla,
    pub scrim_soft: Hsla,
    pub on_scrim: Hsla,
    /// Error text over a scrim.
    pub danger_text: Hsla,
    /// Resize handles.
    pub handle_fill: Hsla,
    /// A screen under the pointer in Screens mode.
    pub screen_hover_outline: Hsla,
}

/// Light or dark: which way the desktop leans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Dark,
    Light,
}

impl From<WindowAppearance> for Scheme {
    fn from(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
        }
    }
}

/// The four colours a desktop gives; the rest of the theme follows from them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Basis {
    pub surface: Hsla,
    pub on_surface: Hsla,
    pub accent: Hsla,
    pub danger: Hsla,
}

impl ChromeTheme {
    /// The built-in theme for `scheme`.
    #[must_use]
    pub fn built_in(scheme: Scheme) -> Self {
        Self::from_basis(Basis::built_in(scheme))
    }

    /// The overlay's original look: dark bars, blue accent.
    #[must_use]
    pub fn dark() -> Self {
        Self::built_in(Scheme::Dark)
    }

    /// The theme `basis` makes: text, accent and danger kept readable on what they sit on.
    #[must_use]
    pub fn from_basis(basis: Basis) -> Self {
        let Basis {
            surface,
            on_surface,
            accent,
            danger,
        } = basis;
        let marks = ContentMarks::over_screenshots(danger);
        Self {
            surface: surface.opacity(0.94),
            on_surface: made_readable_on(on_surface, surface),
            on_surface_muted: made_readable_on(on_surface.opacity(0.7), surface),
            border: on_surface.opacity(0.12),
            separator: on_surface.opacity(0.15),
            hover: on_surface.opacity(0.12),
            accent,
            accent_hint: accent.opacity(0.5),
            on_accent: readable_on(accent),
            danger_fill: made_readable_on(danger, surface),
            swatch_ring_selected: on_surface,
            swatch_ring_idle: on_surface.opacity(0.15),
            marks,
        }
    }
}

impl ContentMarks {
    fn over_screenshots(danger: Hsla) -> Self {
        Self {
            scrim: from_rgba(0x0000_00c0),
            scrim_soft: from_rgba(0x0000_00a0),
            on_scrim: white(),
            danger_text: made_readable_on(danger, black()),
            handle_fill: white(),
            screen_hover_outline: hsla(0., 0., 1., 0.5),
        }
    }
}

impl Basis {
    fn built_in(scheme: Scheme) -> Self {
        match scheme {
            Scheme::Dark => Self::dark(),
            Scheme::Light => Self::light(),
        }
    }

    fn dark() -> Self {
        Self {
            surface: from_rgba(0x1e1e_22ff),
            on_surface: hsla(0., 0., 0.92, 1.),
            accent: hsla(0.58, 0.85, 0.44, 1.),
            danger: from_rgba(0xff8a_80ff),
        }
    }

    fn light() -> Self {
        Self {
            surface: from_rgba(0xf6f6_f8ff),
            on_surface: hsla(0., 0., 0.12, 1.),
            accent: hsla(0.58, 0.85, 0.40, 1.),
            danger: from_rgba(0xc02d_2dff),
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
    use contrast::{READABLE, ratio};

    fn themes() -> [ChromeTheme; 2] {
        [Scheme::Dark, Scheme::Light].map(ChromeTheme::built_in)
    }

    #[test]
    fn text_reads_on_the_bars_in_every_built_in_theme() {
        for theme in themes() {
            assert!(ratio(theme.on_surface, theme.surface) >= READABLE);
        }
    }

    #[test]
    fn secondary_text_reads_on_the_bars_in_every_built_in_theme() {
        for theme in themes() {
            assert!(ratio(theme.on_surface_muted, theme.surface) >= READABLE);
        }
    }

    #[test]
    fn the_active_button_reads_in_every_built_in_theme() {
        for theme in themes() {
            assert!(ratio(theme.on_accent, theme.accent) >= READABLE);
        }
    }

    #[test]
    fn error_text_reads_on_its_scrim_in_every_built_in_theme() {
        for theme in themes() {
            assert!(ratio(theme.marks.danger_text, black()) >= READABLE);
        }
    }

    #[test]
    fn the_recording_dot_shows_on_the_bars_in_every_built_in_theme() {
        for theme in themes() {
            assert!(ratio(theme.danger_fill, theme.surface) >= 3.);
        }
    }

    #[test]
    fn light_bars_are_lighter_than_dark_bars() {
        let [dark, light] = themes();
        assert!(light.surface.l > dark.surface.l);
    }

    #[test]
    fn a_dim_desktop_text_is_made_to_read_on_its_surface() {
        let basis = Basis {
            on_surface: hsla(0., 0., 0.3, 1.),
            ..Basis::dark()
        };
        let theme = ChromeTheme::from_basis(basis);
        assert!(ratio(theme.on_surface, theme.surface) >= READABLE);
    }

    #[test]
    fn a_dark_window_appearance_is_the_dark_scheme() {
        assert_eq!(Scheme::from(WindowAppearance::Dark), Scheme::Dark);
    }

    #[test]
    fn a_light_window_appearance_is_the_light_scheme() {
        assert_eq!(Scheme::from(WindowAppearance::Light), Scheme::Light);
    }

    #[test]
    fn a_theme_set_is_the_one_in_use() {
        let light = ChromeTheme::built_in(Scheme::Light);
        set_current(light);
        assert_eq!(current(), light);
    }
}
