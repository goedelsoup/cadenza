//! Plugin host: scans directories for VST3/CLAP plugins and constructs
//! `Box<dyn Instrument>` instances the audio thread can swap into the
//! cpal callback.
//!
//! ## Architecture
//!
//! - **Scanning** walks a directory and returns paths whose extension is
//!   `.vst3` or `.clap`. Runs on `tokio::task::spawn_blocking` so the
//!   tokio runtime never sees a sync filesystem call.
//! - **Loading** dispatches by extension to either [`vst3::load`] or
//!   [`clap::load`], each of which constructs a concrete `Instrument`.
//! - **Swapping** is handled by [`crate::audio::AudioEngine::swap_instrument`].
//!   The host is responsible only for *constructing* instruments; lifetime
//!   on the audio thread belongs to the engine.
//!
//! ## Backends in this revision
//!
//! Both VST3 and CLAP loaders are **scaffolded stubs**. They construct a
//! [`StubInstrument`] that returns silence and logs a one-time warning.
//! The trait surface, error types, IPC wire format, scan logic, and audio
//! thread swap mechanism are all fully wired — landing real hosting is a
//! drop-in replacement of `vst3::load` and `clap::load`. See the comments
//! at the top of each module for the concrete crate / unsafe boundary
//! each backend will need.

use crate::instrument::{Instrument, InstrumentBox};
use cadenza_ipc::{PluginId, PluginParam};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("plugin not found: {0}")]
    NotFound(String),
    #[error("unsupported plugin format: {0}")]
    UnsupportedFormat(String),
    #[error("plugin scan failed: {0}")]
    ScanFailed(String),
    /// Reserved for real backends — `vst3-sys` / `clack-host` will return
    /// this on plugin instantiation failure.
    #[allow(dead_code)]
    #[error("plugin load failed: {0}")]
    LoadFailed(String),
}

/// Metadata about a loaded plugin. The boxed instrument is held in
/// `LoadedEntry::instrument` until it's checked out for the audio thread.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub id:     PluginId,
    pub name:   String,
    pub params: Vec<PluginParam>,
}

struct LoadedEntry {
    /// Used by future plugin metadata queries; the wire format already
    /// carries name in `PluginLoaded`.
    #[allow(dead_code)]
    name:   String,
    /// Used once real backends populate per-plugin parameter lists.
    #[allow(dead_code)]
    params: Vec<PluginParam>,
    /// `Some` while the plugin is owned by the host (idle); `None` while
    /// the audio thread is rendering through it. Returned via
    /// [`PluginHost::return_instrument`] after a swap-out.
    instrument: Option<InstrumentBox>,
}

pub struct PluginHost {
    next_id: PluginId,
    plugins: HashMap<PluginId, LoadedEntry>,
}

impl Default for PluginHost {
    fn default() -> Self {
        Self { next_id: 1, plugins: HashMap::new() }
    }
}

impl PluginHost {
    pub fn new() -> Self { Self::default() }

