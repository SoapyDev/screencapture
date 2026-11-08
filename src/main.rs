mod cli;

#[cfg(all(target_os = "linux", not(any(feature = "wayland", feature = "x11"))))]
compile_error!("enable at least one of the `wayland` or `x11` features");

use std::process::ExitCode;

use clap::Parser;
use screencapture_capture::{job, save};
#[cfg(unix)]
use screencapture_core::daemon;
use screencapture_core::{capture, clipboard, config, memory, overlay, selection};
pub use screencapture_core::{trace, trace_restart};
use screencapture_recording as record;

use crate::{
    cli::{Cli, Plan},
    selection::Request,
};

fn main() -> ExitCode {
    trace_restart();
    if let Err(e) = memory::tune() {
        eprintln!("screencapture: memory tuning: {e:#}");
    }
    let cli = Cli::parse();
    let plan = cli.plan();
    if let Some(dir) = &cli.output {
        remember_folder(&plan, dir);
    }

    let result = match plan {
        Plan::List => list(),
        #[cfg(unix)]
        Plan::Daemon => daemon::bind(&daemon::socket_path()).map(run_daemon),
        #[cfg(not(unix))]
        Plan::Daemon => Err(anyhow::anyhow!("daemon mode needs a Unix system")),
        Plan::Capture(request) => capture(&request, &config::Config::load().section()),
        Plan::Record(command) => return record(command),
        Plan::Overlay(mode) => return overlay_mode(mode),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("screencapture: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The features built into this program, with their remembered settings, in the order they are
/// asked (and their actions shown).
fn features() -> Vec<Box<dyn overlay::feature::Feature>> {
    let config = config::Config::load();
    vec![
        Box::new(screencapture_editing::overlay::Editing::new(config.section())),
        #[cfg(target_os = "linux")]
        Box::new(record::overlay::Recordings::new(config.section())),
        Box::new(screencapture_capture::overlay::Capturing::new(config.section())),
    ]
}

/// The daemon, with what this program serves besides the overlay.
#[cfg(unix)]
fn run_daemon(listener: std::os::unix::net::UnixListener) {
    #[cfg(target_os = "linux")]
    overlay::run_daemon(listener, record::overlay::service::Recording::default(), features);
    #[cfg(not(target_os = "linux"))]
    overlay::run_daemon(listener, overlay::service::NoService, features);
}

/// An explicit `-o` folder becomes the remembered one: for videos when recording.
fn remember_folder(plan: &Plan, dir: &std::path::Path) {
    let dir = Some(std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf()));
    let config = config::Config::load();
    let saved = if matches!(plan, Plan::Record(_)) {
        let mut recording: record::settings::Recording = config.section();
        recording.video_dir = dir;
        config::remember(&recording)
    } else {
        let mut screenshots: save::Screenshots = config.section();
        screenshots.output_dir = dir;
        config::remember(&screenshots)
    };
    if let Err(e) = saved {
        eprintln!("screencapture: could not remember folder: {e:#}");
    }
}

/// Hand the request to a running daemon (instant), or run the overlay in this process.
fn overlay_mode(mode: selection::Mode) -> ExitCode {
    #[cfg(unix)]
    if let Some(reply) = daemon::request(&daemon::socket_path(), mode) {
        trace("daemon replied");
        return match reply {
            Ok(daemon::Reply::Saved(path)) => {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }
            Ok(daemon::Reply::Cancelled | daemon::Reply::NotRecording) => ExitCode::FAILURE,
            Ok(daemon::Reply::Busy) => {
                eprintln!("screencapture: a selection is already open");
                ExitCode::FAILURE
            }
            // Started from this overlay, or already running: either way, a recording runs.
            Ok(daemon::Reply::Recording(path)) => {
                eprintln!(
                    "screencapture: recording to {}; `screencapture record stop` ends it",
                    path.display()
                );
                ExitCode::SUCCESS
            }
            Ok(daemon::Reply::Error(e)) => {
                eprintln!("screencapture: {e}");
                ExitCode::FAILURE
            }
            Err(e) => {
                eprintln!("screencapture: daemon: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    // Freeze the screens now, overlapping gpui start-up.
    let frozen = std::thread::spawn(overlay::freeze);
    if overlay::run(mode, frozen, features) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn list() -> anyhow::Result<()> {
    let mut monitors = capture::monitors()?;
    monitors.sort_by_key(|m| (m.rect.x, m.rect.y));
    for m in monitors {
        println!("{}\t{},{} {}x{}", m.name, m.rect.x, m.rect.y, m.rect.w, m.rect.h);
    }
    Ok(())
}

/// Hand a recording command to the daemon; with none running, record in this process.
fn record(command: record::command::Command) -> ExitCode {
    #[cfg(target_os = "linux")]
    {
        use record::command::Command;
        let socket = daemon::socket_path();
        let answer = match daemon::send(&socket, &daemon::Request::Feature(command.clone())) {
            Some(answer) => answer,
            None if command == Command::Stop => Ok(daemon::Answer {
                reply: daemon::Reply::NotRecording,
                notes: Vec::new(),
            }),
            None => record::overlay::launch::with_recording_frame(move |show| {
                record::foreground::run(command, &socket, show)
            })
            .map(|outcome| {
                let (reply, notes) = outcome.reply();
                daemon::Answer { reply, notes }
            }),
        };
        report_record(answer)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = command;
        eprintln!("screencapture: recording needs Linux");
        ExitCode::FAILURE
    }
}

#[cfg(target_os = "linux")]
fn report_record(answer: anyhow::Result<daemon::Answer>) -> ExitCode {
    use daemon::Reply;
    let answer = match answer {
        Ok(answer) => answer,
        Err(e) => {
            eprintln!("screencapture: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    for note in &answer.notes {
        eprintln!("screencapture: {note}");
    }
    match answer.reply {
        Reply::Recording(path) => {
            eprintln!("screencapture: recording to {}", path.display());
            ExitCode::SUCCESS
        }
        Reply::Saved(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Reply::NotRecording => {
            eprintln!("screencapture: no recording is running");
            ExitCode::FAILURE
        }
        Reply::Busy => {
            // A start while recording already said why in a note.
            if answer.notes.is_empty() {
                eprintln!("screencapture: a selection is open; close it first");
            }
            ExitCode::FAILURE
        }
        Reply::Error(e) => {
            eprintln!("screencapture: {e}");
            ExitCode::FAILURE
        }
        // Only a discarded recording answers this way.
        Reply::Cancelled => {
            eprintln!("screencapture: recording discarded");
            ExitCode::SUCCESS
        }
    }
}

/// No-UI path: nothing but capture and encode.
fn capture(request: &Request, screenshots: &save::Screenshots) -> anyhow::Result<()> {
    let saved = job::run(request, &screenshots.dir(None))?;
    trace("saved");
    clipboard::copy_png(saved.png);
    println!("{}", saved.path.display());
    Ok(())
}
