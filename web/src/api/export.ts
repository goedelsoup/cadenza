import type { AiPhrase } from '../types'

export function buildMidiBytes(phrase: AiPhrase, tempo: number): Uint8Array {
  const TPQ = 480
  const micros = Math.round(60_000_000 / tempo)

  function varLen(val: number): number[] {
    if (val < 128) return [val]
    const out: number[] = []
    let v = val
    while (v > 0) { out.unshift(v & 0x7f); v >>= 7 }
    return out.map((b, i) => i < out.length - 1 ? b | 0x80 : b)
  }

  const be32 = (v: number) => [(v>>24)&0xff,(v>>16)&0xff,(v>>8)&0xff,v&0xff]
  const be16 = (v: number) => [(v>>8)&0xff,v&0xff]

  const header = [0x4d,0x54,0x68,0x64,0,0,0,6,0,0,...be16(1),...be16(TPQ)]

  type E = { tick: number; data: number[] }
  const evts: E[] = [
    { tick: 0, data: [0xff,0x51,0x03,(micros>>16)&0xff,(micros>>8)&0xff,micros&0xff] }
  ]

  ;(phrase.notes ?? []).forEach(n => {
    const s = Math.round(n.start * TPQ)
    const e = Math.round((n.start + n.dur) * TPQ)
    evts.push({ tick: s, data: [0x90, n.pitch, n.vel ?? 80] })
    evts.push({ tick: e, data: [0x80, n.pitch, 0] })
  })

  evts.sort((a, b) => a.tick - b.tick)

  const rawTrack: number[] = []
  let prev = 0
  for (const ev of evts) {
    rawTrack.push(...varLen(ev.tick - prev), ...ev.data)
    prev = ev.tick
  }
  rawTrack.push(0, 0xff, 0x2f, 0)

  return new Uint8Array([...header, ...be32(rawTrack.length + 8), 0x4d,0x54,0x72,0x6b, ...be32(rawTrack.length), ...rawTrack])
}

export function download(bytes: Uint8Array, filename: string, mime: string): void {
  const url = URL.createObjectURL(new Blob([bytes], { type: mime }))
  const a = Object.assign(document.createElement('a'), { href: url, download: filename })
  document.body.appendChild(a); a.click(); document.body.removeChild(a)
  URL.revokeObjectURL(url)
}
