# Cadenza — AI Context for Claude

## What this is
Cadenza is an AI-assisted songwriting workstation. It generates MIDI phrases,
chord progressions, and MuseScore-compatible MusicXML from natural language
prompts via the Anthropic API. It is NOT a synthesis pipeline — it is a
composition and notation tool.

## Repo structure

    cadenza/
    ├── apps/
    │   ├── cadenza-daemon/      # Rust — native audio host (cpal + CLAP/VST3)
    │   ├── cadenza-shell/       # Rust — Tauri 2 native shell that hosts cadenza-web and supervises cadenza-daemon
    │   └── cadenza-web/         # SvelteKit app
    ├── packages/
    │   ├── cadenza-theory/      # Rust, no_std — music theory primitives
    │   ├── cadenza-midi/        # Rust — MIDI 1.0 serializer + reader
    │   ├── cadenza-musicxml/    # Rust — MusicXML 4.0 renderer (MuseScore)
    │   ├── cadenza-wasm/        # Rust — wasm-bindgen surface, thread-local session
    │   ├── cadenza-ipc/         # Rust — daemon ↔ frontend wire protocol
    │   ├── cadenza-types/       # TS — shared type contracts (no runtime deps)
    │   ├── cadenza-session/     # TS — session state + context builder
    │   ├── cadenza-player/      # TS — Tone.js fallback playback engine
    │   └── cadenza-api/         # TS — Anthropic SDK, WASM bridge, MIDI export
    ├── Cargo.toml               # Rust workspace
    ├── nx.json                  # Nx task graph config (appsDir=apps, libsDir=packages)
    ├── pnpm-workspace.yaml      # `apps/*` and `packages/*` globs
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
| 5b | ✅ done | Real CLAP hosting (`clack-host`); real VST3 hosting (`vst3 = "0.3.0"` from coupler-rs); Tauri 2 native shell at `apps/cadenza-shell/` |
| 5c | ⬜ | Plugin parameter automation; plugin GUIs; sandboxing; Tauri shell code-signing/notarization/auto-update; restart-on-crash daemon supervision |

### Phase 5 architecture (current)

The native daemon (`apps/cadenza-daemon`) hosts a cpal output stream
on a parked OS thread, an SPSC ringbuf for `TimedCmd { frame, AudioCmd }`
events, and a sample-accurate `Renderer` that holds a `Box<dyn Instrument>`
swappable via a second SPSC ringbuf. Evicted instruments are tagged with
their `PluginId` and routed back through `swap_out_rx` → a 100ms-tick
tokio task → `PluginHost::return_instrument`, so re-activating a previously
loaded plugin is a hot swap and not a reload from disk.

The `Instrument` trait (`apps/cadenza-daemon/src/instrument.rs`) is the
abstraction over the built-in `PolySynth`, hosted CLAP plugins, and (in
progress) hosted VST3 plugins. The CLAP backend
(`apps/cadenza-daemon/src/host/clap_backend.rs`) is real and gated behind
the on-by-default `clap-host` cargo feature. It owns a dedicated
`clap-main` OS thread to satisfy `PluginInstance`'s `!Send` constraint;
only the `Send` `StartedPluginAudioProcessor` crosses to the audio thread.
The smoke test under `--features clap-host-tests` loads a committed
nih-plug `gain` example bundle and verifies the full lifecycle.

Daemon discovery for the web app:
- **Standalone (`nx run cadenza-web:dev`):** users run `mise run daemon`
  (or `cargo run -p cadenza-daemon`) in a separate terminal. The web
  app's header status indicator shows the live connection state via
  `daemon().onStatusChange()`.
- **Bundled (`cargo run -p cadenza-shell`):** the Tauri shell at
  `apps/cadenza-shell/` spawns the daemon as a child process at startup,
  hosts `apps/cadenza-web/build/` inside a native window via the
  platform webview, and reaps the child on window close. A
  system-tray / menu-bar item polls the child every 1s and reports
  `daemon: running | exited (N) | not started`. The shell is purely
  additive — running `nx run cadenza-web:dev` standalone keeps working.
  See [`apps/cadenza-shell/src/main.rs`](apps/cadenza-shell/src/main.rs)
  for daemon binary discovery (env override → exe sibling →
  workspace target dir) and the `DaemonSupervisor` struct.

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

## Workflow

- **Never create git commits.** The user always handles committing themselves.
  Stage changes if asked, but do not run `git commit` — leave the working tree
  dirty for the user to review and commit.