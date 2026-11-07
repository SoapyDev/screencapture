//! The recorder process: gpu-screen-recorder (GPU capture and encoding), or wf-recorder.

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use super::plan::{self, Job};

/// Which program records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    Gsr(PathBuf),
    Wf(PathBuf),
}

/// `program` on `PATH`.
fn which(program: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| {
            std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// gpu-screen-recorder if installed, else wf-recorder. `SCREENCAPTURE_RECORDER=wf-recorder`
/// forces the latter.
pub fn find_backend() -> Result<Backend> {
    let gsr = || which("gpu-screen-recorder").map(Backend::Gsr);
    let wf = || which("wf-recorder").map(Backend::Wf);
    let found = match std::env::var("SCREENCAPTURE_RECORDER").as_deref() {
        Ok("wf-recorder") => wf(),
        Ok("gpu-screen-recorder") => gsr(),
        _ => gsr().or_else(wf),
    };
    found.ok_or_else(|| anyhow::anyhow!("recording needs gpu-screen-recorder (or wf-recorder) installed"))
}

/// How we know the recorder is really recording.
enum Ready {
    /// This file appears (gpu-screen-recorder's first-frame timestamp).
    File(PathBuf),
    /// Still running after this long.
    After(Duration),
}

/// Longest wait for the first frame.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest wait for the recorder to finish the file once told to stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// Recorder output lines kept for error messages.
const LOG_LINES: usize = 12;

/// A running recording.
pub struct Active {
    child: Child,
    /// The file the user gets.
    pub path: PathBuf,
    /// What the recorder writes: `path`, or a temporary MP4 for a GIF.
    video: PathBuf,
    /// Removed at stop.
    leftovers: Vec<PathBuf>,
    job: Job,
    pub started: Instant,
    log: Arc<Mutex<VecDeque<String>>>,
}

impl Active {
    /// Record `job` into a new file in `dir`.
    pub fn start(
        backend: &Backend,
        job: Job,
        dir: &Path,
        time: chrono::DateTime<chrono::Local>,
    ) -> Result<Self> {
        let ext = if job.gif { "gif" } else { "mp4" };
        // Reserved now, so no other file can take the name; the recorder writes over it.
        let (path, _) = screencapture_core::naming::reserve(dir, "screencast", ext, time)?;
        let video = if job.gif {
            path.with_extension("part.mp4")
        } else {
            path.clone()
        };
        Self::launch(backend, job, path, video)
    }

    /// Record `job` (one screen of several) into `video`.
    pub fn start_part(backend: &Backend, job: Job, video: PathBuf) -> Result<Self> {
        Self::launch(backend, job, video.clone(), video)
    }

    fn launch(backend: &Backend, job: Job, path: PathBuf, video: PathBuf) -> Result<Self> {
        match backend {
            Backend::Gsr(program) => {
                let stamp = first_frame_file(&video);
                let args = plan::gsr_args(&job, &video);
                let files = Files {
                    path,
                    video,
                    leftovers: vec![stamp.clone()],
                };
                Self::spawn(program, &args, &Ready::File(stamp), files, job)
            }
            Backend::Wf(program) => {
                let args = plan::wf_args(&job, &video);
                let files = Files {
                    path,
                    video,
                    leftovers: Vec::new(),
                };
                Self::spawn(
                    program,
                    &args,
                    &Ready::After(Duration::from_millis(500)),
                    files,
                    job,
                )
            }
        }
    }

    fn spawn(program: &Path, args: &[String], ready: &Ready, files: Files, job: Job) -> Result<Self> {
        use std::os::unix::process::CommandExt;
        let spawned = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // Its own process group: Ctrl+C in a terminal reaches us, and we stop it properly.
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting {}", program.display()));
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                files.remove();
                return Err(e);
            }
        };
        let log = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_LINES)));
        let reader = child.stderr.take().map(|stderr| {
            let log = log.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut log = log.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if log.len() == LOG_LINES {
                        log.pop_front();
                    }
                    log.push_back(line);
                }
            })
        });
        let mut active = Self {
            child,
            path: files.path,
            video: files.video,
            leftovers: files.leftovers,
            job,
            started: Instant::now(),
            log,
        };
        let begun = Instant::now();
        loop {
            if let Some(status) = active.child.try_wait()? {
                // Let the last words reach the log (a helper may hold the pipe open: don't hang).
                if let Some(reader) = reader {
                    let until = Instant::now() + Duration::from_millis(200);
                    while !reader.is_finished() && Instant::now() < until {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                let message = format!("the recorder stopped at once ({status}){}", active.log_tail());
                active.discard();
                bail!(message);
            }
            match ready {
                Ready::File(file) if file.exists() => break,
                Ready::After(wait) if begun.elapsed() >= *wait => break,
                Ready::File(_) if begun.elapsed() >= START_TIMEOUT => {
                    let message = format!(
                        "the recorder didn't start within {START_TIMEOUT:?}{}",
                        active.log_tail()
                    );
                    active.cancel();
                    bail!(message);
                }
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        active.started = Instant::now();
        Ok(active)
    }

    fn log_tail(&self) -> String {
        let lines: Vec<String> = self
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect();
        if lines.is_empty() {
            String::new()
        } else {
            format!(":\n{}", lines.join("\n"))
        }
    }

    /// Ask the recorder to finish (SIGINT, which both recorders treat as "save and quit"),
    /// and wait for it; kill it if it hangs.
    fn finish(&mut self) -> Result<std::process::ExitStatus> {
        use rustix::process::{Pid, Signal, kill_process};
        if let Some(status) = self.child.try_wait()? {
            return Ok(status);
        }
        let _ = kill_process(Pid::from_child(&self.child), Signal::INT);
        let until = Instant::now() + STOP_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= until {
                let _ = self.child.kill();
                return Ok(self.child.wait()?);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn discard(&self) {
        Files {
            path: self.path.clone(),
            video: self.video.clone(),
            leftovers: self.leftovers.clone(),
        }
        .remove();
    }

    /// Whether the recorder is still running.
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Finish the file (and the GIF) and return its path.
    pub fn stop(mut self) -> Result<PathBuf> {
        self.finish_video()?;
        if self.job.gif {
            convert_to_gif(self.job.fps, &self.video, &self.path)?;
        }
        Ok(self.path.clone())
    }

    /// Finish the file of one screen of several, and say when its first frame was taken.
    pub fn stop_part(mut self) -> Result<Stopped> {
        let first_frame = self.finish_video()?;
        Ok(Stopped {
            video: self.video.clone(),
            first_frame,
        })
    }

    /// Stop the recorder and check it left a video; the first frame's time, if it said.
    fn finish_video(&mut self) -> Result<Option<u64>> {
        self.finish()?;
        let first_frame = std::fs::read_to_string(first_frame_file(&self.video))
            .ok()
            .and_then(|text| parse_first_frame(&text));
        for file in &self.leftovers {
            let _ = std::fs::remove_file(file);
        }
        if std::fs::metadata(&self.video).map_or(true, |m| m.len() == 0) {
            let message = format!("the recorder saved nothing{}", self.log_tail());
            self.discard();
            bail!(message);
        }
        Ok(first_frame)
    }

    /// Stop and throw the recording away.
    pub fn cancel(mut self) {
        let _ = self.finish();
        self.discard();
    }
}

impl Drop for Active {
    /// Never leave a recorder running behind us; what it recorded is kept.
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// A finished part of a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stopped {
    pub video: PathBuf,
    /// When its first frame was taken, µs on the monotonic clock.
    pub first_frame: Option<u64>,
}

/// gpu-screen-recorder's first-frame timestamp file for `video`.
fn first_frame_file(video: &Path) -> PathBuf {
    PathBuf::from(format!("{}.ts", video.display()))
}

/// The monotonic µs in a first-frame timestamp file (a header line, then the values).
#[must_use]
pub fn parse_first_frame(text: &str) -> Option<u64> {
    let values = text.lines().rev().find(|line| !line.trim().is_empty())?;
    values.split_whitespace().next()?.parse().ok()
}

/// Run ffmpeg with `args`; `what` names the job in errors.
pub fn ffmpeg(args: &[String], what: &str) -> Result<()> {
    let ran = Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .with_context(|| format!("running ffmpeg to {what}"))?;
    if !ran.status.success() {
        bail!(
            "ffmpeg could not {what}: {}",
            String::from_utf8_lossy(&ran.stderr).trim()
        );
    }
    Ok(())
}

/// `video` as an animated GIF at `fps`; the video is removed once converted, kept otherwise.
pub fn convert_to_gif(fps: u32, video: &Path, gif: &Path) -> Result<()> {
    if let Err(e) = ffmpeg(&plan::gif_args(fps, video, gif), "make the GIF") {
        let _ = std::fs::remove_file(gif);
        bail!("{e:#}; the video is kept at {}", video.display());
    }
    let _ = std::fs::remove_file(video);
    Ok(())
}

/// A stand-in recorder for tests: writes `$1` and `$1.ts` at once, finishes the file on SIGINT.
#[cfg(test)]
pub const FAKE_RECORDER: &str = r#"out="$1"; trap 'echo end >> "$out"; exit 0' INT; echo start > "$out"; echo "1 2" > "$out.ts"; while :; do sleep 0.01; done"#;

/// Run `script` (see [`FAKE_RECORDER`]) as the recorder of `job`, in `dir`.
#[cfg(test)]
pub fn spawn_fake(dir: &Path, script: &str, job: Job) -> Result<Active> {
    let (path, _) = screencapture_core::naming::reserve(dir, "screencast", "mp4", chrono::Local::now())?;
    let stamp = PathBuf::from(format!("{}.ts", path.display()));
    let files = Files {
        path: path.clone(),
        video: path.clone(),
        leftovers: vec![stamp.clone()],
    };
    let args = [
        "-c".into(),
        script.into(),
        "fake".into(),
        path.display().to_string(),
    ];
    Active::spawn(Path::new("/bin/sh"), &args, &Ready::File(stamp), files, job)
}

/// Where a recording's files go.
struct Files {
    path: PathBuf,
    video: PathBuf,
    leftovers: Vec<PathBuf>,
}

impl Files {
    fn remove(&self) {
        for file in std::iter::once(&self.path)
            .chain([&self.video])
            .chain(&self.leftovers)
        {
            let _ = std::fs::remove_file(file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use screencapture_core::capture::Rect;

    use crate::{
        plan::Codec,
        settings::{Audio, Quality},
    };

    fn job(gif: bool) -> Job {
        Job {
            area: Rect::new(0, 0, 640, 480),
            fps: 25,
            variable: false,
            size: None,
            codec: Codec::H264,
            quality: Quality::VeryHigh,
            audio: Audio::Off,
            gif,
        }
    }

    const FAKE: &str = FAKE_RECORDER;

    fn fake(dir: &Path, script: &str, ready: Ready, gif: bool) -> Result<Active> {
        let path = dir.join("clip.mp4");
        let ts = PathBuf::from(format!("{}.ts", path.display()));
        let ready = match ready {
            Ready::File(_) => Ready::File(ts.clone()),
            other @ Ready::After(_) => other,
        };
        let files = Files {
            path: path.clone(),
            video: path.clone(),
            leftovers: vec![ts],
        };
        let args = vec![
            "-c".into(),
            script.into(),
            "fake".into(),
            path.display().to_string(),
        ];
        Active::spawn(Path::new("/bin/sh"), &args, &ready, files, job(gif))
    }

    #[test]
    fn the_first_frame_time_follows_the_header() {
        let text = "monotonic_microsec\trealtime_microsec\n123456\t1700000000000000\n";
        assert_eq!(parse_first_frame(text), Some(123_456));
    }

    #[test]
    fn an_empty_first_frame_file_has_no_time() {
        assert_eq!(parse_first_frame(""), None);
    }

    #[test]
    fn programs_are_found_on_the_path() {
        assert!(which("sh").is_some());
        assert!(which("no-such-recorder-anywhere").is_none());
    }

    #[test]
    fn stopping_lets_the_recorder_finish_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut active = fake(dir.path(), FAKE, Ready::File(PathBuf::new()), false).unwrap();
        assert!(active.alive());
        let path = active.stop().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "start\nend\n");
        assert!(
            !dir.path().join("clip.mp4.ts").exists(),
            "timestamp file cleaned up"
        );
    }

    #[test]
    fn a_recorder_that_dies_at_start_is_an_error_with_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let err = fake(
            dir.path(),
            "echo 'gsr error: no such monitor' >&2; exit 1",
            Ready::File(PathBuf::new()),
            false,
        )
        .err()
        .unwrap();
        assert!(format!("{err:#}").contains("no such monitor"), "{err:#}");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "nothing left behind"
        );
    }

    #[test]
    fn a_recorder_without_a_start_signal_is_trusted_once_it_stays_up() {
        let dir = tempfile::tempdir().unwrap();
        let active = fake(dir.path(), FAKE, Ready::After(Duration::from_millis(50)), false).unwrap();
        assert!(active.stop().is_ok());
        assert!(
            fake(
                dir.path(),
                "exit 3",
                Ready::After(Duration::from_millis(200)),
                false
            )
            .is_err()
        );
    }

    #[test]
    fn cancelling_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fake(dir.path(), FAKE, Ready::File(PathBuf::new()), false)
            .unwrap()
            .cancel();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_recorder_that_wrote_nothing_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let script =
            r#"out="$1"; trap 'rm -f "$out"; exit 0' INT; : > "$out.ts"; while :; do sleep 0.01; done"#;
        let active = fake(dir.path(), script, Ready::File(PathBuf::new()), false).unwrap();
        assert!(active.stop().is_err());
    }

    /// Records one second of the middle of the first screen and checks the file.
    #[test]
    #[ignore = "records the screen with the installed recorder"]
    fn records_a_real_clip() {
        let monitors = screencapture_core::capture::monitors().unwrap();
        let m = &monitors[0];
        let area = Rect::new(m.rect.x + 100, m.rect.y + 100, 640, 360);
        let job = Job {
            area,
            fps: 30,
            codec: Codec::Av1,
            ..job(false)
        };
        let dir = tempfile::tempdir().unwrap();
        let active = Active::start(&find_backend().unwrap(), job, dir.path(), chrono::Local::now()).unwrap();
        std::thread::sleep(Duration::from_secs(1));
        let path = active.stop().unwrap();
        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-count_frames"])
            .args([
                "-show_entries",
                "stream=codec_name,width,height,nb_read_frames",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&probe.stdout);
        let fields: Vec<&str> = text.trim().split(',').collect();
        assert_eq!(&fields[..3], ["av1", "640", "360"], "{text}");
        let frames: u32 = fields[3].parse().unwrap();
        assert!(frames >= 10, "{frames} frames");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "only the video"
        );
    }

    #[test]
    #[ignore = "records the screen with the installed recorder and ffmpeg"]
    fn records_a_real_gif() {
        let monitors = screencapture_core::capture::monitors().unwrap();
        let m = &monitors[0];
        let job = Job {
            area: Rect::new(m.rect.x + 100, m.rect.y + 100, 320, 240),
            ..job(true)
        };
        let dir = tempfile::tempdir().unwrap();
        let active = Active::start(&find_backend().unwrap(), job, dir.path(), chrono::Local::now()).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let path = active.stop().unwrap();
        assert_eq!(path.extension().unwrap(), "gif");
        assert!(std::fs::read(&path).unwrap().starts_with(b"GIF89a"));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temporary video removed"
        );
    }
}
