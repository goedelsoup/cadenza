import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { DaemonBridge, type DaemonStatus } from './daemon-bridge'

// Hand-rolled mock WebSocket — avoids pulling in mock-socket. Tracks every
// instance so the tests can drive open/close/message events deterministically.
class MockWebSocket {
  static OPEN       = 1
  static CLOSED     = 3
  static CONNECTING = 0
  static instances: MockWebSocket[] = []
  static reset(): void { MockWebSocket.instances = [] }
  static last(): MockWebSocket {
    const i = MockWebSocket.instances[MockWebSocket.instances.length - 1]
    if (!i) throw new Error('no MockWebSocket instances yet')
    return i
  }

  readyState = MockWebSocket.CONNECTING
  url:      string
  sent:     string[] = []
  closed    = false
  onopen:    (() => void) | null = null
  onclose:   (() => void) | null = null
  onerror:   (() => void) | null = null
  onmessage: ((ev: { data: string }) => void) | null = null

  constructor(url: string) {
    this.url = url
    MockWebSocket.instances.push(this)
  }

  send(data: string): void { this.sent.push(data) }

  close(): void {
    if (this.closed) return
    this.closed     = true
    this.readyState = MockWebSocket.CLOSED
    this.onclose?.()
  }

  // ── test-only helpers ────────────────────────────────────────────────────
  emitOpen(): void {
    this.readyState = MockWebSocket.OPEN
    this.onopen?.()
  }
  emitMessage(data: string): void {
    this.onmessage?.({ data })
  }
  emitClose(): void {
    if (this.closed) return
    this.closed     = true
    this.readyState = MockWebSocket.CLOSED
    this.onclose?.()
  }
}

function makeBridge(): DaemonBridge {
  return new DaemonBridge({
    url:           'ws://127.0.0.1:7878/',
    webSocketCtor: MockWebSocket as unknown as typeof WebSocket,
  })
}

describe('DaemonBridge', () => {
  beforeEach(() => {
    MockWebSocket.reset()
    vi.useFakeTimers()
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  it('reports connected after onopen', async () => {
    const b = makeBridge()
    const p = b.connect()
    MockWebSocket.last().emitOpen()
    await expect(p).resolves.toBe(true)
    expect(b.isConnected()).toBe(true)
    expect(b.getStatus()).toBe('connected')
    b.dispose()
  })

  it('emits status transitions: connecting → connected → disconnected', async () => {
    const b = makeBridge()
    const seen: DaemonStatus[] = []
    b.onStatusChange((s) => seen.push(s))
    const p = b.connect()
    MockWebSocket.last().emitOpen()
    await p
    MockWebSocket.last().emitClose()
    expect(seen).toEqual(['connecting', 'connected', 'disconnected'])
    b.dispose()
  })

  it('reconnects with exponential backoff (500ms → 1s → 2s) after close', async () => {
    const b = makeBridge()
    const p = b.connect()
    MockWebSocket.last().emitOpen()
    await p
    expect(MockWebSocket.instances.length).toBe(1)

    // First close → reconnect at 500ms
    MockWebSocket.last().emitClose()
    vi.advanceTimersByTime(499)
    expect(MockWebSocket.instances.length).toBe(1)
    vi.advanceTimersByTime(1)
    expect(MockWebSocket.instances.length).toBe(2)

    // Second close (without ever opening) → reconnect at 1000ms
    MockWebSocket.last().emitClose()
    vi.advanceTimersByTime(999)
    expect(MockWebSocket.instances.length).toBe(2)
    vi.advanceTimersByTime(1)
    expect(MockWebSocket.instances.length).toBe(3)

    // Third close → 2000ms
    MockWebSocket.last().emitClose()
    vi.advanceTimersByTime(2000)
    expect(MockWebSocket.instances.length).toBe(4)

    b.dispose()
  })

  it('resets backoff to 500ms after a successful open', async () => {
    const b = makeBridge()
    const p = b.connect()
    MockWebSocket.last().emitOpen()
    await p

    MockWebSocket.last().emitClose()
    vi.advanceTimersByTime(500)
    expect(MockWebSocket.instances.length).toBe(2)
    MockWebSocket.last().emitClose()
    vi.advanceTimersByTime(1000)
    expect(MockWebSocket.instances.length).toBe(3)
    // Now succeed.
    MockWebSocket.last().emitOpen()
    MockWebSocket.last().emitClose()
    // Backoff should have reset to 500ms.
    vi.advanceTimersByTime(500)
    expect(MockWebSocket.instances.length).toBe(4)

    b.dispose()
  })

  it('sends Ping every 5s and force-closes if no Pong arrives within 2s', async () => {
    const b = makeBridge()
    const p = b.connect()
    MockWebSocket.last().emitOpen()
    await p
    const ws = MockWebSocket.last()
    expect(ws.sent).toEqual([])

    vi.advanceTimersByTime(5000)
    expect(ws.sent).toEqual(['"Ping"'])
    expect(ws.closed).toBe(false)

    vi.advanceTimersByTime(2000)
    expect(ws.closed).toBe(true)
    expect(b.isConnected()).toBe(false)

    b.dispose()
  })

  it('does not force-close when Pong arrives in time', async () => {
    const b = makeBridge()
    const p = b.connect()
    MockWebSocket.last().emitOpen()
    await p
    const ws = MockWebSocket.last()

    vi.advanceTimersByTime(5000)
    expect(ws.sent).toEqual(['"Ping"'])
    ws.emitMessage('"Pong"')
    vi.advanceTimersByTime(2000)
    expect(ws.closed).toBe(false)
    expect(b.isConnected()).toBe(true)

    b.dispose()
  })

  it('dispose() prevents further reconnects and clears timers', async () => {
    const b = makeBridge()
    const p = b.connect()
    MockWebSocket.last().emitOpen()
    await p

    b.dispose()
    vi.advanceTimersByTime(60_000)
    // No new instance was created after dispose.
    expect(MockWebSocket.instances.length).toBe(1)
    expect(b.isConnected()).toBe(false)
  })

  it('forwards parsed messages to listeners', async () => {
    const b = makeBridge()
    const p = b.connect()
    MockWebSocket.last().emitOpen()
    await p

    const seen: unknown[] = []
    b.on((m) => seen.push(m))
    MockWebSocket.last().emitMessage('"PlaybackStarted"')
    MockWebSocket.last().emitMessage('{"Error":"boom"}')
    expect(seen).toEqual(['PlaybackStarted', { Error: 'boom' }])

    b.dispose()
  })
})
