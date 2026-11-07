//! Settings remembered between runs, in one file. Each feature keeps its own section (its own
//! top-level keys) and reads and writes only that, so a feature left out of the build leaves
//! its settings alone, and a broken section costs only its own settings.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

/// One feature's part of the config: its fields are top-level keys of the file. `None` fields
/// are written as `null`, which removes the key.
pub trait Section: Serialize + DeserializeOwned + Default {}

/// The whole file: every section's keys, including those of features not built in.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Config {
    keys: Map<String, Value>,
}

impl Config {
    /// `S`'s settings, or its defaults when they are missing or don't make sense.
    pub fn section<S: Section>(&self) -> S {
        S::deserialize(&self.keys).unwrap_or_default()
    }

    /// Replace `S`'s settings, leaving every other key as it was.
    pub fn set_section<S: Section>(&mut self, section: &S) {
        let Ok(Value::Object(keys)) = serde_json::to_value(section) else {
            return;
        };
        for (key, value) in keys {
            if value.is_null() {
                self.keys.remove(&key);
            } else {
                self.keys.insert(key, value);
            }
        }
    }

    /// `~/.config/screencapture/config.json`.
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("screencapture").join("config.json"))
    }

    /// A missing or unreadable file reads as empty: a broken config must not block a capture.
    pub fn load_from(path: &Path) -> Self {
        let read = std::fs::read(path).ok();
        let keys = read.and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Self {
            keys: keys.unwrap_or_default(),
        }
    }

    /// Replace the file at `path` whole: written beside it, then renamed over it, so a reader
    /// (or a crash) never finds it half-written.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let written = beside(path);
        std::fs::write(&written, serde_json::to_vec_pretty(&self.keys)?)?;
        std::fs::rename(&written, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&written);
        })?;
        Ok(())
    }

    pub fn load() -> Self {
        Self::path().map_or_else(Self::default, |path| Self::load_from(&path))
    }

    pub fn save(&self) -> Result<()> {
        Self::path().map_or(Ok(()), |path| self.save_to(&path))
    }
}

/// Remember `section` in the config file (~20 µs), keeping everything else in it.
pub fn remember<S: Section>(section: &S) -> Result<()> {
    let mut config = Config::load();
    config.set_section(section);
    config.save()
}

/// A new file name next to `path`, for one save: saves at the same time never share one.
fn beside(path: &Path) -> PathBuf {
    static SAVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let save = SAVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{save}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct Fruit {
        #[serde(default)]
        apple: Option<String>,
    }
    impl Section for Fruit {}

    #[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct Count {
        #[serde(default)]
        count: u32,
    }
    impl Section for Count {}

    fn config(json: &str) -> Config {
        Config {
            keys: serde_json::from_str(json).expect("the test's JSON is valid"),
        }
    }

    #[test]
    fn a_section_reads_its_own_keys() {
        let apple = config(r#"{"apple":"red","count":3}"#).section::<Fruit>().apple;
        assert_eq!(apple.as_deref(), Some("red"));
    }

    #[test]
    fn a_missing_section_is_its_default() {
        assert_eq!(Config::default().section::<Count>(), Count::default());
    }

    #[test]
    fn a_broken_section_is_its_default() {
        assert_eq!(config(r#"{"count":"lots"}"#).section::<Count>(), Count::default());
    }

    #[test]
    fn a_broken_section_leaves_the_others_readable() {
        let apple = config(r#"{"apple":"red","count":"lots"}"#)
            .section::<Fruit>()
            .apple;
        assert_eq!(apple.as_deref(), Some("red"));
    }

    #[test]
    fn setting_a_section_keeps_the_others() {
        let mut config = config(r#"{"apple":"red"}"#);
        config.set_section(&Count { count: 2 });
        assert_eq!(config.section::<Fruit>().apple.as_deref(), Some("red"));
    }

    #[test]
    fn setting_a_section_keeps_unknown_keys() {
        let mut config = config(r#"{"future":true}"#);
        config.set_section(&Count { count: 2 });
        assert_eq!(config.keys.get("future"), Some(&Value::Bool(true)));
    }

    #[test]
    fn a_field_set_to_none_drops_its_key() {
        let mut config = config(r#"{"apple":"red"}"#);
        config.set_section(&Fruit { apple: None });
        assert!(!config.keys.contains_key("apple"));
    }

    #[test]
    fn a_saved_config_loads_back() {
        let dir = tempfile::tempdir().expect("the system has a temporary folder");
        let path = dir.path().join("sub/config.json");
        let saved = config(r#"{"apple":"red","count":3}"#);
        saved.save_to(&path).expect("the folder is writable");
        assert_eq!(Config::load_from(&path), saved);
    }

    #[test]
    fn a_missing_file_is_an_empty_config() {
        let dir = tempfile::tempdir().expect("the system has a temporary folder");
        assert_eq!(
            Config::load_from(&dir.path().join("none.json")),
            Config::default()
        );
    }

    #[test]
    fn a_corrupt_file_is_an_empty_config() {
        let dir = tempfile::tempdir().expect("the system has a temporary folder");
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{not json").expect("the folder is writable");
        assert_eq!(Config::load_from(&path), Config::default());
    }

    /// Saves of `config` to `path`, many times over, on another thread.
    fn saving_over_and_over(config: Config, path: PathBuf) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            for _ in 0..500 {
                config.save_to(&path).expect("the folder is writable");
            }
        })
    }

    #[test]
    fn a_config_being_saved_is_never_read_half_written() {
        let dir = tempfile::tempdir().expect("the system has a temporary folder");
        let path = dir.path().join("config.json");
        let saved = config(r#"{"apple":"red"}"#);
        saved.save_to(&path).expect("the folder is writable");
        let saving = saving_over_and_over(saved.clone(), path.clone());
        let mut broken = 0;
        while !saving.is_finished() {
            broken += usize::from(Config::load_from(&path) != saved);
        }
        assert_eq!(broken, 0, "reads that found no config");
    }

    #[test]
    fn saving_a_config_leaves_only_the_config() {
        let dir = tempfile::tempdir().expect("the system has a temporary folder");
        let path = dir.path().join("config.json");
        Config::default().save_to(&path).expect("the folder is writable");
        let files = std::fs::read_dir(dir.path()).expect("the folder exists").count();
        assert_eq!(files, 1);
    }
}
