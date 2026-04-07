use wasm_bindgen::prelude::*;
use cadenza_theory::{phrase::Phrase, rhythm::TimeSignature, scale::Scale, pitch::PitchClass};
use cadenza_midi::writer::MidiWriter;
use cadenza_musicxml::renderer::MusicXmlRenderer;
use crate::{session::Session, parse::{parse_ai_phrase, parse_ai_lyric_line}};
use std::cell::RefCell;

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
}

// ── Config ────────────────────────────────────────────────────────────────────
#[wasm_bindgen] pub fn set_tempo(bpm: u16) { SESSION.with(|s| s.borrow_mut().tempo = bpm); }
#[wasm_bindgen] pub fn set_key(root: &str, mode: &str) {
    if let Some(s) = Scale::parse(root, mode) { SESSION.with(|sess| sess.borrow_mut().key = Some(s)); }
}
#[wasm_bindgen] pub fn set_time_signature(sig: &str) {
    if let Some(ts) = TimeSignature::parse(sig) { SESSION.with(|s| s.borrow_mut().time_sig = ts); }
}

// ── Phrase summary (JS-visible) ───────────────────────────────────────────────
#[wasm_bindgen]
pub struct JsPhraseSummary { pub id: u32, label: String, pub bars: u8, pub tempo: u16, pub note_count: usize, warnings: Vec<String> }
#[wasm_bindgen]
impl JsPhraseSummary {
    #[wasm_bindgen(getter)] pub fn label(&self) -> String { self.label.clone() }
    #[wasm_bindgen(getter)] pub fn warnings_json(&self) -> String { serde_json::to_string(&self.warnings).unwrap_or_default() }
}

// ── Ingest ────────────────────────────────────────────────────────────────────
#[wasm_bindgen]
pub fn ingest_phrase(json: &str) -> Result<JsPhraseSummary, JsError> {
    let (tempo, ts) = SESSION.with(|s| { let s = s.borrow(); (s.tempo, s.time_sig.clone()) });
    let phrase = parse_ai_phrase(json, tempo, &ts).map_err(|e| JsError::new(&e))?;
    let warnings = phrase.validate_against_scale().into_iter().map(|w| w.message).collect::<Vec<_>>();
    let summary = JsPhraseSummary { id: 0, label: phrase.label.clone(), bars: phrase.bars, tempo: phrase.tempo, note_count: phrase.events.len(), warnings };
    let id = SESSION.with(|s| s.borrow_mut().add_phrase(phrase));
    Ok(JsPhraseSummary { id, ..summary })
}

// ── Lyrics ────────────────────────────────────────────────────────────────────
#[wasm_bindgen]
pub fn attach_lyrics(phrase_id: u32, json: &str) -> Result<(), JsError> {
    let line = parse_ai_lyric_line(json, phrase_id).map_err(|e| JsError::new(&e))?;
    SESSION.with(|s| s.borrow_mut().attach_lyrics(phrase_id, line)).map_err(|e| JsError::new(&e))
}

// ── Export ────────────────────────────────────────────────────────────────────
#[wasm_bindgen]
pub fn phrase_to_midi(phrase_id: u32) -> Result<Vec<u8>, JsError> {
    SESSION.with(|s| {
        let s = s.borrow();
        let p = s.get(phrase_id).ok_or_else(|| JsError::new(&format!("phrase {phrase_id} not found")))?;
        Ok(MidiWriter::to_bytes(&MidiWriter::from_phrase(p)))
    })
}
#[wasm_bindgen]
pub fn session_to_midi() -> Vec<u8> {
    SESSION.with(|s| {
        let s = s.borrow();
        let phrases: Vec<Phrase> = s.phrases.values().cloned().collect();
        MidiWriter::to_bytes(&MidiWriter::from_phrases(&phrases))
    })
}
#[wasm_bindgen]
pub fn phrase_to_musicxml(phrase_id: u32, title: &str, composer: &str) -> Result<String, JsError> {
    SESSION.with(|s| {
        let s = s.borrow();
        let p = s.get(phrase_id).ok_or_else(|| JsError::new(&format!("phrase {phrase_id} not found")))?;
        Ok(MusicXmlRenderer::from_phrase(p, title, composer))
    })
}

// ── Theory queries ────────────────────────────────────────────────────────────
#[wasm_bindgen]
pub fn scale_pitches(root: &str, mode: &str) -> Vec<u8> {
    Scale::parse(root, mode)
        .map(|s| s.pitch_classes().iter().filter_map(|pc| pc.in_octave(4)).map(|p| p.midi()).collect())
        .unwrap_or_default()
}
#[wasm_bindgen]
pub fn is_diatonic(midi_pitch: u8) -> bool {
    SESSION.with(|s| s.borrow().key.as_ref().map(|k| k.contains(PitchClass(midi_pitch%12))).unwrap_or(true))
}
#[wasm_bindgen]
pub fn phrase_ids() -> String {
    SESSION.with(|s| { let ids: Vec<u32> = s.borrow().phrases.keys().copied().collect(); serde_json::to_string(&ids).unwrap_or_default() })
}
#[wasm_bindgen]
pub fn clear_session() { SESSION.with(|s| *s.borrow_mut() = Session::default()); }

#[wasm_bindgen(start)]
pub fn start() {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();
}
