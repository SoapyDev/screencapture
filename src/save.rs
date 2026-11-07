//! Writing screenshots: timestamped names that never overwrite, and the remembered folder.

use std::path::{Path, PathBuf};

use crate::annotate::{Color, PASTELS, parse_color};
use anyhow::Result;
use chrono::{DateTime, Local};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

/// `2026-11-06_14-35-38.123`, as in `screenshot_2026-11-06_14-35-38.123.png`: sorts
/// chronologically, safe on every filesystem.
fn stamp(time: DateTime<Local>) -> String {
    time.format("%Y-%m-%d_%H-%M-%S%.3f").to_string()
}

/// Encode `image` as PNG into `out`, and report errors up to the last byte (buffered writes
/// included).
pub fn write_png(image: &RgbaImage, out: impl std::io::Write) -> Result<()> {
    use image::{
        ImageEncoder,
        codecs::png::{CompressionType, FilterType, PngEncoder},
    };
    let mut writer = std::io::BufWriter::new(out);
    // Fast compression: a few percent bigger files, several times quicker to write.
    PngEncoder::new_with_quality(&mut writer, CompressionType::Fast, FilterType::Adaptive).write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    // Dropping a BufWriter would swallow this last write's error.
    writer.into_inner().map_err(std::io::IntoInnerError::into_error)?;
    Ok(())
}

/// A written screenshot: where, and the PNG bytes themselves (for the clipboard).
pub struct Saved {
    pub path: PathBuf,
    pub png: Vec<u8>,
}

/// Encode `image` as PNG into `dir` (created if missing) under a timestamped name. If that name
/// is taken, `_1`, `_2`, ... is appended; existing files are never overwritten.
pub fn save_png(image: &RgbaImage, dir: &Path, time: DateTime<Local>) -> Result<Saved> {
    use std::io::Write;

    // Encoded once, in memory: the same bytes go to the file and the clipboard.
    let mut png = Vec::with_capacity(image.as_raw().len() / 4);
    write_png(image, &mut png)?;
    let (path, file) = reserve(dir, "screenshot", "png", time)?;

    let mut file = file;
    if let Err(e) = file.write_all(&png) {
        // Never leave a truncated screenshot behind.
        let _ = std::fs::remove_file(&path);
        return Err(anyhow::Error::from(e).context(format!("writing {}", path.display())));
    }
    Ok(Saved { path, png })
}

/// Settings remembered between runs.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub output_dir: Option<PathBuf>,
    /// Drawing colours as `#RRGGBB` / `#RRGGBBAA`; the built-in pastels when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<Vec<String>>,
    /// Last colour chosen in the picker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_color: Option<String>,
    /// Where recordings go; `~/Videos/Screencapture` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_dir: Option<PathBuf>,
    #[serde(default)]
    pub record: crate::record::settings::Settings,
}

impl Config {
    /// The drawing palette: the configured colours that parse, else the built-in pastels.
    pub fn colors(&self) -> Vec<Color> {
        let parsed: Vec<Color> = self
            .palette
            .iter()
            .flatten()
            .filter_map(|c| parse_color(c))
            .collect();
        if parsed.is_empty() {
            PASTELS.to_vec()
        } else {
            parsed
        }
    }

