//! Built-in polyphonic synth used before VST3 hosting is wired up.
//!
//! - 16 voices, all pre-allocated
//! - Sine oscillator with linear AR envelope
//! - No allocation in `render_interleaved*` or any `note_*` method
//!
//! `render_interleaved_with_events` accepts a slice of
//! `(frame_offset, AudioCmd)` events sorted by offset and dispatches them at
//! sample-exact positions inside the buffer by rendering in chunks between
//! events. The convenience `render_interleaved` is a thin wrapper that
//! passes an empty event slice for callers that just want to render audio.

use crate::audio::AudioCmd;
use crate::instrument::Instrument;

pub const MAX_VOICES: usize = 16;

#[derive(Clone, Copy)]
struct Voice {
    pitch:    u8,
    active:   bool,
    gate:     bool,
    phase:    f32,
    inc:      f32,   // phase increment per sample
    env:      f32,   // current envelope level [0, 1]
    velocity: f32,   // [0, 1]
}

impl Voice {
    fn silent() -> Self {
        Self { pitch: 0, active: false, gate: false, phase: 0.0, inc: 0.0, env: 0.0, velocity: 0.0 }
    }
}

pub struct PolySynth {
    voices:      [Voice; MAX_VOICES],
    sample_rate: f32,
    attack_rate: f32,
    release_rate: f32,
}

