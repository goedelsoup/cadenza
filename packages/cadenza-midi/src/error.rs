use thiserror::Error;
#[derive(Debug, Error)]
pub enum MidiError {
    #[error("invalid MIDI header chunk")]           InvalidHeader,
    #[error("invalid track chunk at offset {0}")]   InvalidTrack(usize),
    #[error("unexpected end of data")]              UnexpectedEof,
    #[error("unsupported MIDI format {0}")]         UnsupportedFormat(u16),
    #[error("variable-length encoding overflow")]   VarLenOverflow,
}
