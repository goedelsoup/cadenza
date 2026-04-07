#![cfg_attr(not(feature = "std"), no_std)]

pub mod pitch;
pub mod scale;
pub mod chord;
pub mod interval;
pub mod rhythm;
pub mod phrase;
pub mod validation;

pub use pitch::{Pitch, PitchClass};
pub use scale::{Scale, Mode};
pub use chord::{Chord, ChordQuality, Extension};
pub use interval::Interval;
pub use rhythm::{Duration, TimeSignature, NoteEvent};
pub use phrase::{Phrase, LyricLine, LyricSyllable};
pub use validation::{ValidationWarning, ValidationLevel};
