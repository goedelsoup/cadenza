use cadenza_theory::phrase::Phrase;
use cadenza_theory::rhythm::TimeSignature;
use crate::file::*;

pub struct MidiWriter;

impl MidiWriter {
    const TICKS_PER_QUARTER: u16 = 480;

    pub fn from_phrase(phrase: &Phrase) -> MidiFile {
        let micros_per_beat = 60_000_000u32 / phrase.tempo as u32;
        let ts = &phrase.time_sig;

        let mut events: Vec<MidiEvent> = vec![];

        // Header meta events (delta=0)
        events.push(MidiEvent { delta: 0, message: MidiMessage::Tempo(micros_per_beat) });
        events.push(MidiEvent {
            delta: 0,
            message: MidiMessage::TimeSignature {
                numerator: ts.numerator,
                denominator: ts.denominator,
                clocks: 24,
                notated_32nds: 8,
            }
        });
        if let Some(ref name) = phrase.key {
            events.push(MidiEvent {
                delta: 0,
                message: MidiMessage::TrackName(
                    format!("{} {:?}", name.root.name(), name.mode)
                ),
            });
        }

        // Collect absolute note-on / note-off pairs
        struct AbsEvent { tick: u32, msg: MidiMessage }
        let mut abs: Vec<AbsEvent> = vec![];

        for ev in &phrase.events {
            abs.push(AbsEvent {
                tick: ev.start,
                msg: MidiMessage::NoteOn { channel: ev.channel, pitch: ev.pitch, velocity: ev.velocity }
            });
            abs.push(AbsEvent {
                tick: ev.start + ev.duration,
                msg: MidiMessage::NoteOff { channel: ev.channel, pitch: ev.pitch, velocity: 0 }
            });
        }

        abs.sort_by_key(|e| e.tick);

        // Convert to delta-time
        let mut last_tick = 0u32;
        for ae in abs {
            let delta = ae.tick.saturating_sub(last_tick);
            last_tick = ae.tick;
            events.push(MidiEvent { delta, message: ae.msg });
        }

        events.push(MidiEvent { delta: 0, message: MidiMessage::EndOfTrack });

        let track = MidiTrack { name: Some(phrase.label.clone()), events };

        MidiFile {
            header: MidiHeader {
                format: MidiFormat::SingleTrack,
                ticks_per_quarter: Self::TICKS_PER_QUARTER,
            },
            tracks: vec![track],
        }
    }

    pub fn from_phrases(phrases: &[Phrase]) -> MidiFile {
        // Multi-track: tempo track + one track per phrase
        if phrases.is_empty() {
            return MidiFile {
                header: MidiHeader { format: MidiFormat::MultiTrack, ticks_per_quarter: Self::TICKS_PER_QUARTER },
                tracks: vec![],
            };
        }

        let first = &phrases[0];
        let micros = 60_000_000u32 / first.tempo as u32;

        let tempo_track = MidiTrack {
            name: Some("Tempo".into()),
            events: vec![
                MidiEvent { delta: 0, message: MidiMessage::Tempo(micros) },
                MidiEvent { delta: 0, message: MidiMessage::EndOfTrack },
            ],
        };

        let mut tracks = vec![tempo_track];
        for phrase in phrases {
            let single = Self::from_phrase(phrase);
            if let Some(t) = single.tracks.into_iter().next() {
                tracks.push(t);
            }
        }

        MidiFile {
            header: MidiHeader { format: MidiFormat::MultiTrack, ticks_per_quarter: Self::TICKS_PER_QUARTER },
            tracks,
        }
    }

    /// Serialize MidiFile to raw bytes
    pub fn to_bytes(file: &MidiFile) -> Vec<u8> {
        let mut out = Vec::with_capacity(1024);

        // Header chunk
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        let fmt: u16 = match file.header.format {
            MidiFormat::SingleTrack  => 0,
            MidiFormat::MultiTrack   => 1,
            MidiFormat::MultiPattern => 2,
        };
        out.extend_from_slice(&fmt.to_be_bytes());
        out.extend_from_slice(&(file.tracks.len() as u16).to_be_bytes());
        out.extend_from_slice(&file.header.ticks_per_quarter.to_be_bytes());

        // Track chunks
        for track in &file.tracks {
            let track_bytes = Self::encode_track(track);
            out.extend_from_slice(b"MTrk");
            out.extend_from_slice(&(track_bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(&track_bytes);
        }

        out
    }

    fn encode_track(track: &MidiTrack) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        for ev in &track.events {
            Self::write_var_len(&mut out, ev.delta);
            Self::encode_message(&mut out, &ev.message);
        }
        out
    }

    fn write_var_len(out: &mut Vec<u8>, mut val: u32) {
        let mut bytes = [0u8; 4];
        let mut len = 0usize;
        loop {
            bytes[len] = (val & 0x7F) as u8;
            len += 1;
            val >>= 7;
            if val == 0 { break; }
        }
        for i in (0..len).rev() {
            out.push(bytes[i] | if i > 0 { 0x80 } else { 0x00 });
        }
    }

    fn encode_message(out: &mut Vec<u8>, msg: &MidiMessage) {
        match msg {
            MidiMessage::NoteOn { channel, pitch, velocity } => {
                out.push(0x90 | (channel & 0x0F));
                out.push(*pitch); out.push(*velocity);
            }
            MidiMessage::NoteOff { channel, pitch, velocity } => {
                out.push(0x80 | (channel & 0x0F));
                out.push(*pitch); out.push(*velocity);
            }
            MidiMessage::ProgramChange { channel, program } => {
                out.push(0xC0 | (channel & 0x0F));
                out.push(*program);
            }
            MidiMessage::ControlChange { channel, controller, value } => {
                out.push(0xB0 | (channel & 0x0F));
                out.push(*controller); out.push(*value);
            }
            MidiMessage::PitchBend { channel, value } => {
                let v = (*value + 8192) as u16;
                out.push(0xE0 | (channel & 0x0F));
                out.push((v & 0x7F) as u8);
                out.push(((v >> 7) & 0x7F) as u8);
            }
            MidiMessage::Tempo(micros) => {
                out.extend_from_slice(&[0xFF, 0x51, 0x03]);
                out.push(((micros >> 16) & 0xFF) as u8);
                out.push(((micros >> 8) & 0xFF) as u8);
                out.push((micros & 0xFF) as u8);
            }
            MidiMessage::TimeSignature { numerator, denominator, clocks, notated_32nds } => {
                out.extend_from_slice(&[0xFF, 0x58, 0x04]);
                out.push(*numerator);
                out.push((*denominator as f32).log2() as u8);
                out.push(*clocks); out.push(*notated_32nds);
            }
            MidiMessage::KeySignature { sharps_flats, minor } => {
                out.extend_from_slice(&[0xFF, 0x59, 0x02]);
                out.push(*sharps_flats as u8);
                out.push(*minor as u8);
            }
            MidiMessage::TrackName(name) => {
                out.extend_from_slice(&[0xFF, 0x03]);
                Self::write_var_len(out, name.len() as u32);
                out.extend_from_slice(name.as_bytes());
            }
            MidiMessage::EndOfTrack => {
                out.extend_from_slice(&[0xFF, 0x2F, 0x00]);
            }
            MidiMessage::SysEx(data) => {
                out.push(0xF0);
                Self::write_var_len(out, data.len() as u32);
                out.extend_from_slice(data);
                out.push(0xF7);
            }
        }
    }
}
