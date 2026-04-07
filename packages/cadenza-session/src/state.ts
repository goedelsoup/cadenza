import type { SessionPhrase, GenerationTarget, StyleTag } from '@cadenza/types'

export interface SessionState {
  root:          string
  mode:          string
  tempo:         number
  timeSignature: string
  styles:        StyleTag[]
  bars:          number
  target:        GenerationTarget
  phrases:       SessionPhrase[]
}

export function defaultSession(): SessionState {
  return {
    root: 'D', mode: 'dorian', tempo: 120, timeSignature: '4/4',
    styles: ['jazz', 'modal'], bars: 8, target: 'chord progression', phrases: [],
  }
}

export function withPhrase(state: SessionState, phrase: SessionPhrase): SessionState {
  return { ...state, phrases: [phrase, ...state.phrases] }
}

export function withoutPhrase(state: SessionState, id: number): SessionState {
  return { ...state, phrases: state.phrases.filter(p => p.id !== id) }
}
