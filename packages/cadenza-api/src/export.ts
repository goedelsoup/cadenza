import type { AiPhrase } from '@cadenza/types'

/**
 * Pure-TS MIDI builder — used before WASM is compiled,
 * and as a fallback in environments where WASM fails to load.
 */
export function buildMidiBytes(phrase: AiPhrase, tempo: number): Uint8Array {
  const TPQ    = 480
  const micros = Math.round(60_000_000 / tempo)

  function varLen(v: number): number[] {
    if (v < 128) return [v]
    const out: number[] = []
    let n = v
    while (n > 0) { out.unshift(n & 0x7f); n >>= 7 }
    return out.map((b, i) => i < out.length - 1 ? b | 0x80 : b)
  }

  const be32 = (v: number) => [(v>>24)&0xff, (v>>16)&0xff, (v>>8)&0xff, v&0xff]
  const be16 = (v: number) => [(v>>8)&0xff, v&0xff]

  const header = [0x4d,0x54,0x68,0x64, 0,0,0,6, 0,0, ...be16(1), ...be16(TPQ)]

  type Ev = { tick: number; data: number[] }
  const evs: Ev[] = [
    { tick: 0, data: [0xff,0x51,0x03, (micros>>16)&0xff, (micros>>8)&0xff, micros&0xff] },
  ]

  for (const n of phrase.notes ?? []) {
    const s = Math.round(n.start * TPQ)
    const e = Math.round((n.start + n.dur) * TPQ)
    evs.push({ tick: s, data: [0x90, n.pitch, n.vel ?? 80] })
    evs.push({ tick: e, data: [0x80, n.pitch, 0] })
  }
  evs.sort((a, b) => a.tick - b.tick)

  const track: number[] = []
  let prev = 0
  for (const ev of evs) { track.push(...varLen(ev.tick - prev), ...ev.data); prev = ev.tick }
  track.push(0, 0xff, 0x2f, 0)

  return new Uint8Array([...header, ...be32(8 + track.length), 0x4d,0x54,0x72,0x6b, ...be32(track.length), ...track])
}

export function triggerDownload(bytes: Uint8Array, filename: string, mime = 'audio/midi'): void {
  const url = URL.createObjectURL(new Blob([bytes], { type: mime }))
  const a   = Object.assign(document.createElement('a'), { href: url, download: filename })
  document.body.appendChild(a)
  a.click()
  document.body.removeChild(a)
  URL.revokeObjectURL(url)
}
