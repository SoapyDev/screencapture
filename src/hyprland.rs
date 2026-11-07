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

/// One request to Hyprland's IPC socket, and its whole answer.
fn ask(request: &str) -> Option<String> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let path = dirs::runtime_dir()?
        .join("hypr")
        .join(signature)
        .join(".socket.sock");
    let mut stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(50))).ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    let mut text = String::new();
    stream.read_to_string(&mut text).ok()?;
    Some(text)
}

pub fn cursor() -> Option<(i32, i32)> {
    parse_cursor(&ask("cursorpos")?)
}

/// `hyprctl -j monitors`: the name of the one marked focused.
fn parse_focused(json: &str) -> Option<String> {
    let monitors: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    monitors
        .iter()
        .find(|m| m["focused"] == true)
        .and_then(|m| m["name"].as_str())
        .map(String::from)
}

/// The monitor with keyboard focus.
pub fn focused_monitor() -> Option<String> {
    parse_focused(&ask("j/monitors")?)
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
    fn finds_the_focused_monitor() {
        let json = r#"[{"id":0,"name":"HDMI-A-1","focused":false},{"id":1,"name":"DP-2","focused":true}]"#;
        assert_eq!(parse_focused(json), Some("DP-2".into()));
        assert_eq!(parse_focused(r#"[{"name":"DP-1","focused":false}]"#), None);
        assert_eq!(parse_focused("garbage"), None);
    }

    #[test]
    #[ignore = "needs a Hyprland session"]
    fn reads_live_focused_monitor() {
        assert!(focused_monitor().is_some());
    }

    #[test]
    #[ignore = "needs a Hyprland session"]
    fn reads_live_cursor() {
        assert!(cursor().is_some());
    }
}
