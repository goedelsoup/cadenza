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
| 5 | ✅ | Native Rust daemon · sample-accurate scheduling · `Instrument` trait |
| 5b | ✅ | Real CLAP (`clack-host`) and VST3 (`vst3 = 0.3`) plugin loading; Tauri 2 native shell |
| 5c | ⬜ | Plugin parameter automation · plugin GUIs · Tauri shell code-signing/notarization · restart-on-crash supervision |

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
through `cpal` and the built-in `PolySynth`. CLAP plugin hosting is real,
backed by [`clack-host`](https://github.com/prokopyl/clack), and lives at
[`apps/cadenza-daemon/src/host/clap_backend.rs`](apps/cadenza-daemon/src/host/clap_backend.rs).
VST3 hosting is real, backed by [`vst3 = 0.3.0`](https://crates.io/crates/vst3)
(coupler-rs), and lives at
[`apps/cadenza-daemon/src/host/vst3_backend.rs`](apps/cadenza-daemon/src/host/vst3_backend.rs).
Both backends are gated behind the on-by-default `clap-host` and `vst3-host`
cargo features, and both have feature-gated smoke tests
(`--features clap-host-tests` / `--features vst3-host-tests`) that load
real bundled plugins from `apps/cadenza-daemon/tests/fixtures/`.

## Native shell (optional)

The Tauri 2 shell at [`apps/cadenza-shell/`](apps/cadenza-shell/) wraps
the web app in a native window and supervises the daemon for the user,
so you don't have to run `mise run daemon` in a separate terminal.

```bash
# One-time: build the daemon and the cadenza-web static bundle
cargo build -p cadenza-daemon
nx run cadenza-web:build

# Launch the shell — it will spawn the daemon, render cadenza-web inside
# a native window, and reap the child on close.
cargo run -p cadenza-shell
```

The shell adds a system-tray / menu-bar item with a `daemon: running |
exited (N) | not started` label that polls the supervised child every
1s. The child is killed via the OS signal mechanism on window close (or
on clicking the tray's "Quit Cadenza" item).

The shell is **purely additive** — `nx run cadenza-web:dev` standalone
keeps working exactly as before, with or without the shell installed.

### Bundling for distribution

For Phase 5b v1 the shell builds as a regular Cargo binary
(`bundle.active = false` in
[`apps/cadenza-shell/tauri.conf.json`](apps/cadenza-shell/tauri.conf.json)).
To produce a redistributable `.app` / `.AppImage` / `.exe` you'll need
the Tauri CLI and a full icon set. That's documented as Phase 5c work;
code-signing, notarization, and auto-update are also 5c.

## Session context layer

Each API call sends a compact JSON context object (~50–200 tokens) rather than
replaying conversation history. As phrases accumulate, `phraseRefs` lets you
reference prior material by ID:

> *"variation of phrase 3, but resolve to the relative major"*

The WASM session state mirrors the JS session state — set via `wasmSetKey`,
`wasmSetTempo`, `wasmSetTimeSig` on startup and parameter changes.

## License

MIT — Shawnee Smart Systems LLC
