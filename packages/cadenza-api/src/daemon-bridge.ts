/**
 * Bridge to the optional native cadenza-daemon (Phase 5).
 *
 * The daemon is a separate Rust process that hosts an audio thread and
 * (eventually) VST3 plugins. It speaks JSON over WebSocket on
 * ws://127.0.0.1:7878 by default.
 *
 * Like the WASM bridge, this layer degrades gracefully: if the daemon is
 * unreachable, every send() resolves to `false` and the bridge transparently
 * keeps trying to reconnect in the background. Callers should fall back to
 * Tone.js playback.
 *
 * Lifecycle:
 *   - `connect()` — initial open. Best-effort; resolves true on success.
 *   - On any unexpected `onclose`, schedules a reconnect with exponential
 *     backoff (500ms → 1s → 2s → 4s → 8s → 16s → 30s, capped).
 *   - While connected, sends a `Ping` every 5s. If no `Pong` arrives within
 *     2s, the bridge force-closes the socket (which triggers reconnect).
 *   - `dispose()` cancels all timers and prevents further reconnects. Wired
 *     to `beforeunload` from the singleton accessor.
 *
 * The wire format mirrors `cadenza-ipc::DaemonMessage` (serde
 * externally-tagged enum). Keep these types in sync with
 * `packages/cadenza-ipc/src/lib.rs`.
 */

// Mirrors `cadenza_theory::phrase::Phrase` — only the fields the daemon
// actually reads. Keep narrow on purpose; the daemon owns the canonical
// schema.
export type DaemonNoteEvent = {
  pitch: number
  start: number       // ticks (480 PPQ)
  duration: number    // ticks
  velocity: number
  channel: number
  voice?: number
  slur_group?: number | null
}

export type DaemonPhrase = {
  id: number
  label: string
  events: DaemonNoteEvent[]
  time_sig: { numerator: number; denominator: number }
  tempo: number
  key?: unknown | null
  bars: number
}

export type PluginParam = {
  id: number
  name: string
  min: number
  max: number
  default: number
}

// Discriminated union mirroring serde's externally-tagged enum encoding.
// Each variant is `{ "VariantName": payload }` or the bare string for
// unit variants like `"Stop"`.
export type DaemonMessage =
  | { ScanPlugins: { dir: string } }
  | { LoadPlugin: { path: string } }
  | { UnloadPlugin: { id: number } }
  | { SetInstrument: { plugin_id: number } }
  | 'UseBuiltinSynth'
  | { PlayPhrase: { phrase: DaemonPhrase; plugin_id: number | null } }
  | 'Stop'
  | { SetTempo: number }
  | { SetParam: { plugin_id: number; param_id: number; value: number } }
  | 'Ping'
  | { ScannedPlugins: { paths: string[] } }
  | { PluginLoaded: { id: number; name: string; params: PluginParam[] } }
  | { PluginUnloaded: { id: number } }
  | { PluginActivated: { id: number } }
  | 'BuiltinSynthActivated'
  | 'PlaybackStarted'
  | 'PlaybackStopped'
  | 'Pong'
  | { Error: string }

export type DaemonEventListener = (msg: DaemonMessage) => void
export type DaemonStatus = 'disconnected' | 'connecting' | 'connected'
export type DaemonStatusListener = (status: DaemonStatus) => void

const DEFAULT_URL          = 'ws://127.0.0.1:7878/'
const HEARTBEAT_INTERVAL_MS = 5000
const HEARTBEAT_TIMEOUT_MS  = 2000
const BACKOFF_INITIAL_MS    = 500
const BACKOFF_MAX_MS        = 30_000

type WebSocketCtor = typeof WebSocket

export interface DaemonBridgeOptions {
  url?:           string
  webSocketCtor?: WebSocketCtor
}

