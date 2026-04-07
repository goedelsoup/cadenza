use serde::{Deserialize, Serialize};
use crate::pitch::PitchClass;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Major,
    NaturalMinor,
    HarmonicMinor,
    MelodicMinor,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Locrian,
    WholeTone,
    Diminished,
    Pentatonic,
    BluesPentatonic,
}

impl Mode {
    /// Semitone intervals from root
    pub fn intervals(&self) -> &'static [u8] {
        match self {
            Mode::Major          => &[0,2,4,5,7,9,11],
            Mode::NaturalMinor   => &[0,2,3,5,7,8,10],
            Mode::HarmonicMinor  => &[0,2,3,5,7,8,11],
            Mode::MelodicMinor   => &[0,2,3,5,7,9,11],
            Mode::Dorian         => &[0,2,3,5,7,9,10],
            Mode::Phrygian       => &[0,1,3,5,7,8,10],
            Mode::Lydian         => &[0,2,4,6,7,9,11],
            Mode::Mixolydian     => &[0,2,4,5,7,9,10],
            Mode::Locrian        => &[0,1,3,5,6,8,10],
            Mode::WholeTone      => &[0,2,4,6,8,10],
            Mode::Diminished     => &[0,2,3,5,6,8,9,11],
            Mode::Pentatonic     => &[0,2,4,7,9],
            Mode::BluesPentatonic=> &[0,3,5,6,7,10],
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "major" | "ionian"          => Some(Mode::Major),
            "natural minor" | "aeolian" | "minor" => Some(Mode::NaturalMinor),
            "harmonic minor"            => Some(Mode::HarmonicMinor),
            "melodic minor"             => Some(Mode::MelodicMinor),
            "dorian"                    => Some(Mode::Dorian),
            "phrygian"                  => Some(Mode::Phrygian),
            "lydian"                    => Some(Mode::Lydian),
            "mixolydian"                => Some(Mode::Mixolydian),
            "locrian"                   => Some(Mode::Locrian),
            "whole tone"                => Some(Mode::WholeTone),
            "diminished"                => Some(Mode::Diminished),
            "pentatonic"                => Some(Mode::Pentatonic),
            "blues"                     => Some(Mode::BluesPentatonic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scale {
    pub root: PitchClass,
    pub mode: Mode,
}

impl Scale {
    pub fn new(root: PitchClass, mode: Mode) -> Self {
        Self { root, mode }
    }

    pub fn pitch_classes(&self) -> Vec<PitchClass> {
        self.mode.intervals().iter()
            .map(|&i| PitchClass((self.root.0 + i) % 12))
            .collect()
    }

    pub fn contains(&self, pc: PitchClass) -> bool {
        self.pitch_classes().contains(&pc)
    }

    pub fn degree_of(&self, pc: PitchClass) -> Option<u8> {
        self.pitch_classes().iter().position(|&p| p == pc).map(|i| i as u8 + 1)
    }

    pub fn parse(root_str: &str, mode_str: &str) -> Option<Self> {
        let root = PitchClass::parse(root_str)?;
        let mode = Mode::parse(mode_str)?;
        Some(Self::new(root, mode))
    }
}
