//! One recording as the user sees it: a single recorder, or one per screen joined at stop.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::{
    join::{self, Encoder, Input},
    plan::{Join, Plan},
    recorder::{self, Active, Backend, Stopped},
};
use crate::capture::Rect;

pub struct Session {
    /// The recorders, one per screen, and where each lands in the joined video.
    parts: Vec<(Active, Rect)>,
    /// How to join them; `None` for one recorder writing the file itself.
    join: Option<Join>,
    /// The file the user gets.
    path: PathBuf,
}

impl Session {
    /// Start every recorder of `plan`, at once, into a new file in `dir`.
    pub fn start(
        backend: &Backend,
        plan: Plan,
        dir: &Path,
        time: chrono::DateTime<chrono::Local>,
    ) -> Result<Self> {
        let Some(join) = plan.join else {
            let part = plan
                .parts
                .into_iter()
                .next()
                .expect("a plan has a part per screen, at least one");
            let active = Active::start(backend, part.job, dir, time)?;
            return Ok(Self::single(active));
        };
        let ext = if join.gif { "gif" } else { "mp4" };
        let (path, _) = crate::naming::reserve(dir, "screencast", ext, time)?;
        let started = start_parts(backend, plan, &path);
        if started.is_err() {
            let _ = std::fs::remove_file(&path);
        }
        Ok(Self {
            parts: started?,
            join: Some(join),
            path,
        })
    }

    /// A recording made by `active` alone.
    pub fn single(active: Active) -> Self {
        let path = active.path.clone();
        Self {
            parts: vec![(active, Rect::new(0, 0, 0, 0))],
            join: None,
            path,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether every recorder still runs (one quitting ends the recording).
    pub fn alive(&mut self) -> bool {
        self.parts.iter_mut().all(|(active, _)| active.alive())
    }

    /// Stop and throw the recording away.
    pub fn cancel(self) {
        for (active, _) in self.parts {
            active.cancel();
        }
        if self.join.is_some() {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Stop every recorder: what is left to do before the file is ready.
    pub fn stop(self) -> Result<Finishing> {
        let Some(plan) = self.join else {
            let (active, _) = self.parts.into_iter().next().expect("a session has a recorder");
            return active.stop().map(Finishing::Done);
        };
        let slots: Vec<Rect> = self.parts.iter().map(|(_, slot)| *slot).collect();
        let parts = stop_parts(self.parts.into_iter().map(|(active, _)| active).collect())?;
        Ok(Finishing::Join {
            parts,
            slots,
            plan,
            path: self.path,
        })
    }
}

/// A recording whose recorders have stopped: what is left before its file is ready.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finishing {
    /// One recorder wrote the file itself.
    Done(PathBuf),
    /// Parts to join into `path` (then convert, for a GIF): slow, ~45 s per recorded minute.
    Join {
        parts: Vec<Stopped>,
        slots: Vec<Rect>,
        plan: Join,
        path: PathBuf,
    },
}

impl Finishing {
    /// Do what is left: the file's path.
    pub fn finish(self) -> Result<PathBuf> {
        match self {
            Self::Done(path) => Ok(path),
            Self::Join {
                parts,
                slots,
                plan,
                path,
            } => join_parts(&parts, &slots, &plan, &path),
        }
    }
}

/// Where part `index` of the recording into `path` is written meanwhile.
fn part_video(path: &Path, index: usize) -> PathBuf {
    path.with_extension(format!("part{index}.mp4"))
}

/// Start the recorders side by side: each takes a moment to get its first frame.
#[expect(
    clippy::needless_collect,
    reason = "every thread is spawned before any is joined"
)]
fn start_parts(backend: &Backend, plan: Plan, path: &Path) -> Result<Vec<(Active, Rect)>> {
    let started: Vec<Result<(Active, Rect)>> = std::thread::scope(|scope| {
        let starting: Vec<_> = plan
            .parts
            .into_iter()
            .enumerate()
            .map(|(index, part)| {
                let video = part_video(path, index);
                scope.spawn(move || {
                    Active::start_part(backend, part.job, video).map(|active| (active, part.slot))
                })
            })
            .collect();
        starting
            .into_iter()
            .map(|handle| handle.join().expect("starting a recorder doesn't panic"))
            .collect()
    });
    all_or_nothing(started)
}

/// Every part, or the first error, with the started ones cancelled.
fn all_or_nothing(started: Vec<Result<(Active, Rect)>>) -> Result<Vec<(Active, Rect)>> {
    if started.iter().all(Result::is_ok) {
        return started.into_iter().collect();
    }
    let mut first_error = None;
    for result in started {
        match result {
            Ok((active, _)) => active.cancel(),
            Err(e) => first_error = first_error.or(Some(e)),
        }
    }
    Err(first_error.expect("not every part started"))
}

/// Stop every recorder at once, so they end at the same moment.
#[expect(
    clippy::needless_collect,
    reason = "every thread is spawned before any is joined"
)]
fn stop_parts(parts: Vec<Active>) -> Result<Vec<Stopped>> {
    std::thread::scope(|scope| {
        let stopping: Vec<_> = parts
            .into_iter()
            .map(|active| scope.spawn(move || active.stop_part()))
            .collect();
        stopping
            .into_iter()
            .map(|handle| handle.join().expect("stopping a recorder doesn't panic"))
            .collect()
    })
}

