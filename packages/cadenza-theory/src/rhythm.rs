use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Duration(pub u32);

impl Duration {
    pub const WHOLE:             Self = Self(1920);
    pub const HALF:              Self = Self(960);
    pub const QUARTER:           Self = Self(480);
    pub const EIGHTH:            Self = Self(240);
    pub const SIXTEENTH:         Self = Self(120);
    pub const TRIPLET_QUARTER:   Self = Self(320);
    pub const TRIPLET_EIGHTH:    Self = Self(160);

    pub fn from_beats(beats: f64) -> Self { Self((beats * 480.0).round() as u32) }
    pub fn beats(&self) -> f64 { self.0 as f64 / 480.0 }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSignature { pub numerator: u8, pub denominator: u8 }

impl TimeSignature {
    pub fn four_four()  -> Self { Self { numerator: 4, denominator: 4 } }
    pub fn three_four() -> Self { Self { numerator: 3, denominator: 4 } }
    pub fn six_eight()  -> Self { Self { numerator: 6, denominator: 8 } }
    pub fn ticks_per_bar(&self) -> u32 {
        let qt = 480u32;
        let bt = match self.denominator { 4 => qt, 8 => qt/2, 2 => qt*2, _ => qt };
        bt * self.numerator as u32
    }
    pub fn parse(s: &str) -> Option<Self> {
        let p: Vec<&str> = s.split('/').collect();
        if p.len() != 2 { return None; }
        Some(Self { numerator: p[0].trim().parse().ok()?, denominator: p[1].trim().parse().ok()? })
    }
}

fn default_voice() -> u8 { 1 }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteEvent {
    pub pitch: u8,
    pub start: u32,
    pub duration: u32,
    pub velocity: u8,
    pub channel: u8,
    #[serde(default = "default_voice")]
    pub voice: u8,
    #[serde(default)]
    pub slur_group: Option<u8>,
}

impl NoteEvent {
    pub fn from_beats(pitch: u8, start_beats: f64, dur_beats: f64, velocity: u8) -> Self {
        Self {
            pitch,
            start:    Duration::from_beats(start_beats).0,
            duration: Duration::from_beats(dur_beats).0,
            velocity,
            channel: 0,
            voice: 1,
            slur_group: None,
        }
    }
}
