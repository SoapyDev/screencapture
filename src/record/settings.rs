//! What a recording looks like: remembered in the config, overridable per recording.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Frames per second.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
pub enum Fps {
    /// The screen's refresh rate.
    #[default]
    #[serde(rename = "native")]
    #[value(name = "native")]
    Native,
    /// Picked from the CPU and GPU load when the recording starts; frames only when the
    /// screen changes.
    #[serde(rename = "adaptive")]
    #[value(name = "adaptive")]
    Adaptive,
    #[serde(rename = "120")]
    #[value(name = "120")]
    F120,
    #[serde(rename = "60")]
    #[value(name = "60")]
    F60,
    #[serde(rename = "30")]
    #[value(name = "30")]
    F30,
    #[serde(rename = "15")]
    #[value(name = "15")]
    F15,
}

impl Fps {
    /// The rate of a fixed choice.
    pub const fn fixed(self) -> Option<u32> {
        match self {
            Self::Native | Self::Adaptive => None,
            Self::F120 => Some(120),
            Self::F60 => Some(60),
            Self::F30 => Some(30),
            Self::F15 => Some(15),
        }
    }
}

/// Largest picture height; the width follows the aspect ratio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
pub enum Resolution {
    #[default]
    #[serde(rename = "native")]
    #[value(name = "native")]
    Native,
    #[serde(rename = "1440p")]
    #[value(name = "1440p")]
    P1440,
    #[serde(rename = "1080p")]
    #[value(name = "1080p")]
    P1080,
    #[serde(rename = "720p")]
    #[value(name = "720p")]
    P720,
    #[serde(rename = "480p")]
    #[value(name = "480p")]
    P480,
}

impl Resolution {
    pub const fn max_height(self) -> Option<u32> {
        match self {
            Self::Native => None,
            Self::P1440 => Some(1440),
            Self::P1080 => Some(1080),
            Self::P720 => Some(720),
            Self::P480 => Some(480),
        }
    }
}

/// File format: MP4 with a video codec, or an animated GIF made from one at stop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
pub enum Format {
    /// Smallest files; modern browsers and players.
    #[default]
    #[serde(rename = "av1")]
    #[value(name = "av1")]
    Av1,
    /// Plays everywhere; at most 4096 px wide.
    #[serde(rename = "h264")]
    #[value(name = "h264")]
    H264,
    #[serde(rename = "hevc")]
    #[value(name = "hevc")]
    Hevc,
    /// Converted after stop; large files, short clips only.
    #[serde(rename = "gif")]
    #[value(name = "gif")]
    Gif,
}

/// Encoder quality preset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
pub enum Quality {
    #[serde(rename = "medium")]
    #[value(name = "medium")]
    Medium,
    #[serde(rename = "high")]
    #[value(name = "high")]
    High,
    #[default]
    #[serde(rename = "very-high")]
    #[value(name = "very-high")]
    VeryHigh,
    #[serde(rename = "ultra")]
    #[value(name = "ultra")]
    Ultra,
}

/// Sound recorded with the picture, from the default devices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
pub enum Audio {
    #[default]
    #[serde(rename = "off")]
    #[value(name = "off")]
    Off,
    /// What the computer plays.
    #[serde(rename = "output")]
    #[value(name = "output")]
    Output,
    /// The microphone.
    #[serde(rename = "input")]
    #[value(name = "input")]
    Input,
    #[serde(rename = "both")]
    #[value(name = "both")]
    Both,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub fps: Fps,
    pub resolution: Resolution,
    pub format: Format,
    pub quality: Quality,
    pub audio: Audio,
}

/// Per-recording choices from the command line; unset ones keep the remembered settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::Args)]
pub struct Overrides {
    /// Frames per second.
    #[arg(long, global = true)]
    pub fps: Option<Fps>,
    /// Largest picture height.
    #[arg(long, global = true)]
    pub resolution: Option<Resolution>,
    #[arg(long, global = true)]
    pub format: Option<Format>,
    #[arg(long, global = true)]
    pub quality: Option<Quality>,
    #[arg(long, global = true)]
    pub audio: Option<Audio>,
}

