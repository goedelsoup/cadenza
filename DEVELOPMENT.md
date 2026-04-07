# Development

Everything you need to hack on Cadenza locally — toolchain bootstrap, the
Nx task graph, coding conventions, the Phase 5 daemon architecture, and the
current list of known gaps.

For a high-level product overview, see the [README](README.md).

## Workspace layout

```
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

Or, if you have [mise](https://mise.jdx.dev/) installed:

```bash
mise install
mise run bootstrap
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

## Coding conventions

- **Rust:** standard `cargo fmt` + `clippy -D warnings`; **no `unwrap()` in
  library code**.
- **TypeScript:** oxlint enforced; `prefer-const`, `eqeqeq always`, no
  `var`.
- **Svelte:** logic in `<script>`, layout in template, all styles scoped in
  `<style>`.
- **No direct `console.log`** in TS library packages — use the `log()`
  callback pattern established in `+page.svelte`.
- **AI prompt strings** live in [`packages/cadenza-api/src/prompts.ts`](packages/cadenza-api/src/prompts.ts)
  only — never inline in components.

## Phase 5 daemon architecture

The native daemon (`apps/cadenza-daemon`) hosts a `cpal` output stream on a
parked OS thread, an SPSC ringbuf for `TimedCmd { frame, AudioCmd }` events,
and a sample-accurate `Renderer` that holds a `Box<dyn Instrument>`
swappable via a second SPSC ringbuf. Evicted instruments are tagged with
their `PluginId` and routed back through `swap_out_rx` → a 100ms-tick tokio
task → `PluginHost::return_instrument`, so re-activating a previously
loaded plugin is a hot swap and not a reload from disk.

The `Instrument` trait
([`apps/cadenza-daemon/src/instrument.rs`](apps/cadenza-daemon/src/instrument.rs))
is the abstraction over the built-in `PolySynth`, hosted CLAP plugins, and
hosted VST3 plugins. The CLAP backend
([`apps/cadenza-daemon/src/host/clap_backend.rs`](apps/cadenza-daemon/src/host/clap_backend.rs))
is gated behind the on-by-default `clap-host` cargo feature. It owns a
dedicated `clap-main` OS thread to satisfy `PluginInstance`'s `!Send`
constraint; only the `Send` `StartedPluginAudioProcessor` crosses to the
audio thread. The smoke test under `--features clap-host-tests` loads a
committed nih-plug `gain` example bundle and verifies the full lifecycle.

### Daemon discovery for the web app

- **Standalone (`nx run cadenza-web:dev`):** users run `mise run daemon`
  (or `cargo run -p cadenza-daemon`) in a separate terminal. The web app's
  header status indicator shows the live connection state via
  `daemon().onStatusChange()`.
- **Bundled (`cargo run -p cadenza-shell`):** the Tauri shell at
  [`apps/cadenza-shell/`](apps/cadenza-shell/) spawns the daemon as a child
  process at startup, hosts `apps/cadenza-web/build/` inside a native
  window via the platform webview, and reaps the child on window close.
  See [`apps/cadenza-shell/src/main.rs`](apps/cadenza-shell/src/main.rs)
  for daemon binary discovery (env override → exe sibling → workspace
  target dir) and the `DaemonSupervisor` struct.

### Plugin host smoke tests

Both backends ship feature-gated smoke tests that load real bundled plugins
from `apps/cadenza-daemon/tests/fixtures/`:

```bash
cargo test -p cadenza-daemon --features clap-host-tests
cargo test -p cadenza-daemon --features vst3-host-tests
```

## Bundling for distribution

For Phase 5b v1 the shell builds as a regular Cargo binary
(`bundle.active = false` in
[`apps/cadenza-shell/tauri.conf.json`](apps/cadenza-shell/tauri.conf.json)).
To produce a redistributable `.app` / `.AppImage` / `.exe` you'll need the
Tauri CLI and a full icon set. That's tracked as Phase 5c work, alongside
code-signing, notarization, and auto-update.

## Known gaps / immediate TODOs

- [`apps/cadenza-web`](apps/cadenza-web) needs `@sveltejs/adapter-static`
  added to `devDependencies` (currently missing from `package.json`).
- [`packages/cadenza-api/src/wasm-bridge.ts`](packages/cadenza-api/src/wasm-bridge.ts)
  import path is a relative `../../../` hack — should resolve via tsconfig
  `paths` alias `@cadenza/wasm` once the WASM dist exists.
- No `.env.local` handling yet — the Anthropic API key currently requires
  `dangerouslyAllowBrowser: true`. A SvelteKit server route
  (`+server.ts`) proxying the call should replace this before any
  non-local deployment.
- [`packages/cadenza-session`](packages/cadenza-session) and
  [`packages/cadenza-api`](packages/cadenza-api) have no Vitest tests yet.
- [`packages/cadenza-theory`](packages/cadenza-theory) has no `#[cfg(test)]`
  unit tests yet — priority is `Scale`, `Chord` voicing, and
  `Phrase::validate_against_scale`.
- MusicXML renderer does not yet handle ties, slurs, or multi-voice
  staves.
- MIDI reader
  ([`packages/cadenza-midi/src/reader.rs`](packages/cadenza-midi/src/reader.rs))
  is implemented but untested against real `.mid` files.
