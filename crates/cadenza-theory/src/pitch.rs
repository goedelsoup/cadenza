use serde::{Deserialize, Serialize};
use crate::interval::Interval;

/// MIDI pitch number (0–127). Middle C = 60.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Pitch(pub u8);

/// Pitch class (0 = C, 1 = C#/Db, … 11 = B)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PitchClass(pub u8);

const NAMES: [&str; 12] = ["C","C#","D","D#","E","F","F#","G","G#","A","A#","B"];

impl Pitch {
    pub fn new(midi: u8) -> Option<Self> {
        if midi <= 127 { Some(Self(midi)) } else { None }
    }

    pub fn midi(&self) -> u8 { self.0 }

    pub fn pc(&self) -> PitchClass {
        PitchClass(self.0 % 12)
    }

    pub fn octave(&self) -> i8 {
        (self.0 as i8 / 12) - 1
    }

    /// e.g. "D4", "F#3"
    pub fn name(&self) -> &'static str {
        NAMES[(self.0 % 12) as usize]
    }

    /// e.g. "D4"
    #[cfg(feature = "std")]
    pub fn label(&self) -> std::string::String {
        format!("{}{}", self.name(), self.octave())
    }

    pub fn interval_to(&self, other: Pitch) -> Interval {
        Interval(other.0 as i8 - self.0 as i8)
    }

    pub fn transpose(&self, interval: Interval) -> Option<Pitch> {
        let n = self.0 as i8 + interval.0;
        if n >= 0 && n <= 127 { Some(Pitch(n as u8)) } else { None }
    }
}

impl PitchClass {
    pub fn name(&self) -> &'static str {
        NAMES[(self.0 % 12) as usize]
    }

    /// Parse from string: "D", "F#", "Bb"
    pub fn parse(s: &str) -> Option<Self> {
        let normalized = s.replace("b", "#").to_uppercase();
        NAMES.iter().position(|&n| n == normalized)
             .or_else(|| match s {
                "Db" | "db" => Some(1), "Eb" | "eb" => Some(3),
                "Gb" | "gb" => Some(6), "Ab" | "ab" => Some(8),
                "Bb" | "bb" => Some(10), _ => None,
             })
             .map(|i| PitchClass(i as u8))
    }

    pub fn in_octave(&self, octave: i8) -> Option<Pitch> {
        let n = (octave + 1) * 12 + self.0 as i8;
        if n >= 0 && n <= 127 { Some(Pitch(n as u8)) } else { None }
    }
}
