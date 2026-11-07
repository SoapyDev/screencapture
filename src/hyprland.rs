//! Optional Hyprland IPC: where the pointer is, before any of our surfaces has seen it.
//! Wayland only tells a client about the pointer once it enters one of its surfaces; knowing
//! it up front lets the overlay open on the pointer's monitor first and place the toolbar there.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

/// Parse `hyprctl cursorpos` output: `"1234, 567"`.
fn parse_cursor(text: &str) -> Option<(i32, i32)> {
    let (x, y) = text.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

pub fn cursor() -> Option<(i32, i32)> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let path = dirs::runtime_dir()?
        .join("hypr")
        .join(signature)
        .join(".socket.sock");
    let mut stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(50))).ok()?;
    stream.write_all(b"cursorpos").ok()?;
    let mut text = String::new();
    stream.read_to_string(&mut text).ok()?;
    parse_cursor(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cursorpos() {
        assert_eq!(parse_cursor("1234, 567"), Some((1234, 567)));
        assert_eq!(parse_cursor("-10, 0\n"), Some((-10, 0)));
        assert_eq!(parse_cursor("unknown request"), None);
    }

    #[test]
    #[ignore = "needs a Hyprland session"]
    fn reads_live_cursor() {
        assert!(cursor().is_some());
    }
}
