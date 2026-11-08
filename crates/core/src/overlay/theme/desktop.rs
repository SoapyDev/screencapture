//! Where the desktop's colours are on this machine, and which source to trust first.

use std::path::{Path, PathBuf};

use super::{
    ChromeTheme, Scheme,
    choice::{ThemeChoice, ThemeSource, resolve_theme},
    gtk::{self, StyleFiles},
    kde,
    roles::Roles,
};

/// Which desktop is running, as far as colours go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    Kde,
    Other,
}

impl Desktop {
    /// From `XDG_CURRENT_DESKTOP`, a colon-separated list (`KDE`, `niri:GNOME`, `Hyprland`).
    #[must_use]
    pub fn from_current_desktop(value: &str) -> Self {
        let is_kde = value.split(':').any(|name| name.eq_ignore_ascii_case("kde"));
        if is_kde { Self::Kde } else { Self::Other }
    }
}

/// Stylesheets and settings files on the disk.
pub struct DiskFiles;

impl StyleFiles for DiskFiles {
    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir()
}

/// What the files say, most trusted first. Auto: KDE's own settings on KDE, GTK's stylesheets
/// elsewhere (where Matugen and friends write the real palette). The others hold to one file.
#[must_use]
pub fn file_sources(
    source: ThemeSource,
    desktop: Desktop,
    config_dir: &Path,
    files: &dyn StyleFiles,
) -> Vec<Roles> {
    let from_gtk = || gtk::roles(config_dir, files);
    let from_kde = || {
        let text = files.read(&config_dir.join("kdeglobals"));
        text.map_or_else(Roles::default, |text| kde::roles(&text))
    };
    match (source, desktop) {
        (ThemeSource::Gtk, _) => vec![from_gtk()],
        (ThemeSource::Kde, _) => vec![from_kde()],
        (ThemeSource::Portal | ThemeSource::BuiltIn, _) => Vec::new(),
        (ThemeSource::Auto, Desktop::Kde) => vec![from_kde(), from_gtk()],
        (ThemeSource::Auto, Desktop::Other) => vec![from_gtk(), from_kde()],
    }
}

/// What is known of the desktop's colours: read once, kept for as long as the program runs.
/// Light or dark is not part of it: that one changes while the program runs, and is free.
#[derive(Clone)]
pub struct DesktopColours {
    choice: ThemeChoice,
    sources: Vec<Roles>,
}

impl DesktopColours {
    #[must_use]
    pub const fn new(choice: ThemeChoice, sources: Vec<Roles>) -> Self {
        Self { choice, sources }
    }

    /// The theme these colours make, with the desktop at `scheme`.
    #[must_use]
    pub fn theme(&self, scheme: Scheme) -> ChromeTheme {
        resolve_theme(&self.choice, scheme, &self.sources)
    }

    /// Whether asking the desktop portal for an accent is worth it: the source allows it and
    /// no file and no config gave one.
    #[must_use]
    pub fn wants_portal_accent(&self) -> bool {
        let allowed = matches!(self.choice.theme_source, ThemeSource::Auto | ThemeSource::Portal);
        let given =
            self.choice.theme_accent.is_some() || self.sources.iter().any(|roles| roles.accent.is_some());
        allowed && !given
    }

