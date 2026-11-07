//! What a key does while typing in a text box, independent of gpui.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextEdit {
    /// Stop typing (the box stays, unless empty).
    Done,
    Newline,
    Backspace,
    DeleteWord,
    Undo,
    Type(String),
    /// Used up without effect, so it can't trigger an overlay shortcut.
    Swallow,
}

/// Modifiers that matter here.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub control: bool,
    pub alt: bool,
    pub platform: bool,
}

/// `key` is gpui's key name, `typed` what it would insert (`Keystroke::key_char`).
pub fn text_edit(key: &str, typed: Option<&str>, mods: Mods) -> TextEdit {
    match key {
        "z" if mods.control => return TextEdit::Undo,
        "backspace" if mods.control => return TextEdit::DeleteWord,
        _ if mods.control || mods.alt || mods.platform => return TextEdit::Swallow,
        "escape" => return TextEdit::Done,
        "enter" => return TextEdit::Newline,
        "backspace" => return TextEdit::Backspace,
        _ => {}
    }
    match typed {
        Some(text) if !text.is_empty() && !text.chars().any(char::is_control) => TextEdit::Type(text.into()),
        _ => TextEdit::Swallow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods {
        control: false,
        alt: false,
        platform: false,
    };
    const CTRL: Mods = Mods {
        control: true,
        alt: false,
        platform: false,
    };

    #[test]
    fn plain_keys_edit() {
        assert_eq!(text_edit("escape", None, NONE), TextEdit::Done);
        assert_eq!(text_edit("enter", None, NONE), TextEdit::Newline);
        assert_eq!(text_edit("backspace", None, NONE), TextEdit::Backspace);
        assert_eq!(text_edit("a", Some("a"), NONE), TextEdit::Type("a".into()));
        assert_eq!(
            text_edit("a", Some("A"), NONE),
            TextEdit::Type("A".into()),
            "shifted"
        );
        assert_eq!(text_edit("space", Some(" "), NONE), TextEdit::Type(" ".into()));
        assert_eq!(text_edit("é", Some("é"), NONE), TextEdit::Type("é".into()));
    }

    #[test]
    fn keys_that_type_nothing_are_swallowed() {
        for key in ["left", "f5", "delete", "tab", "shift"] {
            assert_eq!(text_edit(key, None, NONE), TextEdit::Swallow, "{key}");
        }
        assert_eq!(text_edit("tab", Some("\t"), NONE), TextEdit::Swallow);
    }

    #[test]
    fn shortcuts_never_leak_while_typing() {
        assert_eq!(text_edit("z", Some("z"), CTRL), TextEdit::Undo);
        assert_eq!(text_edit("backspace", None, CTRL), TextEdit::DeleteWord);
        for key in ["a", "s", "r", "c", "enter", "delete"] {
            assert_eq!(text_edit(key, Some(key), CTRL), TextEdit::Swallow, "ctrl+{key}");
        }
        let alt = Mods { alt: true, ..NONE };
        assert_eq!(text_edit("a", Some("a"), alt), TextEdit::Swallow);
        let sup = Mods {
            platform: true,
            ..NONE
        };
        assert_eq!(text_edit("s", Some("s"), sup), TextEdit::Swallow);
    }
}
