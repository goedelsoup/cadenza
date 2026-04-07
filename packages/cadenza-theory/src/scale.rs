use serde::{Deserialize, Serialize};
use crate::pitch::PitchClass;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Major, NaturalMinor, HarmonicMinor, MelodicMinor,
    Dorian, Phrygian, Lydian, Mixolydian, Locrian,
    WholeTone, Diminished, Pentatonic, BluesPentatonic,
}

impl Mode {
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
        match s.to_lowercase().trim() {
            "major"|"ionian"                    => Some(Mode::Major),
            "natural minor"|"aeolian"|"minor"   => Some(Mode::NaturalMinor),
            "harmonic minor"                    => Some(Mode::HarmonicMinor),
            "melodic minor"                     => Some(Mode::MelodicMinor),
            "dorian"                            => Some(Mode::Dorian),
            "phrygian"                          => Some(Mode::Phrygian),
            "lydian"                            => Some(Mode::Lydian),
            "mixolydian"                        => Some(Mode::Mixolydian),
            "locrian"                           => Some(Mode::Locrian),
            "whole tone"                        => Some(Mode::WholeTone),
            "diminished"                        => Some(Mode::Diminished),
            "pentatonic"                        => Some(Mode::Pentatonic),
            "blues"                             => Some(Mode::BluesPentatonic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scale { pub root: PitchClass, pub mode: Mode }

impl Scale {
    pub fn new(root: PitchClass, mode: Mode) -> Self { Self { root, mode } }
    pub fn pitch_classes(&self) -> Vec<PitchClass> {
        self.mode.intervals().iter().map(|&i| PitchClass((self.root.0 + i) % 12)).collect()
    }
    pub fn contains(&self, pc: PitchClass) -> bool { self.pitch_classes().contains(&pc) }
    pub fn degree_of(&self, pc: PitchClass) -> Option<u8> {
        self.pitch_classes().iter().position(|&p| p == pc).map(|i| i as u8 + 1)
    }
    pub fn parse(root_str: &str, mode_str: &str) -> Option<Self> {
        Some(Self::new(PitchClass::parse(root_str)?, Mode::parse(mode_str)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcs(values: &[u8]) -> Vec<PitchClass> {
        values.iter().map(|&v| PitchClass(v)).collect()
    }

    #[test]
    fn pitch_classes_dorian_harmonic_minor_pentatonic() {
        // D Dorian: D E F G A B C
        let d_dorian = Scale::new(PitchClass(2), Mode::Dorian).pitch_classes();
        assert_eq!(d_dorian, pcs(&[2, 4, 5, 7, 9, 11, 0]));

        // A Harmonic minor: A B C D E F G#
        let a_hm = Scale::new(PitchClass(9), Mode::HarmonicMinor).pitch_classes();
        assert_eq!(a_hm, pcs(&[9, 11, 0, 2, 4, 5, 8]));

        // C Pentatonic: C D E G A
        let c_pent = Scale::new(PitchClass(0), Mode::Pentatonic).pitch_classes();
        assert_eq!(c_pent, pcs(&[0, 2, 4, 7, 9]));
    }

    #[test]
    fn contains_diatonic_and_chromatic() {
        let c_major = Scale::new(PitchClass(0), Mode::Major);
        // Diatonic: every white key.
        for &pc in &[0u8, 2, 4, 5, 7, 9, 11] {
            assert!(c_major.contains(PitchClass(pc)), "C major should contain {pc}");
        }
        // Chromatic: every black key.
        for &pc in &[1u8, 3, 6, 8, 10] {
            assert!(!c_major.contains(PitchClass(pc)), "C major should not contain {pc}");
        }
    }

    #[test]
    fn degree_of_returns_one_based() {
        let c_major = Scale::new(PitchClass(0), Mode::Major);
        assert_eq!(c_major.degree_of(PitchClass(0)),  Some(1)); // C  → 1
        assert_eq!(c_major.degree_of(PitchClass(2)),  Some(2)); // D  → 2
        assert_eq!(c_major.degree_of(PitchClass(4)),  Some(3)); // E  → 3
        assert_eq!(c_major.degree_of(PitchClass(11)), Some(7)); // B  → 7
        assert_eq!(c_major.degree_of(PitchClass(1)),  None);    // C# → out
    }

    #[test]
    fn mode_parse_roundtrip_all_variants() {
        // Each accepted spelling must parse to the matching variant.
        let cases: &[(&str, Mode)] = &[
            ("major",          Mode::Major),
            ("ionian",         Mode::Major),
            ("natural minor",  Mode::NaturalMinor),
            ("aeolian",        Mode::NaturalMinor),
            ("minor",          Mode::NaturalMinor),
            ("harmonic minor", Mode::HarmonicMinor),
            ("melodic minor",  Mode::MelodicMinor),
            ("dorian",         Mode::Dorian),
            ("phrygian",       Mode::Phrygian),
            ("lydian",         Mode::Lydian),
            ("mixolydian",     Mode::Mixolydian),
            ("locrian",        Mode::Locrian),
            ("whole tone",     Mode::WholeTone),
            ("diminished",     Mode::Diminished),
            ("pentatonic",     Mode::Pentatonic),
            ("blues",          Mode::BluesPentatonic),
        ];
        for (s, expected) in cases {
            assert_eq!(Mode::parse(s), Some(*expected), "parse({s})");
            // Case + whitespace should not matter.
            let upper = s.to_uppercase();
            assert_eq!(Mode::parse(&format!("  {upper}  ")), Some(*expected));
        }
        assert_eq!(Mode::parse("nonsense"), None);
    }
}
