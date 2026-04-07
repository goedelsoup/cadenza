import type { MusicalContext, SessionPhrase } from '@cadenza/types'

export const AI_PHRASE_SCHEMA = `{
  "type":            string,       // generation target label
  "summary":         string,       // 1-sentence description for context memory
  "key":             string,       // e.g. "D dorian"
  "tempo":           number,
  "time_signature":  string,       // e.g. "4/4"
  "bars":            number,
  "chords":          string[],     // chord symbols e.g. ["Dm7","G7","Cmaj9"]
  "notes": [
    {
      "pitch": number,
      "start": number,
      "dur":   number,
      "vel":   number,
      "voice": number,    // optional, default 1; use 2+ for multi-voice staves
      "slur":  number     // optional; consecutive notes sharing the same slur id are slurred together
    }
  ]
}`

export function systemPrompt(ctx: MusicalContext, target: string): string {
  return `You are Cadenza, a music composition AI. Respond ONLY with valid JSON — no markdown fences, no preamble.

Session context: ${JSON.stringify(ctx)}

Generate a "${target}" for ${ctx.bars} bars.

Required schema:
${AI_PHRASE_SCHEMA}

Rules:
- pitch: MIDI integers (60 = middle C, 69 = A4)
- start / dur: quarter-note beats as floats (0.5 = eighth note, 1.0 = quarter, 2.0 = half)
- vel: 40–110
- For chord progressions: arpeggiate or voice the chords as note events
- voice: omit for single-voice melodies; use 1 + 2 to write a counter-melody on the same staff
- slur: only set on phrases of 2+ legato notes; the same id marks one continuous slur
- Honor key, mode, and style from context
- Use idiomatic voice leading; vary rhythm within the style`
}

export const LYRIC_SCHEMA = `{
  "syllables": [
    {
      "text":      string,    // one syllable; punctuation may attach to the syllable it follows
      "wordIndex": number,    // 0-based; same value = same word, increments at each new word
      "stress":    string     // "strong" | "weak" | "unstressed"
    }
  ],
  "raw": string                // the full lyric line as a human-readable string
}`

/**
 * Build the prompt that asks Claude to write a lyric line aligned to an
 * existing phrase. The required syllable count must equal phrase.noteCount —
 * the constraint is stated up front and repeated at the end so the model
 * cannot drift.
 */
export function lyricPrompt(
  ctx: MusicalContext,
  phrase: SessionPhrase,
  userPrompt: string,
): string {
  const noteCount = phrase.noteCount
  const notes     = phrase.raw.notes ?? []
  // Compact rhythmic fingerprint: average note duration in quarters.
  const avgDur = notes.length
    ? (notes.reduce((s, n) => s + n.dur, 0) / notes.length).toFixed(2)
    : '?'

  return `You are Cadenza, a songwriting AI. Respond ONLY with valid JSON — no markdown fences, no preamble.

CRITICAL: produce EXACTLY ${noteCount} syllables — one per note in the target phrase. Do not produce more or fewer.

Session context: ${JSON.stringify(ctx)}

Target phrase: #${phrase.id} "${phrase.label}" — ${noteCount} notes across ${phrase.bars} bars in ${ctx.timeSignature} at ${phrase.tempo} bpm; average note duration ${avgDur} quarter-notes.

User request: ${userPrompt}

Required schema:
${LYRIC_SCHEMA}

Rules:
- Each syllable in "syllables" maps positionally to the corresponding note onset (index 0 → first note).
- "wordIndex" must be a 0-based integer that stays constant across syllables of the same word and increments by 1 when a new word begins.
- "stress" must be one of "strong" | "weak" | "unstressed" and should follow the natural prosody of the word.
- "raw" must be the full lyric line as plain text, matching the syllables when read aloud.
- Honor the mood, key, and style from the session context.
- Reminder: the "syllables" array MUST contain exactly ${noteCount} entries.`
}
