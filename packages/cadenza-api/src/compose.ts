import Anthropic from '@anthropic-ai/sdk'
import type {
  AiPhrase,
  AiLyricResponse,
  LyricLine,
  LyricSyllable,
  MusicalContext,
  SessionPhrase,
} from '@cadenza/types'
import { systemPrompt, lyricPrompt } from './prompts'

const isBrowser = typeof window !== 'undefined'

// In browser, default to proxying through the SvelteKit `/api/compose` route so
// the API key stays server-side. Set `VITE_DIRECT_ANTHROPIC=true` to fall back
// to the in-browser SDK call (only safe for local Vite dev with adapter-static
// where there is no server process).
const useDirectAnthropic =
  isBrowser &&
  typeof import.meta !== 'undefined' &&
  (import.meta as { env?: Record<string, string> }).env?.VITE_DIRECT_ANTHROPIC === 'true'

let directClient: Anthropic | null = null
function getDirectClient(): Anthropic {
  if (!directClient) directClient = new Anthropic({ dangerouslyAllowBrowser: true })
  return directClient
}

export interface ComposeOptions {
  onToken?:   (delta: string) => void
  maxTokens?: number
}

async function streamDirect(
  prompt: string,
  context: MusicalContext,
  target: string,
  opts: ComposeOptions,
  systemOverride?: string,
): Promise<string> {
  let raw = ''
  const stream = await getDirectClient().messages.stream({
    model:      'claude-sonnet-4-20250514',
    max_tokens: opts.maxTokens ?? 1024,
    system:     systemOverride ?? systemPrompt(context, target),
    messages:   [{ role: 'user', content: prompt }],
  })
  for await (const event of stream) {
    if (event.type === 'content_block_delta' && event.delta.type === 'text_delta') {
      raw += event.delta.text
      opts.onToken?.(event.delta.text)
    }
  }
  return raw
}

async function streamProxy(
  prompt: string,
  context: MusicalContext,
  target: string,
  opts: ComposeOptions,
  systemOverride?: string,
): Promise<string> {
  const res = await fetch('/api/compose', {
    method:  'POST',
    headers: { 'content-type': 'application/json' },
    body:    JSON.stringify({ prompt, context, target, maxTokens: opts.maxTokens, system: systemOverride }),
  })
  if (!res.ok || !res.body) {
    throw new Error(`compose proxy ${res.status}: ${await res.text().catch(() => res.statusText)}`)
  }
  let raw = ''
  const reader  = res.body.getReader()
  const decoder = new TextDecoder()
  for (;;) {
    const { value, done } = await reader.read()
    if (done) break
    const chunk = decoder.decode(value, { stream: true })
    if (chunk) {
      raw += chunk
      opts.onToken?.(chunk)
    }
  }
  return raw
}

export async function generatePhrase(
  prompt: string,
  context: MusicalContext,
  target: string,
  opts: ComposeOptions = {},
): Promise<AiPhrase> {
  const useProxy = isBrowser && !useDirectAnthropic
  const raw      = useProxy
    ? await streamProxy(prompt, context, target, opts)
    : await streamDirect(prompt, context, target, opts)

  return JSON.parse(raw.replace(/```json|```/g, '').trim()) as AiPhrase
}

export interface GeneratedLyric {
  line:     LyricLine
  warnings: string[]
}

/**
 * Generate a lyric line aligned to an existing phrase. The result is always
 * exactly `phrase.noteCount` syllables long: excess syllables are truncated
 * and shortfalls are padded with empty rest-syllables (`wordIndex: -1`).
 * Mismatch handling produces warnings rather than throwing.
 */
export async function generateLyrics(
  userPrompt: string,
  context: MusicalContext,
  phrase: SessionPhrase,
  opts: ComposeOptions = {},
): Promise<GeneratedLyric> {
  const system   = lyricPrompt(context, phrase, userPrompt)
  const useProxy = isBrowser && !useDirectAnthropic
  const raw      = useProxy
    ? await streamProxy(userPrompt, context, 'lyric', opts, system)
    : await streamDirect(userPrompt, context, 'lyric', opts, system)

  const parsed = JSON.parse(raw.replace(/```json|```/g, '').trim()) as AiLyricResponse
  const warnings: string[] = []
  const target = phrase.noteCount
  const incoming = parsed.syllables ?? []

  let syllables: LyricSyllable[]
  if (incoming.length === target) {
    syllables = incoming.map((s, i) => ({
      text:      s.text,
      noteId:    i,
      wordIndex: s.wordIndex,
      stress:    (s.stress as LyricSyllable['stress']) ?? 'unstressed',
    }))
  } else if (incoming.length > target) {
    warnings.push(`lyric truncated: ${incoming.length} syllables → ${target}`)
    syllables = incoming.slice(0, target).map((s, i) => ({
      text:      s.text,
      noteId:    i,
      wordIndex: s.wordIndex,
      stress:    (s.stress as LyricSyllable['stress']) ?? 'unstressed',
    }))
  } else {
    warnings.push(`lyric padded: ${incoming.length} syllables → ${target}`)
    syllables = []
    for (let i = 0; i < target; i++) {
      const s = incoming[i]
      if (s) {
        syllables.push({
          text:      s.text,
          noteId:    i,
          wordIndex: s.wordIndex,
          stress:    (s.stress as LyricSyllable['stress']) ?? 'unstressed',
        })
      } else {
        syllables.push({ text: '', noteId: i, wordIndex: -1, stress: 'unstressed' })
      }
    }
  }

  return {
    line: {
      phraseId:  phrase.id,
      syllables,
      rawText:   parsed.raw ?? '',
    },
    warnings,
  }
}

export async function generateVariation(
  ref: { id: number; type: string; summary: string },
  context: MusicalContext,
  instruction: string,
  opts: ComposeOptions = {},
): Promise<AiPhrase> {
  const prompt = `Variation of phrase #${ref.id} (${ref.type}: "${ref.summary}").
Instruction: ${instruction}`
  return generatePhrase(prompt, context, 'motif variation', opts)
}
