import { describe, it, expect } from 'vitest'
import type { AiNoteEvent } from '@cadenza/types'
import { scheduleNotes, phraseLengthBeats, midiToNoteName } from './scheduler'

describe('scheduleNotes', () => {
  it('normalises velocity from MIDI to 0..1 with default of 80/127', () => {
    const notes: AiNoteEvent[] = [
      { pitch: 60, start: 0, dur: 1 },
      { pitch: 62, start: 1, dur: 1, vel: 127 },
    ]
    const out = scheduleNotes(notes)
    expect(out).toHaveLength(2)
    expect(out[0].velocity).toBeCloseTo(80 / 127)
    expect(out[1].velocity).toBe(1)
  })

  it('sorts events by start time', () => {
    const notes: AiNoteEvent[] = [
      { pitch: 60, start: 2, dur: 1 },
      { pitch: 62, start: 0, dur: 1 },
      { pitch: 64, start: 1, dur: 1 },
    ]
    const out = scheduleNotes(notes)
    expect(out.map(n => n.time)).toEqual([0, 1, 2])
  })

  it('drops invalid notes (non-positive duration, out-of-range pitch, negative start)', () => {
    const notes: AiNoteEvent[] = [
      { pitch: 60, start: 0, dur: 0 },
      { pitch: 60, start: 0, dur: -1 },
      { pitch: -5, start: 0, dur: 1 },
      { pitch: 200, start: 0, dur: 1 },
      { pitch: 60, start: -1, dur: 1 },
      { pitch: 60, start: 0, dur: 1 },
    ]
    const out = scheduleNotes(notes)
    expect(out).toHaveLength(1)
    expect(out[0].pitch).toBe(60)
  })

  it('clamps velocity below 1 to 1 and above 127 to 127', () => {
    const out = scheduleNotes([
      { pitch: 60, start: 0, dur: 1, vel: 0 },
      { pitch: 60, start: 0, dur: 1, vel: 999 },
    ])
    expect(out[0].velocity).toBeCloseTo(1 / 127)
    expect(out[1].velocity).toBe(1)
  })
})

describe('phraseLengthBeats', () => {
  it('returns the maximum end time across notes', () => {
    const out = scheduleNotes([
      { pitch: 60, start: 0, dur: 2 },
      { pitch: 62, start: 1, dur: 1.5 },
      { pitch: 64, start: 3, dur: 0.5 },
    ])
    expect(phraseLengthBeats(out)).toBe(3.5)
  })

  it('returns 0 for an empty list', () => {
    expect(phraseLengthBeats([])).toBe(0)
  })
})

describe('midiToNoteName', () => {
  it('maps middle C (60) to C4', () => {
    expect(midiToNoteName(60)).toBe('C4')
  })

  it('maps A4 (69)', () => {
    expect(midiToNoteName(69)).toBe('A4')
  })

  it('maps low C (24) to C1', () => {
    expect(midiToNoteName(24)).toBe('C1')
  })

  it('handles sharps', () => {
    expect(midiToNoteName(61)).toBe('C#4')
  })
})
