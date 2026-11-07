//! Resident mode. gpui's first window costs ~200 ms (GPU device and pipeline setup), later
//! ones ~15 ms. `screencapture daemon` pays that once; later invocations hand their request
//! to it over a Unix socket so the overlay appears almost immediately.
//!
//! Protocol: the client sends one line (`region`, `screens`, or `record <command>`), the daemon
//! answers with any number of `note <text>` lines, then one reply line when done: `saved <path>`,
//! `cancelled`, `busy`, `recording <path>`, `not-recording` or `error <message>`.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::selection::Mode;

#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Saved(PathBuf),
    Cancelled,
    Busy,
    /// A recording runs into this file (just started, or the reason nothing else can start).
    Recording(PathBuf),
    NotRecording,
    Error(String),
}

/// What a client asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Overlay(Mode),
    Record(crate::record::command::Command),
}

impl Request {
    pub fn encode(&self) -> String {
        match self {
            Self::Overlay(mode) => encode_mode(*mode).into(),
            Self::Record(command) => format!("record {}", command.encode()),
        }
    }

    pub fn parse(line: &str) -> Option<Self> {
        match line.trim().split_once(' ') {
            Some(("record", command)) => crate::record::command::Command::parse(command).map(Self::Record),
            _ => parse_mode(line).map(Self::Overlay),
        }
    }
}

/// A reply and the notes that came before it.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    pub reply: Reply,
    pub notes: Vec<String>,
}

pub const fn encode_mode(mode: Mode) -> &'static str {
    match mode {
        Mode::Region => "region",
        Mode::Screens => "screens",
    }
}

pub fn parse_mode(line: &str) -> Option<Mode> {
    match line.trim() {
        "region" => Some(Mode::Region),
        "screens" => Some(Mode::Screens),
        _ => None,
    }
}

impl Reply {
    pub fn encode(&self) -> String {
        match self {
            Self::Saved(path) => format!("saved {}", path.display()),
            Self::Cancelled => "cancelled".into(),
            Self::Busy => "busy".into(),
            Self::Recording(path) => format!("recording {}", path.display()),
            Self::NotRecording => "not-recording".into(),
            // One line per reply.
            Self::Error(message) => format!("error {}", message.replace('\n', " ")),
        }
    }

    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches(['\r', '\n']);
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        Some(match word {
            "saved" if !rest.is_empty() => Self::Saved(rest.into()),
            "cancelled" => Self::Cancelled,
            "busy" => Self::Busy,
            "recording" if !rest.is_empty() => Self::Recording(rest.into()),
            "not-recording" => Self::NotRecording,
            "error" => Self::Error(rest.into()),
            _ => return None,
        })
    }
}

/// `$XDG_RUNTIME_DIR/screencapture.sock`: per user, not readable by others.
pub fn socket_path() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("screencapture-{}", rustix::process::getuid().as_raw()))
        })
        .join("screencapture.sock")
}

/// Ask a running daemon to show the overlay. `None` when no daemon listens at `path`.
pub fn request(path: &Path, mode: Mode) -> Option<Result<Reply>> {
    send(path, &Request::Overlay(mode)).map(|answer| answer.map(|a| a.reply))
}

/// Hand `request` to a running daemon and wait for its answer. `None` when no daemon listens
/// at `path`.
pub fn send(path: &Path, request: &Request) -> Option<Result<Answer>> {
    // Anyone who could write that directory could be impersonating the daemon.
    check_private_dir(path.parent()?).ok()?;
    let mut stream = UnixStream::connect(path).ok()?;
    Some((|| {
        writeln!(stream, "{}", request.encode())?;
        let mut notes = Vec::new();
        for line in BufReader::new(stream).lines() {
            let line = line?;
            if let Some(note) = line.strip_prefix("note ") {
                notes.push(note.to_string());
                continue;
            }
            let reply = Reply::parse(&line)
                .ok_or_else(|| anyhow::anyhow!("daemon sent an unexpected reply: {line:?}"))?;
            return Ok(Answer { reply, notes });
        }
        anyhow::bail!("the daemon hung up without answering")
    })())
}

/// A request received by the daemon; answer it with [`Pending::reply`].
pub struct Pending {
    pub request: Request,
    stream: UnixStream,
}

impl Pending {
    /// Something the user should hear about, ahead of the reply.
    pub fn note(&mut self, text: &str) {
        let _ = writeln!(self.stream, "note {}", text.replace('\n', " "));
    }

    pub fn reply(mut self, reply: &Reply) {
        let _ = writeln!(self.stream, "{}", reply.encode());
    }
}

/// Make sure `dir` exists, is a real directory (not a symlink) owned by us, and is closed to
/// everyone else; create it `0700` if missing. The socket inside is only as safe as this.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if std::fs::symlink_metadata(dir).is_err() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        // The umask may have taken bits away, never added; make it exact anyway.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    check_private_dir(dir)
}

