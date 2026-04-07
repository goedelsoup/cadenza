import * as Tone from 'tone'
import type { SessionPhrase } from '@cadenza/types'
import {
  daemon,
  type DaemonMessage,
  type DaemonNoteEvent,
  type DaemonPhrase,
} from '@cadenza/api'
import {
  scheduleNotes,
  phraseLengthBeats,
  midiToNoteName,
  type ScheduledNote,
} from './scheduler'
import { createInstrument, type InstrumentPreset } from './instruments'

export type PlayerEvent = 'start' | 'stop' | 'beat'
type Listener = () => void

interface PartEvent {
  time:     number
  pitch:    string
  duration: number
  velocity: number
}

// Daemon expects ticks at 480 PPQ. AiNoteEvent uses quarter-note beats.
const TICKS_PER_QUARTER = 480
const DEFAULT_VELOCITY_MIDI = 80

export class CadenzaPlayer {
  private instrument: Tone.PolySynth | null = null
  private part:       Tone.Part<PartEvent> | null = null
  private beatLoop:   Tone.Loop | null = null
  private endEventId: number | null = null
  private loaded   = false
  private playing  = false
  private daemonActive = false
  private daemonUnsub: (() => void) | null = null
  private listeners: Record<PlayerEvent, Listener[]> = { start: [], stop: [], beat: [] }

  constructor(private preset: InstrumentPreset = 'piano') {}

  /**
   * Start the AudioContext and build the instrument. MUST be called from a
   * user-gesture handler (button click) to satisfy browser autoplay policy.
   *
   * Also opportunistically connects to the native daemon. The bridge degrades
   * gracefully — if the daemon is not running, this resolves successfully and
   * playback transparently uses Tone.js instead.
   */
  async load(): Promise<void> {
    if (this.loaded) return
    // Best-effort daemon connect — never throws.
    await daemon().connect()
    await Tone.start()
    this.instrument = createInstrument(this.preset)
    this.loaded = true
  }

  get isPlaying(): boolean {
    return this.playing
  }

  /** True if the next play() will route through the native daemon. */
  usingDaemon(): boolean {
    return daemon().isConnected()
  }

  setTempo(bpm: number): void {
    Tone.Transport.bpm.value = bpm
  }

  play(phrase: SessionPhrase): void {
    if (!this.loaded || !this.instrument) {
      throw new Error('CadenzaPlayer.play() called before load()')
    }
    this.stop()

    const bridge = daemon()
    if (bridge.isConnected()) {
      this.playViaDaemon(phrase)
      return
    }

    this.playViaTone(phrase)
  }

  private playViaDaemon(phrase: SessionPhrase): void {
    const bridge = daemon()
    const dp = sessionPhraseToDaemonPhrase(phrase)
    if (dp.events.length === 0) return

    this.daemonActive = true
    this.daemonUnsub = bridge.on((msg: DaemonMessage) => {
      if (msg === 'PlaybackStarted') {
        if (!this.playing) {
          this.playing = true
          this.emit('start')
        }
      } else if (msg === 'PlaybackStopped') {
        this.handleDaemonStopped()
      }
    })

    void bridge.playPhrase(dp).then((ok) => {
      if (!ok) this.handleDaemonStopped()
    })
  }

  private playViaTone(phrase: SessionPhrase): void {
    const inst = this.instrument!
    const notes  = scheduleNotes(phrase.raw.notes ?? [])
    if (notes.length === 0) return

    const lenBts = phraseLengthBeats(notes)
    this.setTempo(phrase.tempo)

    const events: PartEvent[] = notes.map((n: ScheduledNote) => ({
      time:     n.time,
      pitch:    midiToNoteName(n.pitch),
      duration: n.duration,
      velocity: n.velocity,
    }))

    const beatToSec  = 60 / Tone.Transport.bpm.value
    this.part = new Tone.Part<PartEvent>((time, ev) => {
      inst.triggerAttackRelease(ev.pitch, ev.duration * beatToSec, time, ev.velocity)
    }, events as unknown as ConstructorParameters<typeof Tone.Part>[1])

    this.part.start(0)

    this.beatLoop = new Tone.Loop(() => this.emit('beat'), '4n').start(0)

    this.endEventId = Tone.Transport.scheduleOnce(() => {
      this.stop()
    }, `+${lenBts * (60 / Tone.Transport.bpm.value)}`)

    Tone.Transport.start()
    this.playing = true
    this.emit('start')
  }

  private handleDaemonStopped(): void {
    if (this.daemonUnsub) {
      this.daemonUnsub()
      this.daemonUnsub = null
    }
    this.daemonActive = false
    if (this.playing) {
      this.playing = false
      this.emit('stop')
    }
  }

  stop(): void {
    if (this.daemonActive) {
      void daemon().stop()
      this.handleDaemonStopped()
      return
    }
    if (this.endEventId !== null) {
      Tone.Transport.clear(this.endEventId)
      this.endEventId = null
    }
    if (this.part) {
      this.part.stop(0)
      this.part.dispose()
      this.part = null
    }
    if (this.beatLoop) {
      this.beatLoop.stop(0)
      this.beatLoop.dispose()
      this.beatLoop = null
    }
    Tone.Transport.stop()
    Tone.Transport.cancel(0)
    if (this.playing) {
      this.playing = false
      this.emit('stop')
    }
  }

  on(event: PlayerEvent, cb: Listener): void {
    this.listeners[event].push(cb)
  }

  private emit(event: PlayerEvent): void {
    for (const cb of this.listeners[event]) cb()
  }
}

/**
 * Convert a hydrated SessionPhrase into the wire shape the native daemon
 * expects. Times go from quarter-note beats to 480 PPQ ticks.
 */
function sessionPhraseToDaemonPhrase(p: SessionPhrase): DaemonPhrase {
  const rawNotes = p.raw.notes ?? []
  const events: DaemonNoteEvent[] = []
  for (const n of rawNotes) {
    if (n.dur <= 0) continue
    if (n.pitch < 0 || n.pitch > 127) continue
    if (n.start < 0) continue
    events.push({
      pitch:    n.pitch,
      start:    Math.round(n.start * TICKS_PER_QUARTER),
      duration: Math.round(n.dur * TICKS_PER_QUARTER),
      velocity: clampVel(n.vel ?? DEFAULT_VELOCITY_MIDI),
      channel:  0,
    })
  }
  events.sort((a, b) => a.start - b.start)

  return {
    id:       p.id,
    label:    p.label,
    events,
    time_sig: parseTimeSig(p.raw.time_signature ?? '4/4'),
    tempo:    p.tempo,
    key:      null,
    bars:     p.bars,
  }
}

function parseTimeSig(s: string): { numerator: number; denominator: number } {
  const parts = s.split('/')
  const n = Number(parts[0])
  const d = Number(parts[1])
  return {
    numerator:   Number.isFinite(n) && n > 0 ? n : 4,
    denominator: Number.isFinite(d) && d > 0 ? d : 4,
  }
}

function clampVel(v: number): number {
  if (v < 1)   return 1
  if (v > 127) return 127
  return Math.round(v)
}
