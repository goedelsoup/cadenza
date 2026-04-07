//! Basic MIDI file reader — enough to round-trip generated phrases
use crate::file::*;
use crate::error::MidiError;

pub struct MidiReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> MidiReader<'a> {
    pub fn new(data: &'a [u8]) -> Self { Self { data, pos: 0 } }

    fn read_u8(&mut self) -> Result<u8, MidiError> {
        self.data.get(self.pos).copied().map(|b| { self.pos += 1; b }).ok_or(MidiError::UnexpectedEof)
    }

    fn read_u16(&mut self) -> Result<u16, MidiError> {
        let hi = self.read_u8()? as u16;
        let lo = self.read_u8()? as u16;
        Ok((hi << 8) | lo)
    }

    fn read_u32(&mut self) -> Result<u32, MidiError> {
        let a = self.read_u8()? as u32; let b = self.read_u8()? as u32;
        let c = self.read_u8()? as u32; let d = self.read_u8()? as u32;
        Ok((a << 24) | (b << 16) | (c << 8) | d)
    }

    fn read_var_len(&mut self) -> Result<u32, MidiError> {
        let mut val = 0u32;
        for _ in 0..4 {
            let b = self.read_u8()?;
            val = (val << 7) | (b & 0x7F) as u32;
            if b & 0x80 == 0 { return Ok(val); }
        }
        Err(MidiError::VarLenOverflow)
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], MidiError> {
        if self.pos + n > self.data.len() { return Err(MidiError::UnexpectedEof); }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn parse(data: &'a [u8]) -> Result<MidiFile, MidiError> {
        let mut r = Self::new(data);

        // MThd
        let tag = r.read_bytes(4)?;
        if tag != b"MThd" { return Err(MidiError::InvalidHeader); }
        let _len = r.read_u32()?;
        let format = match r.read_u16()? {
            0 => MidiFormat::SingleTrack,
            1 => MidiFormat::MultiTrack,
            2 => MidiFormat::MultiPattern,
            f => return Err(MidiError::UnsupportedFormat(f)),
        };
        let num_tracks = r.read_u16()?;
        let tpq = r.read_u16()?;

        let mut tracks = Vec::with_capacity(num_tracks as usize);
        for _ in 0..num_tracks {
            let tag = r.read_bytes(4)?;
            if tag != b"MTrk" { return Err(MidiError::InvalidTrack(r.pos)); }
            let len = r.read_u32()? as usize;
            let end = r.pos + len;
            tracks.push(r.parse_track(end)?);
        }

        Ok(MidiFile { header: MidiHeader { format, ticks_per_quarter: tpq }, tracks })
    }

    fn parse_track(&mut self, end: usize) -> Result<MidiTrack, MidiError> {
        let mut events = vec![];
        let mut running_status = 0u8;
        let mut name = None;

        while self.pos < end {
            let delta = self.read_var_len()?;
            let first = self.read_u8()?;

            let status = if first & 0x80 != 0 { running_status = first; first }
                         else { running_status };

            let message = if status == 0xFF {
                // Meta event
                let meta_type = if first == 0xFF { self.read_u8()? } else { first };
                let len = self.read_var_len()? as usize;
                let bytes = self.read_bytes(len)?;
                match meta_type {
                    0x03 => {
                        let s = String::from_utf8_lossy(bytes).into_owned();
                        name = Some(s.clone());
                        MidiMessage::TrackName(s)
                    }
                    0x2F => MidiMessage::EndOfTrack,
                    0x51 => {
                        let micros = ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | bytes[2] as u32;
                        MidiMessage::Tempo(micros)
                    }
                    0x58 => MidiMessage::TimeSignature {
                        numerator: bytes[0], denominator: 1 << bytes[1],
                        clocks: bytes[2], notated_32nds: bytes[3],
                    },
                    0x59 => MidiMessage::KeySignature { sharps_flats: bytes[0] as i8, minor: bytes[1] != 0 },
                    _ => { continue; }
                }
            } else {
                let data1 = if first & 0x80 == 0 { first } else { self.read_u8()? };
                let kind = status & 0xF0;
                let ch = status & 0x0F;
                match kind {
                    0x80 => { let v = self.read_u8()?; MidiMessage::NoteOff { channel: ch, pitch: data1, velocity: v } }
                    0x90 => { let v = self.read_u8()?; MidiMessage::NoteOn  { channel: ch, pitch: data1, velocity: v } }
                    0xB0 => { let v = self.read_u8()?; MidiMessage::ControlChange { channel: ch, controller: data1, value: v } }
                    0xC0 => MidiMessage::ProgramChange { channel: ch, program: data1 },
                    0xE0 => { let hi = self.read_u8()?; MidiMessage::PitchBend { channel: ch, value: (((hi as i16) << 7) | data1 as i16) - 8192 } }
                    _ => continue,
                }
            };

            events.push(MidiEvent { delta, message });
        }

        Ok(MidiTrack { name, events })
    }
}
