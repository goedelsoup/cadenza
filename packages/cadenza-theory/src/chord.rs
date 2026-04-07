use serde::{Deserialize, Serialize};
use crate::pitch::{Pitch, PitchClass};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChordQuality {
    Major, Minor, Dominant7, Major7, Minor7,
    HalfDiminished, Diminished, Augmented, Sus2, Sus4, Power,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Extension {
    Flat9, Sharp9, Natural9, Natural11, Sharp11, Natural13, Flat13, Add9, Add11,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chord {
    pub root: PitchClass,
    pub quality: ChordQuality,
    pub extensions: Vec<Extension>,
    pub bass: Option<PitchClass>,
}

impl Chord {
    pub fn new(root: PitchClass, quality: ChordQuality) -> Self {
        Self { root, quality, extensions: vec![], bass: None }
    }
    pub fn intervals(&self) -> Vec<u8> {
        let mut base: Vec<u8> = match self.quality {
            ChordQuality::Major          => vec![0,4,7],
            ChordQuality::Minor          => vec![0,3,7],
            ChordQuality::Dominant7      => vec![0,4,7,10],
            ChordQuality::Major7         => vec![0,4,7,11],
            ChordQuality::Minor7         => vec![0,3,7,10],
            ChordQuality::HalfDiminished => vec![0,3,6,10],
            ChordQuality::Diminished     => vec![0,3,6,9],
            ChordQuality::Augmented      => vec![0,4,8],
            ChordQuality::Sus2           => vec![0,2,7],
            ChordQuality::Sus4           => vec![0,5,7],
            ChordQuality::Power          => vec![0,7],
        };
        for ext in &self.extensions {
            let s = match ext {
                Extension::Flat9 => 1, Extension::Sharp9 => 3, Extension::Natural9 => 2,
                Extension::Natural11 => 5, Extension::Sharp11 => 6,
                Extension::Natural13 => 9, Extension::Flat13 => 8,
                Extension::Add9 => 14, Extension::Add11 => 17,
            };
            if !base.contains(&s) { base.push(s); }
        }
        base.sort();
        base
    }
    pub fn pitches(&self, root_octave: i8) -> Vec<Pitch> {
        let root_midi = ((root_octave + 1) * 12) as i16 + self.root.0 as i16;
        self.intervals().iter().filter_map(|&i| {
            let m = root_midi + i as i16;
            if (0..=127).contains(&m) { Some(Pitch(m as u8)) } else { None }
        }).collect()
    }
    pub fn voice_lead_from(&self, prev: &Chord, register: i8) -> Vec<Pitch> {
        let target = self.pitches(register);
        let prev_pitches = prev.pitches(register);
        if prev_pitches.is_empty() { return target; }
        let prev_center = prev_pitches.iter().map(|p| p.0 as f32).sum::<f32>()
                          / prev_pitches.len() as f32;
        (-1..=2_i8).map(|o| self.pitches(register + o))
            .min_by_key(|ps| {
                let c = ps.iter().map(|p| p.0 as f32).sum::<f32>() / ps.len().max(1) as f32;
                (c - prev_center).abs() as i32
            })
            .unwrap_or(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn center(pitches: &[Pitch]) -> f32 {
        pitches.iter().map(|p| p.0 as f32).sum::<f32>() / pitches.len() as f32
    }

    #[test]
    fn intervals_major7_half_dim_dom7_with_extensions() {
        // Plain Major7: root, M3, P5, M7.
        let cmaj7 = Chord::new(PitchClass(0), ChordQuality::Major7);
        assert_eq!(cmaj7.intervals(), vec![0, 4, 7, 11]);

        // Plain HalfDiminished: root, m3, dim5, m7.
        let bm7b5 = Chord::new(PitchClass(11), ChordQuality::HalfDiminished);
        assert_eq!(bm7b5.intervals(), vec![0, 3, 6, 10]);

        // Dominant7 b9: base [0,4,7,10] + flat-9 (1) → sorted.
        let mut g7b9 = Chord::new(PitchClass(7), ChordQuality::Dominant7);
        g7b9.extensions.push(Extension::Flat9);
        assert_eq!(g7b9.intervals(), vec![0, 1, 4, 7, 10]);

        // Dominant7 #9 #11 13: base [0,4,7,10] + 3, 6, 9.
        let mut alt = Chord::new(PitchClass(7), ChordQuality::Dominant7);
        alt.extensions.push(Extension::Sharp9);
        alt.extensions.push(Extension::Sharp11);
        alt.extensions.push(Extension::Natural13);
        assert_eq!(alt.intervals(), vec![0, 3, 4, 6, 7, 9, 10]);

        // Major7 add9: base [0,4,7,11] + 14 (above the octave, kept distinct).
        let mut maj9 = Chord::new(PitchClass(0), ChordQuality::Major7);
        maj9.extensions.push(Extension::Add9);
        assert_eq!(maj9.intervals(), vec![0, 4, 7, 11, 14]);
    }

    #[test]
    fn pitches_in_octave_4_are_correct_midi_numbers() {
        // C major in octave 4 → C4=60, E4=64, G4=67.
        let c = Chord::new(PitchClass(0), ChordQuality::Major).pitches(4);
        assert_eq!(c, vec![Pitch(60), Pitch(64), Pitch(67)]);

        // A minor in octave 4 → A4=69, C5=72, E5=76.
        let am = Chord::new(PitchClass(9), ChordQuality::Minor).pitches(4);
        assert_eq!(am, vec![Pitch(69), Pitch(72), Pitch(76)]);

        // G dominant 7 in octave 4 → G4=67, B4=71, D5=74, F5=77.
        let g7 = Chord::new(PitchClass(7), ChordQuality::Dominant7).pitches(4);
        assert_eq!(g7, vec![Pitch(67), Pitch(71), Pitch(74), Pitch(77)]);
    }

    #[test]
    fn voice_lead_from_minimises_center_distance() {
        // Previous chord: C major in octave 4 (center ≈ 63.67).
        let prev = Chord::new(PitchClass(0), ChordQuality::Major);
        let prev_center = center(&prev.pitches(4));

        // Target chord: G major. Naïve octave-4 voicing has center ≈ 70.67.
        let next = Chord::new(PitchClass(7), ChordQuality::Major);
        let naive = next.pitches(4);
        let naive_dist = (center(&naive) - prev_center).abs();

        // Voice-led result should be at least as close to the prev center.
        let voiced = next.voice_lead_from(&prev, 4);
        let voiced_dist = (center(&voiced) - prev_center).abs();

        assert!(
            voiced_dist < naive_dist,
            "voice-led center {} (dist {}) should beat naïve center {} (dist {})",
            center(&voiced), voiced_dist, center(&naive), naive_dist,
        );
    }
}