    pub fn custom_color(&self) -> Option<Color> {
        self.custom_color.as_deref().and_then(parse_color)
    }

    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("screencapture").join("config.json"))
    }

    /// Missing or unreadable config falls back to defaults: a broken file must not block a capture.
    pub fn load_from(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Replace the file at `path` whole: written beside it, then renamed over it, so a reader
    /// (or a crash) never finds it half-written.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let written = beside(path);
        std::fs::write(&written, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&written, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&written);
        })?;
        Ok(())
    }

    pub fn load() -> Self {
        Self::path().map_or_default(|p| Self::load_from(&p))
    }

    pub fn save(&self) -> Result<()> {
        Self::path().map_or(Ok(()), |p| self.save_to(&p))
    }

    /// Where to save: explicit choice, else last used folder, else `~/Pictures/Screenshots`.
    pub fn output_dir(&self, explicit: Option<&Path>) -> PathBuf {
        explicit
            .map(Path::to_path_buf)
            .or_else(|| self.output_dir.clone())
            .unwrap_or_else(default_output_dir)
    }

    /// Where to save recordings: explicit choice, else remembered, else `~/Videos/Screencapture`.
    pub fn video_dir(&self, explicit: Option<&Path>) -> PathBuf {
        explicit
            .map(Path::to_path_buf)
            .or_else(|| self.video_dir.clone())
            .unwrap_or_else(default_video_dir)
    }
}

/// Create a new, empty file in `dir` (created if missing) named `<prefix>_<timestamp>.<ext>`,
/// with `_1`, `_2`, ... appended if taken. Existing files are never touched.
pub fn reserve(
    dir: &Path,
    prefix: &str,
    ext: &str,
    time: DateTime<Local>,
) -> Result<(PathBuf, std::fs::File)> {
    use std::{fs::OpenOptions, io::ErrorKind};

    use anyhow::Context;

    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let base = format!("{prefix}_{}", stamp(time));
    (0_u32..)
        .map(|n| match n {
            0 => dir.join(format!("{base}.{ext}")),
            n => dir.join(format!("{base}_{n}.{ext}")),
        })
        .find_map(
            |path| match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => Some(Ok((path, file))),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => None,
                Err(e) => Some(Err(e).with_context(|| format!("creating {}", path.display()))),
            },
        )
        .expect("unbounded name search")
}

pub fn default_video_dir() -> PathBuf {
    dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Screencapture")
}

