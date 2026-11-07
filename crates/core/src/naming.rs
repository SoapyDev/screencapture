//! Names for saved files: timestamped, sorting by time, never overwriting.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Local};

/// `2026-11-06_14-35-38.123`, as in `screenshot_2026-11-06_14-35-38.123.png`: sorts
/// chronologically, safe on every filesystem.
#[must_use]
pub fn stamp(time: DateTime<Local>) -> String {
    time.format("%Y-%m-%d_%H-%M-%S%.3f").to_string()
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

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn time() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 6, 14, 5, 9).unwrap() + chrono::Duration::milliseconds(42)
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
}
