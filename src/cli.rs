//! Command line: with no command, open the selection overlay; commands with all their
//! inputs given capture straight away without any UI (fastest path, ideal for key bindings).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{
    capture::Rect,
    selection::{Mode, Request},
};

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Cli {
    /// Folder to save into (remembered for next time). Default: ~/Pictures/Screenshots.
    #[arg(short, long, global = true, value_name = "DIR")]
    pub output: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Select a region with the mouse (default), or capture the given geometry.
    Region {
        /// Region as "X,Y `WxH`" in logical desktop pixels (slurp format): no UI.
        #[arg(short, long, value_parser = Rect::parse)]
        geometry: Option<Rect>,
    },
    /// Capture the named screens stitched into one image; with no names, click to pick.
    Screen { names: Vec<String> },
    /// Capture every screen stitched into one image, without any UI.
    All,
    /// List screens and their geometry.
    List,
    /// Stay resident so the overlay opens instantly (start it with your session, e.g.
    /// Hyprland `exec-once = screencapture daemon`). Other invocations use it automatically.
    #[cfg(unix)]
    Daemon,
}

/// What a command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Overlay(Mode),
    Capture(Request),
    List,
    Daemon,
}

impl Cli {
    pub fn plan(&self) -> Plan {
        match &self.command {
            None | Some(Command::Region { geometry: None }) => Plan::Overlay(Mode::Region),
            Some(Command::Region { geometry: Some(rect) }) => Plan::Capture(Request::Region(*rect)),
            Some(Command::Screen { names }) if names.is_empty() => Plan::Overlay(Mode::Screens),
            Some(Command::Screen { names }) => Plan::Capture(Request::Screens(names.clone())),
            // Empty list means "every screen", resolved once the monitors are known.
            Some(Command::All) => Plan::Capture(Request::Screens(Vec::new())),
            Some(Command::List) => Plan::List,
            #[cfg(unix)]
            Some(Command::Daemon) => Plan::Daemon,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(args: &[&str]) -> Plan {
        Cli::try_parse_from(std::iter::once("screencapture").chain(args.iter().copied()))
            .unwrap()
            .plan()
    }

    #[test]
    fn no_command_opens_region_overlay() {
        assert_eq!(plan(&[]), Plan::Overlay(Mode::Region));
        assert_eq!(plan(&["region"]), Plan::Overlay(Mode::Region));
    }

    #[test]
    fn geometry_captures_without_ui() {
        assert_eq!(
            plan(&["region", "-g", "10,20 300x400"]),
            Plan::Capture(Request::Region(Rect::new(10, 20, 300, 400)))
        );
    }

    #[test]
    fn named_screens_capture_without_ui() {
        assert_eq!(
            plan(&["screen", "DP-1", "DP-2"]),
            Plan::Capture(Request::Screens(vec!["DP-1".into(), "DP-2".into()]))
        );
    }

    #[test]
    fn screen_without_names_opens_picker() {
        assert_eq!(plan(&["screen"]), Plan::Overlay(Mode::Screens));
    }

    #[test]
    fn all_and_list() {
        assert_eq!(plan(&["all"]), Plan::Capture(Request::Screens(Vec::new())));
        assert_eq!(plan(&["list"]), Plan::List);
    }

    #[test]
    fn daemon_command() {
        assert_eq!(plan(&["daemon"]), Plan::Daemon);
    }

    #[test]
    fn output_is_global() {
        let cli = Cli::try_parse_from(["screencapture", "all", "-o", "/tmp/x"]).unwrap();
        assert_eq!(cli.output.as_deref(), Some(std::path::Path::new("/tmp/x")));
    }

    #[test]
    fn bad_geometry_is_rejected() {
        assert!(Cli::try_parse_from(["screencapture", "region", "-g", "nope"]).is_err());
    }
}
