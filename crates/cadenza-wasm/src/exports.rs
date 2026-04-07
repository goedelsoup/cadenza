use wasm_bindgen::prelude::*;
use cadenza_theory::{
    phrase::Phrase,
    rhythm::TimeSignature,
    scale::Scale,
    pitch::PitchClass,
};
use cadenza_midi::writer::MidiWriter;
use cadenza_musicxml::renderer::MusicXmlRenderer;
use crate::session::Session;
use crate::parse::parse_ai_phrase;
use std::cell::RefCell;

// Thread-local session (single-threaded WASM)
thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
}

/// JS-visible wrapper for a phrase summary
#[wasm_bindgen]
pub struct JsPhraseSummary {
    id: u32,
    label: String,
    bars: u8,
    tempo: u16,
    note_count: usize,
    warnings: Vec<String>,
}

#[wasm_bindgen]
impl JsPhraseSummary {
    #[wasm_bindgen(getter)] pub fn id(&self) -> u32 { self.id }
    #[wasm_bindgen(getter)] pub fn label(&self) -> String { self.label.clone() }
    #[wasm_bindgen(getter)] pub fn bars(&self) -> u8 { self.bars }
    #[wasm_bindgen(getter)] pub fn tempo(&self) -> u16 { self.tempo }
    #[wasm_bindgen(getter)] pub fn note_count(&self) -> usize { self.note_count }
    #[wasm_bindgen(getter)] pub fn warnings_json(&self) -> String {
        serde_json::to_string(&self.warnings).unwrap_or_default()
    }
}

// ── Session config ────────────────────────────────────────────────────────

#[wasm_bindgen]
pub fn set_tempo(bpm: u16) {
    SESSION.with(|s| s.borrow_mut().tempo = bpm);
}

#[wasm_bindgen]
pub fn set_key(root: &str, mode: &str) {
    if let Some(scale) = Scale::parse(root, mode) {
        SESSION.with(|s| s.borrow_mut().key = Some(scale));
    }
}

#[wasm_bindgen]
pub fn set_time_signature(sig: &str) {
    if let Some(ts) = TimeSignature::parse(sig) {
        SESSION.with(|s| s.borrow_mut().time_sig = ts);
    }
}

// ── Phrase ingestion ──────────────────────────────────────────────────────

/// Parse JSON from the AI layer, validate, store in session.
/// Returns JsPhraseSummary on success, throws on parse failure.
#[wasm_bindgen]
pub fn ingest_phrase(json: &str) -> Result<JsPhraseSummary, JsError> {
    let (tempo, ts) = SESSION.with(|s| {
        let s = s.borrow();
        (s.tempo, s.time_sig.clone())
    });

    let phrase = parse_ai_phrase(json, tempo, &ts)
        .map_err(|e| JsError::new(&e))?;

    let warnings = phrase.validate_against_scale()
        .into_iter()
        .map(|w| w.message)
        .collect::<Vec<_>>();

    let summary = JsPhraseSummary {
        id: 0,  // will be set after insert
        label: phrase.label.clone(),
        bars: phrase.bars,
        tempo: phrase.tempo,
        note_count: phrase.events.len(),
        warnings,
    };

    let id = SESSION.with(|s| s.borrow_mut().add_phrase(phrase));

    Ok(JsPhraseSummary { id, ..summary })
}

// ── Export ────────────────────────────────────────────────────────────────

/// Returns raw MIDI bytes for a stored phrase
#[wasm_bindgen]
pub fn phrase_to_midi(phrase_id: u32) -> Result<Vec<u8>, JsError> {
    SESSION.with(|s| {
        let s = s.borrow();
        let phrase = s.get(phrase_id)
            .ok_or_else(|| JsError::new(&format!("phrase {phrase_id} not found")))?;
        let midi_file = MidiWriter::from_phrase(phrase);
        Ok(MidiWriter::to_bytes(&midi_file))
    })
}

/// Returns all stored phrases as a multi-track MIDI file
#[wasm_bindgen]
pub fn session_to_midi() -> Vec<u8> {
    SESSION.with(|s| {
        let s = s.borrow();
        let phrases: Vec<&Phrase> = s.phrases.values().collect();
        let midi_file = MidiWriter::from_phrases(
            &phrases.into_iter().cloned().collect::<Vec<_>>()
        );
        MidiWriter::to_bytes(&midi_file)
    })
}

/// Returns MusicXML string for a stored phrase
#[wasm_bindgen]
pub fn phrase_to_musicxml(phrase_id: u32, title: &str, composer: &str) -> Result<String, JsError> {
    SESSION.with(|s| {
        let s = s.borrow();
        let phrase = s.get(phrase_id)
            .ok_or_else(|| JsError::new(&format!("phrase {phrase_id} not found")))?;
        Ok(MusicXmlRenderer::from_phrase(phrase, title, composer))
    })
}

// ── Theory utilities (UI hints) ────────────────────────────────────────────

/// Scale pitch classes as MIDI note numbers in octave 4
#[wasm_bindgen]
pub fn scale_pitches(root: &str, mode: &str) -> Vec<u8> {
    Scale::parse(root, mode)
        .map(|s| s.pitch_classes().iter()
            .filter_map(|pc| pc.in_octave(4))
            .map(|p| p.midi())
            .collect())
        .unwrap_or_default()
}

/// Check if a MIDI pitch is diatonic to the current session key
#[wasm_bindgen]
pub fn is_diatonic(midi_pitch: u8) -> bool {
    SESSION.with(|s| {
        let s = s.borrow();
        s.key.as_ref().map(|k| k.contains(PitchClass(midi_pitch % 12))).unwrap_or(true)
    })
}

/// Stored phrase IDs as JSON array
#[wasm_bindgen]
pub fn phrase_ids() -> String {
    SESSION.with(|s| {
        let ids: Vec<u32> = s.borrow().phrases.keys().copied().collect();
        serde_json::to_string(&ids).unwrap_or_default()
    })
}

/// Clear session
#[wasm_bindgen]
pub fn clear_session() {
    SESSION.with(|s| *s.borrow_mut() = Session::default());
}

#[wasm_bindgen(start)]
pub fn start() {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();
}
