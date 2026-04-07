use cadenza_theory::{phrase::Phrase, rhythm::TimeSignature};
use crate::file::*;

pub struct MidiWriter;

impl MidiWriter {
    pub const TICKS_PER_QUARTER: u16 = 480;

    pub fn from_phrase(phrase: &Phrase) -> MidiFile {
        let micros = 60_000_000u32 / phrase.tempo as u32;
        let ts = &phrase.time_sig;
        let mut events = vec![
            MidiEvent { delta: 0, message: MidiMessage::Tempo(micros) },
            MidiEvent { delta: 0, message: MidiMessage::TimeSignature {
                numerator: ts.numerator, denominator: ts.denominator, clocks: 24, notated_32nds: 8,
            }},
        ];
        if let Some(ref key) = phrase.key {
            events.push(MidiEvent { delta: 0,
                message: MidiMessage::TrackName(format!("{} {:?}", key.root.name(), key.mode))});
        }

        struct Abs { tick: u32, msg: MidiMessage }
        let mut abs: Vec<Abs> = vec![];
        for ev in &phrase.events {
            abs.push(Abs { tick: ev.start,
                msg: MidiMessage::NoteOn  { channel: ev.channel, pitch: ev.pitch, velocity: ev.velocity }});
            abs.push(Abs { tick: ev.start + ev.duration,
                msg: MidiMessage::NoteOff { channel: ev.channel, pitch: ev.pitch, velocity: 0 }});
        }
        abs.sort_by_key(|e| e.tick);

        let mut last = 0u32;
        for a in abs {
            events.push(MidiEvent { delta: a.tick.saturating_sub(last), message: a.msg });
            last = a.tick;
        }
        events.push(MidiEvent { delta: 0, message: MidiMessage::EndOfTrack });

        MidiFile {
            header: MidiHeader { format: MidiFormat::SingleTrack, ticks_per_quarter: Self::TICKS_PER_QUARTER },
            tracks: vec![MidiTrack { name: Some(phrase.label.clone()), events }],
        }
    }

    pub fn from_phrases(phrases: &[Phrase]) -> MidiFile {
        if phrases.is_empty() {
            return MidiFile {
                header: MidiHeader { format: MidiFormat::MultiTrack, ticks_per_quarter: Self::TICKS_PER_QUARTER },
                tracks: vec![],
            };
        }
        let micros = 60_000_000u32 / phrases[0].tempo as u32;
        let tempo_track = MidiTrack { name: Some("Tempo".into()), events: vec![
            MidiEvent { delta: 0, message: MidiMessage::Tempo(micros) },
            MidiEvent { delta: 0, message: MidiMessage::EndOfTrack },
        ]};
        let mut tracks = vec![tempo_track];
        for p in phrases {
            if let Some(t) = Self::from_phrase(p).tracks.into_iter().next() {
                tracks.push(t);
            }
        }
        MidiFile {
            header: MidiHeader { format: MidiFormat::MultiTrack, ticks_per_quarter: Self::TICKS_PER_QUARTER },
            tracks,
        }
    }

    pub fn to_bytes(file: &MidiFile) -> Vec<u8> {
        let mut out = Vec::with_capacity(1024);
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        let fmt: u16 = match file.header.format {
            MidiFormat::SingleTrack => 0, MidiFormat::MultiTrack => 1, MidiFormat::MultiPattern => 2,
        };
        out.extend_from_slice(&fmt.to_be_bytes());
        out.extend_from_slice(&(file.tracks.len() as u16).to_be_bytes());
        out.extend_from_slice(&file.header.ticks_per_quarter.to_be_bytes());
        for track in &file.tracks {
            let tb = Self::encode_track(track);
            out.extend_from_slice(b"MTrk");
            out.extend_from_slice(&(tb.len() as u32).to_be_bytes());
            out.extend_from_slice(&tb);
        }
        out
    }

    fn encode_track(track: &MidiTrack) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        for ev in &track.events { Self::write_var_len(&mut out, ev.delta); Self::encode_msg(&mut out, &ev.message); }
        out
    }

    fn write_var_len(out: &mut Vec<u8>, mut val: u32) {
        let mut buf = [0u8; 4]; let mut len = 0;
        loop { buf[len] = (val & 0x7F) as u8; len += 1; val >>= 7; if val == 0 { break; } }
        for i in (0..len).rev() { out.push(buf[i] | if i > 0 { 0x80 } else { 0x00 }); }
    }

    fn encode_msg(out: &mut Vec<u8>, msg: &MidiMessage) {
        match msg {
            MidiMessage::NoteOn  { channel, pitch, velocity } => { out.push(0x90|(channel&0xF)); out.push(*pitch); out.push(*velocity); }
            MidiMessage::NoteOff { channel, pitch, velocity } => { out.push(0x80|(channel&0xF)); out.push(*pitch); out.push(*velocity); }
            MidiMessage::ProgramChange { channel, program }   => { out.push(0xC0|(channel&0xF)); out.push(*program); }
            MidiMessage::ControlChange { channel, controller, value } => { out.push(0xB0|(channel&0xF)); out.push(*controller); out.push(*value); }
            MidiMessage::PitchBend { channel, value } => {
                let v = (*value + 8192) as u16;
                out.push(0xE0|(channel&0xF)); out.push((v&0x7F) as u8); out.push(((v>>7)&0x7F) as u8);
            }
            MidiMessage::Tempo(m) => { out.extend_from_slice(&[0xFF,0x51,0x03]); out.push(((m>>16)&0xFF) as u8); out.push(((m>>8)&0xFF) as u8); out.push((m&0xFF) as u8); }
            MidiMessage::TimeSignature { numerator, denominator, clocks, notated_32nds } => {
                out.extend_from_slice(&[0xFF,0x58,0x04,*numerator,(*denominator as f32).log2() as u8,*clocks,*notated_32nds]);
            }
            MidiMessage::KeySignature { sharps_flats, minor } => { out.extend_from_slice(&[0xFF,0x59,0x02,*sharps_flats as u8,*minor as u8]); }
            MidiMessage::TrackName(name) => { out.extend_from_slice(&[0xFF,0x03]); Self::write_var_len(out, name.len() as u32); out.extend_from_slice(name.as_bytes()); }
            MidiMessage::EndOfTrack => { out.extend_from_slice(&[0xFF,0x2F,0x00]); }
            MidiMessage::SysEx(data) => { out.push(0xF0); Self::write_var_len(out, data.len() as u32); out.extend_from_slice(data); out.push(0xF7); }
        }
    }
}
