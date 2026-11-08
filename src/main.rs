mod cli;
#[cfg(feature = "recording")]
mod recording;

#[cfg(all(target_os = "linux", not(any(feature = "wayland", feature = "x11"))))]
compile_error!("enable at least one of the `wayland` or `x11` features");

use std::process::ExitCode;

use clap::Parser;
#[cfg(unix)]
use screencapture_core::daemon;
use screencapture_core::{capture, config, memory, overlay, selection};
pub use screencapture_core::{trace, trace_restart};
#[cfg(feature = "recording")]
use screencapture_recording as record;

use crate::cli::{Cli, Plan};

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
    run(plan)
}

/// Do what the command line asks.
#[cfg_attr(
    not(any(feature = "capture", feature = "recording")),
    expect(clippy::needless_pass_by_value, reason = "only their plans carry anything")
)]
fn run(plan: Plan) -> ExitCode {
    let done = match plan {
        Plan::List => list(),
        Plan::Daemon => daemon_mode(),
        #[cfg(feature = "capture")]
        Plan::Capture(request) => capture(&request),
        #[cfg(feature = "recording")]
        Plan::Record(command) => return recording::record(command),
        Plan::Overlay(mode) => return overlay_mode(mode),
        #[cfg(not(feature = "capture"))]
        Plan::Missing(what) => Err(anyhow::anyhow!("this screencapture was built without {what}")),
    };
    exit_code(done)
}

fn exit_code(done: anyhow::Result<()>) -> ExitCode {
    match done {
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
    #[cfg(not(any(feature = "editing", feature = "capture", feature = "recording")))]
    let _ = config;
    vec![
        #[cfg(feature = "editing")]
        Box::new(screencapture_editing::overlay::Editing::new(config.section())),
        #[cfg(feature = "capture")]
        Box::new(screencapture_capture::overlay::Capturing::new(config.section())),
        #[cfg(all(feature = "recording", target_os = "linux"))]
        Box::new(record::overlay::Recordings::new(config.section())),
    ]
}

/// The daemon, with what this program serves besides the overlay.
#[cfg(unix)]
fn daemon_mode() -> anyhow::Result<()> {
    let listener = daemon::bind(&daemon::socket_path())?;
    #[cfg(all(feature = "recording", target_os = "linux"))]
    overlay::run_daemon(listener, record::overlay::service::Recording::default(), features);
    #[cfg(not(all(feature = "recording", target_os = "linux")))]
    overlay::run_daemon(listener, overlay::service::NoService, features);
    Ok(())
}

#[cfg(not(unix))]
fn daemon_mode() -> anyhow::Result<()> {
    anyhow::bail!("daemon mode needs a Unix system")
}

/// An explicit `-o` folder becomes the remembered one: for videos when recording.
fn remember_folder(plan: &Plan, dir: &std::path::Path) {
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    if let Err(e) = remember_dir(plan, dir) {
        eprintln!("screencapture: could not remember folder: {e:#}");
    }
}

#[cfg_attr(
    not(any(feature = "capture", feature = "recording")),
    expect(clippy::unnecessary_wraps, reason = "nothing is saved into a folder")
)]
fn remember_dir(plan: &Plan, dir: std::path::PathBuf) -> anyhow::Result<()> {
    let config = config::Config::load();
    match plan {
        #[cfg(feature = "recording")]
        Plan::Record(_) => {
            let mut recording: record::settings::Recording = config.section();
            recording.video_dir = Some(dir);
            config::remember(&recording)
        }
        #[cfg(feature = "capture")]
        _ => {
            let mut screenshots: screencapture_capture::save::Screenshots = config.section();
            screenshots.output_dir = Some(dir);
            config::remember(&screenshots)
        }
        #[cfg(not(feature = "capture"))]
        _ => {
            let _ = (config, dir);
            Ok(())
        }
    }
}

/// Hand the request to a running daemon (instant), or run the overlay in this process.
fn overlay_mode(mode: selection::Mode) -> ExitCode {
    #[cfg(unix)]
    if let Some(answer) = daemon::request(&daemon::socket_path(), mode) {
        trace("daemon replied");
        return report_overlay(answer);
    }
    // Freeze the screens now, overlapping gpui start-up.
    let frozen = std::thread::spawn(overlay::freeze);
    if overlay::run(mode, frozen, features) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// What the daemon's overlay came to, after what the user should hear about.
#[cfg(unix)]
fn report_overlay(answer: anyhow::Result<daemon::Answer>) -> ExitCode {
    report_overlay_reply(answer.map(told_notes))
}

/// `answer`'s reply, once its notes are on stderr.
#[cfg(unix)]
fn told_notes(answer: daemon::Answer) -> daemon::Reply {
    for note in &answer.notes {
        eprintln!("screencapture: {note}");
    }
    answer.reply
}

/// What the daemon's overlay replied.
#[cfg(unix)]
fn report_overlay_reply(reply: anyhow::Result<daemon::Reply>) -> ExitCode {
    use daemon::Reply;
    match reply {
        Ok(Reply::Saved(path)) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Ok(Reply::Cancelled | Reply::NotRecording) => ExitCode::FAILURE,
        Ok(Reply::Busy) => failure("a selection is already open"),
        // Started from this overlay, or already running: either way, a recording runs.
        Ok(Reply::Recording(path)) => {
            let path = path.display();
            eprintln!("screencapture: recording to {path}; `screencapture record stop` ends it");
            ExitCode::SUCCESS
        }
        Ok(Reply::Error(e)) => failure(&e),
        Err(e) => failure(&format!("daemon: {e:#}")),
    }
}

/// Say `why` on stderr, and fail.
fn failure(why: &str) -> ExitCode {
    eprintln!("screencapture: {why}");
    ExitCode::FAILURE
}

fn list() -> anyhow::Result<()> {
    let mut monitors = capture::monitors()?;
    monitors.sort_by_key(|m| (m.rect.x, m.rect.y));
    for m in monitors {
        println!("{}\t{},{} {}x{}", m.name, m.rect.x, m.rect.y, m.rect.w, m.rect.h);
    }
    Ok(())
}

/// No-UI path: nothing but capture and encode.
#[cfg(feature = "capture")]
fn capture(request: &selection::Request) -> anyhow::Result<()> {
    let screenshots: screencapture_capture::save::Screenshots = config::Config::load().section();
    let saved = screencapture_capture::job::run(request, &screenshots.dir(None))?;
    trace("saved");
    screencapture_core::clipboard::copy_png(saved.png);
    println!("{}", saved.path.display());
    Ok(())
}
