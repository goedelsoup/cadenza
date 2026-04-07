# Cadenza

AI-assisted songwriting workstation. Generates MIDI phrases, chord progressions,
and MuseScore-compatible MusicXML from natural language prompts via the Anthropic API.

## Stack

| Layer | Tech |
|-------|------|
| Monorepo | Nx + PNPM workspaces (package-based) |
| Frontend | SvelteKit + TypeScript |
| Core engine | Rust (WASM via wasm-pack + native daemon, future) |
| Linting | oxlint |
| Testing | Vitest (TS) + cargo test (Rust) |
| Build | Vite (web) + custom Nx executor → wasm-pack (WASM) |

## Workspace layout

```
cadenza/
├── packages/
│   ├── cadenza-theory/      # Rust: music theory primitives (no_std)
│   ├── cadenza-midi/        # Rust: MIDI 1.0 serialization + reader
│   ├── cadenza-musicxml/    # Rust: MusicXML 4.0 renderer (MuseScore)
│   ├── cadenza-wasm/        # Rust: wasm-bindgen surface (→ dist/wasm/)
│   ├── cadenza-types/       # TS:   shared type contracts
│   ├── cadenza-session/     # TS:   session state + context builder
│   ├── cadenza-api/         # TS:   Anthropic API + WASM bridge + export
│   └── cadenza-web/         # SvelteKit app
├── Cargo.toml               # Rust workspace
├── nx.json
├── pnpm-workspace.yaml
└── tsconfig.base.json
```

## Prerequisites

```bash
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack

# Node toolchain
node >= 20
npm install -g pnpm@9
```

## Getting started

```bash
pnpm install

# Build WASM core (required before dev server)
nx run cadenza-wasm:wasm:build:dev    # fast dev build
# or
nx run cadenza-wasm:wasm:build        # optimised release

# Start dev server
nx run cadenza-web:dev
```

## Common Nx commands

```bash
nx run cadenza-web:build              # production build
nx run-many -t build                  # build everything
nx run-many -t test                   # test everything
nx run-many -t lint                   # lint everything
nx affected -t test                   # test only affected packages
nx graph                              # visualise dependency graph
```

## Nx task graph

```
cadenza-wasm:wasm:build
  └─ cadenza-theory (cargo)
  └─ cadenza-midi (cargo)
  └─ cadenza-musicxml (cargo)

cadenza-web:dev
  └─ cadenza-wasm:wasm:build:dev

cadenza-web:build
  └─ cadenza-wasm:wasm:build
  └─ cadenza-types (ts)
  └─ cadenza-session (ts)
  └─ cadenza-api (ts)
```

## Phase roadmap

| Phase | Status | Scope |
|-------|--------|-------|
| 1 | ✅ | SvelteKit UI · Anthropic API · TS MIDI fallback export |
| 2 | 🔧 | Rust/WASM core wired in (proper MIDI + MusicXML) |
| 3 | ⬜ | Lyric generation with syllable/meter alignment |
| 4 | ✅ | In-browser playback (WebAudio / Tone.js) |
| 5 | 🔧 | Native Rust daemon · sample-accurate scheduling · VST3/CLAP host scaffolding |
| 5b | ⬜ | Real VST3 (`vst3-sys`) and CLAP (`clack-host`) plugin loading; bundled launcher |

## Native daemon (optional)

Cadenza ships an optional native audio daemon (`cadenza-daemon`) that provides
lower-latency playback than the in-browser Tone.js fallback. The web app works
**without** it — when the daemon is offline, playback transparently falls back
to Tone.js.

```bash
# Run the daemon (in a separate terminal)
mise run daemon
# or directly:
cargo run -p cadenza-daemon
```

The daemon binds to `ws://127.0.0.1:7878` only — there is no network exposure.
The web app's header shows a `daemon: connected | connecting | disconnected`
indicator that updates live as the bridge connects, heartbeats, and reconnects.

When the daemon is running, the web app routes all `play` commands through
the WebSocket bridge; the daemon's audio thread renders sample-accurate output
through `cpal` and the built-in `PolySynth`. VST3 and CLAP plugin hosting is
scaffolded (the trait, swap mechanism, scanning, and IPC surface are all in
place) but the actual loaders are stubs that produce silence — see the
backend modules in [`packages/cadenza-daemon/src/host.rs`](packages/cadenza-daemon/src/host.rs)
for what's needed to wire `vst3-sys` and `clack-host` in.

### Future native shell

A bundled launcher (Tauri / Electron / native menu-bar app) that starts the
daemon automatically alongside the web UI is **not** in this phase. For now
the daemon is run manually from a terminal.

## Session context layer

Each API call sends a compact JSON context object (~50–200 tokens) rather than
replaying conversation history. As phrases accumulate, `phraseRefs` lets you
reference prior material by ID:

> *"variation of phrase 3, but resolve to the relative major"*

The WASM session state mirrors the JS session state — set via `wasmSetKey`,
`wasmSetTempo`, `wasmSetTimeSig` on startup and parameter changes.

## License

MIT — Shawnee Smart Systems LLC
