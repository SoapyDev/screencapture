//! Writing screenshots: timestamped names that never overwrite, and the remembered folder.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Local};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

use screencapture_core::config::Section;

/// Encode `image` as PNG into `out`, and report errors up to the last byte (buffered writes
/// included).
///
/// # Errors
///
/// When encoding fails, or `out` refuses a write.
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
///
/// # Errors
///
/// When encoding fails, or the folder or file can't be created or written.
pub fn save_png(image: &RgbaImage, dir: &Path, time: DateTime<Local>) -> Result<Saved> {
    use std::io::Write;

    // Encoded once, in memory: the same bytes go to the file and the clipboard.
    let mut png = Vec::with_capacity(image.as_raw().len() / 4);
    write_png(image, &mut png)?;
    let (path, file) = screencapture_core::naming::reserve(dir, "screenshot", "png", time)?;

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
        let mut config = screencapture_core::config::Config::default();
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
