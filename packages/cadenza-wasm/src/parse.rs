use cadenza_theory::{phrase::{Phrase, LyricLine, LyricSyllable}, rhythm::{NoteEvent, TimeSignature}, scale::Scale};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AiPhrase {
    #[serde(rename = "type")] pub phrase_type: Option<String>,
    pub summary:        Option<String>,
    pub key:            Option<String>,
    pub tempo:          Option<u16>,
    pub time_signature: Option<String>,
    pub bars:           Option<u8>,
    pub notes:          Option<Vec<AiNote>>,
}

#[derive(Deserialize)]
pub struct AiNote {
    pub pitch: u8,
    pub start: f64,
    pub dur: f64,
    pub vel: Option<u8>,
    pub voice: Option<u8>,
    pub slur: Option<u8>,
}

pub fn parse_ai_phrase(json: &str, fallback_tempo: u16, fallback_ts: &TimeSignature) -> Result<Phrase, String> {
    let ai: AiPhrase = serde_json::from_str(json).map_err(|e| format!("JSON parse: {e}"))?;
    let label  = ai.phrase_type.unwrap_or_else(|| "phrase".into());
    let tempo  = ai.tempo.unwrap_or(fallback_tempo);
    let ts     = ai.time_signature.as_deref().and_then(TimeSignature::parse).unwrap_or_else(|| fallback_ts.clone());
    let bars   = ai.bars.unwrap_or(4);
    let key    = ai.key.as_deref().and_then(|k| {
        let p: Vec<&str> = k.splitn(2, ' ').collect();
        if p.len()==2 { Scale::parse(p[0], p[1]) } else { Scale::parse(k, "major") }
    });
    let events = ai.notes.unwrap_or_default().iter()
        .map(|n| {
            let mut ev = NoteEvent::from_beats(n.pitch, n.start, n.dur, n.vel.unwrap_or(80));
            ev.voice = n.voice.unwrap_or(1).max(1);
            ev.slur_group = n.slur;
            ev
        })
        .collect();
    let mut phrase = Phrase::new(0, label, ts, tempo);
    phrase.key = key; phrase.bars = bars; phrase.events = events;
    Ok(phrase)
}

#[derive(Deserialize)]
struct AiLyricSyllable {
    text:       String,
    #[serde(rename = "wordIndex")]
    word_index: i32,
    stress:     String,
}

#[derive(Deserialize)]
struct AiLyricResponse {
    syllables: Vec<AiLyricSyllable>,
    raw:       String,
}

/// Parse a Claude lyric response into a LyricLine for the given phrase id.
/// Each syllable's note_id is set to its index in the syllables array, on the
/// assumption that the caller has already truncated/padded to phrase.note_count.
pub fn parse_ai_lyric_line(json: &str, phrase_id: u32) -> Result<LyricLine, String> {
    let ai: AiLyricResponse = serde_json::from_str(json).map_err(|e| format!("JSON parse: {e}"))?;
    let syllables = ai.syllables.into_iter().enumerate().map(|(i, s)| LyricSyllable {
        text:       s.text,
        note_id:    i as u32,
        word_index: s.word_index,
        stress:     s.stress,
    }).collect();
    Ok(LyricLine { phrase_id, syllables, raw_text: ai.raw })
}
