//! End-to-end runs of the real binary against the live desktop:
//! `cargo test -- --ignored` inside a Wayland (wlroots) session.

// The virtual pointer is a wlroots Wayland protocol.
#[cfg(feature = "wayland")]
#[path = "support/pointer.rs"]
mod pointer;

use std::{
    path::Path,
    process::{Command, Output},
};

#[cfg(feature = "wayland")]
use pointer::{Pointer, Spot};

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

/// The tests share one desktop: their overlays, recordings and pointer moves would get in each
/// other's way (and slow each other past their waits), so they take turns.
fn desktop() -> std::sync::MutexGuard<'static, ()> {
    static DESKTOP: std::sync::Mutex<()> = std::sync::Mutex::new(());
    DESKTOP.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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
    let _desktop = desktop();
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
    let _desktop = desktop();
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
    let _desktop = desktop();
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
    #[cfg_attr(
        not(feature = "wayland"),
        expect(dead_code, reason = "only the drawer tests look there")
    )]
    videos: std::path::PathBuf,
}

impl Session {
    fn new() -> Self {
        let runtime = fresh_dir();
        // The socket goes there: it must be private, like a real runtime folder.
        let private = std::os::unix::fs::PermissionsExt::from_mode(0o700);
        std::fs::set_permissions(&runtime, private).expect("a fresh temporary folder can be made private");
        let config = fresh_dir();
        let videos = fresh_dir();
        let settings = format!(r#"{{"video_dir":{videos:?}}}"#);
        std::fs::create_dir_all(config.join("screencapture")).expect("temporary folders are writable");
        std::fs::write(config.join("screencapture/config.json"), settings)
            .expect("temporary folders are writable");
        Self {
            runtime,
            config,
            videos,
        }
    }

    /// Whether this session's recording process is up within 20 s: from then on, it answers
    /// `record stop`.
    fn recording_soon(&self) -> bool {
        let socket = self.runtime.join("screencapture.sock");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !socket.exists() {
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        true
    }

    /// Let `recording` record for 1.5 s once it is up (the recorder itself takes a moment to
    /// start), then stop it: what `record stop` and
    /// the recording printed. Kills it, and fails the test, if it never starts or won't stop.
    fn stop_once_recorded(&self, mut recording: std::process::Child) -> (Output, Output) {
        let started = self.recording_soon();
        if started {
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
        let stopped = self.command().args(["record", "stop"]).output();
        let stopped = stopped.expect("the binary under test runs");
        if !(started && stopped.status.success()) {
            let _ = recording.kill();
        }
        let out = recording.wait_with_output();
        let out = out.expect("the recording process can be waited on");
        assert!(
            stopped.status.success(),
            "stop failed: {}; the recording said: {}",
            String::from_utf8_lossy(&stopped.stderr),
            String::from_utf8_lossy(&out.stderr)
        );
        (stopped, out)
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
    saved_path(&overlay_recording(&[]))
}

/// Like `record_from_overlay`, with the test hook also told `hooks`: what the overlay printed.
fn overlay_recording(hooks: &[(&str, &str)]) -> std::process::Output {
    let session = Session::new();
    let overlay = session
        .command()
        .arg("region")
        .envs([
            ("SCREENCAPTURE_E2E_SELECT", GEOMETRY),
            ("SCREENCAPTURE_E2E_LINE", "1"),
            ("SCREENCAPTURE_E2E_RECORD", "1"),
        ])
        .envs(hooks.iter().copied())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test runs");
    session.stop_once_recorded(overlay).1
}

#[test]
#[ignore = "needs a graphical session and gpu-screen-recorder; shows the overlay, then records"]
fn capturing_while_the_bars_slide_away_is_ignored() {
    let _desktop = desktop();
    let out = overlay_recording(&[("SCREENCAPTURE_E2E_CAPTURE_TOO", "1")]);
    let printed = String::from_utf8_lossy(&out.stdout);
    let saved: Vec<&Path> = printed.lines().map(Path::new).collect();
    let only_a_video = matches!(saved.as_slice(), [video] if video.extension() == Some("mp4".as_ref()));
    assert!(only_a_video, "printed: {printed}");
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
    let _desktop = desktop();
    let frame = last_frame(&record_from_overlay());
    assert_eq!(frame.dimensions(), (640, 360));
}

#[test]
#[ignore = "needs a graphical session and gpu-screen-recorder; shows the overlay, then records"]
fn the_frozen_overlay_is_not_in_the_recording() {
    let _desktop = desktop();
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
    let _desktop = desktop();
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

/// The widest screen, from `screencapture list` (`name<TAB>x,y WxH` lines).
fn widest_screen() -> u32 {
    let listed = bin().arg("list").output().expect("the binary under test runs");
    let text = String::from_utf8(listed.stdout).expect("screen names are text");
    let width = |line: &str| line.rsplit_once(' ')?.1.split_once('x')?.0.parse::<u32>().ok();
    text.lines().filter_map(width).max().expect("at least one screen")
}

#[test]
#[ignore = "needs a graphical session with two screens or more, and gpu-screen-recorder; records every screen"]
fn recording_every_screen_makes_one_video_wider_than_any_screen() {
    let _desktop = desktop();
    let session = Session::new();
    let recording = session
        .command()
        .args(["record", "all"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the binary under test runs");
    let (stopped, _) = session.stop_once_recorded(recording);
    let video = saved_path(&stopped);
    assert!(last_frame(&video).width() > widest_screen());
}

#[cfg(feature = "wayland")]
mod drawer {
    use super::*;

    /// A screen as `screencapture list` prints it (`name<TAB>x,y WxH`).
    struct Screen {
        name: String,
        x: u32,
        width: u32,
    }

    fn screens() -> Vec<Screen> {
        let listed = bin().arg("list").output().expect("the binary under test runs");
        let text = String::from_utf8(listed.stdout).expect("screen names are text");
        text.lines().filter_map(parse_screen).collect()
    }

    fn parse_screen(line: &str) -> Option<Screen> {
        let (name, geometry) = line.split_once('\t')?;
        let (position, size) = geometry.split_once(' ')?;
        let x = position.split_once(',')?.0.parse().ok()?;
        let width = size.split_once('x')?.0.parse().ok()?;
        Some(Screen {
            name: name.into(),
            x,
            width,
        })
    }

    /// The desktop's extent, for absolute pointer moves.
    fn layout() -> (u32, u32) {
        let right = screens()
            .iter()
            .map(|s| s.x + s.width)
            .max()
            .expect("at least one screen");
        (right, 1440)
    }

    /// Record the first screen from the command line, open the drawer with the pointer, and click
    /// at `offset` px right of its centre. The recording process.
    fn click_in_drawer(session: &Session, offset: u32) -> std::process::Child {
        let screen = screens().into_iter().next().expect("at least one screen");
        let recording = session
            .command()
            .args(["record", "screen", &screen.name])
            .spawn()
            .expect("the binary under test runs");
        assert!(session.recording_soon(), "the recording did not start");
        // Then the strips open.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let centre = screen.x + screen.width / 2;
        let mut pointer = Pointer::new(layout());
        pointer.glide(Spot { x: centre, y: 300 }, Spot { x: centre, y: 0 });
        // Rest on the strip until the drawer is open.
        std::thread::sleep(std::time::Duration::from_millis(400));
        pointer.glide(
            Spot { x: centre, y: 0 },
            Spot {
                x: centre + offset,
                y: 27,
            },
        );
        pointer.click();
        recording
    }

    /// Where the drawer's buttons are, right of its centre, with the default settings' summary.
    const STOP_BUTTON: u32 = 63;
    const DISCARD_BUTTON: u32 = 150;

    /// Whether `process` ends within a few seconds.
    fn ends_soon(process: &mut std::process::Child) -> bool {
        for _ in 0..50 {
            if process
                .try_wait()
                .expect("the process can be waited on")
                .is_some()
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = process.kill();
        false
    }

    #[test]
    #[ignore = "needs a wlroots session and gpu-screen-recorder; moves the pointer and records"]
    fn the_drawer_stops_the_recording() {
        let _desktop = desktop();
        let session = Session::new();
        let mut recording = click_in_drawer(&session, STOP_BUTTON);
        assert!(ends_soon(&mut recording), "the recording went on");
    }

    #[test]
    #[ignore = "needs a wlroots session and gpu-screen-recorder; moves the pointer and records"]
    fn the_drawer_discards_the_recording() {
        let _desktop = desktop();
        let session = Session::new();
        let mut recording = click_in_drawer(&session, DISCARD_BUTTON);
        let _ = ends_soon(&mut recording);
        assert_eq!(
            std::fs::read_dir(&session.videos)
                .expect("the folder exists")
                .count(),
            0
        );
    }
}
