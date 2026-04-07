export interface AiNoteEvent {
  pitch: number;
  start: number;
  dur: number;
  vel?: number;
}

export interface AiPhrase {
  type: string;
  summary: string;
  key?: string;
  tempo?: number;
  time_signature?: string;
  bars?: number;
  chords?: string[];
  notes?: AiNoteEvent[];
}

export interface SessionPhrase {
  id: number;
  label: string;
  bars: number;
  tempo: number;
  noteCount: number;
  warnings: string[];
  raw: AiPhrase;
}

export interface MusicalContext {
  key: string;
  timeSignature: string;
  tempo: number;
  style: string[];
  bars: number;
  phraseRefs: PhraseRef[];
}

export interface PhraseRef {
  id: number;
  type: string;
  summary: string;
}

export type GenerationTarget =
  | 'chord progression'
  | 'melody phrase'
  | 'bass line'
  | 'full arrangement'
  | 'motif variation';
