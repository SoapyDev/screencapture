//! Recording without the daemon: this process records, and takes the daemon's place on the
//! socket until the recording ends, so `screencapture record stop` (and Print, which is told a
//! recording runs) work the same.

use std::{
    os::unix::net::UnixListener,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::{Env, Outcome, Recorder, command::Command};
use crate::daemon::{self, Reply, Request};

/// The recorded areas (one per screen) and the screens, while recording; `None` once it ended.
pub type ShowFrame<'a> = &'a mut dyn FnMut(Option<&super::OnScreen>);

/// Record `command` (a start), in this process, until told to stop. Prints how to stop it.
pub fn run(command: Command, socket: &Path, show_frame: ShowFrame) -> anyhow::Result<Outcome> {
    let listener = daemon::bind(socket)?;
    let stop = stop_on_signals()?;
    let mut recorder = Recorder::default();
    let outcome = match recorder.run(command, &mut super::LiveEnv) {
        Outcome::Started { path, notes } => {
            for note in notes {
                eprintln!("screencapture: {note}");
            }
            eprintln!(
                "screencapture: recording to {} (Ctrl+C or `screencapture record stop` ends it)",
                path.display()
            );
            show_frame(recorder.frame());
            let (ended, client) = serve(&listener, &mut recorder, &mut super::LiveEnv, &stop);
            // The frame goes first: joining screens takes a while.
            show_frame(None);
            let ended = ended.finished();
            if let Some(client) = client {
                ended.clone().answer(client);
            }
            ended
        }
        other => other,
    };
    let _ = std::fs::remove_file(socket);
    Ok(outcome)
}

/// A flag that Ctrl+C, SIGTERM and SIGHUP raise (instead of killing the process).
fn stop_on_signals() -> std::io::Result<Arc<AtomicBool>> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [SIGINT, SIGTERM, SIGHUP] {
        signal_hook::flag::register(signal, stop.clone())?;
    }
    Ok(stop)
}

/// Answer clients while the recording runs; returns how it ended, and the client that ended it
/// (answered once the file is finished).
fn serve(
    listener: &UnixListener,
    recorder: &mut Recorder,
    env: &mut impl Env,
    stop: &AtomicBool,
) -> (Outcome, Option<daemon::Pending>) {
    loop {
        if stop.load(Ordering::Relaxed) {
            return (recorder.run(Command::Stop, env), None);
        }
        if let Some(outcome) = recorder.ended_by_itself() {
            return (outcome, None);
        }
        if !daemon::readable_within(listener, Duration::from_millis(100)) {
            continue;
        }
        let Some(pending) = daemon::accept(listener) else {
            continue;
        };
        let (outcome, ends) = match pending.request.clone() {
            Request::Record(command @ Command::Start(..)) => (recorder.run(command, env), false),
            Request::Record(Command::Stop | Command::Toggle(..)) => (recorder.run(Command::Stop, env), true),
            Request::Record(Command::Discard) => (recorder.run(Command::Discard, env), true),
            Request::Overlay(_) => {
                pending.reply(&Reply::Recording(
                    recorder
                        .recording()
                        .map_or_else(Default::default, Path::to_path_buf),
                ));
                continue;
            }
        };
        if ends {
            return (outcome, Some(pending));
        }
        outcome.answer(pending);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{control::testing::Fake, plan::Target, settings::Overrides};

    fn recording() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        UnixListener,
        Recorder,
        Fake,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("run").join("d.sock");
        let listener = daemon::bind(&socket).unwrap();
        let mut env = Fake::new();
        let mut recorder = Recorder::default();
        let started = recorder.run(Command::Start(Target::Focused, Overrides::default()), &mut env);
        assert!(matches!(started, Outcome::Started { .. }));
        (dir, socket, listener, recorder, env)
    }

    #[test]
    fn a_stop_request_ends_it_and_gets_the_file() {
        let (_dir, socket, listener, mut recorder, mut env) = recording();
        let path = recorder
            .recording()
            .expect("a recording was just started")
            .to_path_buf();
        let client = std::thread::spawn(move || {
            let print = daemon::request(&socket, crate::selection::Mode::Region)
                .unwrap()
                .unwrap();
            let start = daemon::send(
                &socket,
                &Request::Record(Command::Start(Target::All, Overrides::default())),
            );
            let stop = daemon::send(&socket, &Request::Record(Command::Stop))
                .unwrap()
                .unwrap();
            (print, start.unwrap().unwrap().reply, stop.reply)
        });
        let (outcome, waiting) = serve(&listener, &mut recorder, &mut env, &AtomicBool::new(false));
        let outcome = outcome.finished();
        outcome.clone().answer(waiting.expect("the stop's client waits"));
        let (print, start, stop) = client.join().unwrap();
        assert_eq!(
            print,
            Reply::Recording(path.clone()),
            "Print is told a recording runs"
        );
        assert_eq!(start, Reply::Busy);
        assert_eq!(stop, Reply::Saved(path.clone()));
        assert_eq!(outcome, Outcome::Saved(path.clone()));
        assert!(path.exists());
    }

    #[test]
    fn the_stop_client_waits_for_the_file_to_be_finished() {
        let (_dir, socket, listener, mut recorder, mut env) = recording();
        let client = std::thread::spawn(move || daemon::send(&socket, &Request::Record(Command::Stop)));
        let (_, waiting) = serve(&listener, &mut recorder, &mut env, &AtomicBool::new(false));
        let handed_back = waiting.is_some();
        drop(waiting);
        let _ = client.join();
        assert!(handed_back, "serve answered the stop itself");
    }

    #[test]
    fn stop_signals_raise_the_stop_flag() {
        let stop = stop_on_signals().unwrap();
        assert!(!stop.load(Ordering::Relaxed));
        // SIGHUP: SIGINT would also reach a terminal's test runner if this went wrong.
        signal_hook::low_level::raise(signal_hook::consts::SIGHUP).unwrap();
        assert!(stop.load(Ordering::Relaxed));
    }

    #[test]
    fn a_discard_request_ends_it() {
        let (_dir, socket, listener, mut recorder, mut env) = recording();
        let client = std::thread::spawn(move || daemon::send(&socket, &Request::Record(Command::Discard)));
        let (outcome, waiting) = serve(&listener, &mut recorder, &mut env, &AtomicBool::new(false));
        drop(waiting);
        let _ = client.join();
        assert_eq!(outcome, Outcome::Discarded);
    }

    #[test]
    fn a_signal_ends_it() {
        let (_dir, _socket, listener, mut recorder, mut env) = recording();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
        });
        assert!(matches!(
            serve(&listener, &mut recorder, &mut env, &stop).0.finished(),
            Outcome::Saved(_)
        ));
    }

    #[test]
    fn a_recorder_quitting_by_itself_ends_it() {
        let dir = tempfile::tempdir().unwrap();
        let listener = daemon::bind(&dir.path().join("run").join("d.sock")).unwrap();
        let mut env = Fake::new();
        env.script = r#"out="$1"; echo data > "$out"; : > "$out.ts"; sleep 0.1"#;
        let mut recorder = Recorder::default();
        recorder.run(Command::Start(Target::Focused, Overrides::default()), &mut env);
        assert!(matches!(
            serve(&listener, &mut recorder, &mut env, &AtomicBool::new(false))
                .0
                .finished(),
            Outcome::Saved(_)
        ));
    }
}