impl Overrides {
    pub fn apply(self, base: Settings) -> Settings {
        Settings {
            fps: self.fps.unwrap_or(base.fps),
            resolution: self.resolution.unwrap_or(base.resolution),
            format: self.format.unwrap_or(base.format),
            quality: self.quality.unwrap_or(base.quality),
            audio: self.audio.unwrap_or(base.audio),
        }
    }

    /// `key=value` words for the daemon protocol, unset ones left out.
    pub fn encode(self) -> Vec<String> {
        fn word<T: ValueEnum>(key: &str, value: Option<T>) -> Option<String> {
            let value = value?.to_possible_value()?;
            Some(format!("{key}={}", value.get_name()))
        }
        [
            word("fps", self.fps),
            word("resolution", self.resolution),
            word("format", self.format),
            word("quality", self.quality),
            word("audio", self.audio),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// One `key=value` word; `None` if it isn't one.
    pub fn parse_word(&mut self, word: &str) -> Option<()> {
        fn value<T: ValueEnum>(text: &str) -> Option<T> {
            T::from_str(text, false).ok()
        }
        let (key, text) = word.split_once('=')?;
        match key {
            "fps" => self.fps = Some(value(text)?),
            "resolution" => self.resolution = Some(value(text)?),
            "format" => self.format = Some(value(text)?),
            "quality" => self.quality = Some(value(text)?),
            "audio" => self.audio = Some(value(text)?),
            _ => return None,
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_native_av1_very_high_without_sound() {
        assert_eq!(
            Settings::default(),
            Settings {
                fps: Fps::Native,
                resolution: Resolution::Native,
                format: Format::Av1,
                quality: Quality::VeryHigh,
                audio: Audio::Off,
            }
        );
    }

    #[test]
    fn settings_read_as_plain_words_and_missing_keys_default() {
        let s: Settings = serde_json::from_str(r#"{"fps":"30","format":"h264","quality":"ultra"}"#).unwrap();
        assert_eq!(s.fps, Fps::F30);
        assert_eq!(s.format, Format::H264);
        assert_eq!(s.quality, Quality::Ultra);
        assert_eq!(s.resolution, Resolution::Native);
        let json = serde_json::to_string(&Settings {
            resolution: Resolution::P720,
            ..Settings::default()
        })
        .unwrap();
        assert!(json.contains(r#""resolution":"720p""#), "{json}");
    }

    #[test]
    fn fixed_rates_and_heights() {
        assert_eq!(Fps::F120.fixed(), Some(120));
        assert_eq!(Fps::Adaptive.fixed(), None);
        assert_eq!(Resolution::P1080.max_height(), Some(1080));
        assert_eq!(Resolution::Native.max_height(), None);
    }

    #[test]
    fn overrides_replace_only_what_they_set() {
        let base = Settings {
            quality: Quality::High,
            ..Settings::default()
        };
        let o = Overrides {
            fps: Some(Fps::F60),
            audio: Some(Audio::Both),
            ..Overrides::default()
        };
        assert_eq!(
            o.apply(base),
            Settings {
                fps: Fps::F60,
                audio: Audio::Both,
                quality: Quality::High,
                ..Settings::default()
            }
        );
    }

    #[test]
    fn overrides_round_trip_through_words() {
        let o = Overrides {
            fps: Some(Fps::F15),
            resolution: Some(Resolution::P1440),
            format: Some(Format::Gif),
            quality: Some(Quality::VeryHigh),
            audio: Some(Audio::Input),
        };
        let words = o.encode();
        assert_eq!(
            words,
            [
                "fps=15",
                "resolution=1440p",
                "format=gif",
                "quality=very-high",
                "audio=input"
            ]
        );
        let mut back = Overrides::default();
        for w in &words {
            back.parse_word(w).unwrap();
        }
        assert_eq!(back, o);
        assert_eq!(Overrides::default().encode(), Vec::<String>::new());
        assert!(back.parse_word("fps=7").is_none());
        assert!(back.parse_word("colour=red").is_none());
    }
}