fn check_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(dir)?;
    let ours = meta.uid() == rustix::process::getuid().as_raw();
    if !meta.is_dir() || !ours || meta.mode() & 0o077 != 0 {
        anyhow::bail!(
            "{} must be a directory owned by you and closed to others (chmod 700)",
            dir.display()
        );
    }
    Ok(())
}

/// Bind the socket, replacing a stale one. Fails if another daemon is alive.
pub fn bind(path: &Path) -> Result<UnixListener> {
    use anyhow::Context;
    if UnixStream::connect(path).is_ok() {
        anyhow::bail!("a screencapture daemon is already running ({})", path.display());
    }
    if let Some(dir) = path.parent() {
        ensure_private_dir(dir)?;
    }
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).with_context(|| format!("listening on {}", path.display()))
}

/// Long enough for any real client to send its one line.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Accept requests forever, handing each to `handle` (malformed ones are answered here). After
/// `idle` without a request, `on_idle` runs, once per quiet spell. `watch` runs before every
/// wait and says when it wants to run again (`None`: only after the next request).
pub fn serve(
    listener: &UnixListener,
    idle: Option<std::time::Duration>,
    mut on_idle: impl FnMut(),
    mut watch: impl FnMut() -> Option<std::time::Duration>,
    mut handle: impl FnMut(Pending),
) {
    let mut idled = false;
    let mut quiet_since = std::time::Instant::now();
    loop {
        let check = watch();
        let idle_left = idle
            .filter(|_| !idled)
            .map(|after| (quiet_since + after).saturating_duration_since(std::time::Instant::now()));
        let wait = match (idle_left, check) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if let Some(wait) = wait
            && !readable_within(listener, wait)
        {
            if let Some(after) = idle
                && !idled
                && quiet_since.elapsed() >= after
            {
                on_idle();
                idled = true;
            }
            continue;
        }
        let Some(pending) = accept(listener) else { continue };
        idled = false;
        quiet_since = std::time::Instant::now();
        handle(pending);
    }
}

/// Take the next client's request (blocks until one connects); malformed ones are answered
/// here and give `None`.
pub fn accept(listener: &UnixListener) -> Option<Pending> {
    let (stream, _) = listener.accept().ok()?;
    let mut line = String::new();
    let reader = stream.try_clone().ok()?;
    // A client that connects and says nothing must not hold up everyone else.
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    BufReader::new(reader).read_line(&mut line).ok()?;
    let _ = stream.set_read_timeout(None);
    if let Some(request) = Request::parse(&line) {
        return Some(Pending { request, stream });
    }
    Pending {
        request: Request::Overlay(Mode::Region),
        stream,
    }
    .reply(&Reply::Error(format!("unknown request {line:?}")));
    None
}

