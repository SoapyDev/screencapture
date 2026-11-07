//! One recording at a time, driven by commands: shared by the daemon and foreground mode.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{
    command::Command,
    plan::{self, Plan, Target},
    session::Session,
    settings::{Fps, Overrides, Settings},
};
use crate::capture::{MonitorInfo, Rect};

/// What a command did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Started {
        path: PathBuf,
        notes: Vec<String>,
    },
    Saved(PathBuf),
    /// Stopped and thrown away.
    Discarded,
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
    fn start(&mut self, plan: Plan, dir: &Path) -> Result<Session>;
}

/// What stays on screen while recording: the frame around the recorded parts, and the controls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnScreen {
    /// The recorded parts, one per screen.
    pub areas: Vec<Rect>,
    /// The screens when the recording started.
    pub monitors: Vec<MonitorInfo>,
    /// What is recorded, for the controls (see [`Plan::summary`]).
    pub summary: String,
}

#[derive(Default)]
pub struct Recorder {
    active: Option<Session>,
    /// What is on screen, while recording.
    frame: Option<OnScreen>,
}

impl Recorder {
    pub fn run(&mut self, command: Command, env: &mut impl Env) -> Outcome {
        let running = self.active.as_mut().map(Session::alive);
        match (command, running) {
            // A recorder that quit by itself is still stopped: what it saved is reported.
            (Command::Stop | Command::Toggle(..), Some(_)) => self.stop(),
            (Command::Stop | Command::Discard, None) => Outcome::NotRecording,
            (Command::Discard, Some(_)) => self.discard(),
            (Command::Start(..), Some(true)) => {
                Outcome::AlreadyRecording(self.recording().map_or_else(Default::default, Path::to_path_buf))
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
            let (plan, dir, monitors) = plan_for(target, overrides, env)?;
            let notes = plan.notes.clone();
            let areas = plan.parts.iter().map(|part| part.job.area).collect();
            let summary = plan.summary();
            let on_screen = OnScreen {
                areas,
                monitors,
                summary,
            };
            anyhow::Ok((env.start(plan, &dir)?, notes, on_screen))
        })();
        match started {
            Ok((session, notes, frame)) => {
                let path = session.path().to_path_buf();
                self.active = Some(session);
                self.frame = Some(frame);
                Outcome::Started { path, notes }
            }
            Err(e) => Outcome::Failed(format!("{e:#}")),
        }
    }

    fn stop(&mut self) -> Outcome {
        self.frame = None;
        match self.active.take().map(Session::stop) {
            None => Outcome::NotRecording,
            Some(Ok(path)) => Outcome::Saved(path),
            Some(Err(e)) => Outcome::Failed(format!("{e:#}")),
        }
    }

    fn discard(&mut self) -> Outcome {
        self.frame = None;
        if let Some(session) = self.active.take() {
            session.cancel();
        }
        Outcome::Discarded
    }

    /// The recorded area and the screens around it, for the frame shown while recording.
    pub const fn frame(&self) -> Option<&OnScreen> {
        self.frame.as_ref()
    }

    /// The file being recorded, if a recording runs.
    pub fn recording(&self) -> Option<&Path> {
        self.active.as_ref().map(Session::path)
    }

    /// The recorder quit on its own (screen unplugged, crash): finish what it left.
    pub fn ended_by_itself(&mut self) -> Option<Outcome> {
        if self.active.as_mut()?.alive() {
            return None;
        }
        Some(self.stop())
    }
}

/// The plan for `target` in `env`, the folder to record into, and the screens it saw.
fn plan_for(
    target: &Target,
    overrides: Overrides,
    env: &mut impl Env,
) -> Result<(Plan, PathBuf, Vec<MonitorInfo>)> {
    let (settings, dir) = env.settings();
    let settings = overrides.apply(settings);
    let monitors = env.monitors()?;
    let focused = matches!(target, Target::Focused).then(|| env.focused()).flatten();
    let load = (settings.fps == Fps::Adaptive).then(|| env.load()).flatten();
    let plan = plan::plan(target, settings, &monitors, focused.as_deref(), load)?;
    Ok((plan, dir, monitors))
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

    fn start(&mut self, plan: Plan, dir: &Path) -> Result<Session> {
        let backend = super::recorder::find_backend()?;
        Session::start(&backend, plan, dir, chrono::Local::now())
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
            Self::Discarded => (Reply::Cancelled, Vec::new()),
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
        pub jobs: Vec<super::plan::Job>,
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
        fn start(&mut self, plan: Plan, dir: &Path) -> Result<Session> {
            self.jobs.extend(plan.parts.iter().map(|part| part.job.clone()));
            let job = plan.parts.into_iter().next().expect("plans have a part").job;
            super::super::recorder::spawn_fake(dir, self.script, job).map(Session::single)
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
        assert_eq!(r.recording(), Some(path.as_path()));
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
    fn no_frame_before_recording() {
        assert_eq!(Recorder::default().frame(), None);
    }

    #[test]
    fn the_frame_shows_the_recorded_area() {
        let mut env = Fake::new();
        let mut r = Recorder::default();
        r.run(start(), &mut env);
        let on_screen = r.frame().expect("a recording runs");
        assert_eq!(on_screen.areas, [env.jobs[0].area]);
    }

    #[test]
    fn the_frame_says_what_is_recorded() {
        let mut env = Fake::new();
        let mut r = Recorder::default();
        r.run(start(), &mut env);
        assert_eq!(
            r.frame().expect("a recording runs").summary,
            "MP4 · AV1 · 144 fps"
        );
    }

    #[test]
    fn the_frame_goes_with_the_recording() {
        let mut env = Fake::new();
        let mut r = Recorder::default();
        r.run(start(), &mut env);
        r.run(Command::Stop, &mut env);
        assert_eq!(r.frame(), None);
    }

    #[test]
    fn discarding_reports_it() {
        let mut env = Fake::new();
        let mut r = Recorder::default();
        r.run(start(), &mut env);
        assert_eq!(r.run(Command::Discard, &mut env), Outcome::Discarded);
    }

    #[test]
    fn discarding_leaves_no_file() {
        let mut env = Fake::new();
        let mut r = Recorder::default();
        let Outcome::Started { path, .. } = r.run(start(), &mut env) else {
            panic!("the fake recorder starts");
        };
        r.run(Command::Discard, &mut env);
        assert!(!path.exists());
    }

    #[test]
    fn discarding_ends_the_recording() {
        let mut env = Fake::new();
        let mut r = Recorder::default();
        r.run(start(), &mut env);
        r.run(Command::Discard, &mut env);
        assert_eq!(r.recording(), None);
    }

    #[test]
    fn discard_without_a_recording() {
        assert_eq!(
            Recorder::default().run(Command::Discard, &mut Fake::new()),
            Outcome::NotRecording
        );
    }

    #[test]
    fn a_discarded_recording_answers_cancelled() {
        assert_eq!(Outcome::Discarded.reply().0, Reply::Cancelled);
    }

    #[test]
    fn stop_without_a_recording() {
        assert_eq!(
            Recorder::default().run(Command::Stop, &mut Fake::new()),
            Outcome::NotRecording
        );
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
