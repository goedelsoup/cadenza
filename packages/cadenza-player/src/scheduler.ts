import type { AiNoteEvent } from '@cadenza/types'

// A timed event ready to feed into Tone.Part. Times and durations are in
// quarter-note beats; Tone.js accepts that natively via the "Xn" / number
// notation when its Transport bpm is set. We keep this layer pure (no Tone
// imports) so it can be unit-tested without an AudioContext.
export interface ScheduledNote {
  time:     number  // beats from phrase start
  pitch:    number  // MIDI 0-127
  duration: number  // beats
  velocity: number  // 0..1 (normalised from MIDI 1-127)
}

const DEFAULT_VELOCITY_MIDI = 80

/**
 * Convert raw AiNoteEvents into normalised ScheduledNotes, sorted by start time.
 * Notes with non-positive duration or out-of-range pitch are dropped.
 */
export function scheduleNotes(notes: readonly AiNoteEvent[]): ScheduledNote[] {
  const out: ScheduledNote[] = []
  for (const n of notes) {
    if (n.dur <= 0) continue
    if (n.pitch < 0 || n.pitch > 127) continue
    if (n.start < 0) continue
    const velMidi = clampMidiVel(n.vel ?? DEFAULT_VELOCITY_MIDI)
    out.push({
      time:     n.start,
      pitch:    n.pitch,
      duration: n.dur,
      velocity: velMidi / 127,
    })
  }
  out.sort((a, b) => a.time - b.time)
  return out
}

/** Total length of a scheduled phrase in beats (max end time). */
export function phraseLengthBeats(notes: readonly ScheduledNote[]): number {
  let max = 0
  for (const n of notes) {
    const end = n.time + n.duration
    if (end > max) max = end
  }
  return max
}

/** Convert a MIDI pitch number to a Tone.js note name (e.g. 60 -> "C4"). */
export function midiToNoteName(pitch: number): string {
  const NAMES = ['C','C#','D','D#','E','F','F#','G','G#','A','A#','B']
  const octave = Math.floor(pitch / 12) - 1
  return `${NAMES[pitch % 12]}${octave}`
}

function clampMidiVel(v: number): number {
  if (v < 1)   return 1
  if (v > 127) return 127
  return v
}
