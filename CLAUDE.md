# Cadenza — AI Context for Claude

## What this is
Cadenza is an AI-assisted songwriting workstation. It generates MIDI phrases,
chord progressions, and MuseScore-compatible MusicXML from natural language
prompts via the Anthropic API. It is NOT a synthesis pipeline — it is a
composition and notation tool.

## Repo structure

    cadenza/
    ├── packages/
    │   ├── cadenza-theory/      # Rust, no_std — music theory primitives
    │   ├── cadenza-midi/        # Rust — MIDI 1.0 serializer + reader
    │   ├── cadenza-musicxml/    # Rust — MusicXML 4.0 renderer (MuseScore)
    │   ├── cadenza-wasm/        # Rust — wasm-bindgen surface, thread-local session
    │   ├── cadenza-types/       # TS — shared type contracts (no runtime deps)
    │   ├── cadenza-session/     # TS — session state + context builder
    │   ├── cadenza-api/         # TS — Anthropic SDK, WASM bridge, MIDI export
    │   └── cadenza-web/         # SvelteKit app
    ├── Cargo.toml               # Rust workspace
    ├── nx.json                  # Nx task graph config
    ├── pnpm-workspace.yaml
    ├── tsconfig.base.json
    └── mise.toml                # Dev environment bootstrap

## Stack

- **Monorepo**: Nx (package-based) + PNPM workspaces
- **Frontend**: SvelteKit + TypeScript
- **Core engine**: Rust compiled to WASM via wasm-pack
- **Linting**: oxlint
- **Testing**: Vitest (TS), cargo test (Rust)
- **Build**: Vite (web), custom Nx executor → wasm-pack (WASM)
- **AI**: Anthropic API, claude-sonnet-4-20250514, streaming

## Key architectural decisions

### Session context layer

Each API call sends a compact JSON `MusicalContext` object (~50–200 tokens)
rather than replaying conversation history. As phrases accumulate, `phraseRefs`
lets the user reference prior material by ID without re-describing it.
The context is built in `packages/cadenza-session/src/context.ts`.

### WASM / TS split

The Rust WASM module is authoritative for MIDI serialization and MusicXML
rendering once built. The TS fallback in `cadenza-api/src/export.ts` handles
MIDI only and is used before `nx run cadenza-wasm:wasm:build` has been run.
The WASM bridge in `cadenza-api/src/wasm-bridge.ts` lazy-loads and degrades
gracefully — never throws.

### AI output schema

Claude is instructed to return a single JSON object matching `AiPhrase`
(defined in `cadenza-types/src/index.ts`). The Rust parser in
`cadenza-wasm/src/parse.rs` is the canonical deserializer; the TS types
are kept in sync manually.

### Nx task graph ordering

    cadenza-wasm:wasm:build
      └─ (implicit) cadenza-theory, cadenza-midi, cadenza-musicxml via Cargo

    cadenza-web:dev
      └─ cadenza-wasm:wasm:build:dev

    cadenza-web:build
      └─ cadenza-wasm:wasm:build

## Common commands

    mise install                              # bootstrap tools
    rustup target add wasm32-unknown-unknown  # one-time
    cargo install wasm-pack                   # one-time (or: mise run bootstrap)

    pnpm install                              # install JS deps

    nx run cadenza-wasm:wasm:build:dev        # build WASM (dev, unoptimised)
    nx run cadenza-wasm:wasm:build            # build WASM (release, lto+opt-s)
    nx run cadenza-web:dev                    # start dev server

    nx run-many -t build                      # build all packages
    nx run-many -t test                       # test all packages
    nx run-many -t lint                       # lint all packages
    nx affected -t test                       # test only affected
    nx graph                                  # visualise dependency graph

## Phase roadmap

| Phase | Status | Scope |
|-------|--------|-------|
| 1 | ✅ done | SvelteKit UI · Anthropic API · TS MIDI fallback |
| 2 | 🔧 next | Wire WASM into web app; MusicXML export working end-to-end |
| 3 | ⬜ | Lyric generation with syllable/meter alignment to phrase rhythm |
| 4 | ✅ done | In-browser playback via WebAudio / Tone.js |
| 5 | 🔧 next | Native Rust daemon · sample-accurate scheduling · `Instrument` trait + VST3/CLAP scaffolding |
| 5b | ⬜ | Real VST3 (`vst3-sys`) + CLAP (`clack-host`) plugin loading; daemon launcher (Tauri/menu-bar app); systemd/launchd units |

### Phase 5 architecture (current)

The native daemon (`packages/cadenza-daemon`) hosts a cpal output stream
on a parked OS thread, an SPSC ringbuf for `TimedCmd { frame, AudioCmd }`
events, and a sample-accurate `Renderer` that holds a `Box<dyn Instrument>`
swappable via a second SPSC ringbuf. Old instruments are evicted to a
return ringbuf and dropped on the control thread (drops allocate, which
is forbidden on the audio thread).

The `Instrument` trait (`packages/cadenza-daemon/src/instrument.rs`) is the
abstraction over the built-in `PolySynth`, the VST3 backend, and the CLAP
backend. The two plugin backends are scaffolded but their actual loaders
are stubs that produce silence — the trait, swap mechanism, plugin scanning
(by file extension on `tokio::task::spawn_blocking`), the IPC surface
(`ScanPlugins`/`ScannedPlugins`/`LoadPlugin`/`SetInstrument`/`UseBuiltinSynth`),
the server handlers, and the TS bridge mirror are all real. To finish:
add `vst3-sys` / `clack-host` deps and replace the loaders in the
`vst3` and `clap` modules of `host.rs`. The doc comments at the top of
each module describe exactly what's needed.

Daemon discovery for the web app: there is no auto-launcher today. Users
run `mise run daemon` (or `cargo run -p cadenza-daemon`) in a terminal.
The header status indicator on the web app shows the live connection
state via `daemon().onStatusChange()`. A bundled native shell (Tauri or
similar) is deferred to phase 5b.

## Known gaps / immediate TODOs

- `cadenza-web` needs `@sveltejs/adapter-static` added to devDeps (missing from package.json)
- `cadenza-api/src/wasm-bridge.ts` import path is a relative `../../../` hack;
  should resolve via tsconfig `paths` alias `@cadenza/wasm` once WASM dist exists
- No `.env.local` handling yet — API key currently requires `dangerouslyAllowBrowser: true`;
  a SvelteKit server route (`+server.ts`) proxying the Anthropic call should replace this
  before any non-local deployment
- `cadenza-session` and `cadenza-api` have no Vitest tests yet
- `cadenza-theory` has no `#[cfg(test)]` unit tests yet — priority is Scale, Chord voicing,
  and `Phrase::validate_against_scale`
- MusicXML renderer does not yet handle ties, slurs, or multi-voice staves
- MIDI reader (`cadenza-midi/src/reader.rs`) is implemented but untested against real .mid files

## Coding conventions

- Rust: standard `cargo fmt` + `clippy -D warnings`; no `unwrap()` in library code
- TypeScript: oxlint enforced; `prefer-const`, `eqeqeq always`, no `var`
- Svelte: logic in `<script>`, layout in template, all styles scoped in `<style>`
- No direct `console.log` in TS library packages; use the `log()` callback pattern
  established in `+page.svelte`
- AI prompt strings live in `cadenza-api/src/prompts.ts` only — not inline in components