impl PolySynth {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            voices: [Voice::silent(); MAX_VOICES],
            sample_rate,
            attack_rate:  1.0 / (0.005 * sample_rate),
            release_rate: 1.0 / (0.150 * sample_rate),
        }
    }

    #[allow(dead_code)] // referenced from tests and external diagnostics
    pub fn voice_count(&self) -> usize { MAX_VOICES }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        // Voice steal: prefer an inactive slot, otherwise the quietest one.
        let idx = self
            .voices
            .iter()
            .position(|v| !v.active)
            .unwrap_or_else(|| {
                let mut min_i = 0;
                let mut min_e = f32::INFINITY;
                for (i, v) in self.voices.iter().enumerate() {
                    if v.env < min_e { min_e = v.env; min_i = i; }
                }
                min_i
            });
        let v = &mut self.voices[idx];
        v.pitch    = pitch;
        v.active   = true;
        v.gate     = true;
        v.phase    = 0.0;
        v.inc      = midi_to_freq(pitch) / self.sample_rate;
        v.velocity = velocity as f32 / 127.0;
        // env continues from where it was — gives a soft retrigger
    }

    pub fn note_off(&mut self, pitch: u8) {
        for v in self.voices.iter_mut() {
            if v.active && v.pitch == pitch {
                v.gate = false;
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut() { v.gate = false; }
    }

    /// Render `samples` of interleaved audio into `out`. No allocation.
    /// Convenience wrapper for callers (and tests) that have no timed events.
    #[allow(dead_code)] // exercised by unit tests; production path uses the
                       // event-aware variant via the audio Renderer.
    pub fn render_interleaved(&mut self, out: &mut [f32], channels: usize) {
        self.render_interleaved_with_events(out, channels, &[]);
    }

    /// Render interleaved audio into `out` with sample-exact event dispatch.
    ///
    /// `events` is a slice of `(frame_offset, AudioCmd)` sorted ascending by
    /// offset. Offsets must be `< frames_in_buffer`. The synth renders the
    /// buffer in chunks between events, applying each command at the exact
    /// frame boundary so a NoteOn at offset 100 begins on frame 100.
    ///
    /// No allocation.
    pub fn render_interleaved_with_events(
        &mut self,
        out:      &mut [f32],
        channels: usize,
        events:   &[(u32, AudioCmd)],
    ) {
        // Zero the output first; voices add into it per chunk.
        for s in out.iter_mut() { *s = 0.0; }

        let attack_rate  = self.attack_rate;
        let release_rate = self.release_rate;
        let frames = out.len() / channels.max(1);

        let mut cursor = 0usize;
        let mut ev_idx = 0usize;

        while cursor < frames {
            // Apply any events whose offset has been reached.
            while ev_idx < events.len() && (events[ev_idx].0 as usize) <= cursor {
                Self::apply_event(self, events[ev_idx].1);
                ev_idx += 1;
            }

            // Render up to the next event or end of buffer.
            let next_boundary = if ev_idx < events.len() {
                (events[ev_idx].0 as usize).min(frames)
            } else {
                frames
            };
            let chunk_end = next_boundary.max(cursor);
            if chunk_end > cursor {
                render_chunk(
                    &mut self.voices,
                    out,
                    channels,
                    cursor,
                    chunk_end,
                    attack_rate,
                    release_rate,
                );
            }
            cursor = chunk_end;
            if cursor >= frames && ev_idx >= events.len() { break; }
        }
    }

    fn apply_event(&mut self, cmd: AudioCmd) {
        match cmd {
            AudioCmd::NoteOn  { pitch, velocity } => self.note_on(pitch, velocity),
            AudioCmd::NoteOff { pitch }           => self.note_off(pitch),
            AudioCmd::AllNotesOff                  => self.all_notes_off(),
        }
    }
}

impl Instrument for PolySynth {
    fn note_on(&mut self, pitch: u8, velocity: u8) {
        PolySynth::note_on(self, pitch, velocity);
    }
    fn note_off(&mut self, pitch: u8) {
        PolySynth::note_off(self, pitch);
    }
    fn all_notes_off(&mut self) {
        PolySynth::all_notes_off(self);
    }
    fn render_with_events(
        &mut self,
        out:      &mut [f32],
        channels: usize,
        events:   &[(u32, AudioCmd)],
    ) {
        self.render_interleaved_with_events(out, channels, events);
    }
}

/// Free function over the voice array to avoid borrowing `self` while
/// also borrowing `self.voices` mutably.
fn render_chunk(
    voices:       &mut [Voice; MAX_VOICES],
    out:          &mut [f32],
    channels:     usize,
    start:        usize,
    end:          usize,
    attack_rate:  f32,
    release_rate: f32,
) {
    for v in voices.iter_mut() {
        if !v.active { continue; }
        for f in start..end {
            if v.gate {
                v.env = (v.env + attack_rate).min(1.0);
            } else {
                v.env -= release_rate;
                if v.env <= 0.0 { v.env = 0.0; v.active = false; break; }
            }
            let sample = (v.phase * std::f32::consts::TAU).sin() * v.env * v.velocity * 0.15;
            v.phase += v.inc;
            if v.phase >= 1.0 { v.phase -= 1.0; }

            let base = f * channels;
            for c in 0..channels {
                out[base + c] += sample;
            }
        }
    }
}

fn midi_to_freq(pitch: u8) -> f32 {
    440.0 * 2f32.powf((pitch as f32 - 69.0) / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_count(s: &PolySynth) -> usize {
        s.voices.iter().filter(|v| v.active).count()
    }

    #[test]
    fn note_on_allocates_a_voice() {
        let mut s = PolySynth::new(48_000.0);
        assert_eq!(active_count(&s), 0);
        s.note_on(60, 100);
        assert_eq!(active_count(&s), 1);
    }

    #[test]
    fn voice_steal_when_exceeding_max_voices() {
        let mut s = PolySynth::new(48_000.0);
        // Fill every voice slot.
        for i in 0..MAX_VOICES as u8 { s.note_on(60 + i, 100); }
        assert_eq!(active_count(&s), MAX_VOICES);
        // One more note must steal — total active count stays at MAX_VOICES.
        s.note_on(90, 100);
        assert_eq!(active_count(&s), MAX_VOICES);
        // The new pitch is now playing.
        assert!(s.voices.iter().any(|v| v.active && v.pitch == 90));
    }

    #[test]
    fn note_off_drains_envelope_to_zero() {
        let mut s = PolySynth::new(48_000.0);
        s.note_on(69, 127);
        // Render enough samples for the AR envelope to attack.
        let mut buf = vec![0.0f32; 2 * 1024];
        s.render_interleaved(&mut buf, 2);
        s.note_off(69);
        // Render well past the 150ms release at 48kHz (~7200 samples).
        let mut buf = vec![0.0f32; 2 * 16_384];
        s.render_interleaved(&mut buf, 2);
        assert_eq!(active_count(&s), 0, "voice should have released and deactivated");
    }

    #[test]
    fn render_output_contains_no_nans() {
        let mut s = PolySynth::new(44_100.0);
        s.note_on(60, 100);
        s.note_on(64, 100);
        s.note_on(67, 100);
        let mut buf = vec![0.0f32; 2 * 2048];
        s.render_interleaved(&mut buf, 2);
        assert!(buf.iter().all(|x| x.is_finite()));
        // Synth should produce *some* signal after the attack ramp.
        assert!(buf.iter().any(|x| x.abs() > 1e-6));
    }

    #[test]
    fn render_into_silent_buffer_with_no_voices_is_silent() {
        let mut s = PolySynth::new(44_100.0);
        let mut buf = vec![1.0f32; 2 * 64];
        s.render_interleaved(&mut buf, 2);
        assert!(buf.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn note_on_event_at_offset_keeps_pre_offset_silent() {
        let mut s = PolySynth::new(48_000.0);
        let channels = 2;
        let frames = 2048;
        let mut buf = vec![0.0f32; frames * channels];
        let events = [(500u32, AudioCmd::NoteOn { pitch: 69, velocity: 127 })];
        s.render_interleaved_with_events(&mut buf, channels, &events);

        // Frames before the event are pristine zeros.
        for f in 0..500 {
            assert_eq!(buf[f * channels], 0.0, "frame {f} should be silent");
        }
        // The synth's sine starts at phase=0, so the first audible frame
        // is exactly one frame after the event offset.
        let first_nonzero = (0..frames).find(|&f| buf[f * channels].abs() > 1e-6);
        assert_eq!(first_nonzero, Some(501));
    }

    #[test]
    fn multiple_events_dispatched_in_order() {
        let mut s = PolySynth::new(48_000.0);
        let channels = 1;
        let frames = 2048;
        let mut buf = vec![0.0f32; frames];
        let events = [
            (100u32, AudioCmd::NoteOn { pitch: 60, velocity: 100 }),
            (800u32, AudioCmd::NoteOn { pitch: 64, velocity: 100 }),
        ];
        s.render_interleaved_with_events(&mut buf, channels, &events);
        let first = (0..frames).find(|&f| buf[f].abs() > 1e-6);
        assert_eq!(first, Some(101));
        // Two voices active after both NoteOns.
        assert_eq!(s.voices.iter().filter(|v| v.active).count(), 2);
    }
}