    /// These colours, with `accent` as the last resort for the accent.
    #[must_use]
    pub fn with_fallback_accent(&self, accent: gpui::Hsla) -> Self {
        let mut sources = self.sources.clone();
        sources.push(Roles {
            accent: Some(accent),
            ..Roles::default()
        });
        Self {
            choice: self.choice.clone(),
            sources,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::*;

    struct Fake(HashMap<PathBuf, String>);

    impl StyleFiles for Fake {
        fn read(&self, path: &Path) -> Option<String> {
            self.0.get(path).cloned()
        }
    }

    fn files(entries: &[(&str, &str)]) -> Fake {
        Fake(
            entries
                .iter()
                .map(|(path, text)| (PathBuf::from(path), (*text).to_owned()))
                .collect(),
        )
    }

    const BOTH: [(&str, &str); 2] = [
        ("/c/gtk-4.0/gtk.css", "@define-color accent_bg_color #ffffff;"),
        ("/c/kdeglobals", "[General]\nAccentColor=0,0,0\n"),
    ];

    #[test]
    fn kde_is_recognised_in_a_list_of_desktops() {
        assert_eq!(Desktop::from_current_desktop("KDE"), Desktop::Kde);
    }

    #[test]
    fn niri_with_gnome_is_not_kde() {
        assert_eq!(Desktop::from_current_desktop("niri:GNOME"), Desktop::Other);
    }

    #[test]
    fn on_kde_the_kde_file_comes_first() {
        let sources = file_sources(ThemeSource::Auto, Desktop::Kde, Path::new("/c"), &files(&BOTH));
        assert_eq!(sources[0].accent, Some(gpui::black()));
    }

    #[test]
    fn elsewhere_the_gtk_stylesheet_comes_first() {
        let sources = file_sources(ThemeSource::Auto, Desktop::Other, Path::new("/c"), &files(&BOTH));
        assert_eq!(sources[0].accent, Some(gpui::white()));
    }

    #[test]
    fn no_files_means_the_portal_is_worth_asking_for_an_accent() {
        let colours = DesktopColours::new(ThemeChoice::default(), Vec::new());
        assert!(colours.wants_portal_accent());
    }

    #[test]
    fn an_accent_from_a_file_means_the_portal_is_not_asked() {
        let sources = file_sources(ThemeSource::Auto, Desktop::Other, Path::new("/c"), &files(&BOTH));
        let colours = DesktopColours::new(ThemeChoice::default(), sources);
        assert!(!colours.wants_portal_accent());
    }

    #[test]
    fn a_configured_accent_means_the_portal_is_not_asked() {
        let choice = ThemeChoice {
            theme_accent: Some("#ff0000".into()),
            ..ThemeChoice::default()
        };
        assert!(!DesktopColours::new(choice, Vec::new()).wants_portal_accent());
    }

    #[test]
    fn the_portal_accent_is_used_when_nothing_else_gave_one() {
        let colours = DesktopColours::new(ThemeChoice::default(), Vec::new());
        let theme = colours.with_fallback_accent(gpui::white()).theme(Scheme::Dark);
        assert_eq!(theme.accent, gpui::white());
    }

    fn only(source: ThemeSource) -> Vec<Roles> {
        file_sources(source, Desktop::Other, Path::new("/c"), &files(&BOTH))
    }

    #[test]
    fn choosing_gtk_reads_only_the_stylesheet() {
        assert_eq!(only(ThemeSource::Gtk).len(), 1);
    }

    #[test]
    fn choosing_gtk_gives_the_stylesheet_accent() {
        assert_eq!(only(ThemeSource::Gtk)[0].accent, Some(gpui::white()));
    }

    #[test]
    fn choosing_kde_gives_the_kde_accent_even_off_kde() {
        assert_eq!(only(ThemeSource::Kde)[0].accent, Some(gpui::black()));
    }

    #[test]
    fn choosing_built_in_reads_no_files() {
        assert_eq!(only(ThemeSource::BuiltIn), Vec::<Roles>::new());
    }

    #[test]
    fn choosing_the_portal_reads_no_files() {
        assert_eq!(only(ThemeSource::Portal), Vec::<Roles>::new());
    }

    #[test]
    fn choosing_built_in_never_asks_the_portal() {
        let choice = ThemeChoice {
            theme_source: ThemeSource::BuiltIn,
            ..ThemeChoice::default()
        };
        assert!(!DesktopColours::new(choice, Vec::new()).wants_portal_accent());
    }

    #[test]
    fn choosing_gtk_never_asks_the_portal() {
        let choice = ThemeChoice {
            theme_source: ThemeSource::Gtk,
            ..ThemeChoice::default()
        };
        assert!(!DesktopColours::new(choice, Vec::new()).wants_portal_accent());
    }
}
