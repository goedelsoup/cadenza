import Anthropic from '@anthropic-ai/sdk'
import type { AiPhrase, MusicalContext } from '../types'

const client = new Anthropic({ dangerouslyAllowBrowser: true })

export async function generatePhrase(
  prompt: string,
  context: MusicalContext,
  target: string,
  onToken?: (delta: string) => void
): Promise<AiPhrase> {
  const systemPrompt = `You are Cadenza, a music composition AI. Respond ONLY with valid JSON — no markdown fences, no preamble.

Session context: ${JSON.stringify(context)}

Generate a "${target}" for ${context.bars} bars.

Required JSON schema:
{
  "type": string,
  "summary": string,
  "key": string,
  "tempo": number,
  "time_signature": string,
  "bars": number,
  "chords": string[],
  "notes": [{ "pitch": number, "start": number, "dur": number, "vel": number }]
}

Rules:
- pitch: MIDI integers (60 = middle C)
- start/dur: quarter-note beats as floats (0.5 = eighth, 1.0 = quarter, 2.0 = half)
- vel: 40–110
- chords: symbol notation e.g. ["Dm7","G7","Cmaj7"]
- Include arpeggiated/voiced note events for chord progressions
- Honor key, mode, style from context
- Idiomatic voice leading`

  let rawJson = ''
  const stream = await client.messages.stream({
    model: 'claude-sonnet-4-20250514',
    max_tokens: 1024,
    system: systemPrompt,
    messages: [{ role: 'user', content: prompt }],
  })

  for await (const event of stream) {
    if (event.type === 'content_block_delta' && event.delta.type === 'text_delta') {
      rawJson += event.delta.text
      onToken?.(event.delta.text)
    }
  }

  const clean = rawJson.replace(/```json|```/g, '').trim()
  return JSON.parse(clean) as AiPhrase
}