    /// Walk a directory looking for `.vst3` and `.clap` files. Recursive
    /// one level deep — VST3 bundles on macOS are directories with the
    /// `.vst3` extension; on Linux/Windows they're files. CLAP plugins are
    /// always files.
    ///
    /// This is a *blocking* function; call it from `spawn_blocking`.
    pub fn scan(&self, dir: &Path) -> Result<Vec<String>, HostError> {
        let mut out = Vec::new();
        let entries = std::fs::read_dir(dir)
            .map_err(|e| HostError::ScanFailed(format!("{}: {e}", dir.display())))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if matches_plugin_ext(&path) {
                if let Some(s) = path.to_str() {
                    out.push(s.to_string());
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Load a plugin by absolute path. Dispatches to the VST3 or CLAP
    /// backend by file extension. Returns metadata; the constructed
    /// `Box<dyn Instrument>` is stashed on the host and can be checked
    /// out via [`Self::take_instrument`] when activating it.
    ///
    /// `sample_rate` is the audio device's current sample rate; the
    /// plugin must be prepared for this rate before being handed to the
    /// audio thread.
    ///
    /// Blocking — call from `spawn_blocking`.
    pub fn load(&mut self, path: &str, sample_rate: u32) -> Result<LoadedPlugin, HostError> {
        let p = PathBuf::from(path);
        if !p.exists() {
            return Err(HostError::NotFound(path.to_string()));
        }
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);

        let (instrument, name) = match ext.as_deref() {
            Some("vst3") => vst3::load(&p, sample_rate)?,
            Some("clap") => clap::load(&p, sample_rate)?,
            _ => return Err(HostError::UnsupportedFormat(path.to_string())),
        };

        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        let entry = LoadedEntry {
            name:   name.clone(),
            params: Vec::new(),
            instrument: Some(instrument),
        };
        self.plugins.insert(id, entry);
        Ok(LoadedPlugin { id, name, params: Vec::new() })
    }

    /// Take ownership of the boxed instrument for plugin `id`. Returns
    /// `None` if the plugin doesn't exist or is already checked out.
    /// Caller is expected to hand the instrument to
    /// [`crate::audio::AudioEngine::swap_instrument`].
    pub fn take_instrument(&mut self, id: PluginId) -> Option<InstrumentBox> {
        self.plugins.get_mut(&id).and_then(|e| e.instrument.take())
    }

    /// Return an evicted instrument to the host so it can be re-activated
    /// later without re-loading the plugin from disk. Called from the
    /// server's periodic drain task after pulling tagged evictions out of
    /// the audio engine via `AudioEngine::take_dropped_instruments`.
    pub fn return_instrument(&mut self, id: PluginId, inst: InstrumentBox) {
        if let Some(entry) = self.plugins.get_mut(&id) {
            entry.instrument = Some(inst);
        }
        // If the plugin was unloaded between take and return, just drop
        // the instrument here on the control thread.
    }

    pub fn unload(&mut self, id: PluginId) -> Result<(), HostError> {
        match self.plugins.remove(&id) {
            Some(_) => Ok(()),
            None    => Err(HostError::NotFound(format!("id {id}"))),
        }
    }
}

fn matches_plugin_ext(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("vst3") | Some("clap")
    )
}

// ── Stub instrument shared by both backends ─────────────────────────────────

/// Silence-producing instrument used by the VST3 and CLAP backend stubs.
/// Logs a one-time warning per instance so the user can see that the
/// scaffolding is in place but real hosting hasn't been wired up yet.
pub(crate) struct StubInstrument {
    name:        String,
    backend:     &'static str,
    warned:      bool,
    /// Sample rate the plugin was prepared for. Real backends will use
    /// this to drive plugin process() calls.
    #[allow(dead_code)]
    sample_rate: u32,
}

impl StubInstrument {
    pub(crate) fn new(name: String, backend: &'static str, sample_rate: u32) -> Self {
        Self { name, backend, warned: false, sample_rate }
    }
}

impl Instrument for StubInstrument {
    fn note_on(&mut self, _pitch: u8, _velocity: u8)  {}
    fn note_off(&mut self, _pitch: u8)                {}
    fn all_notes_off(&mut self)                        {}

