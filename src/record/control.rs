//! One recording at a time, driven by commands: shared by the daemon and foreground mode.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{
    command::Command,
    plan::{self, Job, Target},
    recorder::Active,
    settings::{Fps, Overrides, Settings},
};
use crate::capture::MonitorInfo;

/// What a command did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Started { path: PathBuf, notes: Vec<String> },
    Saved(PathBuf),
    /// A start while this one runs.
    AlreadyRecording(PathBuf),
    NotRecording,
    Failed(String),
}

/// The world a recording starts in.
pub trait Env {
    fn monitors(&mut self) -> Result<Vec<MonitorInfo>>;
    fn focused(&mut self) -> Option<String>;
    /// Remembered settings and the folder to record into.
    fn settings(&mut self) -> (Settings, PathBuf);
    /// Busier of CPU and GPU (0..=1); only asked for the Adaptive frame rate.
    fn load(&mut self) -> Option<f32>;
    fn start(&mut self, job: Job, dir: &Path) -> Result<Active>;
}

#[derive(Default)]
pub struct Recorder {
    active: Option<Active>,
}

impl Recorder {
    pub fn run(&mut self, command: Command, env: &mut impl Env) -> Outcome {
        let running = self.active.as_mut().map(Active::alive);
        match (command, running) {
            // A recorder that quit by itself is still stopped: what it saved is reported.
            (Command::Stop | Command::Toggle(..), Some(_)) => self.stop(),
            (Command::Stop, None) => Outcome::NotRecording,
            (Command::Start(..), Some(true)) => {
                Outcome::AlreadyRecording(self.recording().unwrap_or_default())
            }
            (Command::Start(target, overrides) | Command::Toggle(target, overrides), _) => {
                if running == Some(false) {
                    let _ = self.stop();
                }
                self.start(&target, overrides, env)
            }
        }
    }

    fn start(&mut self, target: &Target, overrides: Overrides, env: &mut impl Env) -> Outcome {
        let started = (|| {
            let (settings, dir) = env.settings();
            let settings = overrides.apply(settings);
            let monitors = env.monitors()?;
            let focused = matches!(target, Target::Focused).then(|| env.focused()).flatten();
            let load = (settings.fps == Fps::Adaptive).then(|| env.load()).flatten();
            let job = plan::plan(target, settings, &monitors, focused.as_deref(), load)?;
            let notes = job.notes.clone();
            anyhow::Ok((env.start(job, &dir)?, notes))
        })();
        match started {
            Ok((active, notes)) => {
                let path = active.path.clone();
                self.active = Some(active);
                Outcome::Started { path, notes }
            }
            Err(e) => Outcome::Failed(format!("{e:#}")),
        }
    }

    fn stop(&mut self) -> Outcome {
        match self.active.take().map(Active::stop) {
            None => Outcome::NotRecording,
            Some(Ok(path)) => Outcome::Saved(path),
            Some(Err(e)) => Outcome::Failed(format!("{e:#}")),
        }
    }

    /// The file being recorded, if a recording runs.
    pub fn recording(&self) -> Option<PathBuf> {
        self.active.as_ref().map(|a| a.path.clone())
    }

    /// The recorder quit on its own (screen unplugged, crash): finish what it left.
    pub fn ended_by_itself(&mut self) -> Option<Outcome> {
        if self.active.as_mut()?.alive() {
            return None;
        }
        Some(self.stop())
    }
}

/// The real machine: Wayland/Hyprland, the config file, the installed recorder.
pub struct LiveEnv;

impl Env for LiveEnv {
    fn monitors(&mut self) -> Result<Vec<MonitorInfo>> {
        crate::capture::monitors()
    }

    fn focused(&mut self) -> Option<String> {
        crate::hyprland::focused_monitor()
    }

    fn settings(&mut self) -> (Settings, PathBuf) {
        let config = crate::save::Config::load();
        (config.record, config.video_dir(None))
    }

    fn load(&mut self) -> Option<f32> {
        super::load::sample(std::time::Duration::from_millis(200))
    }

    fn start(&mut self, job: Job, dir: &Path) -> Result<Active> {
        let backend = super::recorder::find_backend()?;
        Active::start(&backend, job, dir, chrono::Local::now())
    }
}

impl Outcome {
    /// The daemon's answer for it.
    pub fn reply(self) -> (crate::daemon::Reply, Vec<String>) {
        use crate::daemon::Reply;
        match self {
            Self::Started { path, notes } => (Reply::Recording(path), notes),
            Self::Saved(path) => (Reply::Saved(path), Vec::new()),
            Self::AlreadyRecording(path) => (
                Reply::Busy,
                vec![format!("already recording to {}", path.display())],
            ),
            Self::NotRecording => (Reply::NotRecording, Vec::new()),
            Self::Failed(message) => (Reply::Error(message), Vec::new()),
        }
    }
}

