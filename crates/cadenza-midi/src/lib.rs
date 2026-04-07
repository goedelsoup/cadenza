//! cadenza-midi — MIDI file serialization / deserialization

pub mod file;
pub mod error;
pub mod writer;
pub mod reader;

pub use file::{MidiFile, MidiHeader, MidiTrack, MidiEvent, MidiMessage};
pub use error::MidiError;
pub use writer::MidiWriter;