export class DaemonBridge {
  private ws: WebSocket | null = null
  private url:    string
  private wsCtor: WebSocketCtor | undefined
  private listeners       = new Set<DaemonEventListener>()
  private statusListeners = new Set<DaemonStatusListener>()
  private status:    DaemonStatus = 'disconnected'
  private backoffMs: number       = BACKOFF_INITIAL_MS
  private reconnectTimer: ReturnType<typeof setTimeout>  | null = null
  private heartbeatTimer: ReturnType<typeof setInterval> | null = null
  private pongDeadline:   ReturnType<typeof setTimeout>  | null = null
  private disposed         = false
  private reconnectEnabled = false
  private pendingOpen: Promise<boolean> | null = null

  constructor(opts: DaemonBridgeOptions = {}) {
    this.url    = opts.url ?? DEFAULT_URL
    this.wsCtor = opts.webSocketCtor
      ?? (typeof globalThis !== 'undefined'
            ? (globalThis as { WebSocket?: WebSocketCtor }).WebSocket
            : undefined)
  }

  /**
   * Best-effort initial open. Subsequent reconnects happen automatically.
   * Resolves true if the socket is open by the end of this attempt.
   */
  async connect(): Promise<boolean> {
    if (this.disposed) return false
    this.reconnectEnabled = true
    if (this.status === 'connected') return true
    return this.openOnce()
  }

  isConnected(): boolean {
    return this.status === 'connected'
  }

  getStatus(): DaemonStatus {
    return this.status
  }

  /** Subscribe to status changes. Returns an unsubscribe function. */
  onStatusChange(cb: DaemonStatusListener): () => void {
    this.statusListeners.add(cb)
    return () => { this.statusListeners.delete(cb) }
  }

  on(listener: DaemonEventListener): () => void {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  /** Send a message. Returns false if the daemon is unreachable. */
  async send(msg: DaemonMessage): Promise<boolean> {
    if (!this.isConnected()) {
      if (!(await this.connect())) return false
    }
    if (!this.ws) return false
    try {
      this.ws.send(JSON.stringify(msg))
      return true
    } catch (e) {
      console.warn('[cadenza-daemon] send failed', e)
      return false
    }
  }

  /** Cancel timers, close the socket, prevent further reconnects. */
  dispose(): void {
    this.disposed         = true
    this.reconnectEnabled = false
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    this.stopHeartbeat()
    if (this.ws) {
      try { this.ws.close() } catch { /* ignore */ }
      this.ws = null
    }
    this.setStatus('disconnected')
  }

  // ── internals ────────────────────────────────────────────────────────────

  private openOnce(): Promise<boolean> {
    if (this.pendingOpen) return this.pendingOpen
    if (!this.wsCtor) {
      this.setStatus('disconnected')
      return Promise.resolve(false)
    }
    // Coalesce: if a reconnect was queued, fire it now instead.
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }

    this.setStatus('connecting')

    // Synchronous bookkeeping: we clear pendingOpen at resolve sites instead
    // of via `.finally`. With vitest fake timers a `.finally` callback runs
    // as a microtask and would not have fired yet when a reconnect setTimeout
    // callback runs, leaving pendingOpen pointing at an already-settled
    // promise and short-circuiting the next openOnce().
    const p = new Promise<boolean>((resolve) => {
      let ws: WebSocket
      try {
        ws = new this.wsCtor!(this.url)
      } catch {
        this.pendingOpen = null
        this.setStatus('disconnected')
        this.scheduleReconnect()
        resolve(false)
        return
      }

      let resolved = false
      const settle = (ok: boolean) => {
        if (resolved) return
        resolved = true
        this.pendingOpen = null
        resolve(ok)
      }

      ws.onopen = () => {
        this.ws        = ws
        this.backoffMs = BACKOFF_INITIAL_MS
        this.setStatus('connected')
        this.startHeartbeat()
        settle(true)
      }
      ws.onerror = () => {
        // onclose will follow; let it handle cleanup + reconnect.
      }
      ws.onclose = () => {
        if (this.ws === ws) this.ws = null
        this.stopHeartbeat()
        this.setStatus('disconnected')
        settle(false)
        if (this.reconnectEnabled && !this.disposed) {
          this.scheduleReconnect()
        }
      }
      ws.onmessage = (ev) => {
        let parsed: DaemonMessage
        try {
          parsed = JSON.parse(ev.data as string) as DaemonMessage
        } catch (e) {
          console.warn('[cadenza-daemon] bad message', e)
          return
        }
        // Heartbeat: clear the pong deadline regardless of who sent it.
        if (parsed === 'Pong' && this.pongDeadline) {
          clearTimeout(this.pongDeadline)
          this.pongDeadline = null
        }
        for (const fn of this.listeners) fn(parsed)
      }
    })
    this.pendingOpen = p
    return p
  }