/// Whether a client knocks on `listener` within `wait`.
pub fn readable_within(listener: &UnixListener, wait: std::time::Duration) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let deadline = std::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let timeout = Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos().into(),
        };
        let mut fds = [PollFd::new(listener, PollFlags::IN)];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => return false,
            Err(rustix::io::Errno::INTR) => {}
            // Ready, or a poll error: let accept() report it.
            _ => return true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip() {
        for mode in [Mode::Region, Mode::Screens] {
            assert_eq!(parse_mode(encode_mode(mode)), Some(mode));
        }
        assert_eq!(parse_mode("region\n"), Some(Mode::Region));
        assert_eq!(parse_mode("nonsense"), None);
    }

    #[test]
    fn requests_round_trip() {
        use crate::record::{command::Command, plan::Target, settings::Overrides};
        for request in [
            Request::Overlay(Mode::Region),
            Request::Overlay(Mode::Screens),
            Request::Record(Command::Stop),
            Request::Record(Command::Start(Target::All, Overrides::default())),
        ] {
            assert_eq!(Request::parse(&request.encode()), Some(request));
        }
        assert_eq!(
            encode_mode(Mode::Region),
            Request::Overlay(Mode::Region).encode(),
            "old clients"
        );
        assert_eq!(Request::parse("record dance"), None);
        assert_eq!(Request::parse("dance"), None);
    }

    #[test]
    fn notes_arrive_before_the_reply() {
        use crate::record::command::Command;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("d.sock");
        let listener = bind(&path).unwrap();
        std::thread::spawn(move || {
            serve(
                &listener,
                None,
                || {},
                || None,
                |mut pending| {
                    assert_eq!(pending.request, Request::Record(Command::Stop));
                    pending.note("first");
                    pending.note("two\nlines");
                    pending.reply(&Reply::NotRecording);
                },
            );
        });
        let answer = send(&path, &Request::Record(Command::Stop)).unwrap().unwrap();
        assert_eq!(
            answer,
            Answer {
                reply: Reply::NotRecording,
                notes: vec!["first".into(), "two lines".into()],
            }
        );
    }

    #[test]
    fn replies_round_trip() {
        for reply in [
            Reply::Saved("/tmp/a b/screenshot.png".into()),
            Reply::Cancelled,
            Reply::Busy,
            Reply::Recording("/v/a b.mp4".into()),
            Reply::NotRecording,
            Reply::Error("it broke".into()),
        ] {
            assert_eq!(Reply::parse(&reply.encode()), Some(reply));
        }
        assert_eq!(Reply::parse("what"), None);
    }

    #[test]
    fn no_daemon_means_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(request(&dir.path().join("none.sock"), Mode::Region).is_none());
    }

    #[test]
    fn client_and_daemon_talk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("d.sock");
        let listener = bind(&path).unwrap();
        std::thread::spawn(move || {
            serve(
                &listener,
                None,
                || {},
                || None,
                |pending| {
                    let reply = match pending.request {
                        Request::Overlay(Mode::Region) => Reply::Saved("/x.png".into()),
                        _ => Reply::Cancelled,
                    };
                    pending.reply(&reply);
                },
            );
        });
        assert_eq!(
            request(&path, Mode::Region).unwrap().unwrap(),
            Reply::Saved("/x.png".into())
        );
        assert_eq!(request(&path, Mode::Screens).unwrap().unwrap(), Reply::Cancelled);
    }

    #[test]
    fn second_daemon_is_refused_but_stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("d.sock");
        let first = bind(&path).unwrap();
        assert!(bind(&path).is_err());
        drop(first); // socket file stays behind, nobody listens
        assert!(bind(&path).is_ok());
    }

    #[test]
    fn a_silent_client_does_not_block_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("d.sock");
        let listener = bind(&path).unwrap();
        std::thread::spawn(move || {
            serve(
                &listener,
                None,
                || {},
                || None,
                |pending| pending.reply(&Reply::Cancelled),
            );
        });
        let _silent = UnixStream::connect(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(request(&path, Mode::Region).unwrap().unwrap()));
        let reply = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("second client served");
        assert_eq!(reply, Reply::Cancelled);
    }

    #[test]
    fn each_quiet_spell_runs_the_idle_hook_once() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("d.sock");
        let listener = bind(&path).unwrap();
        let idled = Arc::new(AtomicUsize::new(0));
        let counter = idled.clone();
        std::thread::spawn(move || {
            serve(
                &listener,
                Some(std::time::Duration::from_millis(50)),
                move || {
                    counter.fetch_add(1, Ordering::Relaxed);
                },
                || None,
                |pending| pending.reply(&Reply::Cancelled),
            );
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(idled.load(Ordering::Relaxed), 1, "once, not every 50 ms");
        assert_eq!(request(&path, Mode::Region).unwrap().unwrap(), Reply::Cancelled);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(idled.load(Ordering::Relaxed), 2, "again after the next request");
    }

    #[test]
    fn the_watch_hook_runs_as_often_as_it_asks() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("d.sock");
        let listener = bind(&path).unwrap();
        let ticks = Arc::new(AtomicUsize::new(0));
        let counter = ticks.clone();
        std::thread::spawn(move || {
            serve(
                &listener,
                None,
                || {},
                // Every 20 ms for the first 5 calls, then no more.
                move || {
                    (counter.fetch_add(1, Ordering::Relaxed) < 5)
                        .then_some(std::time::Duration::from_millis(20))
                },
                |pending| pending.reply(&Reply::Cancelled),
            );
        });
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert_eq!(
            ticks.load(Ordering::Relaxed),
            6,
            "5 timed wake-ups, then it waits for clients"
        );
        assert_eq!(request(&path, Mode::Region).unwrap().unwrap(), Reply::Cancelled);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(ticks.load(Ordering::Relaxed), 7, "and once more after a request");
    }

    #[test]
    fn socket_dir_must_be_private_and_ours() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let fresh = root.path().join("fresh");
        ensure_private_dir(&fresh).unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o700
        );
        ensure_private_dir(&fresh).unwrap();

        let open = root.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ensure_private_dir(&open).is_err());
        assert!(bind(&open.join("d.sock")).is_err());

        let link = root.path().join("link");
        std::os::unix::fs::symlink(&fresh, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());

        let file = root.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(ensure_private_dir(&file).is_err());
    }

    #[test]
    fn clients_do_not_trust_a_socket_in_an_open_dir() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let open = root.path().join("open");
        std::fs::create_dir(&open).unwrap();
        let path = open.join("d.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::thread::spawn(move || {
            serve(
                &listener,
                None,
                || {},
                || None,
                |pending| pending.reply(&Reply::Busy),
            );
        });
        assert!(request(&path, Mode::Region).is_none(), "falls back to standalone");
    }
}
