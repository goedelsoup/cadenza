// Thin async bridge to cadenza-wasm (built via: pnpm wasm:build)
// Falls back gracefully when WASM is not yet built

let wasm: typeof import('../wasm/cadenza_wasm') | null = null
let initialized = false

async function ensureInit() {
  if (initialized) return
  try {
    const mod = await import('../wasm/cadenza_wasm.js')
    await mod.default()
    wasm = mod
    initialized = true
  } catch {
    console.warn('[cadenza-wasm] WASM module not found — run pnpm wasm:build')
  }
}

export async function ingestPhrase(json: string) {
  await ensureInit()
  return wasm?.ingest_phrase(json) ?? null
}

export async function phraseToMidi(phraseId: number): Promise<Uint8Array | null> {
  await ensureInit()
  return wasm?.phrase_to_midi(phraseId) ?? null
}

export async function phraseToMusicXml(id: number, title: string, composer: string): Promise<string | null> {
  await ensureInit()
  return wasm?.phrase_to_musicxml(id, title, composer) ?? null
}

export async function setKey(root: string, mode: string) {
  await ensureInit(); wasm?.set_key(root, mode)
}
export async function setTempo(bpm: number) {
  await ensureInit(); wasm?.set_tempo(bpm)
}
export async function setTimeSig(sig: string) {
  await ensureInit(); wasm?.set_time_signature(sig)
}
export async function sessionToMidi(): Promise<Uint8Array | null> {
  await ensureInit(); return wasm?.session_to_midi() ?? null
}
export async function clearSession() {
  await ensureInit(); wasm?.clear_session()
}
