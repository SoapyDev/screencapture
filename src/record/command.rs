//! Recording commands, as the client sends them to the daemon (one line, words).

use super::{plan::Target, settings::Overrides};
use crate::capture::Rect;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Start(Target, Overrides),
    Stop,
    /// Stop and throw the recording away.
    Discard,
    /// Start if idle, else stop.
    Toggle(Target, Overrides),
}

impl Command {
    /// Words after `record`.
    pub fn encode(&self) -> String {
        let (verb, target, overrides) = match self {
            Self::Stop => return "stop".into(),
            Self::Discard => return "discard".into(),
            Self::Start(target, overrides) => ("start", target, overrides),
            Self::Toggle(target, overrides) => ("toggle", target, overrides),
        };
        let target = match target {
            Target::Focused => "focused".into(),
            Target::All => "all".into(),
            Target::Screens(names) => format!("screens {}", names.join(",")),
            Target::Region(Rect { x, y, w, h }) => format!("region {x},{y},{w},{h}"),
        };
        std::iter::once(verb.to_string())
            .chain([target])
            .chain(overrides.encode())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let verb = words.next()?;
        let alone = |command| words.clone().next().is_none().then_some(command);
        match verb {
            "stop" => return alone(Self::Stop),
            "discard" => return alone(Self::Discard),
            _ => {}
        }
        let target = match words.next()? {
            "focused" => Target::Focused,
            "all" => Target::All,
            "screens" => Target::Screens(words.next()?.split(',').map(String::from).collect()),
            "region" => {
                let n: Vec<i32> = words
                    .next()?
                    .split(',')
                    .map(str::parse)
                    .collect::<Result<_, _>>()
                    .ok()?;
                let [x, y, w, h] = n[..] else { return None };
                let rect = Rect::new(x, y, w, h);
                if rect.is_empty() {
                    return None;
                }
                Target::Region(rect)
            }
            _ => return None,
        };
        let mut overrides = Overrides::default();
        for word in words {
            overrides.parse_word(word)?;
        }
        match verb {
            "start" => Some(Self::Start(target, overrides)),
            "toggle" => Some(Self::Toggle(target, overrides)),
            _ => None,
        }
    }

    /// Whether this would start a recording if none runs.
    pub const fn starts(&self) -> bool {
        !matches!(self, Self::Stop | Self::Discard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::settings::{Format, Fps};

    #[test]
    fn commands_round_trip() {
        let fast = Overrides {
            fps: Some(Fps::F30),
            format: Some(Format::H264),
            ..Overrides::default()
        };
        for command in [
            Command::Start(Target::Focused, Overrides::default()),
            Command::Start(Target::All, fast),
            Command::Start(
                Target::Screens(vec!["DP-1".into(), "HDMI-A-1".into()]),
                Overrides::default(),
            ),
            Command::Start(Target::Region(Rect::new(-10, 20, 300, 400)), fast),
            Command::Stop,
            Command::Toggle(Target::Focused, fast),
        ] {
            let line = command.encode();
            assert!(!line.contains('\n'));
            assert_eq!(Command::parse(&line), Some(command), "{line}");
        }
    }

    #[test]
    fn discard_round_trips() {
        assert_eq!(Command::parse(&Command::Discard.encode()), Some(Command::Discard));
    }

    #[test]
    fn discard_starts_nothing() {
        assert!(!Command::Discard.starts());
    }

    #[test]
    fn nonsense_is_rejected() {
        for line in [
            "",
            "begin",
            "start",
            "start moon",
            "start region 1,2,3",
            "start all fps=7",
            "stop now",
        ] {
            assert_eq!(Command::parse(line), None, "{line:?}");
        }
    }
}
