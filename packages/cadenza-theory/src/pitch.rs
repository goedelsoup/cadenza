use serde::{Deserialize, Serialize};
use crate::interval::Interval;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Pitch(pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PitchClass(pub u8);

const NAMES: [&str; 12] = ["C","C#","D","D#","E","F","F#","G","G#","A","A#","B"];

impl Pitch {
    pub fn new(midi: u8) -> Option<Self> {
        if midi <= 127 { Some(Self(midi)) } else { None }
    }
    pub fn midi(&self) -> u8 { self.0 }
    pub fn pc(&self) -> PitchClass { PitchClass(self.0 % 12) }
    pub fn octave(&self) -> i8 { (self.0 as i8 / 12) - 1 }
    pub fn name(&self) -> &'static str { NAMES[(self.0 % 12) as usize] }
    pub fn interval_to(&self, other: Pitch) -> Interval { Interval(other.0 as i8 - self.0 as i8) }
    pub fn transpose(&self, interval: Interval) -> Option<Pitch> {
        let n = self.0 as i16 + interval.0 as i16;
        if (0..=127).contains(&n) { Some(Pitch(n as u8)) } else { None }
    }
}

impl PitchClass {
    pub fn name(&self) -> &'static str { NAMES[(self.0 % 12) as usize] }
    pub fn parse(s: &str) -> Option<Self> {
        NAMES.iter().position(|&n| n.eq_ignore_ascii_case(s))
             .or_else(|| match s {
                "Db"|"db" => Some(1), "Eb"|"eb" => Some(3),
                "Gb"|"gb" => Some(6), "Ab"|"ab" => Some(8),
                "Bb"|"bb" => Some(10), _ => None,
             })
             .map(|i| PitchClass(i as u8))
    }
    pub fn in_octave(&self, octave: i8) -> Option<Pitch> {
        let n = (octave as i16 + 1) * 12 + self.0 as i16;
        if (0..=127).contains(&n) { Some(Pitch(n as u8)) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_name_all_twelve_classes() {
        // Middle C = 60, walk one chromatic octave.
        let expected = ["C","C#","D","D#","E","F","F#","G","G#","A","A#","B"];
        for (i, name) in expected.iter().enumerate() {
            assert_eq!(Pitch(60 + i as u8).name(), *name);
        }
        // Confirm wraparound: 72 (C5) and 0 (C-1) are both "C".
        assert_eq!(Pitch(72).name(), "C");
        assert_eq!(Pitch(0).name(), "C");
    }

    #[test]
    fn pitch_transpose_boundaries() {
        // Identity transpose at low and high ends stays in range.
        assert_eq!(Pitch(0).transpose(Interval(0)), Some(Pitch(0)));
        assert_eq!(Pitch(127).transpose(Interval(0)), Some(Pitch(127)));
        // Stepping inside range works.
        assert_eq!(Pitch(60).transpose(Interval::PERFECT_FIFTH), Some(Pitch(67)));
        // Overflow above 127 returns None.
        assert_eq!(Pitch(127).transpose(Interval(1)), None);
        assert_eq!(Pitch(120).transpose(Interval::OCTAVE), None);
        // Underflow below 0 returns None.
        assert_eq!(Pitch(0).transpose(Interval(-1)), None);
    }

    #[test]
    fn pitch_class_parse_sharps_flats_edges() {
        // Sharps via canonical names (case insensitive).
        assert_eq!(PitchClass::parse("C#"), Some(PitchClass(1)));
        assert_eq!(PitchClass::parse("f#"), Some(PitchClass(6)));
        // Flats remap to enharmonic sharps.
        assert_eq!(PitchClass::parse("Bb"), Some(PitchClass(10)));
        assert_eq!(PitchClass::parse("Db"), Some(PitchClass(1)));
        assert_eq!(PitchClass::parse("eb"), Some(PitchClass(3)));
        // Naturals.
        assert_eq!(PitchClass::parse("B"), Some(PitchClass(11)));
        assert_eq!(PitchClass::parse("c"), Some(PitchClass(0)));
        // Edge case: "Cb" is not in either name list — currently unsupported.
        assert_eq!(PitchClass::parse("Cb"), None);
        // Garbage input.
        assert_eq!(PitchClass::parse("H"), None);
        assert_eq!(PitchClass::parse(""), None);
    }
}
