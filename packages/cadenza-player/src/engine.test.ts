import { describe, it, expect, vi, beforeEach } from 'vitest'
import type { SessionPhrase } from '@cadenza/types'

// ── Mocks ───────────────────────────────────────────────────────────────────
//
// We mock both `@cadenza/api` and `tone` so the engine can be exercised in
// a Node test environment without an AudioContext or a live daemon.

const mockBridge = vi.hoisted(() => ({
  isConnected: vi.fn<[], boolean>().mockReturnValue(false),
  connect:     vi.fn().mockResolvedValue(true),
  on:          vi.fn().mockReturnValue(() => {}),
  playPhrase:  vi.fn().mockResolvedValue(true),
  stop:        vi.fn().mockResolvedValue(true),
}))

vi.mock('@cadenza/api', () => ({
  daemon: () => mockBridge,
}))

vi.mock('tone', () => {
  class FakePolySynth {
    triggerAttackRelease() {}
    dispose() {}
    toDestination() { return this }
  }
  class FakePart {
    constructor(_cb: unknown, _events: unknown) {}
    start() { return this }
    stop()  { return this }
    dispose() {}
  }
  class FakeLoop {
    constructor(_cb: unknown, _interval: unknown) {}
    start() { return this }
    stop()  { return this }
    dispose() {}
  }
  return {
    start:     vi.fn().mockResolvedValue(undefined),
    PolySynth: FakePolySynth,
    Synth:     class {},
    Part:      FakePart,
    Loop:      FakeLoop,
    Transport: {
      bpm:          { value: 120 },
      start:        vi.fn(),
      stop:         vi.fn(),
      cancel:       vi.fn(),
      scheduleOnce: vi.fn().mockReturnValue(0),
      clear:        vi.fn(),
    },
  }
})

import { CadenzaPlayer } from './engine'

const samplePhrase: SessionPhrase = {
  id: 1,
  label: 'test',
  bars: 1,
  tempo: 120,
  noteCount: 2,
  warnings: [],
  raw: {
    type: 'melody',
    summary: 'test',
    tempo: 120,
    time_signature: '4/4',
    bars: 1,
    notes: [
      { pitch: 60, start: 0, dur: 1, vel: 100 },
      { pitch: 64, start: 1, dur: 1, vel: 100 },
    ],
  },
}

describe('CadenzaPlayer backend selection', () => {
  beforeEach(() => {
    mockBridge.isConnected.mockReset().mockReturnValue(false)
    mockBridge.connect.mockClear().mockResolvedValue(true)
    mockBridge.playPhrase.mockClear().mockResolvedValue(true)
    mockBridge.stop.mockClear().mockResolvedValue(true)
    mockBridge.on.mockReset().mockReturnValue(() => {})
  })

  it('routes playback through the daemon when connected', async () => {
    mockBridge.isConnected.mockReturnValue(true)
    const player = new CadenzaPlayer()
    await player.load()
    expect(player.usingDaemon()).toBe(true)

    player.play(samplePhrase)

    expect(mockBridge.playPhrase).toHaveBeenCalledTimes(1)
    const sent = mockBridge.playPhrase.mock.calls[0][0]
    expect(sent.events).toHaveLength(2)
    // 1 quarter-note beat = 480 ticks
    expect(sent.events[0].start).toBe(0)
    expect(sent.events[0].duration).toBe(480)
    expect(sent.events[1].start).toBe(480)
    expect(sent.events[1].duration).toBe(480)
    expect(sent.events[0].velocity).toBe(100)
    expect(sent.tempo).toBe(120)
    expect(sent.time_sig).toEqual({ numerator: 4, denominator: 4 })
    expect(sent.id).toBe(1)
  })

  it('falls back to Tone.js when the daemon is not connected', async () => {
    mockBridge.isConnected.mockReturnValue(false)
    const player = new CadenzaPlayer()
    await player.load()
    expect(player.usingDaemon()).toBe(false)

    player.play(samplePhrase)
    expect(mockBridge.playPhrase).not.toHaveBeenCalled()
  })

  it('emits start when the daemon reports PlaybackStarted', async () => {
    mockBridge.isConnected.mockReturnValue(true)
    let captured: ((msg: unknown) => void) | null = null
    mockBridge.on.mockImplementation((cb: (msg: unknown) => void) => {
      captured = cb
      return () => {}
    })

    const player = new CadenzaPlayer()
    await player.load()
    let starts = 0
    let stops  = 0
    player.on('start', () => { starts++ })
    player.on('stop',  () => { stops++  })

    player.play(samplePhrase)
    expect(captured).not.toBeNull()
    captured!('PlaybackStarted')
    expect(starts).toBe(1)
    expect(player.isPlaying).toBe(true)

    captured!('PlaybackStopped')
    expect(stops).toBe(1)
    expect(player.isPlaying).toBe(false)
  })

  it('stop() while daemon-active sends Stop and clears playing state', async () => {
    mockBridge.isConnected.mockReturnValue(true)
    let captured: ((msg: unknown) => void) | null = null
    mockBridge.on.mockImplementation((cb: (msg: unknown) => void) => {
      captured = cb
      return () => {}
    })
    const player = new CadenzaPlayer()
    await player.load()
    player.play(samplePhrase)
    captured!('PlaybackStarted')
    expect(player.isPlaying).toBe(true)

    player.stop()
    expect(mockBridge.stop).toHaveBeenCalledTimes(1)
    expect(player.isPlaying).toBe(false)
  })

  it('converts time signature 6/8 correctly', async () => {
    mockBridge.isConnected.mockReturnValue(true)
    const player = new CadenzaPlayer()
    await player.load()
    player.play({
      ...samplePhrase,
      raw: { ...samplePhrase.raw, time_signature: '6/8' },
    })
    const sent = mockBridge.playPhrase.mock.calls[0][0]
    expect(sent.time_sig).toEqual({ numerator: 6, denominator: 8 })
  })
})
