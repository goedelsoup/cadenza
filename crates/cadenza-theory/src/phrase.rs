use serde::{Deserialize, Serialize};
use crate::rhythm::{NoteEvent, TimeSignature, Duration};
use crate::scale::Scale;
use crate::pitch::Pitch;
use crate::interval::Interval;
use crate::validation::{ValidationWarning, ValidationLevel};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricSyllable {
    pub text: String,
    pub note_id: u32,
    pub word_index: i32,    // -1 for padding rests
    pub stress: String,     // "strong" | "weak" | "unstressed"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    pub phrase_id: u32,
    pub syllables: Vec<LyricSyllable>,
    pub raw_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phrase {
    pub id: u32,
    pub label: String,
    pub events: Vec<NoteEvent>,
    pub time_sig: TimeSignature,
    pub tempo: u16,
    pub key: Option<Scale>,
    pub bars: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lyrics: Vec<LyricLine>,
}

impl Phrase {
    pub fn new(id: u32, label: impl Into<String>, time_sig: TimeSignature, tempo: u16) -> Self {
        Self { id, label: label.into(), events: vec![], time_sig, tempo, key: None, bars: 4, lyrics: vec![] }
    }

    pub fn duration_ticks(&self) -> u32 {
        self.time_sig.ticks_per_bar() * self.bars as u32
    }

    /// Quantize all events to the nearest grid division
    pub fn quantize(&mut self, grid: Duration) {
        let g = grid.0;
        for ev in &mut self.events {
            ev.start = ((ev.start + g / 2) / g) * g;
            ev.duration = ((ev.duration + g / 2) / g).max(1) * g;
        }
        self.events.sort_by_key(|e| e.start);
    }

    /// Transpose all pitches by semitones
    pub fn transpose(&self, semitones: i8) -> Self {
        let mut p = self.clone();
        for ev in &mut p.events {
            let new_pitch = ev.pitch as i16 + semitones as i16;
            ev.pitch = new_pitch.clamp(0, 127) as u8;
        }
        p
    }

    /// Retrograde (reverse note order, preserve durations)
    pub fn retrograde(&self) -> Self {
        let total = self.duration_ticks();
        let mut p = self.clone();
        p.events = self.events.iter().map(|ev| {
            let new_start = total.saturating_sub(ev.start + ev.duration);
            NoteEvent { start: new_start, ..*ev }
        }).collect();
        p.events.sort_by_key(|e| e.start);
        p
    }

    /// Invert around an axis pitch
    pub fn invert(&self, axis: Pitch) -> Self {
        let mut p = self.clone();
        for ev in &mut p.events {
            let interval = ev.pitch as i16 - axis.0 as i16;
            let inverted = axis.0 as i16 - interval;
            ev.pitch = inverted.clamp(0, 127) as u8;
        }
        p
    }

    /// Validate notes against scale membership
    pub fn validate_against_scale(&self) -> Vec<ValidationWarning> {
        let Some(scale) = &self.key else { return vec![]; };
        let mut warnings = vec![];
        for ev in &self.events {
            let pc = crate::pitch::PitchClass(ev.pitch % 12);
            if !scale.contains(pc) {
                warnings.push(ValidationWarning {
                    level: ValidationLevel::Info,
                    message: format!(
                        "pitch {} (tick {}) is outside {} scale",
                        Pitch(ev.pitch).name(), ev.start, scale.root.name()
                    ),
                });
            }
        }
        warnings
    }
}
