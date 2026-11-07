//! End-to-end runs of the real binary against the live desktop:
//! `cargo test -- --ignored` inside a Wayland (wlroots) session.

use std::{path::Path, process::Command};

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_screencapture"));
    // Keep the Wayland socket reachable by absolute path, but hide any daemon the user runs
    // and never touch the user's remembered folder.
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap();
    let display = std::env::var("WAYLAND_DISPLAY").unwrap();
    cmd.env("WAYLAND_DISPLAY", Path::new(&runtime).join(display));
    cmd.env("XDG_RUNTIME_DIR", tempfile::tempdir().unwrap().keep());
    cmd.env("XDG_CONFIG_HOME", tempfile::tempdir().unwrap().keep());
    cmd.env_remove("HYPRLAND_INSTANCE_SIGNATURE");
    cmd.env("SCREENCAPTURE_NO_CLIPBOARD", "1");
    cmd
}

fn mean_brightness(path: &Path) -> f64 {
    let img = image::open(path).unwrap().to_rgba8();
    let sum: u64 = img
        .pixels()
        .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
        .sum();
    sum as f64 / f64::from(img.width() * img.height() * 3)
}

fn saved_path(out: &std::process::Output) -> std::path::PathBuf {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap().trim().into()
}

#[test]
#[ignore = "needs a graphical session; briefly shows the overlay"]
fn overlay_capture_does_not_contain_the_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let geometry = "40,40 400x300";
    let headless = saved_path(
        &bin()
            .args(["region", "-g", geometry, "-o"])
            .arg(dir.path())
            .output()
            .unwrap(),
    );
    let through_overlay = saved_path(
        &bin()
            .args(["region", "-o"])
            .arg(dir.path())
            .env("SCREENCAPTURE_E2E_SELECT", geometry)
            .output()
            .unwrap(),
    );
    assert_ne!(headless, through_overlay, "timestamped names must differ");
    let (a, b) = (mean_brightness(&headless), mean_brightness(&through_overlay));
    // The overlay darkens by 45%; a leak would show as a big drop.
    assert!(
        (a - b).abs() / a.max(1.0) < 0.1,
        "headless {a:.1} vs overlay {b:.1}"
    );
    assert_eq!(image::image_dimensions(&through_overlay).unwrap(), (400, 300));
}

#[test]
#[ignore = "needs a graphical session; briefly shows the overlay"]
fn drawings_are_saved_in_the_capture() {
    let dir = tempfile::tempdir().unwrap();
    let path = saved_path(
        &bin()
            .args(["region", "-o"])
            .arg(dir.path())
            .env("SCREENCAPTURE_E2E_SELECT", "40,40 400x300")
            .env("SCREENCAPTURE_E2E_LINE", "1")
            .output()
            .unwrap(),
    );
    let img = image::open(&path).unwrap().to_rgba8();
    // The hook draws a line across the middle in the first palette colour.
    let first = screencapture_first_colour();
    let hits = (20..380).filter(|&x| img.get_pixel(x, 150).0 == first).count();
    assert!(hits > 300, "line found on {hits} of 360 pixels");
}

/// The built-in palette's first colour (soft red), as the config is isolated.
const fn screencapture_first_colour() -> [u8; 4] {
    [0xFF, 0x8A, 0x80, 0xFF]
}

#[test]
#[ignore = "needs a graphical session; briefly shows the overlay"]
fn redaction_and_text_are_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = saved_path(
        &bin()
            .args(["region", "-o"])
            .arg(dir.path())
            .env("SCREENCAPTURE_E2E_SELECT", "40,40 400x300")
            .env("SCREENCAPTURE_E2E_EXTRAS", "1")
            .output()
            .unwrap(),
    );
    let img = image::open(&path).unwrap().to_rgba8();
    // Left quarter: solid black, every pixel.
    for y in 0..300 {
        for x in 0..100 {
            assert_eq!(img.get_pixel(x, y).0, [0, 0, 0, 255], "({x}, {y}) not redacted");
        }
    }
    // Top-right quarter: the default dark fill, with soft-red letters on it.
    let fill = [0x1E, 0x1E, 0x22, 0xFF];
    assert_eq!(img.get_pixel(398, 148).0, fill);
    let letters = (200..400)
        .flat_map(|x| (0..150).map(move |y| (x, y)))
        .filter(|&(x, y)| img.get_pixel(x, y).0 == screencapture_first_colour())
        .count();
    assert!(letters > 20, "{letters} letter pixels");
}
