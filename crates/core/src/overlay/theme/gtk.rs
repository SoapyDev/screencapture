//! The desktop's colours as GTK stylesheets carry them: Adwaita's named colours, written by
//! Matugen, kde-gtk-config, GNOME and others into `gtk-4.0/gtk.css` and what it imports.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use gpui::Hsla;

use super::{
    css::{self, Sheet},
    roles::Roles,
};

/// Where stylesheets are read from: the disk, or a stand-in in tests.
pub trait StyleFiles {
    fn read(&self, path: &Path) -> Option<String>;
}

/// The most files followed from one stylesheet: a loop of imports ends at the visited set, a
/// very long chain here.
const MOST_FILES: usize = 16;

/// The roles `config_dir`'s GTK 4 stylesheet gives, with GTK 3's for what it leaves out.
#[must_use]
pub fn roles(config_dir: &Path, files: &dyn StyleFiles) -> Roles {
    let from = |folder: &str| roles_of(&config_dir.join(folder).join("gtk.css"), files);
    from("gtk-4.0").or(from("gtk-3.0"))
}

fn roles_of(entry: &Path, files: &dyn StyleFiles) -> Roles {
    let colours = css::resolve(&definitions_in_order(entry, files));
    let first = |names: &[&str]| names.iter().find_map(|name| colours.get(*name).copied());
    Roles {
        surface: first(&["window_bg_color", "theme_bg_color"]),
        on_surface: first(&["window_fg_color", "theme_fg_color"]),
        accent: first(&["accent_bg_color", "accent_color", "theme_selected_bg_color"]),
        danger: first(&["error_color", "error_bg_color", "destructive_color"]),
    }
}

/// Every definition reachable from `entry`, imports first (so the importing file wins).
fn definitions_in_order(entry: &Path, files: &dyn StyleFiles) -> Vec<(String, String)> {
    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    let mut pending = vec![Step::Enter(entry.to_path_buf())];
    while let Some(step) = pending.pop() {
        match step {
            Step::Enter(path) => enter(&path, files, &mut visited, &mut pending),
            Step::Emit(sheet) => ordered.extend(sheet.definitions),
        }
    }
    ordered
}

enum Step {
    Enter(PathBuf),
    Emit(Sheet),
}

fn enter(path: &Path, files: &dyn StyleFiles, visited: &mut HashSet<PathBuf>, pending: &mut Vec<Step>) {
    if visited.len() >= MOST_FILES || !visited.insert(path.to_path_buf()) {
        return;
    }
    let Some(text) = files.read(path) else {
        return;
    };
    let sheet = css::read_sheet(&text);
    let folder = path.parent().unwrap_or_else(|| Path::new(""));
    let imports = sheet.imports.iter().map(|import| folder.join(import)).rev();
    // Pushed reversed: the first import comes off the stack first, the file's own last.
    let entries: Vec<_> = imports.map(Step::Enter).collect();
    pending.push(Step::Emit(sheet));
    pending.extend(entries);
}

/// The colour of a named GTK colour, for tests and callers that want one.
#[must_use]
pub fn named_colour(config_dir: &Path, files: &dyn StyleFiles, name: &str) -> Option<Hsla> {
    let entry = config_dir.join("gtk-4.0").join("gtk.css");
    css::resolve(&definitions_in_order(&entry, files))
        .get(name)
        .copied()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct Fake(HashMap<PathBuf, String>);

    impl Fake {
        fn with(files: &[(&str, &str)]) -> Self {
            Self(
                files
                    .iter()
                    .map(|(path, text)| (PathBuf::from(path), (*text).to_owned()))
                    .collect(),
            )
        }
    }

    impl StyleFiles for Fake {
        fn read(&self, path: &Path) -> Option<String> {
            self.0.get(path).cloned()
        }
    }

    const MATUGEN: &str = "@define-color window_bg_color #1e1e2e;\n\
        @define-color window_fg_color #cdd6f4;\n\
        @define-color accent_bg_color #cba6f7;\n\
        @define-color error_bg_color #f38ba8;\n\
        @define-color headerbar_backdrop_color @window_bg_color;";

    fn dank() -> Fake {
        Fake::with(&[
            ("/c/gtk-4.0/gtk.css", "@import url(\"dank-colors.css\");"),
            ("/c/gtk-4.0/dank-colors.css", MATUGEN),
        ])
    }

    #[test]
    fn the_accent_comes_through_an_import() {
        let given = roles(Path::new("/c"), &dank());
        assert_eq!(given.accent, css::evaluate("#cba6f7", &|_| None));
    }

    #[test]
    fn the_surface_is_the_window_background() {
        let given = roles(Path::new("/c"), &dank());
        assert_eq!(given.surface, css::evaluate("#1e1e2e", &|_| None));
    }

    #[test]
    fn danger_falls_back_to_the_error_background() {
        let given = roles(Path::new("/c"), &dank());
        assert_eq!(given.danger, css::evaluate("#f38ba8", &|_| None));
    }

    #[test]
    fn an_alias_in_an_imported_file_resolves() {
        let colour = named_colour(Path::new("/c"), &dank(), "headerbar_backdrop_color");
        assert_eq!(colour, css::evaluate("#1e1e2e", &|_| None));
    }

    #[test]
    fn the_importing_file_overrides_what_it_imports() {
        let files = Fake::with(&[
            (
                "/c/gtk-4.0/gtk.css",
                "@import \"a.css\";\n@define-color accent_bg_color #ffffff;",
            ),
            ("/c/gtk-4.0/a.css", "@define-color accent_bg_color #000000;"),
        ]);
        assert_eq!(roles(Path::new("/c"), &files).accent, Some(gpui::white()));
    }

    #[test]
    fn a_loop_of_imports_ends() {
        let files = Fake::with(&[
            ("/c/gtk-4.0/gtk.css", "@import \"a.css\";"),
            (
                "/c/gtk-4.0/a.css",
                "@import \"gtk.css\";\n@define-color accent_bg_color #ffffff;",
            ),
        ]);
        assert_eq!(roles(Path::new("/c"), &files).accent, Some(gpui::white()));
    }

    #[test]
    fn gtk_3_fills_in_what_gtk_4_leaves_out() {
        let files = Fake::with(&[
            ("/c/gtk-4.0/gtk.css", "@define-color accent_bg_color #ffffff;"),
            ("/c/gtk-3.0/gtk.css", "@define-color window_bg_color #000000;"),
        ]);
        assert_eq!(roles(Path::new("/c"), &files).surface, Some(gpui::black()));
    }

    #[test]
    fn no_stylesheet_gives_no_roles() {
        assert!(roles(Path::new("/c"), &Fake::with(&[])).is_empty());
    }
}
