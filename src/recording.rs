//! `screencapture record …`: hand the command to the daemon, or record in this process.

use std::process::ExitCode;

use crate::record::command::Command;

/// Hand a recording command to the daemon; with none running, record in this process.
#[cfg(target_os = "linux")]
pub fn record(command: Command) -> ExitCode {
    use screencapture_core::daemon;
    let socket = daemon::socket_path();
    let answer = daemon::send(&socket, &daemon::Request::Feature(command.clone()))
        .unwrap_or_else(|| without_daemon(command, &socket));
    report(answer)
}

#[cfg(not(target_os = "linux"))]
pub fn record(_: Command) -> ExitCode {
    crate::failure("recording needs Linux")
}

/// No daemon: a stop or discard finds nothing to end; a start records here.
#[cfg(target_os = "linux")]
fn without_daemon(
    command: Command,
    socket: &std::path::Path,
) -> anyhow::Result<screencapture_core::daemon::Answer> {
    use screencapture_core::daemon::{Answer, Reply};
    if !command.starts() {
        let reply = Reply::NotRecording;
        return Ok(Answer {
            reply,
            notes: Vec::new(),
        });
    }
    let socket = socket.to_path_buf();
    let outcome = crate::record::overlay::launch::with_recording_frame(move |show| {
        crate::record::foreground::run(command, &socket, show)
    })?;
    let (reply, notes) = outcome.reply();
    Ok(Answer { reply, notes })
}

/// Tell the user what came of it: notes and errors on stderr, a saved file on stdout.
#[cfg(target_os = "linux")]
fn report(answer: anyhow::Result<screencapture_core::daemon::Answer>) -> ExitCode {
    let answer = match answer {
        Ok(answer) => answer,
        Err(e) => return crate::failure(&format!("{e:#}")),
    };
    for note in &answer.notes {
        eprintln!("screencapture: {note}");
    }
    report_reply(answer.reply, !answer.notes.is_empty())
}

/// What the recorder answered; `noted`: a note already said why.
#[cfg(target_os = "linux")]
fn report_reply(reply: screencapture_core::daemon::Reply, noted: bool) -> ExitCode {
    use screencapture_core::daemon::Reply;
    match reply {
        Reply::Recording(path) => succeed(&format!("recording to {}", path.display())),
        Reply::Saved(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Reply::NotRecording => crate::failure("no recording is running"),
        // A start while recording already said why in a note.
        Reply::Busy if noted => ExitCode::FAILURE,
        Reply::Busy => crate::failure("a selection is open; close it first"),
        Reply::Error(e) => crate::failure(&e),
        // Only a discarded recording answers this way.
        Reply::Cancelled => succeed("recording discarded"),
    }
}

/// Say `what` happened on stderr, and succeed.
#[cfg(target_os = "linux")]
fn succeed(what: &str) -> ExitCode {
    eprintln!("screencapture: {what}");
    ExitCode::SUCCESS
}
