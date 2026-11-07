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

/// A private runtime folder (for the socket) and config (videos go to `videos`), so a test's
/// recording never meets the user's daemon or settings.
struct Session {
    runtime: std::path::PathBuf,
    config: std::path::PathBuf,
}

impl Session {
    fn new() -> Self {
        let runtime = fresh_dir();
        // The socket goes there: it must be private, like a real runtime folder.
        let private = std::os::unix::fs::PermissionsExt::from_mode(0o700);
        std::fs::set_permissions(&runtime, private).expect("a fresh temporary folder can be made private");
        let config = fresh_dir();
        let settings = format!(r#"{{"video_dir":{:?}}}"#, fresh_dir());
        std::fs::create_dir_all(config.join("screencapture")).expect("temporary folders are writable");
        std::fs::write(config.join("screencapture/config.json"), settings)
            .expect("temporary folders are writable");
        Self { runtime, config }
    }

    /// The binary, in this session.
    fn command(&self) -> Command {
        let mut cmd = bin();
        cmd.env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_CONFIG_HOME", &self.config);
        cmd
    }
}

fn fresh_dir() -> std::path::PathBuf {
    tempfile::tempdir()
        .expect("the system has a temporary folder")
        .keep()
}

const GEOMETRY: &str = "200,200 640x360";

/// Open the overlay standalone, let the test hook select `GEOMETRY` (with a line across its
/// middle) and record it, then stop: the video's path.
fn record_from_overlay() -> std::path::PathBuf {
    let session = Session::new();
    let overlay = session
        .command()
        .arg("region")
        .envs([
            ("SCREENCAPTURE_E2E_SELECT", GEOMETRY),
            ("SCREENCAPTURE_E2E_LINE", "1"),
            ("SCREENCAPTURE_E2E_RECORD", "1"),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test runs");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let stopped = session
        .command()
        .args(["record", "stop"])
        .status()
        .expect("the binary under test runs");
    let out = overlay
        .wait_with_output()
        .expect("the overlay process can be waited on");
    assert!(
        stopped.success(),
        "stop failed; overlay said: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    saved_path(&out)
}

/// A frame from the end of `video`, as an image.
fn last_frame(video: &Path) -> image::RgbaImage {
    let png = video.with_extension("png");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-sseof", "-0.3", "-i"])
        .arg(video)
        .args(["-frames:v", "1"])
        .arg(&png)
        .status()
        .expect("ffmpeg is installed for these tests");
    assert!(status.success(), "ffmpeg could not read {}", video.display());
    image::open(&png).expect("ffmpeg wrote a PNG").to_rgba8()
}

#[test]
#[ignore = "needs a graphical session and gpu-screen-recorder; shows the overlay, then records"]
fn recording_from_the_overlay_records_the_region() {
    let frame = last_frame(&record_from_overlay());
    assert_eq!(frame.dimensions(), (640, 360));
}

#[test]
#[ignore = "needs a graphical session and gpu-screen-recorder; shows the overlay, then records"]
fn the_frozen_overlay_is_not_in_the_recording() {
    let dir = tempfile::tempdir().expect("the system has a temporary folder");
    let screenshot = bin()
        .args(["region", "-g", GEOMETRY, "-o"])
        .arg(dir.path())
        .output();
    let live = saved_path(&screenshot.expect("the binary under test runs"));
    let frame = dir.path().join("frame.png");
    last_frame(&record_from_overlay())
        .save(&frame)
        .expect("the folder is writable");
    let (screen, video) = (mean_brightness(&live), mean_brightness(&frame));
    // The overlay darkens by 45%: recording it would show as a big drop.
    assert!(
        (screen - video).abs() / screen.max(1.0) < 0.15,
        "screen {screen:.1} vs video {video:.1}"
    );
}

/// Within `tolerance` of `colour` on each channel (videos are lossy).
fn near(pixel: image::Rgba<u8>, colour: [u8; 4], tolerance: u8) -> bool {
    (0..3).all(|channel| pixel[channel].abs_diff(colour[channel]) < tolerance)
}

#[test]
#[ignore = "needs a graphical session and gpu-screen-recorder; shows the overlay, then records"]
fn drawings_made_before_recording_are_recorded() {
    let frame = last_frame(&record_from_overlay());
    let line_rows = 178..183;
    let on_line = |x| {
        line_rows
            .clone()
            .any(|y| near(*frame.get_pixel(x, y), screencapture_first_colour(), 40))
    };
    let hits = (20..620).filter(|&x| on_line(x)).count();
    assert!(hits > 500, "line found on {hits} of 600 columns");
}