  private scheduleReconnect(): void {
    if (this.reconnectTimer || this.disposed || !this.reconnectEnabled) return
    const delay = this.backoffMs
    this.backoffMs = Math.min(this.backoffMs * 2, BACKOFF_MAX_MS)
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null
      void this.openOnce()
    }, delay)
  }

  private startHeartbeat(): void {
    this.stopHeartbeat()
    this.heartbeatTimer = setInterval(() => this.sendHeartbeat(), HEARTBEAT_INTERVAL_MS)
  }

  private stopHeartbeat(): void {
    if (this.heartbeatTimer) { clearInterval(this.heartbeatTimer); this.heartbeatTimer = null }
    if (this.pongDeadline)   { clearTimeout(this.pongDeadline);     this.pongDeadline   = null }
  }

  private sendHeartbeat(): void {
    if (!this.ws || !this.wsCtor) return
    if (this.ws.readyState !== this.wsCtor.OPEN) return
    try {
      this.ws.send(JSON.stringify('Ping'))
    } catch {
      return
    }
    if (this.pongDeadline) clearTimeout(this.pongDeadline)
    this.pongDeadline = setTimeout(() => {
      this.pongDeadline = null
      // Force-close — onclose triggers reconnect.
      try { this.ws?.close() } catch { /* ignore */ }
    }, HEARTBEAT_TIMEOUT_MS)
  }

  private setStatus(s: DaemonStatus): void {
    if (this.status === s) return
    this.status = s
    for (const l of this.statusListeners) l(s)
  }

  // Convenience wrappers — the canonical surface mirrors wasm-bridge.ts.

  playPhrase(phrase: DaemonPhrase, pluginId: number | null = null) {
    return this.send({ PlayPhrase: { phrase, plugin_id: pluginId } })
  }
  stop()                          { return this.send('Stop') }
  setTempo(bpm: number)           { return this.send({ SetTempo: bpm }) }
  scanPlugins(dir: string)        { return this.send({ ScanPlugins: { dir } }) }
  loadPlugin(path: string)        { return this.send({ LoadPlugin: { path } }) }
  unloadPlugin(id: number)        { return this.send({ UnloadPlugin: { id } }) }
  setInstrument(pluginId: number) { return this.send({ SetInstrument: { plugin_id: pluginId } }) }
  useBuiltinSynth()               { return this.send('UseBuiltinSynth') }
  ping()                          { return this.send('Ping') }
}

let singleton: DaemonBridge | null = null

/** Lazy singleton so the web app can `import { daemon } from '@cadenza/api'`. */
export function daemon(): DaemonBridge {
  if (!singleton) {
    singleton = new DaemonBridge()
    // Browser only: tear down on page unload so we don't leak timers.
    if (typeof globalThis !== 'undefined') {
      const g = globalThis as { addEventListener?: (ev: string, cb: () => void) => void }
      if (typeof g.addEventListener === 'function') {
        g.addEventListener('beforeunload', () => singleton?.dispose())
      }
    }
  }
  return singleton
}