    fn render_with_events(
        &mut self,
        out:       &mut [f32],
        _channels: usize,
        _events:   &[(u32, AudioCmd)],
    ) {
        if !self.warned {
            tracing::warn!(
                "{} plugin '{}' is a scaffolded stub — producing silence. \
                 Drop in real hosting via vst3-sys / clack-host to enable.",
                self.backend, self.name
            );
            self.warned = true;
        }
        // Silence: zero the buffer.
        for s in out.iter_mut() { *s = 0.0; }
    }
}

use crate::audio::AudioCmd;

// ── VST3 backend stub ───────────────────────────────────────────────────────

mod vst3 {
    //! VST3 backend stub.
    //!
    //! ## To finish this backend
    //!
    //! 1. Add `vst3-sys = "0.1"` to `cadenza-daemon/Cargo.toml`.
    //! 2. Replace [`load`] below with code that:
    //!    - Loads the plugin bundle (`.vst3` is a directory on macOS, a
    //!      shared library file on Linux/Windows). Use `libloading` to
    //!      open the platform-specific binary inside the bundle.
    //!    - Calls `GetPluginFactory` to obtain `IPluginFactory*`.
    //!    - Enumerates classes and instantiates the first audio effect /
    //!      instrument class as `IAudioProcessor` + `IComponent`.
    //!    - Calls `setupProcessing` with `ProcessSetup` (sampleRate set to
    //!      the device rate, blockSize set to a fixed safe upper bound).
    //!    - Activates the component and audio processor.
    //! 3. Define a `Vst3Instrument` struct that owns the plugin handles
    //!    and implements `Instrument`. The `render_with_events` method
    //!    builds a `ProcessData` with sample-accurate event input from
    //!    the supplied event slice and calls `IAudioProcessor::process`.
    //! 4. Pre-allocate any input/output `AudioBusBuffers` and event lists
    //!    in the constructor — never in `render_with_events`.
    //! 5. Document `unsafe` invariants thoroughly. The cpal callback must
    //!    not own COM-style interfaces that aren't `Send`; if the plugin
    //!    isn't audio-thread safe, host it on its own thread and use a
    //!    second ringbuf for sample-accurate event injection.
    //! 6. Licensing note: `vst3-sys` re-implements the COM interfaces in
    //!    Rust to avoid bundling Steinberg's GPL/proprietary SDK headers.
    //!    Verify this is acceptable for your distribution model before
    //!    shipping.

    use super::{HostError, InstrumentBox, StubInstrument};
    use std::path::Path;

    pub(super) fn load(path: &Path, sample_rate: u32) -> Result<(InstrumentBox, String), HostError> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown.vst3")
            .to_string();
        tracing::info!("VST3 stub: 'loading' {} (real hosting not yet wired)", name);
        let inst: InstrumentBox = Box::new(StubInstrument::new(name.clone(), "VST3", sample_rate));
        Ok((inst, name))
    }
}

// ── CLAP backend ────────────────────────────────────────────────────────────
//
// When the `clap-host` feature is enabled, dispatches to the real
// `clack-host`-backed loader in [`clap_backend`]. Otherwise falls back to a
// stub that produces silence so the daemon still builds and runs without the
// optional dependency.

#[cfg(feature = "clap-host")]
mod clap_backend;

#[cfg(feature = "clap-host")]
mod clap {
    use super::{HostError, InstrumentBox};
    use std::path::Path;

    pub(super) fn load(path: &Path, sample_rate: u32) -> Result<(InstrumentBox, String), HostError> {
        super::clap_backend::load(path, sample_rate)
    }
}

#[cfg(not(feature = "clap-host"))]
mod clap {
    //! CLAP backend stub. Compile with `--features clap-host` (the default)
    //! to enable real plugin loading via clack-host.

    use super::{HostError, InstrumentBox, StubInstrument};
    use std::path::Path;

