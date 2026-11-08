//! The desktop's colours as KDE's `kdeglobals` carries them.

use gpui::Hsla;

use super::roles::Roles;

/// The roles `kdeglobals`' text gives.
#[must_use]
pub fn roles(kdeglobals: &str) -> Roles {
    Roles {
        surface: key(kdeglobals, "Colors:Window", "BackgroundNormal"),
        on_surface: key(kdeglobals, "Colors:Window", "ForegroundNormal"),
        accent: key(kdeglobals, "General", "AccentColor")
            .or_else(|| key(kdeglobals, "Colors:Selection", "BackgroundNormal")),
        danger: key(kdeglobals, "Colors:View", "ForegroundNegative"),
    }
}

/// `key` of `[section]`, a `red,green,blue` colour.
fn key(text: &str, section: &str, key: &str) -> Option<Hsla> {
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if let Some(header) = line.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
            inside = header == section;
        } else if let Some((_, value)) = line
            .split_once('=')
            .filter(|(name, _)| inside && name.trim() == key)
        {
            return rgb(value);
        }
    }
    None
}

fn rgb(value: &str) -> Option<Hsla> {
    let mut channels = value.split(',').map(|part| part.trim().parse::<u8>());
    let (red, green, blue) = (
        channels.next()?.ok()?,
        channels.next()?.ok()?,
        channels.next()?.ok()?,
    );
    let hex = format!("#{red:02x}{green:02x}{blue:02x}");
    super::css::evaluate(&hex, &|_| None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BREEZE_DARK: &str = "[General]\nAccentColor=61,174,233\n\
        [Colors:Window]\nBackgroundNormal=35,38,41\nForegroundNormal=252,252,252\n\
        [Colors:Selection]\nBackgroundNormal=61,174,233\n\
        [Colors:View]\nForegroundNegative=218,68,83\n";

    fn colour(hex: &str) -> Option<Hsla> {
        super::super::css::evaluate(hex, &|_| None)
    }

    #[test]
    fn the_surface_is_the_window_background() {
        assert_eq!(roles(BREEZE_DARK).surface, colour("#232629"));
    }

    #[test]
    fn the_accent_is_the_general_accent() {
        assert_eq!(roles(BREEZE_DARK).accent, colour("#3daee9"));
    }

    #[test]
    fn without_a_general_accent_the_selection_colour_is_the_accent() {
        let text = "[Colors:Selection]\nBackgroundNormal=1,2,3\n";
        assert_eq!(roles(text).accent, colour("#010203"));
    }

    #[test]
    fn danger_is_the_negative_foreground() {
        assert_eq!(roles(BREEZE_DARK).danger, colour("#da4453"));
    }

    #[test]
    fn a_key_in_another_section_is_not_taken() {
        assert!(roles("[Colors:Button]\nBackgroundNormal=1,2,3\n").is_empty());
    }

    #[test]
    fn a_colour_out_of_range_is_not_taken() {
        assert!(roles("[Colors:Window]\nBackgroundNormal=1,2,300\n").is_empty());
    }
}
