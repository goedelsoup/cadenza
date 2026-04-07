// AI-generated phrase schema — mirrors cadenza-wasm's parse::AiPhrase
export interface AiNoteEvent {
  pitch: number   // MIDI 0–127
  start: number   // quarter-note beats (float)
  dur:   number   // quarter-note beats (float)
  vel?:  number   // 1–127, default 80
}

export interface AiPhrase {
  type:            string
  summary:         string
  key?:            string        // e.g. "D dorian"
  tempo?:          number
  time_signature?: string        // e.g. "4/4"
  bars?:           number
  chords?:         string[]      // chord symbols e.g. ["Dm7","G7","Cmaj7"]
  notes?:          AiNoteEvent[]
}

// Lyric primitives — syllables align by index into a phrase's notes array
export interface LyricSyllable {
  text:      string
  noteId:    number    // index into the phrase's notes array
  wordIndex: number    // 0-based; same value = same word; -1 = padding rest
  stress:    'strong' | 'weak' | 'unstressed'
}

export interface LyricLine {
  phraseId:  number
  syllables: LyricSyllable[]
  rawText:   string
}

// Raw shape Claude returns for a lyric request
export interface AiLyricResponse {
  syllables: { text: string; wordIndex: number; stress: string }[]
  raw:       string
}

// Hydrated after WASM ingestion
export interface SessionPhrase {
  id:        number
  label:     string
  bars:      number
  tempo:     number
  noteCount: number
  warnings:  string[]
  raw:       AiPhrase
  lyrics?:   LyricLine[]
}

// Compact context object — sent with every API call (~50–200 tok)
export interface MusicalContext {
  key:           string
  timeSignature: string
  tempo:         number
  style:         string[]
  bars:          number
  phraseRefs:    PhraseRef[]
}

export interface PhraseRef {
  id:      number
  type:    string
  summary: string
}

export type GenerationTarget =
  | 'chord progression'
  | 'melody phrase'
  | 'bass line'
  | 'full arrangement'
  | 'motif variation'
  | 'lyric'

export type StyleTag =
  | 'jazz' | 'blues' | 'modal' | 'latin' | 'bossa'
  | 'funk' | 'classical' | 'folk' | 'ambient' | 'cinematic'
