//! Picking the folder a feature saves into, and showing it on its button.

use std::{
    any::Any,
    path::{Path, PathBuf},
};

use super::feature::{Effect, Feature};

/// Ask for the folder to save `what` in (now `dir`); `chosen` gets the folder picked, on the
/// feature that asked.
pub fn browse<F: Feature>(what: &str, dir: &Path, chosen: fn(&mut F, PathBuf)) -> Effect {
    Effect::Browse {
        prompt: prompt(what, dir),
        picked: Box::new(move |feature: &mut dyn Any, dir| {
            if let Some(feature) = feature.downcast_mut::<F>() {
                chosen(feature, dir);
            }
        }),
    }
}

/// The picker's question.
fn prompt(what: &str, dir: &Path) -> String {
    format!("Save {what} in (now {})", dir.display())
}

/// `dir` for a button: the home folder written `~`.
#[must_use]
pub fn shown(dir: &Path) -> String {
    shortened(dir, dirs::home_dir().as_deref())
}

fn shortened(dir: &Path, home: Option<&Path>) -> String {
    let under_home = home.and_then(|home| dir.strip_prefix(home).ok());
    under_home.map_or_else(
        || dir.display().to_string(),
        |rest| format!("~/{}", rest.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_says_what_is_saved_and_where_now() {
        assert_eq!(
            prompt("videos", Path::new("/clips")),
            "Save videos in (now /clips)"
        );
    }

    #[test]
    fn a_folder_under_home_is_shortened() {
        let shown = shortened(Path::new("/home/me/Videos"), Some(Path::new("/home/me")));
        assert_eq!(shown, "~/Videos");
    }

    #[test]
    fn a_folder_elsewhere_is_shown_whole() {
        let shown = shortened(Path::new("/clips"), Some(Path::new("/home/me")));
        assert_eq!(shown, "/clips");
    }
}
