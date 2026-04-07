import { describe, it, expect } from 'vitest'
import type { AiPhrase } from '@cadenza/types'
import { buildMidiBytes } from './export'

function findSubsequence(haystack: Uint8Array, needle: number[]): number {
  outer: for (let i = 0; i <= haystack.length - needle.length; i++) {
    for (let j = 0; j < needle.length; j++) {
      if (haystack[i + j] !== needle[j]) continue outer
    }
    return i
  }
  return -1
}

const MTHD = [0x4d, 0x54, 0x68, 0x64]
const MTRK = [0x4d, 0x54, 0x72, 0x6b]

const samplePhrase: AiPhrase = {
  type:    'melody phrase',
  summary: 'sample',
  notes: [
    { pitch: 60, start: 0,   dur: 1, vel: 80 },
    { pitch: 64, start: 1,   dur: 1, vel: 80 },
    { pitch: 67, start: 2,   dur: 2, vel: 80 },
  ],
}

describe('buildMidiBytes', () => {
  it('starts with the MThd header', () => {
    const bytes = buildMidiBytes(samplePhrase, 120)
    expect(Array.from(bytes.slice(0, 4))).toEqual(MTHD)
  })

  it('contains an MTrk chunk', () => {
    const bytes = buildMidiBytes(samplePhrase, 120)
    const idx = findSubsequence(bytes, MTRK)
    expect(idx).toBeGreaterThan(0)
  })

  it('encodes the tempo meta event correctly for 120 bpm', () => {
    const bytes = buildMidiBytes(samplePhrase, 120)
    // 60_000_000 / 120 = 500_000 = 0x07A120
    const tempoEvent = [0xff, 0x51, 0x03, 0x07, 0xa1, 0x20]
    expect(findSubsequence(bytes, tempoEvent)).toBeGreaterThan(0)
  })

  it('encodes the tempo meta event correctly for 90 bpm', () => {
    const bytes = buildMidiBytes(samplePhrase, 90)
    // 60_000_000 / 90 = 666_666.66… → round to 666_667 = 0x0A2C2B
    const tempoEvent = [0xff, 0x51, 0x03, 0x0a, 0x2c, 0x2b]
    expect(findSubsequence(bytes, tempoEvent)).toBeGreaterThan(0)
  })

  it('produces a valid minimal MIDI file when notes is empty', () => {
    const bytes = buildMidiBytes({ type: 'melody phrase', summary: 'empty', notes: [] }, 120)
    expect(Array.from(bytes.slice(0, 4))).toEqual(MTHD)
    expect(findSubsequence(bytes, MTRK)).toBeGreaterThan(0)
    // Final track byte sequence is the end-of-track meta event: 00 FF 2F 00
    const eot = [0x00, 0xff, 0x2f, 0x00]
    expect(findSubsequence(bytes, eot)).toBeGreaterThan(0)
  })

  it('handles a phrase with notes undefined', () => {
    const bytes = buildMidiBytes({ type: 'melody phrase', summary: 'no notes' }, 120)
    expect(Array.from(bytes.slice(0, 4))).toEqual(MTHD)
    expect(findSubsequence(bytes, MTRK)).toBeGreaterThan(0)
  })
})
