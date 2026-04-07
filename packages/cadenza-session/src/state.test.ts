import { describe, it, expect } from 'vitest'
import type { SessionPhrase } from '@cadenza/types'
import { defaultSession, withPhrase, withoutPhrase } from './state'

function makePhrase(id: number): SessionPhrase {
  return {
    id,
    label:     'melody phrase',
    bars:      4,
    tempo:     120,
    noteCount: 8,
    warnings:  [],
    raw:       { type: 'melody phrase', summary: `phrase ${id}` },
  }
}

describe('defaultSession', () => {
  it('returns the canonical default shape', () => {
    const s = defaultSession()
    expect(s).toEqual({
      root:          'D',
      mode:          'dorian',
      tempo:         120,
      timeSignature: '4/4',
      styles:        ['jazz', 'modal'],
      bars:          8,
      target:        'chord progression',
      phrases:       [],
    })
  })

  it('returns an empty phrases array', () => {
    expect(defaultSession().phrases).toEqual([])
  })
})

describe('withPhrase', () => {
  it('prepends the new phrase to the list', () => {
    const a = makePhrase(1)
    const b = makePhrase(2)
    const s0 = defaultSession()
    const s1 = withPhrase(s0, a)
    const s2 = withPhrase(s1, b)
    expect(s2.phrases.map(p => p.id)).toEqual([2, 1])
  })

  it('does not mutate the original state', () => {
    const s0 = defaultSession()
    const beforePhrases = s0.phrases
    const s1 = withPhrase(s0, makePhrase(42))
    expect(s0.phrases).toBe(beforePhrases)
    expect(s0.phrases.length).toBe(0)
    expect(s1).not.toBe(s0)
    expect(s1.phrases.length).toBe(1)
  })
})

describe('withoutPhrase', () => {
  it('removes the matching phrase by id and leaves others intact', () => {
    let s = defaultSession()
    s = withPhrase(s, makePhrase(1))
    s = withPhrase(s, makePhrase(2))
    s = withPhrase(s, makePhrase(3))
    const result = withoutPhrase(s, 2)
    expect(result.phrases.map(p => p.id)).toEqual([3, 1])
  })

  it('is a no-op when the id is not present', () => {
    let s = defaultSession()
    s = withPhrase(s, makePhrase(1))
    const result = withoutPhrase(s, 999)
    expect(result.phrases.map(p => p.id)).toEqual([1])
  })

  it('does not mutate the original state', () => {
    let s = defaultSession()
    s = withPhrase(s, makePhrase(1))
    const before = s.phrases
    withoutPhrase(s, 1)
    expect(s.phrases).toBe(before)
    expect(s.phrases.length).toBe(1)
  })
})
