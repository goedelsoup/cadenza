import type { MusicalContext, PhraseRef } from '@cadenza/types'
import type { SessionState } from './state'

export function buildContext(state: SessionState): MusicalContext {
  return {
    key:           `${state.root} ${state.mode}`,
    timeSignature: state.timeSignature,
    tempo:         state.tempo,
    style:         state.styles,
    bars:          state.bars,
    phraseRefs:    state.phrases.map((p): PhraseRef => ({
      id:      p.id,
      type:    p.label,
      summary: p.raw.summary,
    })),
  }
}

/** Rough token estimate — used for the context indicator in the UI */
export function estimateTokens(ctx: MusicalContext): number {
  return Math.ceil(JSON.stringify(ctx).length / 3.5)
}
