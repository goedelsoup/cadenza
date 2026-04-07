use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MidiFormat { SingleTrack, MultiTrack, MultiPattern }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiHeader { pub format: MidiFormat, pub ticks_per_quarter: u16 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiEvent { pub delta: u32, pub message: MidiMessage }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MidiMessage {
    NoteOn         { channel: u8, pitch: u8, velocity: u8 },
    NoteOff        { channel: u8, pitch: u8, velocity: u8 },
    ProgramChange  { channel: u8, program: u8 },
    ControlChange  { channel: u8, controller: u8, value: u8 },
    PitchBend      { channel: u8, value: i16 },
    Tempo(u32),
    TimeSignature  { numerator: u8, denominator: u8, clocks: u8, notated_32nds: u8 },
    KeySignature   { sharps_flats: i8, minor: bool },
    TrackName(String),
    EndOfTrack,
    SysEx(Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiTrack { pub name: Option<String>, pub events: Vec<MidiEvent> }

impl MidiTrack {
    pub fn new(name: impl Into<String>) -> Self { Self { name: Some(name.into()), events: vec![] } }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiFile { pub header: MidiHeader, pub tracks: Vec<MidiTrack> }