/// A stand-in world for tests: one 1920×1080 144 Hz screen, a shell script as the recorder.
#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::capture::Rect;

    pub struct Fake {
        pub dir: tempfile::TempDir,
        pub settings: Settings,
        pub loads: usize,
        pub jobs: Vec<Job>,
        pub script: &'static str,
    }

    impl Fake {
        pub fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
                settings: Settings::default(),
                loads: 0,
                jobs: Vec::new(),
                script: super::super::recorder::FAKE_RECORDER,
            }
        }
    }

    impl Env for Fake {
        fn monitors(&mut self) -> Result<Vec<MonitorInfo>> {
            Ok(vec![MonitorInfo {
                name: "DP-1".into(),
                rect: Rect::new(0, 0, 1920, 1080),
                output_position: (0, 0),
                refresh_mhz: Some(144_000),
            }])
        }
        fn focused(&mut self) -> Option<String> {
            Some("DP-1".into())
        }
        fn settings(&mut self) -> (Settings, PathBuf) {
            (self.settings, self.dir.path().to_path_buf())
        }
        fn load(&mut self) -> Option<f32> {
            self.loads += 1;
            Some(0.8)
        }
        fn start(&mut self, job: Job, dir: &Path) -> Result<Active> {
            self.jobs.push(job.clone());
            super::super::recorder::spawn_fake(dir, self.script, job)
        }
    }

}

#[cfg(test)]
mod tests {
    use super::{testing::Fake, *};
    use crate::{daemon::Reply, record::settings::Format};

    #[test]
    fn outcomes_become_replies() {
        let p = PathBuf::from("/v/a.mp4");
        assert_eq!(
            Outcome::Started {
                path: p.clone(),
                notes: vec!["n".into()]
            }
            .reply(),
            (Reply::Recording(p.clone()), vec!["n".to_string()])
        );
        assert_eq!(Outcome::Saved(p.clone()).reply().0, Reply::Saved(p.clone()));
        let (reply, notes) = Outcome::AlreadyRecording(p).reply();
        assert_eq!(reply, Reply::Busy);
        assert!(notes[0].contains("/v/a.mp4"), "{notes:?}");
        assert_eq!(Outcome::NotRecording.reply().0, Reply::NotRecording);
        assert_eq!(Outcome::Failed("x".into()).reply().0, Reply::Error("x".into()));
    }

    fn start() -> Command {
        Command::Start(Target::Focused, Overrides::default())
    }

    #[test]
    fn start_then_stop_saves_the_file() {
        let mut env = Fake::new();
        let mut r = Recorder::default();
        let Outcome::Started { path, .. } = r.run(start(), &mut env) else {
            panic!("not started")
        };
        assert_eq!(r.recording(), Some(path.clone()));
        assert_eq!(r.run(Command::Stop, &mut env), Outcome::Saved(path.clone()));
        assert!(path.exists());
        assert_eq!(r.recording(), None);
    }

    #[test]
    fn one_recording_at_a_time() {
        let mut env = Fake::new();
        let mut r = Recorder::default();
        let Outcome::Started { path, .. } = r.run(start(), &mut env) else {
            panic!()
        };
        assert_eq!(r.run(start(), &mut env), Outcome::AlreadyRecording(path));
        assert_eq!(env.jobs.len(), 1);
    }

    #[test]
    fn stop_without_a_recording() {
        assert_eq!(Recorder::default().run(Command::Stop, &mut Fake::new()), Outcome::NotRecording);
    }

    #[test]
    fn toggle_starts_then_stops() {
        let mut env = Fake::new();
        let mut r = Recorder::default();
        let toggle = || Command::Toggle(Target::Focused, Overrides::default());
        assert!(matches!(r.run(toggle(), &mut env), Outcome::Started { .. }));
        assert!(matches!(r.run(toggle(), &mut env), Outcome::Saved(_)));
        assert_eq!(r.recording(), None);
    }

    #[test]
    fn a_bad_target_fails_and_nothing_records() {
        let mut env = Fake::new();
        let mut r = Recorder::default();
        let bad = Command::Start(Target::Screens(vec!["DP-9".into()]), Overrides::default());
        assert!(matches!(r.run(bad, &mut env), Outcome::Failed(_)));
        assert_eq!(r.recording(), None);
    }

    #[test]
    fn overrides_win_and_load_is_only_read_for_adaptive() {
        let mut env = Fake::new();
        env.settings.format = Format::H264;
        let mut r = Recorder::default();
        r.run(start(), &mut env);
        r.run(Command::Stop, &mut env);
        assert_eq!(env.loads, 0);
        assert_eq!(env.jobs[0].fps, 144);
        let adaptive = Overrides {
            fps: Some(Fps::Adaptive),
            ..Overrides::default()
        };
        r.run(Command::Start(Target::Focused, adaptive), &mut env);
        assert_eq!(env.loads, 1);
        assert_eq!((env.jobs[1].fps, env.jobs[1].variable), (30, true), "load 0.8");
        assert_eq!(env.jobs[1].codec, plan::Codec::H264, "remembered format kept");
    }

    #[test]
    fn a_recorder_that_quit_by_itself_is_noticed_and_its_file_kept() {
        let mut env = Fake::new();
        // Records a little, then quits.
        env.script = r#"out="$1"; echo data > "$out"; : > "$out.ts"; sleep 0.1"#;
        let mut r = Recorder::default();
        let Outcome::Started { path, .. } = r.run(start(), &mut env) else {
            panic!()
        };
        assert_eq!(r.ended_by_itself(), None, "still running");
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(r.ended_by_itself(), Some(Outcome::Saved(path)));
        assert_eq!(r.recording(), None);
        assert_eq!(r.run(Command::Stop, &mut env), Outcome::NotRecording);
    }
}
