# Cadenza

**An AI-assisted songwriting workstation.** Cadenza turns natural-language
prompts into MIDI phrases, chord progressions, and MuseScore-compatible
MusicXML using the Anthropic API. It is a *composition and notation* tool —
not a synthesis pipeline — designed to sit alongside your DAW or notation
software and accelerate the early, ideation-heavy parts of writing music.

> *"a melancholy ii–V–i in F minor, eighth-note bass walk, ends on a half cadence"*
>
> → JSON phrase → MIDI file → MusicXML score → in-browser playback.

## Highlights

- **Conversational composition.** Iterate on phrases the way you'd talk to a
  collaborator. Reference earlier material by ID (`phrase 3`) instead of
  re-describing it.
- **Real notation output.** MusicXML 4.0 export opens cleanly in MuseScore,
  Dorico, Finale, and Sibelius.
- **MIDI export** that drops straight into any DAW.
- **In-browser playback** via WebAudio / Tone.js — no install required to
  audition ideas.
- **Optional native audio host** with sample-accurate scheduling and real
  CLAP / VST3 plugin loading, so you can audition phrases through your own
  instruments.
- **Optional native desktop shell** (Tauri 2) that bundles the web app and
  daemon into a single window with a tray-icon supervisor.
- **Compact session context** (~50–200 tokens per call) instead of replaying
  full conversation history — keeps responses fast and costs predictable.

## How it works

### Session context layer

Each API call sends a compact JSON `MusicalContext` object describing the
current key, tempo, time signature, and a list of `phraseRefs` for prior
material. As phrases accumulate, you can reference them by ID rather than
re-describing them:

> *"variation of phrase 3, but resolve to the relative major"*

This keeps prompts small and lets long sessions stay coherent without
ballooning token usage.

### WASM core, TS shell

The composition engine — music theory primitives, MIDI serialization, and
MusicXML rendering — is written in Rust and compiled to WebAssembly. The
SvelteKit frontend talks to it through a thin TypeScript bridge. A pure-TS
MIDI fallback exists so the web app stays usable before the WASM bundle has
been built.

### AI output schema

Claude is instructed to return a single JSON object matching the `AiPhrase`
contract. The Rust parser is the canonical deserializer; the TypeScript
types mirror it.

## Native daemon (optional)

Cadenza ships an optional native audio daemon (`cadenza-daemon`) that
provides lower-latency playback than the in-browser Tone.js fallback. The
web app works **without** it — when the daemon is offline, playback
transparently falls back to Tone.js.

```bash
mise run daemon
# or:
cargo run -p cadenza-daemon
```

The daemon binds to `ws://127.0.0.1:7878` only — there is no network
exposure. The web app's header shows a `daemon: connected | connecting |
disconnected` indicator that updates live.

When the daemon is running, the web app routes all `play` commands through
the WebSocket bridge; the daemon's audio thread renders sample-accurate
output through `cpal`. It supports the built-in `PolySynth` as well as
real CLAP and VST3 plugin hosting (backed by
[`clack-host`](https://github.com/prokopyl/clack) and
[`vst3 = 0.3`](https://crates.io/crates/vst3) respectively).

## Native shell (optional)

The Tauri 2 shell at [`apps/cadenza-shell/`](apps/cadenza-shell/) wraps the
web app in a native window and supervises the daemon for you, so you don't
have to run it in a separate terminal.

```bash
cargo build -p cadenza-daemon
nx run cadenza-web:build
cargo run -p cadenza-shell
```

The shell adds a system-tray / menu-bar item showing the daemon's live
status, and reaps the child process on window close. It is **purely
additive** — running the web app standalone keeps working exactly as
before.

## Stack

| Layer        | Tech                                                |
|--------------|-----------------------------------------------------|
| Monorepo     | Nx (package-based) + PNPM workspaces                |
| Frontend     | SvelteKit + TypeScript                              |
| Core engine  | Rust → WASM (`wasm-pack`) + native daemon (`cpal`)  |
| Native shell | Tauri 2                                             |
| AI           | Anthropic API (`claude-sonnet-4-20250514`, streaming) |
| Linting      | oxlint · clippy                                     |
| Testing      | Vitest · cargo test                                 |

## Phase roadmap

| Phase | Status | Scope |
|-------|--------|-------|
| 1   | ✅ | SvelteKit UI · Anthropic API · TS MIDI fallback export |
| 2   | 🔧 | Rust/WASM core wired in (proper MIDI + MusicXML) |
| 3   | ⬜ | Lyric generation with syllable/meter alignment |
| 4   | ✅ | In-browser playback (WebAudio / Tone.js) |
| 5   | ✅ | Native Rust daemon · sample-accurate scheduling · `Instrument` trait |
| 5b  | ✅ | Real CLAP (`clack-host`) and VST3 (`vst3 = 0.3`) plugin loading; Tauri 2 native shell |
| 5c  | ⬜ | Plugin parameter automation · plugin GUIs · code-signing/notarization · restart-on-crash supervision |

## Development

Hacking on Cadenza? See [DEVELOPMENT.md](DEVELOPMENT.md) for prerequisites,
build commands, the Nx task graph, coding conventions, the Phase 5 daemon
architecture deep-dive, and the current list of known gaps.

## License

MIT — Shawnee Smart Systems LLC
