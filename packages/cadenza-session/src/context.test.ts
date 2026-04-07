import { describe, it, expect } from 'vitest'
import type { SessionPhrase } from '@cadenza/types'
import { defaultSession, withPhrase } from './state'
import { buildContext, estimateTokens } from './context'

function makePhrase(id: number, label = 'melody phrase', summary = `summary ${id}`): SessionPhrase {
  return {
    id,
    label,
    bars:      4,
    tempo:     120,
    noteCount: 8,
    warnings:  [],
    raw:       { type: label, summary },
  }
}

describe('buildContext', () => {
  it('produces key as "${root} ${mode}"', () => {
    const s = defaultSession()
    const ctx = buildContext(s)
    expect(ctx.key).toBe('D dorian')
  })

  it('reflects custom root and mode', () => {
    const s = { ...defaultSession(), root: 'F#', mode: 'lydian' }
    expect(buildContext(s).key).toBe('F# lydian')
  })

  it('mirrors the simple scalar fields from state', () => {
    const s = defaultSession()
    const ctx = buildContext(s)
    expect(ctx.timeSignature).toBe(s.timeSignature)
    expect(ctx.tempo).toBe(s.tempo)
    expect(ctx.style).toEqual(s.styles)
    expect(ctx.bars).toBe(s.bars)
  })

  it('builds phraseRefs with id, type (label), and summary', () => {
    let s = defaultSession()
    s = withPhrase(s, makePhrase(1, 'chord progression', 'ii V I in D'))
    s = withPhrase(s, makePhrase(2, 'melody phrase',     'dorian motif'))
    const ctx = buildContext(s)
    expect(ctx.phraseRefs).toEqual([
      { id: 2, type: 'melody phrase',     summary: 'dorian motif' },
      { id: 1, type: 'chord progression', summary: 'ii V I in D' },
    ])
  })

  it('returns an empty phraseRefs list when state has no phrases', () => {
    expect(buildContext(defaultSession()).phraseRefs).toEqual([])
  })
})

describe('estimateTokens', () => {
  it('returns a positive integer for an empty context', () => {
    const n = estimateTokens(buildContext(defaultSession()))
    expect(Number.isInteger(n)).toBe(true)
    expect(n).toBeGreaterThan(0)
  })

  it('grows as more phraseRefs are added', () => {
    let s = defaultSession()
    const empty = estimateTokens(buildContext(s))
    s = withPhrase(s, makePhrase(1))
    const one = estimateTokens(buildContext(s))
    s = withPhrase(s, makePhrase(2))
    const two = estimateTokens(buildContext(s))
    expect(one).toBeGreaterThan(empty)
    expect(two).toBeGreaterThan(one)
  })
})
