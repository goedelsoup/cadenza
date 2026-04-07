import * as Tone from 'tone'

export type InstrumentPreset = 'piano' | 'bass' | 'pad'

export interface Instrument {
  triggerAttackRelease(
    note: string,
    duration: string | number,
    time?: number,
    velocity?: number,
  ): void
  dispose(): void
  toDestination(): Instrument
}

/**
 * Build a polyphonic instrument for a preset. We default to PolySynth voices
 * (no network sample fetch) so playback works offline and in tests/dev with
 * zero asset wiring. A real Tone.Sampler with proper samples can replace these
 * later without changing the engine surface.
 */
export function createInstrument(preset: InstrumentPreset): Tone.PolySynth {
  switch (preset) {
    case 'piano':
      return new Tone.PolySynth(Tone.Synth, {
        oscillator: { type: 'triangle' },
        envelope:   { attack: 0.005, decay: 0.15, sustain: 0.3, release: 1.2 },
      }).toDestination()

    case 'bass':
      return new Tone.PolySynth(Tone.Synth, {
        oscillator: { type: 'sawtooth' },
        envelope:   { attack: 0.01, decay: 0.2, sustain: 0.6, release: 0.4 },
      }).toDestination()

    case 'pad':
      return new Tone.PolySynth(Tone.Synth, {
        oscillator: { type: 'sine' },
        envelope:   { attack: 0.6, decay: 0.4, sustain: 0.7, release: 2.0 },
      }).toDestination()
  }
}
