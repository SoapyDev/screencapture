//! What the user asked of the theme, and the theme that comes out of it and the desktop's
//! colours: the config's `theme` wins over everything the desktop says.

use serde::{Deserialize, Serialize};

use super::{
    Basis, ChromeTheme, Scheme,
    contrast::is_dark,
    css,
    roles::{Roles, merged},
};
use crate::config::Section;

/// Follow the desktop, or hold to one scheme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Follow,
    Dark,
    Light,
}

/// Where the colours come from, for a user whose machine has several answers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeSource {
    /// The desktop's own: KDE's settings on KDE, GTK's stylesheet elsewhere, then the portal.
    #[default]
    Auto,
    /// Only the GTK stylesheets.
    Gtk,
    /// Only KDE's `kdeglobals`.
    Kde,
    /// Only the desktop portal's accent.
    Portal,
    /// Only the built-in colours.
    BuiltIn,
}

/// The `theme` keys of the config file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeChoice {
    #[serde(default)]
    pub theme: ThemeMode,
    /// Where the desktop's colours are read from. Read once when the program starts: a running
    /// daemon needs a restart to follow a change.
    #[serde(default)]
    pub theme_source: ThemeSource,
    /// `#rrggbb`: the accent, instead of the desktop's.
    #[serde(default)]
    pub theme_accent: Option<String>,
}

impl Section for ThemeChoice {}

/// The theme for `choice`, from the desktop's `scheme` and what `sources` (most trusted first)
/// say of its colours.
#[must_use]
pub fn resolve_theme(choice: &ThemeChoice, desktop: Scheme, sources: &[Roles]) -> ChromeTheme {
    let mut roles = merged(sources);
    if let Some(accent) = choice
        .theme_accent
        .as_deref()
        .and_then(|text| css::evaluate(text, &|_| None))
    {
        roles.accent = Some(accent);
    }
    let scheme = scheme_for(choice.theme, desktop, &roles);
    ChromeTheme::from_basis(basis_for(scheme, choice.theme, &roles))
}

fn scheme_for(mode: ThemeMode, desktop: Scheme, roles: &Roles) -> Scheme {
    match (mode, roles.surface) {
        (ThemeMode::Dark, _) => Scheme::Dark,
        (ThemeMode::Follow, Some(surface)) if is_dark(surface) => Scheme::Dark,
        (ThemeMode::Light, _) | (ThemeMode::Follow, Some(_)) => Scheme::Light,
        (ThemeMode::Follow, None) => desktop,
    }
}

/// The built-in colours for `scheme`, the desktop's over them. A forced scheme keeps only the
/// desktop's accent and danger: its surface and text belong to the other scheme.
fn basis_for(scheme: Scheme, mode: ThemeMode, roles: &Roles) -> Basis {
    let built_in = Basis::built_in(scheme);
    let forced = mode != ThemeMode::Follow;
    Basis {
        surface: roles.surface.filter(|_| !forced).unwrap_or(built_in.surface),
        on_surface: roles
            .on_surface
            .filter(|_| !forced)
            .unwrap_or(built_in.on_surface),
        accent: roles.accent.unwrap_or(built_in.accent),
        danger: roles.danger.unwrap_or(built_in.danger),
    }
}

#[cfg(test)]
mod tests {
    use gpui::{black, white};

    use super::*;

    fn follow() -> ThemeChoice {
        ThemeChoice::default()
    }

    fn colour(hex: &str) -> gpui::Hsla {
        css::evaluate(hex, &|_| None).expect("the test's colour is valid")
    }

    fn dark_desktop() -> Roles {
        Roles {
            surface: Some(colour("#1e1e2e")),
            accent: Some(colour("#cba6f7")),
            ..Roles::default()
        }
    }

    #[test]
    fn with_nothing_from_the_desktop_the_scheme_decides() {
        let theme = resolve_theme(&follow(), Scheme::Light, &[]);
        assert_eq!(theme, ChromeTheme::built_in(Scheme::Light));
    }

    #[test]
    fn a_dark_surface_from_a_file_beats_a_light_portal() {
        let theme = resolve_theme(&follow(), Scheme::Light, &[dark_desktop()]);
        assert!(theme.surface.l < 0.5);
    }

    #[test]
    fn the_desktop_accent_is_used_when_following() {
        let theme = resolve_theme(&follow(), Scheme::Dark, &[dark_desktop()]);
        assert_eq!(theme.accent, colour("#cba6f7"));
    }

    #[test]
    fn a_forced_light_theme_ignores_a_dark_desktop_surface() {
        let choice = ThemeChoice {
            theme: ThemeMode::Light,
            ..follow()
        };
        let theme = resolve_theme(&choice, Scheme::Dark, &[dark_desktop()]);
        assert!(theme.surface.l > 0.5);
    }

    #[test]
    fn a_forced_theme_still_takes_the_desktop_accent() {
        let choice = ThemeChoice {
            theme: ThemeMode::Light,
            ..follow()
        };
        let theme = resolve_theme(&choice, Scheme::Dark, &[dark_desktop()]);
        assert_eq!(theme.accent, colour("#cba6f7"));
    }

    #[test]
    fn the_configured_accent_beats_the_desktop_accent() {
        let choice = ThemeChoice {
            theme_accent: Some("#ff0000".into()),
            ..follow()
        };
        let theme = resolve_theme(&choice, Scheme::Dark, &[dark_desktop()]);
        assert_eq!(theme.accent, colour("#ff0000"));
    }

    #[test]
    fn a_configured_accent_that_is_not_a_colour_is_ignored() {
        let choice = ThemeChoice {
            theme_accent: Some("purple-ish".into()),
            ..follow()
        };
        let theme = resolve_theme(&choice, Scheme::Dark, &[dark_desktop()]);
        assert_eq!(theme.accent, colour("#cba6f7"));
    }

    #[test]
    fn the_first_source_decides_a_role_the_others_also_give() {
        let other = Roles {
            accent: Some(white()),
            ..Roles::default()
        };
        let theme = resolve_theme(&follow(), Scheme::Dark, &[dark_desktop(), other]);
        assert_eq!(theme.accent, colour("#cba6f7"));
    }

    #[test]
    fn an_unreadable_accent_button_text_is_still_readable() {
        let pale = Roles {
            accent: Some(white()),
            ..Roles::default()
        };
        let theme = resolve_theme(&follow(), Scheme::Dark, &[pale]);
        assert_eq!(theme.on_accent, black());
    }

    #[test]
    fn the_choice_is_read_from_config_keys() {
        let choice: ThemeChoice = serde_json::from_str(r##"{"theme":"light","theme_accent":"#123456"}"##)
            .expect("the test's JSON is valid");
        assert_eq!(choice.theme, ThemeMode::Light);
    }

    #[test]
    fn the_source_is_read_from_config_keys() {
        let choice: ThemeChoice =
            serde_json::from_str(r#"{"theme_source":"builtin"}"#).expect("the test's JSON is valid");
        assert_eq!(choice.theme_source, ThemeSource::BuiltIn);
    }

    #[test]
    fn a_missing_source_key_is_auto() {
        let choice: ThemeChoice = serde_json::from_str("{}").expect("the test's JSON is valid");
        assert_eq!(choice.theme_source, ThemeSource::Auto);
    }

    #[test]
    fn a_missing_theme_key_follows_the_desktop() {
        let choice: ThemeChoice = serde_json::from_str("{}").expect("the test's JSON is valid");
        assert_eq!(choice.theme, ThemeMode::Follow);
    }
}
