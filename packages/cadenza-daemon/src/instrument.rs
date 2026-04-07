//! Audio-thread instrument trait.
//!
//! All sound producers — the built-in `PolySynth`, hosted VST3 plugins, and
//! hosted CLAP plugins — implement `Instrument`. The audio thread holds a
//! single `Box<dyn Instrument + Send>` and dispatches note events through
//! it with sample-accurate timing.
//!
//! ## Constraints
//!
//! Implementations MUST NOT allocate or take locks inside
//! `render_with_events` or any of the `note_*` methods. They run on the
//! cpal callback thread and any blocking would cause audio dropouts.
//!
//! Plugin loading, parameter changes, and state queries must happen on the
//! control task (typically `tokio::task::spawn_blocking`). The control task
//! constructs a fresh `Box<dyn Instrument + Send>` and hands it to the
//! audio thread via the swap ringbuf in [`crate::audio::AudioEngine`].

use crate::audio::AudioCmd;
use cadenza_ipc::PluginId;

/// Type-erased boxed instrument the audio thread renders through.
pub type InstrumentBox = Box<dyn Instrument + Send>;

/// Sentinel id for the built-in `PolySynth`. `PluginHost` assigns ids to
/// loaded plugins starting at 1, so 0 is reserved for the engine's own
/// instrument and is never confused with a hosted plugin.
pub const BUILTIN_PLUGIN_ID: PluginId = 0;

pub trait Instrument: Send {
    // note_on / note_off are part of the trait surface for callers that
    // want to drive an instrument directly (e.g. test mocks, or future
    // plugin-parameter probes). The audio thread itself goes through
    // `render_with_events`, which dispatches via the `AudioCmd` events
    // it receives — see PolySynth::render_interleaved_with_events.
    #[allow(dead_code)]
    fn note_on(&mut self, pitch: u8, velocity: u8);
    #[allow(dead_code)]
    fn note_off(&mut self, pitch: u8);
    fn all_notes_off(&mut self);

    /// Render `out.len() / channels` interleaved frames of audio with
    /// sample-accurate event dispatch.
    ///
    /// `events` is sorted ascending by `frame_offset` and each offset is
    /// `< frames_in_buffer`. Implementations should:
    ///   - zero or mix into `out` (callers do not pre-zero)
    ///   - apply each event at its exact frame boundary
    ///   - never allocate or block
    fn render_with_events(
        &mut self,
        out:      &mut [f32],
        channels: usize,
        events:   &[(u32, AudioCmd)],
    );
}
