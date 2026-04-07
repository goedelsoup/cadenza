//! Phrase → TimedCmd scheduler.
//!
//! Computes the absolute frame for every NoteOn/NoteOff in the phrase
//! using the audio engine's shared frame counter and sample rate, then
//! pushes the entire timeline into the SPSC ringbuf in one batch. The
//! audio thread holds them in its `pending` queue and dispatches them
//! at sample-exact frame offsets.
//!
//! The future returned by `play_phrase` resolves when the predicted
//! end of the phrase has elapsed in wall-clock time. Aborting the
//! task does not stop the audio thread; the server task should also
//! `engine.send(AudioCmd::AllNotesOff)` after cancelling.

use crate::audio::{AudioCmd, AudioEngine, TimedCmd};
use cadenza_theory::phrase::Phrase;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const TICKS_PER_QUARTER: f64 = 480.0;

/// Convert a tick offset (480 PPQ) at a given tempo to wall-clock time.
/// Retained as a building block / debugging aid even though the new
/// frame-based scheduler uses `ticks_to_frames` directly.
#[allow(dead_code)]
pub(crate) fn ticks_to_duration(ticks: u32, tempo_bpm: u16) -> Duration {
    let secs_per_tick = 60.0 / (tempo_bpm.max(1) as f64) / TICKS_PER_QUARTER;
    Duration::from_secs_f64(ticks as f64 * secs_per_tick)
}

/// Convert a tick offset to an absolute audio frame given tempo and sample rate.
pub(crate) fn ticks_to_frames(ticks: u32, tempo_bpm: u16, sample_rate: u32) -> u64 {
    let secs_per_tick = 60.0 / (tempo_bpm.max(1) as f64) / TICKS_PER_QUARTER;
    (ticks as f64 * secs_per_tick * sample_rate as f64).round() as u64
}

/// Play a phrase to completion. The future resolves when the predicted
/// last note-off frame has elapsed in wall-clock time.
pub async fn play_phrase(engine: Arc<Mutex<AudioEngine>>, phrase: Phrase) {
    // Snapshot sample_rate and the current playback frame in one lock.
    let (sample_rate, start_frame) = {
        let e = engine.lock().await;
        (e.sample_rate, e.now_frame())
    };

    // Build a sorted timeline of frame-tagged commands.
    let mut timeline: Vec<TimedCmd> = Vec::with_capacity(phrase.events.len() * 2);
    let mut last_off_frame = start_frame;
    let tempo = phrase.tempo;

    for ev in &phrase.events {
        let on_ticks  = ev.start;
        let off_ticks = ev.start.saturating_add(ev.duration);
        let on_frame  = start_frame + ticks_to_frames(on_ticks,  tempo, sample_rate);
        let off_frame = start_frame + ticks_to_frames(off_ticks, tempo, sample_rate);

        timeline.push(TimedCmd {
            frame: on_frame,
            cmd:   AudioCmd::NoteOn { pitch: ev.pitch, velocity: ev.velocity },
        });
        timeline.push(TimedCmd {
            frame: off_frame,
            cmd:   AudioCmd::NoteOff { pitch: ev.pitch },
        });

        if off_frame > last_off_frame { last_off_frame = off_frame; }
    }
    timeline.sort_by_key(|tc| tc.frame);

    // Push the entire timeline in one locked section so the audio thread
    // sees a consistent view. Bigger phrases (>~8000 events) may overflow
    // the ringbuf and tail events will be dropped with a warning — that's
    // a known limitation; bump RINGBUF_CAPACITY in audio.rs if it bites.
    {
        let mut e = engine.lock().await;
        for tc in timeline {
            e.send_timed(tc);
        }
    }

    // Sleep until predicted end. The audio thread keeps playing regardless
    // of whether this task is alive; the sleep just gives callers something
    // meaningful to await.
    let total_frames = last_off_frame.saturating_sub(start_frame);
    let total_secs   = total_frames as f64 / sample_rate.max(1) as f64;
    if total_secs > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(total_secs)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_to_duration_120bpm_quarter_note_is_500ms() {
        let d = ticks_to_duration(480, 120);
        assert!((d.as_secs_f64() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn ticks_to_duration_60bpm_quarter_note_is_1s() {
        let d = ticks_to_duration(480, 60);
        assert!((d.as_secs_f64() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ticks_to_duration_200bpm_one_bar_4_4() {
        let d = ticks_to_duration(1920, 200);
        assert!((d.as_secs_f64() - 1.2).abs() < 1e-9);
    }

    #[test]
    fn ticks_to_duration_zero_is_zero() {
        assert_eq!(ticks_to_duration(0, 120), Duration::ZERO);
    }

    #[test]
    fn ticks_to_duration_clamps_zero_tempo() {
        let d = ticks_to_duration(480, 0);
        assert!(d.as_secs_f64().is_finite());
    }

    #[test]
    fn ticks_to_frames_120bpm_quarter_note_at_48khz() {
        // 480 ticks @ 120bpm = 0.5s = 24_000 frames at 48kHz.
        assert_eq!(ticks_to_frames(480, 120, 48_000), 24_000);
    }

    #[test]
    fn ticks_to_frames_zero_is_zero() {
        assert_eq!(ticks_to_frames(0, 120, 48_000), 0);
    }
}
