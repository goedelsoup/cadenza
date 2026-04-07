use crate::{error::MidiError, file::*};

pub struct MidiReader<'a> { data: &'a [u8], pos: usize }

impl<'a> MidiReader<'a> {
    pub fn new(data: &'a [u8]) -> Self { Self { data, pos: 0 } }
    fn u8(&mut self) -> Result<u8, MidiError> {
        self.data.get(self.pos).copied().map(|b| { self.pos += 1; b }).ok_or(MidiError::UnexpectedEof)
    }
    fn u16(&mut self) -> Result<u16, MidiError> { Ok(((self.u8()? as u16)<<8)|self.u8()? as u16) }
    fn u32(&mut self) -> Result<u32, MidiError> {
        Ok(((self.u8()? as u32)<<24)|((self.u8()? as u32)<<16)|((self.u8()? as u32)<<8)|self.u8()? as u32)
    }
    fn var_len(&mut self) -> Result<u32, MidiError> {
        let mut val = 0u32;
        for _ in 0..4 { let b = self.u8()?; val = (val<<7)|(b&0x7F) as u32; if b&0x80==0 { return Ok(val); } }
        Err(MidiError::VarLenOverflow)
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], MidiError> {
        if self.pos+n > self.data.len() { return Err(MidiError::UnexpectedEof); }
        let s = &self.data[self.pos..self.pos+n]; self.pos += n; Ok(s)
    }
    pub fn parse(data: &'a [u8]) -> Result<MidiFile, MidiError> {
        let mut r = Self::new(data);
        if r.bytes(4)? != b"MThd" { return Err(MidiError::InvalidHeader); }
        let _ = r.u32()?;
        let format = match r.u16()? { 0=>MidiFormat::SingleTrack, 1=>MidiFormat::MultiTrack, 2=>MidiFormat::MultiPattern, f=>return Err(MidiError::UnsupportedFormat(f)) };
        let ntracks = r.u16()?;
        let tpq = r.u16()?;
        let mut tracks = Vec::with_capacity(ntracks as usize);
        for _ in 0..ntracks {
            if r.bytes(4)? != b"MTrk" { return Err(MidiError::InvalidTrack(r.pos)); }
            let len = r.u32()? as usize;
            let end = r.pos + len;
            tracks.push(r.parse_track(end)?);
        }
        Ok(MidiFile { header: MidiHeader { format, ticks_per_quarter: tpq }, tracks })
    }
    fn parse_track(&mut self, end: usize) -> Result<MidiTrack, MidiError> {
        let mut events = vec![]; let mut rs = 0u8; let mut name = None;
        while self.pos < end {
            let delta = self.var_len()?;
            let first = self.u8()?;
            let status = if first&0x80!=0 { rs=first; first } else { rs };
            let message = if status==0xFF {
                let mt = if first==0xFF { self.u8()? } else { first };
                let len = self.var_len()? as usize;
                let b = self.bytes(len)?;
                match mt {
                    0x03 => { let s = String::from_utf8_lossy(b).into_owned(); name=Some(s.clone()); MidiMessage::TrackName(s) }
                    0x2F => MidiMessage::EndOfTrack,
                    0x51 => MidiMessage::Tempo(((b[0] as u32)<<16)|((b[1] as u32)<<8)|b[2] as u32),
                    0x58 => MidiMessage::TimeSignature { numerator:b[0], denominator:1<<b[1], clocks:b[2], notated_32nds:b[3] },
                    0x59 => MidiMessage::KeySignature { sharps_flats:b[0] as i8, minor:b[1]!=0 },
                    _ => continue,
                }
            } else {
                let d1 = if first&0x80==0 { first } else { self.u8()? };
                let ch = status&0x0F;
                match status&0xF0 {
                    0x80 => { let v=self.u8()?; MidiMessage::NoteOff { channel:ch, pitch:d1, velocity:v } }
                    0x90 => { let v=self.u8()?; MidiMessage::NoteOn  { channel:ch, pitch:d1, velocity:v } }
                    0xB0 => { let v=self.u8()?; MidiMessage::ControlChange { channel:ch, controller:d1, value:v } }
                    0xC0 => MidiMessage::ProgramChange { channel:ch, program:d1 },
                    0xE0 => { let hi=self.u8()?; MidiMessage::PitchBend { channel:ch, value:(((hi as i16)<<7)|d1 as i16)-8192 } }
                    _ => continue,
                }
            };
            events.push(MidiEvent { delta, message });
        }
        Ok(MidiTrack { name, events })
    }
}
