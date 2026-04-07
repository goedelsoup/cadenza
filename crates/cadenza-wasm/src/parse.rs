//! Parse AI-generated JSON into cadenza-theory types

use cadenza_theory::{
    phrase::Phrase,
    rhythm::{NoteEvent, TimeSignature},
    scale::Scale,
};
use serde::Deserialize;
use serde_json::Value;

/// Shape Claude is instructed to produce
#[derive(Deserialize)]
pub struct AiPhrase {
    #[serde(rename = "type")]
    pub phrase_type: Option<String>,
    pub summary: Option<String>,
    pub key: Option<String>,
    pub tempo: Option<u16>,
    pub time_signature: Option<String>,
    pub bars: Option<u8>,
    pub chords: Option<Vec<String>>,
    pub notes: Option<Vec<AiNote>>,
}

#[derive(Deserialize)]
pub struct AiNote {
    pub pitch: u8,
    pub start: f64,
    pub dur: f64,
    pub vel: Option<u8>,
}

pub fn parse_ai_phrase(json: &str, fallback_tempo: u16, fallback_ts: &TimeSignature) -> Result<Phrase, String> {
    let ai: AiPhrase = serde_json::from_str(json)
        .map_err(|e| format!("JSON parse error: {e}"))?;

    let label = ai.phrase_type.unwrap_or_else(|| "phrase".into());
    let tempo = ai.tempo.unwrap_or(fallback_tempo);
    let ts = ai.time_signature.as_deref()
        .and_then(TimeSignature::parse)
        .unwrap_or_else(|| fallback_ts.clone());
    let bars = ai.bars.unwrap_or(4);

    let key = ai.key.as_deref().and_then(|k| {
        let parts: Vec<&str> = k.splitn(2, ' ').collect();
        if parts.len() == 2 { Scale::parse(parts[0], parts[1]) }
        else { Scale::parse(k, "major") }
    });

    let events: Vec<NoteEvent> = ai.notes.unwrap_or_default()
        .iter()
        .map(|n| NoteEvent::from_beats(n.pitch, n.start, n.dur, n.vel.unwrap_or(80)))
        .collect();

    let mut phrase = Phrase::new(0, label, ts, tempo);
    phrase.key = key;
    phrase.bars = bars;
    phrase.events = events;

    Ok(phrase)
}
