import type { RequestHandler } from './$types'
import { error }               from '@sveltejs/kit'
import { env }                 from '$env/dynamic/private'
import Anthropic               from '@anthropic-ai/sdk'
import { systemPrompt }        from '$api/prompts'
import type { MusicalContext } from '@cadenza/types'

const client = new Anthropic({ apiKey: env.ANTHROPIC_API_KEY })

interface ComposeRequest {
  prompt:     string
  context:    MusicalContext
  target:     string
  maxTokens?: number
  system?:    string   // optional pre-built system prompt; bypasses systemPrompt()
}

export const POST: RequestHandler = async ({ request }) => {
  const body = (await request.json()) as Partial<ComposeRequest>
  const { prompt, context, target, maxTokens, system } = body
  if (!prompt || !context || !target) {
    throw error(400, 'missing prompt, context, or target')
  }

  const encoder = new TextEncoder()
  const stream  = new ReadableStream<Uint8Array>({
    async start(controller) {
      try {
        const upstream = await client.messages.stream({
          model:      'claude-sonnet-4-20250514',
          max_tokens: maxTokens ?? 1024,
          system:     system ?? systemPrompt(context, target),
          messages:   [{ role: 'user', content: prompt }],
        })
        for await (const event of upstream) {
          if (event.type === 'content_block_delta' && event.delta.type === 'text_delta') {
            controller.enqueue(encoder.encode(event.delta.text))
          }
        }
        controller.close()
      } catch (e) {
        controller.error(e)
      }
    },
  })

  return new Response(stream, {
    headers: {
      'content-type':  'text/plain; charset=utf-8',
      'cache-control': 'no-cache',
    },
  })
}
