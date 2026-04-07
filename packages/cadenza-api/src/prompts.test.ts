import { describe, it, expect } from 'vitest'
import type { MusicalContext, SessionPhrase } from '@cadenza/types'
import { systemPrompt, AI_PHRASE_SCHEMA, lyricPrompt, LYRIC_SCHEMA } from './prompts'

const ctx: MusicalContext = {
  key:           'D dorian',
  timeSignature: '4/4',
  tempo:         120,
  style:         ['jazz', 'modal'],
  bars:          8,
  phraseRefs:    [],
}

describe('systemPrompt', () => {
  it('includes the context key', () => {
    const prompt = systemPrompt(ctx, 'melody phrase')
    expect(prompt).toContain('"key":"D dorian"')
  })

  it('includes the target string', () => {
    const prompt = systemPrompt(ctx, 'melody phrase')
    expect(prompt).toContain('"melody phrase"')
  })

  it('includes the JSON schema block', () => {
    const prompt = systemPrompt(ctx, 'melody phrase')
    expect(prompt).toContain(AI_PHRASE_SCHEMA)
  })

  it('includes the bar count from context', () => {
    const prompt = systemPrompt(ctx, 'melody phrase')
    expect(prompt).toContain('for 8 bars')
  })

  it('reflects an updated bar count', () => {
    const prompt = systemPrompt({ ...ctx, bars: 16 }, 'chord progression')
    expect(prompt).toContain('for 16 bars')
    expect(prompt).toContain('"chord progression"')
  })
})

describe('lyricPrompt', () => {
  const phrase: SessionPhrase = {
    id:        7,
    label:     'melody phrase',
    bars:      4,
    tempo:     120,
    noteCount: 8,
    warnings:  [],
    raw: {
      type:    'melody phrase',
      summary: 'wistful descending line',
      bars:    4,
      notes: [
        { pitch: 64, start: 0,   dur: 0.5 },
        { pitch: 62, start: 0.5, dur: 0.5 },
        { pitch: 60, start: 1,   dur: 0.5 },
        { pitch: 59, start: 1.5, dur: 0.5 },
        { pitch: 57, start: 2,   dur: 0.5 },
        { pitch: 55, start: 2.5, dur: 0.5 },
        { pitch: 53, start: 3,   dur: 0.5 },
        { pitch: 52, start: 3.5, dur: 0.5 },
      ],
    },
  }

  it('states the literal note count', () => {
    const prompt = lyricPrompt(ctx, phrase, 'autumn lyric')
    expect(prompt).toContain('EXACTLY 8 syllables')
  })

  it('includes the time signature and phrase metadata', () => {
    const prompt = lyricPrompt(ctx, phrase, 'autumn lyric')
    expect(prompt).toContain('4/4')
    expect(prompt).toContain('4 bars')
  })

  it('includes the lyric schema block', () => {
    const prompt = lyricPrompt(ctx, phrase, 'autumn lyric')
    expect(prompt).toContain(LYRIC_SCHEMA)
  })

  it('includes the user request', () => {
    const prompt = lyricPrompt(ctx, phrase, 'autumn lyric')
    expect(prompt).toContain('autumn lyric')
  })

  it('repeats the syllable-count reminder at the end', () => {
    const prompt = lyricPrompt(ctx, phrase, 'autumn lyric')
    // Two mentions: header constraint + final reminder.
    expect(prompt.match(/exactly 8/gi)?.length ?? 0).toBeGreaterThanOrEqual(2)
  })
})
