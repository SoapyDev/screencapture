//! The two folders the overlay lets the user pick: where screenshots and videos are saved.

use std::path::{Path, PathBuf};

use crate::save::Config;

/// Which folder the user is picking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Folder {
    Screenshots,
    Videos,
}

impl Folder {
    /// Shift picks the videos' folder, like Shift records instead of capturing.
    pub const fn for_key(shift: bool) -> Self {
        if shift { Self::Videos } else { Self::Screenshots }
    }

    /// Where `config` saves into now.
    pub fn current(self, config: &Config) -> PathBuf {
        match self {
            Self::Screenshots => config.output_dir(None),
            Self::Videos => config.video_dir(None),
        }
    }

    /// Save into `dir` from now on.
    pub fn choose(self, config: &mut Config, dir: PathBuf) {
        match self {
            Self::Screenshots => config.output_dir = Some(dir),
            Self::Videos => config.video_dir = Some(dir),
        }
    }

    /// The picker's question.
    pub fn prompt(self, config: &Config) -> String {
        let what = match self {
            Self::Screenshots => "screenshots",
            Self::Videos => "videos",
        };
        format!("Save {what} in (now {})", self.current(config).display())
    }

    /// The button: where it saves, and its key.
    pub fn label(self, config: &Config) -> String {
        let place = shortened(&self.current(config), dirs::home_dir().as_deref());
        match self {
            Self::Screenshots => format!("Save in {place}  O"),
            Self::Videos => format!("Videos in {place}  ⇧O"),
        }
    }
}

/// `dir` with the home folder written `~`.
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

    fn config() -> Config {
        Config {
            output_dir: Some("/shots".into()),
            video_dir: Some("/clips".into()),
            ..Config::default()
        }
    }

    #[test]
    fn plain_o_picks_the_screenshots_folder() {
        assert_eq!(Folder::for_key(false), Folder::Screenshots);
    }

    #[test]
    fn shift_o_picks_the_videos_folder() {
        assert_eq!(Folder::for_key(true), Folder::Videos);
    }

    #[test]
    fn the_videos_folder_is_where_videos_go() {
        assert_eq!(Folder::Videos.current(&config()), Path::new("/clips"));
    }

    #[test]
    fn choosing_the_videos_folder_leaves_screenshots_alone() {
        let mut config = config();
        Folder::Videos.choose(&mut config, "/elsewhere".into());
        assert_eq!(config.output_dir.as_deref(), Some(Path::new("/shots")));
    }

    #[test]
    fn choosing_the_videos_folder_remembers_it() {
        let mut config = config();
        Folder::Videos.choose(&mut config, "/elsewhere".into());
        assert_eq!(config.video_dir.as_deref(), Some(Path::new("/elsewhere")));
    }

    #[test]
    fn the_prompt_says_what_is_saved() {
        assert_eq!(Folder::Videos.prompt(&config()), "Save videos in (now /clips)");
    }

    #[test]
    fn the_videos_button_shows_its_key() {
        assert_eq!(Folder::Videos.label(&config()), "Videos in /clips  ⇧O");
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
