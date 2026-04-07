import type { SessionPhrase, MusicalContext, PhraseRef, GenerationTarget } from '../types'

export interface SessionState {
  root: string
  mode: string
  tempo: number
  timeSignature: string
  styles: string[]
  bars: number
  target: GenerationTarget
  phrases: SessionPhrase[]
}

export function defaultSession(): SessionState {
  return {
    root: 'D', mode: 'dorian', tempo: 120, timeSignature: '4/4',
    styles: ['jazz', 'modal'], bars: 8, target: 'chord progression', phrases: [],
  }
}

export function buildContext(state: SessionState): MusicalContext {
  return {
    key: `${state.root} ${state.mode}`,
    timeSignature: state.timeSignature,
    tempo: state.tempo,
    style: state.styles,
    bars: state.bars,
    phraseRefs: state.phrases.map((p): PhraseRef => ({
      id: p.id, type: p.label, summary: p.raw.summary ?? '',
    })),
  }
}

export function estimateContextTokens(ctx: MusicalContext): number {
  return Math.ceil(JSON.stringify(ctx).length / 3.5)
}

export function addPhrase(state: SessionState, phrase: SessionPhrase): SessionState {
  return { ...state, phrases: [phrase, ...state.phrases] }
}
