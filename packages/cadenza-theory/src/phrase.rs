use serde::{Deserialize, Serialize};
use crate::{
    rhythm::{NoteEvent, TimeSignature, Duration},
    scale::Scale,
    pitch::Pitch,
    validation::{ValidationWarning, ValidationLevel},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricSyllable {
    pub text:       String,
    pub note_id:    u32,
    pub word_index: i32,    // -1 indicates a padding rest
    pub stress:     String, // "strong" | "weak" | "unstressed"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    pub phrase_id: u32,
    pub syllables: Vec<LyricSyllable>,
    pub raw_text:  String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phrase {
    pub id:       u32,
    pub label:    String,
    pub events:   Vec<NoteEvent>,
    pub time_sig: TimeSignature,
    pub tempo:    u16,
    pub key:      Option<Scale>,
    pub bars:     u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lyrics:   Vec<LyricLine>,
}

impl Phrase {
    pub fn new(id: u32, label: impl Into<String>, time_sig: TimeSignature, tempo: u16) -> Self {
        Self { id, label: label.into(), events: vec![], time_sig, tempo, key: None, bars: 4, lyrics: vec![] }
    }
    pub fn duration_ticks(&self) -> u32 { self.time_sig.ticks_per_bar() * self.bars as u32 }
    pub fn quantize(&mut self, grid: Duration) {
        let g = grid.0;
        for ev in &mut self.events {
            ev.start    = ((ev.start    + g/2) / g) * g;
            ev.duration = ((ev.duration + g/2) / g).max(1) * g;
        }
        self.events.sort_by_key(|e| e.start);
    }
    pub fn transpose(&self, semitones: i8) -> Self {
        let mut p = self.clone();
        for ev in &mut p.events {
            ev.pitch = ((ev.pitch as i16 + semitones as i16).clamp(0, 127)) as u8;
        }
        p
    }
    pub fn retrograde(&self) -> Self {
        let total = self.duration_ticks();
        let mut p = self.clone();
        p.events = self.events.iter().map(|ev| NoteEvent {
            start: total.saturating_sub(ev.start + ev.duration), ..*ev
        }).collect();
        p.events.sort_by_key(|e| e.start);
        p
    }
    pub fn invert(&self, axis: Pitch) -> Self {
        let mut p = self.clone();
        for ev in &mut p.events {
            let inv = (axis.0 as i16 * 2 - ev.pitch as i16).clamp(0, 127) as u8;
            ev.pitch = inv;
        }
        p
    }
    pub fn validate_against_scale(&self) -> Vec<ValidationWarning> {
        let Some(scale) = &self.key else { return vec![]; };
        self.events.iter().filter_map(|ev| {
            let pc = crate::pitch::PitchClass(ev.pitch % 12);
            if !scale.contains(pc) {
                Some(ValidationWarning {
                    level: ValidationLevel::Info,
                    message: format!("pitch {} (tick {}) outside {} scale",
                        Pitch(ev.pitch).name(), ev.start, scale.root.name()),
                })
            } else { None }
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        pitch::PitchClass,
        scale::{Mode, Scale},
    };

    fn ev(pitch: u8, start: u32, duration: u32) -> NoteEvent {
        NoteEvent { pitch, start, duration, velocity: 100, channel: 0, voice: 1, slur_group: None }
    }

    fn sample_phrase() -> Phrase {
        let mut p = Phrase::new(1, "test", TimeSignature::four_four(), 120);
        p.bars = 1;
        // Three notes inside a single 4/4 bar (1920 ticks).
        p.events = vec![
            ev(60, 0,    480),
            ev(64, 480,  480),
            ev(67, 960,  480),
        ];
        p
    }

    #[test]
    fn transpose_shifts_all_pitches_and_stays_in_range() {
        let p = sample_phrase().transpose(7);
        assert_eq!(
            p.events.iter().map(|e| e.pitch).collect::<Vec<_>>(),
            vec![67, 71, 74],
        );
        assert!(p.events.iter().all(|e| e.pitch <= 127));

        // Negative transpose works too.
        let down = sample_phrase().transpose(-12);
        assert_eq!(
            down.events.iter().map(|e| e.pitch).collect::<Vec<_>>(),
            vec![48, 52, 55],
        );
    }

    #[test]
    fn retrograde_reverses_event_order_and_preserves_duration() {
        let original = sample_phrase();
        let total = original.duration_ticks();
        let retro = original.retrograde();

        // Phrase length unchanged.
        assert_eq!(retro.duration_ticks(), total);

        // Pitch sequence is the original reversed.
        let original_pitches: Vec<u8> = original.events.iter().map(|e| e.pitch).collect();
        let retro_pitches: Vec<u8> = retro.events.iter().map(|e| e.pitch).collect();
        let mut expected = original_pitches.clone();
        expected.reverse();
        assert_eq!(retro_pitches, expected);

        // Each event was relocated to (total - end), with duration preserved.
        for (orig, r) in original.events.iter().zip(retro.events.iter().rev()) {
            assert_eq!(r.start, total - (orig.start + orig.duration));
            assert_eq!(r.duration, orig.duration);
        }
    }

    #[test]
    fn invert_keeps_axis_and_mirrors_intervals() {
        let mut p = Phrase::new(1, "inv", TimeSignature::four_four(), 120);
        p.bars = 1;
        p.events = vec![ev(60, 0, 240), ev(64, 240, 240), ev(67, 480, 240)];

        let inverted = p.invert(Pitch(60));

        // Axis pitch (60) is fixed.
        assert_eq!(inverted.events[0].pitch, 60);
        // 64 → 56 (M3 above becomes M3 below), 67 → 53 (P5 above becomes P5 below).
        assert_eq!(inverted.events[1].pitch, 56);
        assert_eq!(inverted.events[2].pitch, 53);

        // Intervals (signed) from the axis are mirrored.
        for (orig, inv) in p.events.iter().zip(inverted.events.iter()) {
            let original_offset = orig.pitch as i16 - 60;
            let inverted_offset = inv.pitch  as i16 - 60;
            assert_eq!(original_offset, -inverted_offset);
        }
    }

    #[test]
    fn validate_against_scale_flags_only_chromatic_notes() {
        let mut p = sample_phrase();
        p.key = Some(Scale::new(PitchClass(0), Mode::Major));

        // C, E, G are all diatonic to C major → no warnings.
        assert!(p.validate_against_scale().is_empty());

        // Add a C# (chromatic in C major).
        p.events.push(ev(61, 1440, 240));
        let warnings = p.validate_against_scale();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("C#"));

        // Without a key, validation is a no-op.
        p.key = None;
        assert!(p.validate_against_scale().is_empty());
    }
}
