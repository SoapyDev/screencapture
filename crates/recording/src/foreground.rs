//! Recording without the daemon: this process records.
//!
//! It takes the daemon's place on the socket until the recording ends, so `screencapture record stop` (and Print, which is told a
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
use screencapture_core::daemon::{self, Client, Reply, Request};

/// The recorded areas (one per screen) and the screens, while recording; `None` once it ended.
pub type ShowFrame<'a> = &'a mut dyn FnMut(Option<&super::OnScreen>);

/// Record `command` (a start), in this process, until told to stop. Prints how to stop it.
///
/// # Errors
///
/// When another process holds `socket`, or the stop signals can't be caught.
pub fn run(command: Command, socket: &Path, show_frame: ShowFrame) -> anyhow::Result<Outcome> {
    let listener = daemon::bind(socket)?;
    let stop = stop_on_signals()?;
    let mut recorder = Recorder::default();
    let outcome = match recorder.run(command, &mut super::LiveEnv) {
        Outcome::Started { path, notes } => {
            tell_how_to_stop(&path, &notes);
            record_until_stopped(&listener, &mut recorder, &stop, show_frame)
        }
        other => other,
    };
    let _ = std::fs::remove_file(socket);
    Ok(outcome)
}

fn tell_how_to_stop(path: &Path, notes: &[String]) {
    for note in notes {
        eprintln!("screencapture: {note}");
    }
    let path = path.display();
    eprintln!("screencapture: recording to {path} (Ctrl+C or `screencapture record stop` ends it)");
}

/// Show the frame and answer clients until the recording ends; then finish the file.
fn record_until_stopped(
    listener: &UnixListener,
    recorder: &mut Recorder,
    stop: &AtomicBool,
    show_frame: ShowFrame,
) -> Outcome {
    show_frame(recorder.frame());
    let (ended, client) = serve(listener, recorder, &mut super::LiveEnv, stop);
    // The frame goes first: joining screens takes a while.
    show_frame(None);
    let ended = ended.finished();
    if let Some(client) = client {
        ended.clone().answer(client);
    }
    ended
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
) -> (Outcome, Option<Client>) {
    loop {
        if let Some(ended) = ended_meanwhile(recorder, env, stop) {
            return (ended, None);
        }
        let Some(pending) = next_request(listener) else {
            continue;
        };
        if let Some((ended, client)) = handle(pending, recorder, env) {
            return (ended, Some(client));
        }
    }
}

/// A stop signal arrived, or the recorder quit by itself: how the recording ended.
fn ended_meanwhile(recorder: &mut Recorder, env: &mut impl Env, stop: &AtomicBool) -> Option<Outcome> {
    if stop.load(Ordering::Relaxed) {
        return Some(recorder.run(Command::Stop, env));
    }
    recorder.ended_by_itself()
}

/// The next client's request, if one knocks soon (so signals are noticed meanwhile).
fn next_request(listener: &UnixListener) -> Option<daemon::Pending<Command>> {
    if !daemon::readable_within(listener, Duration::from_millis(100)) {
        return None;
    }
    daemon::accept(listener)
}

/// One request while recording: the outcome and its client if it ends the recording.
fn handle(
    pending: daemon::Pending<Command>,
    recorder: &mut Recorder,
    env: &mut impl Env,
) -> Option<(Outcome, Client)> {
    let daemon::Pending { request, client } = pending;
    let command = match request {
        Request::Feature(command) => command,
        Request::Overlay(_) => {
            let path = recorder
                .recording()
                .map_or_else(Default::default, Path::to_path_buf);
            client.reply(&Reply::Recording(path));
            return None;
        }
    };
    let ends = !matches!(command, Command::Start(..));
    // Toggling a running recording stops it.
    let command = if matches!(command, Command::Toggle(..)) {
        Command::Stop
    } else {
        command
    };
    let outcome = recorder.run(command, env);
    if ends {
        return Some((outcome, client));
    }
    outcome.answer(client);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{control::testing::Fake, plan::Target, settings::Overrides};

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
            let print = daemon::request(&socket, screencapture_core::selection::Mode::Region)
                .expect("this process answers on the socket")
                .expect("it answers")
                .reply;
            let start = daemon::send(
                &socket,
                &Request::Feature(Command::Start(Target::All, Overrides::default())),
            );
            let stop = daemon::send(&socket, &Request::Feature(Command::Stop))
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
        let client = std::thread::spawn(move || daemon::send(&socket, &Request::Feature(Command::Stop)));
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
        let client = std::thread::spawn(move || daemon::send(&socket, &Request::Feature(Command::Discard)));
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
