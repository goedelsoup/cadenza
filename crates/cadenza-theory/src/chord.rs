use serde::{Deserialize, Serialize};
use crate::pitch::{Pitch, PitchClass};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChordQuality {
    Major, Minor, Dominant7, Major7, Minor7,
    HalfDiminished, Diminished, Augmented,
    Sus2, Sus4, Power,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Extension {
    Flat9, Sharp9, Natural9,
    Natural11, Sharp11,
    Natural13, Flat13,
    Add9, Add11,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chord {
    pub root: PitchClass,
    pub quality: ChordQuality,
    pub extensions: Vec<Extension>,
    pub bass: Option<PitchClass>,  // slash chord
}

impl Chord {
    pub fn new(root: PitchClass, quality: ChordQuality) -> Self {
        Self { root, quality, extensions: vec![], bass: None }
    }

    /// Semitone stack above root
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
            let semi = match ext {
                Extension::Flat9    => 1,  Extension::Sharp9  => 3,
                Extension::Natural9 => 2,  Extension::Natural11 => 5,
                Extension::Sharp11  => 6,  Extension::Natural13 => 9,
                Extension::Flat13   => 8,  Extension::Add9    => 14,
                Extension::Add11    => 17,
            };
            if !base.contains(&semi) { base.push(semi); }
        }
        base.sort();
        base
    }

    /// Voiced pitches in a given register (root octave)
    pub fn pitches(&self, root_octave: i8) -> Vec<Pitch> {
        let root_midi = ((root_octave + 1) * 12) as i16 + self.root.0 as i16;
        self.intervals().iter()
            .filter_map(|&i| {
                let m = root_midi + i as i16;
                if m >= 0 && m <= 127 { Some(Pitch(m as u8)) } else { None }
            })
            .collect()
    }

    /// Naive voice leading: minimize movement from previous chord
    pub fn voice_lead_from(&self, prev: &Chord, register: i8) -> Vec<Pitch> {
        let target = self.pitches(register);
        let prev_pitches = prev.pitches(register);

        if prev_pitches.is_empty() { return target; }

        let prev_center = prev_pitches.iter().map(|p| p.0 as f32).sum::<f32>()
                          / prev_pitches.len() as f32;

        // find octave inversion that minimizes distance from prev center
        let candidates: Vec<Vec<Pitch>> = (-1..=2_i8).map(|oct| self.pitches(register + oct)).collect();
        candidates.into_iter().min_by_key(|ps| {
            let center = ps.iter().map(|p| p.0 as f32).sum::<f32>() / ps.len().max(1) as f32;
            (center - prev_center).abs() as i32
        }).unwrap_or(target)
    }
}
