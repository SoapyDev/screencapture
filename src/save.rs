//! Writing screenshots: timestamped names that never overwrite, and the remembered folder.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Local};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::config::Section;

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

/// The screenshots' section of the config.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screenshots {
    /// The folder last chosen; `~/Pictures/Screenshots` when absent.
    #[serde(default)]
    pub output_dir: Option<PathBuf>,
}

impl Section for Screenshots {}

impl Screenshots {
    /// Where to save: explicit choice, else last used folder, else `~/Pictures/Screenshots`.
    pub fn dir(&self, explicit: Option<&Path>) -> PathBuf {
        explicit
            .map(Path::to_path_buf)
            .or_else(|| self.output_dir.clone())
            .unwrap_or_else(default_output_dir)
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

pub fn default_output_dir() -> PathBuf {
    dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Screenshots")
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

    fn remembered() -> Screenshots {
        Screenshots {
            output_dir: Some("/remembered".into()),
        }
    }

    #[test]
    fn an_explicit_folder_wins() {
        let dir = remembered().dir(Some(Path::new("/explicit")));
        assert_eq!(dir, Path::new("/explicit"));
    }

    #[test]
    fn the_remembered_folder_comes_next() {
        assert_eq!(remembered().dir(None), Path::new("/remembered"));
    }

    #[test]
    fn screenshots_go_to_pictures_by_default() {
        assert_eq!(Screenshots::default().dir(None), default_output_dir());
    }

    #[test]
    fn the_folder_reads_from_the_config_file_s_key() {
        let mut config = crate::config::Config::default();
        config.set_section(&remembered());
        assert_eq!(config.section::<Screenshots>(), remembered());
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
