mod annotate;
mod capture;
mod cli;
mod clipboard;
#[cfg(unix)]
mod daemon;
#[cfg(unix)]
mod hyprland;
mod job;
mod memory;
mod overlay;
mod save;
mod selection;
mod text;

use std::{
    process::ExitCode,
    sync::{Mutex, OnceLock},
    time::Instant,
};

use clap::Parser;

use crate::{
    cli::{Cli, Plan},
    save::Config,
    selection::Request,
};

static START: Mutex<Option<Instant>> = Mutex::new(None);

/// With `SCREENCAPTURE_TRACE=1`, print milliseconds since launch (or, in the daemon, since the
/// request arrived) for each milestone.
pub fn trace(what: &str) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("SCREENCAPTURE_TRACE").is_some()) {
        let start = *START.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ms = start.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
        eprintln!("[{ms:7.1} ms] {what}");
    }
}

pub fn trace_restart() {
    *START.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
}

fn main() -> ExitCode {
    trace_restart();
    memory::tune();
    let cli = Cli::parse();
    let mut config = Config::load();
    if let Some(dir) = &cli.output {
        // An explicit folder becomes the remembered one.
        config.output_dir = Some(std::path::absolute(dir).unwrap_or_else(|_| dir.clone()));
        if let Err(e) = config.save() {
            eprintln!("screencapture: could not remember folder: {e:#}");
        }
    }

    let result = match cli.plan() {
        Plan::List => list(),
        #[cfg(unix)]
        Plan::Daemon => daemon::bind(&daemon::socket_path()).map(overlay::run_daemon),
        #[cfg(not(unix))]
        Plan::Daemon => Err(anyhow::anyhow!("daemon mode needs a Unix system")),
        Plan::Capture(request) => capture(&request, &config),
        Plan::Overlay(mode) => return overlay_mode(mode, config),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("screencapture: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Hand the request to a running daemon (instant), or run the overlay in this process.
fn overlay_mode(mode: selection::Mode, config: Config) -> ExitCode {
    #[cfg(unix)]
    if let Some(reply) = daemon::request(&daemon::socket_path(), mode) {
        trace("daemon replied");
        return match reply {
            Ok(daemon::Reply::Saved(path)) => {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }
            Ok(daemon::Reply::Cancelled) => ExitCode::FAILURE,
            Ok(daemon::Reply::Busy) => {
                eprintln!("screencapture: a selection is already open");
                ExitCode::FAILURE
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
    if overlay::run(mode, frozen, config) {
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

/// No-UI path: nothing but capture and encode.
fn capture(request: &Request, config: &Config) -> anyhow::Result<()> {
    let saved = job::run(request, &config.output_dir(None))?;
    trace("saved");
    clipboard::copy_png(saved.png);
    println!("{}", saved.path.display());
    Ok(())
}
