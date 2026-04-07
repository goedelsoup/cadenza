//! IPC message protocol shared between `cadenza-daemon` and the TS frontend.
//!
//! Wire format is JSON with serde's externally-tagged enum representation:
//!
//! ```json
//! { "LoadPlugin": { "path": "/path/to/plugin.vst3" } }
//! { "PluginLoaded": { "id": 1, "name": "...", "params": [] } }
//! ```
//!
//! The TS bridge in `cadenza-api/src/daemon-bridge.ts` mirrors these shapes.

use cadenza_theory::phrase::Phrase;
use serde::{Deserialize, Serialize};

pub type PluginId = u32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginParam {
    pub id:      u32,
    pub name:    String,
    pub min:     f32,
    pub max:     f32,
    pub default: f32,
}

/// Bidirectional message envelope. The same enum is used for both directions
/// so the TS bridge can use a single discriminated union.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonMessage {
    // ── frontend → daemon ────────────────────────────────────────────────
    ScanPlugins   { dir: String },
    LoadPlugin    { path: String },
    UnloadPlugin  { id: PluginId },
    /// Activate a previously-loaded plugin as the audio thread's current
    /// instrument. Sends `PluginActivated` on success.
    SetInstrument { plugin_id: PluginId },
    /// Switch the audio thread back to the built-in PolySynth.
    UseBuiltinSynth,
    PlayPhrase    { phrase: Phrase, plugin_id: Option<PluginId> },
    Stop,
    SetTempo(u16),
    SetParam      { plugin_id: PluginId, param_id: u32, value: f32 },
    Ping,

    // ── daemon → frontend ────────────────────────────────────────────────
    ScannedPlugins { paths: Vec<String> },
    PluginLoaded   { id: PluginId, name: String, params: Vec<PluginParam> },
    PluginUnloaded { id: PluginId },
    PluginActivated { id: PluginId },
    BuiltinSynthActivated,
    PlaybackStarted,
    PlaybackStopped,
    Pong,
    Error(String),
}

impl DaemonMessage {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadenza_theory::rhythm::TimeSignature;

    /// Round-trip a message through JSON and assert the discriminant matches.
    /// We compare on serialized form rather than `PartialEq` because `Phrase`
    /// does not implement it.
    fn round_trip(msg: &DaemonMessage) {
        let json = msg.to_json().expect("serialize");
        let parsed = DaemonMessage::from_json(&json).expect("deserialize");
        let json2 = parsed.to_json().expect("re-serialize");
        assert_eq!(json, json2, "round-trip mismatch for {msg:?}");
    }

    fn sample_phrase() -> cadenza_theory::phrase::Phrase {
        cadenza_theory::phrase::Phrase::new(1, "test", TimeSignature::four_four(), 120)
    }

    #[test]
    fn round_trip_all_variants() {
        let variants = vec![
            DaemonMessage::Ping,
            DaemonMessage::Pong,
            DaemonMessage::Stop,
            DaemonMessage::SetTempo(140),
            DaemonMessage::Error("boom".into()),
            DaemonMessage::ScanPlugins  { dir: "/plugins".into() },
            DaemonMessage::LoadPlugin   { path: "/x.vst3".into() },
            DaemonMessage::UnloadPlugin { id: 7 },
            DaemonMessage::SetInstrument { plugin_id: 4 },
            DaemonMessage::UseBuiltinSynth,
            DaemonMessage::PlayPhrase   { phrase: sample_phrase(), plugin_id: None },
            DaemonMessage::PlayPhrase   { phrase: sample_phrase(), plugin_id: Some(3) },
            DaemonMessage::SetParam     { plugin_id: 1, param_id: 2, value: 0.5 },
            DaemonMessage::ScannedPlugins { paths: vec!["/p/a.vst3".into(), "/p/b.clap".into()] },
            DaemonMessage::PluginLoaded {
                id: 1,
                name: "Test".into(),
                params: vec![PluginParam { id: 1, name: "Cutoff".into(), min: 0.0, max: 1.0, default: 0.5 }],
            },
            DaemonMessage::PluginUnloaded { id: 1 },
            DaemonMessage::PluginActivated { id: 1 },
            DaemonMessage::BuiltinSynthActivated,
            DaemonMessage::PlaybackStarted,
            DaemonMessage::PlaybackStopped,
        ];
        for v in &variants { round_trip(v); }
    }

    #[test]
    fn ping_uses_externally_tagged_form() {
        // Unit variants serialize as JSON strings; this is what the TS
        // bridge sends on the wire.
        assert_eq!(DaemonMessage::Ping.to_json().unwrap(), "\"Ping\"");
    }

    #[test]
    fn load_plugin_wire_shape_is_stable() {
        let json = DaemonMessage::LoadPlugin { path: "/p.vst3".into() }
            .to_json()
            .unwrap();
        assert_eq!(json, r#"{"LoadPlugin":{"path":"/p.vst3"}}"#);
    }

    #[test]
    fn malformed_input_returns_err() {
        assert!(DaemonMessage::from_json("not json").is_err());
        assert!(DaemonMessage::from_json(r#"{"NoSuchVariant":{}}"#).is_err());
    }
}
