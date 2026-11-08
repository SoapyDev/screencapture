//! Moving through the overlay's buttons with the keyboard.

use gpui::Keystroke;

/// What a key does to the buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Navigation {
    FocusNext,
    FocusPrevious,
    /// Open the options bars one after the other, pinned.
    ToggleOptions,
}

/// The navigation `key` asks for, if any: Tab and Shift+Tab move between buttons, `D` opens
/// the options.
#[must_use]
pub fn navigation(key: &Keystroke) -> Option<Navigation> {
    match (key.key.as_str(), key.modifiers.shift) {
        ("tab", false) => Some(Navigation::FocusNext),
        ("tab", true) => Some(Navigation::FocusPrevious),
        ("d", false) => Some(Navigation::ToggleOptions),
        _ => None,
    }
}

/// Whether `key` presses the focused button (so the overlay's own use of it must wait).
#[must_use]
pub fn presses_focused_button(key: &Keystroke) -> bool {
    matches!(key.key.as_str(), "enter" | "space")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &'static str) -> Keystroke {
        Keystroke::parse(text).expect("a valid keystroke")
    }

    #[test]
    fn tab_moves_to_the_next_button() {
        assert_eq!(navigation(&key("tab")), Some(Navigation::FocusNext));
    }

    #[test]
    fn shift_tab_moves_to_the_previous_button() {
        assert_eq!(navigation(&key("shift-tab")), Some(Navigation::FocusPrevious));
    }

    #[test]
    fn d_opens_the_options() {
        assert_eq!(navigation(&key("d")), Some(Navigation::ToggleOptions));
    }

    #[test]
    fn other_keys_are_not_navigation() {
        assert_eq!(navigation(&key("x")), None);
    }

    #[test]
    fn enter_and_space_press_the_focused_button() {
        assert!(presses_focused_button(&key("enter")) && presses_focused_button(&key("space")));
    }

    #[test]
    fn letters_do_not_press_it() {
        assert!(!presses_focused_button(&key("v")));
    }
}