    pub(super) fn load(path: &Path, sample_rate: u32) -> Result<(InstrumentBox, String), HostError> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown.clap")
            .to_string();
        tracing::info!(
            "CLAP backend disabled (feature `clap-host` off): producing silence for {}",
            name
        );
        let inst: InstrumentBox = Box::new(StubInstrument::new(name.clone(), "CLAP", sample_rate));
        Ok((inst, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn make_fake_plugin(dir: &Path, name: &str) {
        let p = dir.join(name);
        File::create(&p).expect("create fake plugin file");
    }

    fn temp_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("cadenza-host-test")
            .tempdir()
            .expect("tempdir")
    }

    #[test]
    fn matches_plugin_ext_recognises_vst3_and_clap() {
        assert!(matches_plugin_ext(Path::new("/p/foo.vst3")));
        assert!(matches_plugin_ext(Path::new("/p/foo.VST3")));
        assert!(matches_plugin_ext(Path::new("/p/foo.clap")));
        assert!(matches_plugin_ext(Path::new("/p/foo.CLAP")));
        assert!(!matches_plugin_ext(Path::new("/p/foo.dll")));
        assert!(!matches_plugin_ext(Path::new("/p/foo")));
    }

    #[test]
    fn scan_returns_only_plugin_files_sorted() {
        let host = PluginHost::new();
        let dir = temp_dir();
        make_fake_plugin(dir.path(), "alpha.vst3");
        make_fake_plugin(dir.path(), "beta.clap");
        make_fake_plugin(dir.path(), "ignored.txt");
        make_fake_plugin(dir.path(), "gamma.vst3");

        let found = host.scan(dir.path()).expect("scan");
        let names: Vec<String> = found
            .iter()
            .map(|p| Path::new(p).file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["alpha.vst3", "beta.clap", "gamma.vst3"]);
    }

    #[test]
    fn scan_returns_empty_for_dir_without_plugins() {
        let host = PluginHost::new();
        let dir = temp_dir();
        make_fake_plugin(dir.path(), "readme.txt");
        let found = host.scan(dir.path()).expect("scan");
        assert!(found.is_empty());
    }

    #[test]
    fn scan_errors_on_missing_dir() {
        let host = PluginHost::new();
        let result = host.scan(Path::new("/definitely/does/not/exist/cadenza-test"));
        assert!(matches!(result, Err(HostError::ScanFailed(_))));
    }

    // These tests use empty `.vst3` files because the VST3 backend is
    // still a stub that accepts any path. The CLAP backend now actually
    // dlopens the file (via clack-host's libloading), so a fake empty
    // `.clap` would correctly fail to load. CLAP coverage moved to
    // tests/clap_smoke.rs which uses the real bundled gain fixture.
    #[test]
    fn load_dispatches_by_extension_and_assigns_unique_ids() {
        let mut host = PluginHost::new();
        let dir = temp_dir();
        make_fake_plugin(dir.path(), "foo.vst3");
        make_fake_plugin(dir.path(), "bar.vst3");

        let foo_path = dir.path().join("foo.vst3");
        let bar_path = dir.path().join("bar.vst3");
        let a = host.load(foo_path.to_str().unwrap(), 48_000).expect("load vst3 a");
        let b = host.load(bar_path.to_str().unwrap(), 48_000).expect("load vst3 b");
        assert_eq!(a.id, 1);
        assert_eq!(b.id, 2);
        assert_eq!(a.name, "foo.vst3");
        assert_eq!(b.name, "bar.vst3");
    }

    #[test]
    fn load_rejects_unknown_extension() {
        let mut host = PluginHost::new();
        let dir = temp_dir();
        make_fake_plugin(dir.path(), "what.dll");
        let path = dir.path().join("what.dll");
        let result = host.load(path.to_str().unwrap(), 48_000);
        assert!(matches!(result, Err(HostError::UnsupportedFormat(_))));
    }

    #[test]
    fn load_rejects_missing_file() {
        let mut host = PluginHost::new();
        let result = host.load("/no/such/plugin.clap", 48_000);
        assert!(matches!(result, Err(HostError::NotFound(_))));
    }

    #[test]
    fn take_instrument_returns_some_then_none() {
        let mut host = PluginHost::new();
        let dir = temp_dir();
        make_fake_plugin(dir.path(), "x.vst3");
        let path = dir.path().join("x.vst3");
        let loaded = host.load(path.to_str().unwrap(), 48_000).unwrap();

        let first = host.take_instrument(loaded.id);
        assert!(first.is_some());
        let second = host.take_instrument(loaded.id);
        assert!(second.is_none());

        // Returning the instrument makes it available again.
        host.return_instrument(loaded.id, first.unwrap());
        assert!(host.take_instrument(loaded.id).is_some());
    }

    #[test]
    fn unload_removes_entry() {
        let mut host = PluginHost::new();
        let dir = temp_dir();
        make_fake_plugin(dir.path(), "x.vst3");
        let path = dir.path().join("x.vst3");
        let loaded = host.load(path.to_str().unwrap(), 48_000).unwrap();

        host.unload(loaded.id).expect("unload");
        assert!(host.unload(loaded.id).is_err());
    }
}