pub fn default_output_dir() -> PathBuf {
    dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Screenshots")
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
    use super::*;
    use chrono::TimeZone;

    fn time() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 6, 14, 5, 9).unwrap() + chrono::Duration::milliseconds(42)
    }

    fn image() -> RgbaImage {
        RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]))
    }

    #[test]
    fn saves_decodable_png_under_timestamp_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_png(&image(), dir.path(), time()).unwrap().path;
        assert_eq!(path, dir.path().join("screenshot_2026-11-06_14-05-09.042.png"));
        assert_eq!(image::open(&path).unwrap().to_rgba8(), image());
    }

    #[test]
    fn saving_returns_the_encoded_png_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let saved = save_png(&image(), dir.path(), time()).unwrap();
        assert_eq!(std::fs::read(&saved.path).unwrap(), saved.png);
        assert_eq!(image::load_from_memory(&saved.png).unwrap().to_rgba8(), image());
    }

    #[test]
    fn never_overwrites_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let first = save_png(&image(), dir.path(), time()).unwrap().path;
        let second = save_png(&image(), dir.path(), time()).unwrap().path;
        let third = save_png(&image(), dir.path(), time()).unwrap().path;
        assert_eq!(
            second.file_name().unwrap(),
            "screenshot_2026-11-06_14-05-09.042_1.png"
        );
        assert_eq!(
            third.file_name().unwrap(),
            "screenshot_2026-11-06_14-05-09.042_2.png"
        );
        assert!(first.exists());
    }

    #[test]
    fn creates_missing_folders() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        assert!(
            save_png(&image(), &nested, time())
                .unwrap()
                .path
                .starts_with(&nested)
        );
    }

    #[test]
    fn reserved_names_carry_their_prefix_and_extension_and_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let (first, _) = reserve(dir.path(), "screencast", "mp4", time()).unwrap();
        let (second, _) = reserve(dir.path(), "screencast", "mp4", time()).unwrap();
        assert_eq!(
            first.file_name().unwrap(),
            "screencast_2026-11-06_14-05-09.042.mp4"
        );
        assert_eq!(
            second.file_name().unwrap(),
            "screencast_2026-11-06_14-05-09.042_1.mp4"
        );
        assert!(first.exists() && second.exists(), "reserved on disk");
    }

    #[test]
    fn videos_have_their_own_folder() {
        let config = Config {
            output_dir: Some("/shots".into()),
            ..Config::default()
        };
        assert_eq!(config.video_dir(None), default_video_dir());
        assert!(
            default_video_dir().ends_with("Videos/Screencapture")
                || default_video_dir().ends_with("Screencapture")
        );
        let config = Config {
            video_dir: Some("/clips".into()),
            ..config
        };
        assert_eq!(config.video_dir(None), Path::new("/clips"));
        assert_eq!(config.video_dir(Some(Path::new("/now"))), Path::new("/now"));
    }

    #[test]
    fn recording_settings_are_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let config = Config {
            record: crate::record::settings::Settings {
                fps: crate::record::settings::Fps::F30,
                ..Default::default()
            },
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), config);
    }

    #[test]
    fn config_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.json");
        let config = Config {
            output_dir: Some("/tmp/shots".into()),
            palette: Some(vec!["#123456".into()]),
            custom_color: Some("#abcdef".into()),
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), config);
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
        let config = Config {
            output_dir: Some("/shots".into()),
            ..Config::default()
        };
        config.save_to(&path).expect("the folder is writable");
        let saving = saving_over_and_over(config.clone(), path.clone());
        let mut broken = 0;
        while !saving.is_finished() {
            broken += usize::from(Config::load_from(&path) != config);
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

    #[test]
    fn missing_or_corrupt_config_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(Config::load_from(&path), Config::default());
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(Config::load_from(&path), Config::default());
    }

    #[test]
    fn output_dir_precedence() {
        let remembered = Config {
            output_dir: Some("/remembered".into()),
            ..Config::default()
        };
        assert_eq!(
            remembered.output_dir(Some(Path::new("/explicit"))),
            Path::new("/explicit")
        );
        assert_eq!(remembered.output_dir(None), Path::new("/remembered"));
        assert_eq!(Config::default().output_dir(None), default_output_dir());
    }

    #[test]
    fn palette_comes_from_config_with_pastel_fallback() {
        assert_eq!(Config::default().colors(), PASTELS.to_vec());
        let custom = Config {
            palette: Some(vec!["#112233".into(), "nonsense".into(), "44556680".into()]),
            ..Config::default()
        };
        assert_eq!(
            custom.colors(),
            vec![[0x11, 0x22, 0x33, 0xFF], [0x44, 0x55, 0x66, 0x80]]
        );
        let broken = Config {
            palette: Some(vec!["nope".into()]),
            ..Config::default()
        };
        assert_eq!(broken.colors(), PASTELS.to_vec());
    }

    #[test]
    fn palette_and_custom_colour_load_from_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r##"{"output_dir":null,"palette":["#ff0000"],"custom_color":"#00ff00"}"##,
        )
        .unwrap();
        let config = Config::load_from(&path);
        assert_eq!(config.colors(), vec![[255, 0, 0, 255]]);
        assert_eq!(config.custom_color(), Some([0, 255, 0, 255]));
        // Older files without the new keys still load.
        std::fs::write(&path, r#"{"output_dir":"/x"}"#).unwrap();
        assert_eq!(Config::load_from(&path).output_dir, Some("/x".into()));
    }

    /// Takes nothing: like a full disk.
    struct Full;
    impl std::io::Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_errors_are_reported_even_when_buffered() {
        let image = RgbaImage::new(4, 4);
        assert!(write_png(&image, Full).is_err());
        let mut ok = Vec::new();
        write_png(&image, &mut ok).unwrap();
        assert!(image::load_from_memory(&ok).is_ok());
    }

    #[test]
    fn a_failed_save_leaves_no_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        // PNG has no zero-sized images: the encoder refuses.
        assert!(save_png(&RgbaImage::new(0, 0), dir.path(), Local::now()).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