/// Join the parts into `path` (through a temporary video for a GIF), then remove them.
fn join_parts(stopped: &[Stopped], slots: &[Rect], plan: &Join, path: &Path) -> Result<PathBuf> {
    let skips = join::skips(&stopped.iter().map(|s| s.first_frame).collect::<Vec<_>>());
    let inputs: Vec<Input> = stopped
        .iter()
        .zip(slots)
        .zip(skips)
        .map(|((part, &slot), skip)| Input {
            video: part.video.clone(),
            slot,
            skip,
        })
        .collect();
    let video = if plan.gif {
        path.with_extension("part.mp4")
    } else {
        path.to_path_buf()
    };
    encode_joined(&inputs, plan, &video)?;
    for part in stopped {
        let _ = std::fs::remove_file(&part.video);
    }
    if plan.gif {
        recorder::convert_to_gif(plan.fps, &video, path)?;
    }
    Ok(path.to_path_buf())
}

/// On the GPU if it can, else in software.
fn encode_joined(inputs: &[Input], plan: &Join, video: &Path) -> Result<()> {
    let on_gpu = recorder::ffmpeg(
        &join::join_args(inputs, plan, video, Encoder::Gpu),
        "join the screens",
    );
    if on_gpu.is_ok() {
        return Ok(());
    }
    let in_software = recorder::ffmpeg(
        &join::join_args(inputs, plan, video, Encoder::Cpu),
        "join the screens",
    );
    if let Err(e) = in_software {
        let kept: Vec<String> = inputs.iter().map(|i| i.video.display().to_string()).collect();
        bail!("{e:#}; the screens' recordings are kept: {}", kept.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{
        plan::{Target, plan},
        settings::Settings,
    };

    /// Record `target` for a second with the installed recorder: the plan and the file.
    fn record_for_a_second(target: &Target) -> (Plan, PathBuf, tempfile::TempDir) {
        let monitors = crate::capture::monitors().expect("a Wayland session lists its screens");
        let plan =
            plan(target, Settings::default(), &monitors, None, None).expect("the target is recordable");
        let dir = tempfile::tempdir().expect("the system has a temporary folder");
        let backend = recorder::find_backend().expect("a recorder is installed");
        let session = Session::start(&backend, plan.clone(), dir.path(), chrono::Local::now())
            .expect("recording starts");
        std::thread::sleep(std::time::Duration::from_secs(1));
        let stopped = session.stop().and_then(Finishing::finish);
        let path = stopped.expect("recording stops and joins");
        (plan, path, dir)
    }

    fn video_size(path: &Path) -> (u32, u32) {
        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(path)
            .output()
            .expect("ffprobe is installed for these tests");
        let text = String::from_utf8_lossy(&probe.stdout);
        let (w, h) = text.trim().split_once(',').expect("ffprobe prints width,height");
        (w.parse().expect("a width"), h.parse().expect("a height"))
    }

    #[test]
    #[ignore = "records every screen with the installed recorder; needs two screens or more"]
    fn every_screen_is_joined_into_one_video_of_the_planned_size() {
        let (plan, path, _dir) = record_for_a_second(&Target::All);
        assert_eq!(
            video_size(&path),
            plan.join.expect("several screens are joined").size
        );
    }

    #[test]
    #[ignore = "records every screen with the installed recorder; needs two screens or more"]
    fn joining_leaves_only_the_video() {
        let (_, _, dir) = record_for_a_second(&Target::All);
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("the folder exists").count(),
            1
        );
    }

    #[test]
    #[ignore = "records across two side-by-side screens with the installed recorder"]
    fn a_region_across_two_screens_is_one_video() {
        let monitors = crate::capture::monitors().expect("a Wayland session lists its screens");
        let touching = |m: &&crate::capture::MonitorInfo| monitors.iter().any(|n| n.rect.x == m.rect.right());
        let seam = monitors
            .iter()
            .find(touching)
            .expect("two screens side by side")
            .rect
            .right();
        let region = Target::Region(Rect::new(seam - 400, 300, 800, 400));
        let (_, path, _dir) = record_for_a_second(&region);
        assert_eq!(video_size(&path), (800, 400));
    }
}
