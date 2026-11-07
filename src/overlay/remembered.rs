//! What the overlay remembers between runs: each feature's section of the config.

use crate::{annotate::Palette, config::Config, record::settings::Recording, save::Screenshots};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remembered {
    pub screenshots: Screenshots,
    pub recording: Recording,
    pub palette: Palette,
}

impl Remembered {
    pub fn load() -> Self {
        let config = Config::load();
        Self {
            screenshots: config.section(),
            recording: config.section(),
            palette: config.section(),
        }
    }
}
