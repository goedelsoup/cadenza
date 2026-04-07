type WasmModule = {
  default: () => Promise<void>
  ingest_phrase: (json: string) => unknown
  attach_lyrics: (phraseId: number, json: string) => void
  phrase_to_midi: (id: number) => Uint8Array
  phrase_to_musicxml: (id: number, title: string, composer: string) => string
  set_key: (root: string, mode: string) => void
  set_tempo: (bpm: number) => void
  set_time_signature: (sig: string) => void
  session_to_midi: () => Uint8Array
  clear_session: () => void
}

let mod: WasmModule | null = null

async function load(): Promise<WasmModule | null> {
  if (mod) return mod
  try {
    // Dynamic string construction prevents Vite static analysis from
    // trying to resolve the path before the WASM module is built
    const path = ['@cadenza', 'wasm'].join('/')
    mod = await import(/* @vite-ignore */ path) as WasmModule
    await mod.default()
  } catch {
    console.warn('[cadenza-wasm] not loaded — run: nx run cadenza-wasm:wasm:build:dev')
  }
  return mod
}

export async function wasmIngestPhrase(json: string) {
  return (await load())?.ingest_phrase(json) ?? null
}
export async function wasmAttachLyrics(phraseId: number, json: string): Promise<void> {
  (await load())?.attach_lyrics(phraseId, json)
}
export async function wasmPhraseToMidi(id: number): Promise<Uint8Array | null> {
  return (await load())?.phrase_to_midi(id) ?? null
}
export async function wasmPhraseToMusicXml(id: number, title: string, composer: string): Promise<string | null> {
  return (await load())?.phrase_to_musicxml(id, title, composer) ?? null
}
export async function wasmSetKey(root: string, mode: string) {
  (await load())?.set_key(root, mode)
}
export async function wasmSetTempo(bpm: number) {
  (await load())?.set_tempo(bpm)
}
export async function wasmSetTimeSig(sig: string) {
  (await load())?.set_time_signature(sig)
}
export async function wasmSessionToMidi(): Promise<Uint8Array | null> {
  return (await load())?.session_to_midi() ?? null
}
export async function wasmClearSession() {
  (await load())?.clear_session()
}