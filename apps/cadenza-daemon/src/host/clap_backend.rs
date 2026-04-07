//! Real CLAP plugin hosting via [`clack-host`](https://github.com/prokopyl/clack).
//!
//! Compiled when the `clap-host` cargo feature is enabled (the default).
//!
//! ## Why a dedicated thread
//!
//! `clack_host::plugin::PluginInstance` is `!Send + !Sync` — the CLAP
//! contract pins the "main thread" identity for plugin lifecycle calls.
//! Cadenza's daemon, however, dispatches plugin loads from
//! `tokio::task::spawn_blocking`, whose worker pool can hop OS threads
//! between tasks, and the `PluginHost` is held behind an `Arc<Mutex<…>>`
//! that is freely moved between such workers. Storing a `PluginInstance`
//! directly in `PluginHost` would therefore violate `!Send`.
//!
//! The solution mirrors how `audio.rs` handles cpal's `!Send` `Stream`:
//! a dedicated, parked OS thread (`clap-main`) owns every `PluginInstance`
//! for its entire lifetime. The control side talks to it through a
//! `crossbeam_channel` of [`ClapCommand`]s. Only the
//! [`StartedPluginAudioProcessor`] — which IS `Send` — crosses back
//! over the channel and into the audio thread via the existing swap
//! ringbuf in [`crate::audio::AudioEngine`].
//!
//! ## Layout assumptions for v1
//!
//! This module currently hardcodes a 1-port stereo input + 1-port stereo
//! output configuration. That covers the nih-plug `gain` test fixture and
//! every common stereo effect / instrument plugin we care about for the
//! Phase 5b smoke test. Plugins with mono, multi-port, surround, or
//! input-less layouts will load and activate but may produce unexpected
//! audio. Querying the plugin's `audio_ports` extension to negotiate the
//! true layout is a Phase 5c task.
//!
//! ## Audio-thread guarantees
//!
//! [`ClapInstrument::render_with_events`] performs no allocation:
//! the input/output channel buffers, [`AudioPorts`], [`EventBuffer`], and
//! the muxing scratch buffer are all sized in the constructor against the
//! `MAX_FRAMES_PER_BUFFER` cap and reused on every callback. If cpal
//! ever hands us a buffer larger than that cap, we render only the prefix
//! that fits and zero the tail — same defensive policy as the I16/U16
//! cpal scratch path in [`crate::audio::build_stream`].

use crate::audio::AudioCmd;
use crate::instrument::{Instrument, InstrumentBox};

use super::HostError;

use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_host::events::{Pckn, io::EventBuffer};
use clack_host::plugin::PluginInstanceError;
use clack_host::prelude::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, HostHandlers, HostInfo, InputChannel,
    InputEvents, MainThreadHandler, OutputEvents, PluginAudioConfiguration, PluginEntry,
    PluginInstance, SharedHandler, StartedPluginAudioProcessor,
};

use crossbeam_channel::{Sender, unbounded};
use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;

/// Largest buffer the audio thread will ever ask us to render in one call.
/// 8192 frames at 48kHz is ~170ms — well above any cpal callback size we
/// see in practice and matches the `MAX_SCRATCH` upper bound used by
/// [`crate::audio::build_stream`].
const MAX_FRAMES_PER_BUFFER: usize = 8192;

/// Stereo I/O. See module-level "Layout assumptions for v1" docs.
const CHANNELS_PER_PORT: usize = 2;

/// How many CLAP events we can stage per callback. NoteOn/NoteOff dense
/// activity at musical tempos is well under this; the audio engine's own
/// pending queue caps at 512 too.
const EVENT_BUFFER_CAPACITY: usize = 512;

/// Identifier the `clap-main` thread uses to track a loaded plugin
/// independently of `cadenza-ipc::PluginId`. Decoupled so the host can
/// assign and free ids without round-tripping through the IPC layer.
type ClapMainId = u64;

/// Cadenza's CLAP host implementation. Intentionally minimal: no
/// extensions, no GUI, no params, no log routing. The plugin's audio
/// thread sees a `()` `AudioProcessor`; we drive it directly via the
/// returned [`StartedPluginAudioProcessor`].
struct CadenzaClapHost;

/// Per-plugin shared host data. We don't need any state here yet — every
/// callback the plugin might invoke is a no-op for the v1 backend — but
/// we keep the struct around so it's trivial to add fields later.
struct CadenzaClapShared;

/// Per-plugin main-thread host data. Empty for the same reason.
struct CadenzaClapMainThread;

impl<'a> SharedHandler<'a> for CadenzaClapShared {
    fn request_restart(&self) {
        // We don't support reloading plugins under the daemon today.
    }
    fn request_process(&self) {
        // The audio thread is always processing; nothing to wake.
    }
    fn request_callback(&self) {
        // No main-thread callback queue yet.
    }
}

impl<'a> MainThreadHandler<'a> for CadenzaClapMainThread {}

impl HostHandlers for CadenzaClapHost {
    type Shared<'a> = CadenzaClapShared;
    type MainThread<'a> = CadenzaClapMainThread;
    type AudioProcessor<'a> = ();
}

// ── clap-main thread command channel ────────────────────────────────────────

/// Reply payload for a successful [`ClapCommand::Load`]. The processor is
/// `Send` and crosses back to the control task to be wrapped in a
/// [`ClapInstrument`] and handed to the audio engine.
struct LoadOk {
    name:      String,
    processor: StartedPluginAudioProcessor<CadenzaClapHost>,
    main_id:   ClapMainId,
}

/// Commands the control side can send to `clap-main`. Replies travel back
/// over a per-command oneshot channel; we use `crossbeam_channel` bounded
/// to 1 instead of pulling in a separate oneshot dependency.
enum ClapCommand {
    Load {
        path:        PathBuf,
        sample_rate: u32,
        reply:       Sender<Result<LoadOk, HostError>>,
    },
    Unload {
        main_id: ClapMainId,
    },
}

/// Lazily-spawned handle to the `clap-main` thread. The first call to
/// [`load`] starts the thread; subsequent calls reuse it.
fn clap_main_sender() -> &'static Sender<ClapCommand> {
    static SENDER: OnceLock<Sender<ClapCommand>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = unbounded::<ClapCommand>();
        thread::Builder::new()
            .name("clap-main".into())
            .spawn(move || run_clap_main(rx))
            .expect("failed to spawn clap-main thread");
        tx
    })
}

/// Owns every `PluginInstance` for its entire lifetime. Receives commands
/// over the unbounded channel and parks waiting for the next one. The
/// thread runs forever — there is no shutdown signal in the daemon today
/// (the OS reaps the thread on process exit).
fn run_clap_main(rx: crossbeam_channel::Receiver<ClapCommand>) {
    let mut instances: HashMap<ClapMainId, PluginInstance<CadenzaClapHost>> = HashMap::new();
    let mut next_id: ClapMainId = 1;

    let host_info = match HostInfo::new("Cadenza", "Cadenza", "https://github.com/goedelsoup/cadenza", env!("CARGO_PKG_VERSION")) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("clap-main: failed to construct HostInfo: {e}");
            return;
        }
    };

    while let Ok(cmd) = rx.recv() {
        match cmd {
            ClapCommand::Load { path, sample_rate, reply } => {
                let result = load_on_main(&path, sample_rate, &host_info, &mut instances, &mut next_id);
                let _ = reply.send(result);
            }
            ClapCommand::Unload { main_id } => {
                // Drop the PluginInstance here on `clap-main`. The
                // associated audio processor — if it still exists — will
                // notice via the shared inner Arc and refuse further work.
                if let Some(_inst) = instances.remove(&main_id) {
                    tracing::debug!("clap-main: unloaded plugin id {main_id}");
                }
            }
        }
    }
}

/// Synchronous load logic that runs on `clap-main`. Returns a fully-
/// activated, started audio processor on success.
fn load_on_main(
    path:      &Path,
    sample_rate: u32,
    host_info: &HostInfo,
    instances: &mut HashMap<ClapMainId, PluginInstance<CadenzaClapHost>>,
    next_id:   &mut ClapMainId,
) -> Result<LoadOk, HostError> {
    // SAFETY: PluginEntry::load mmaps an external dynamic library and
    // executes its initialization code. The user explicitly chose this
    // file via the daemon's WebSocket API; we trust it the same way any
    // DAW trusts a user-selected plugin path.
    let entry = unsafe { PluginEntry::load(path) }
        .map_err(|e| HostError::LoadFailed(format!("clack PluginEntry::load: {e}")))?;

    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| HostError::LoadFailed("plugin entry has no plugin factory".into()))?;

    // Pick the first descriptor — multi-plugin bundles are out of scope
    // for v1 (the IPC `LoadPlugin` message carries only a path). The
    // plugin id and human-readable name are owned by the descriptor and
    // need to be cloned out before we drop the iterator.
    let (plugin_id_cstr, plugin_name) = {
        let mut descriptors = factory.plugin_descriptors();
        let first = descriptors
            .next()
            .ok_or_else(|| HostError::LoadFailed("plugin entry exposes zero plugins".into()))?;
        let id = first
            .id()
            .ok_or_else(|| HostError::LoadFailed("plugin descriptor has null id".into()))?;
        let name = first
            .name()
            .and_then(|n| n.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            });
        // Need an owned CString for PluginInstance::new — descriptor
        // borrows live for as long as the iterator does.
        (CString::new(id.to_bytes()).map_err(|e| HostError::LoadFailed(format!("plugin id had interior NUL: {e}")))?, name)
    };

    let mut instance = PluginInstance::<CadenzaClapHost>::new(
        |_| CadenzaClapShared,
        |_| CadenzaClapMainThread,
        &entry,
        plugin_id_cstr.as_c_str(),
        host_info,
    )
    .map_err(|e| HostError::LoadFailed(format!("PluginInstance::new: {e}")))?;

    let configuration = PluginAudioConfiguration {
        sample_rate:      sample_rate as f64,
        min_frames_count: 1,
        max_frames_count: MAX_FRAMES_PER_BUFFER as u32,
    };

    let stopped = instance
        .activate(|_, _| (), configuration)
        .map_err(|e| HostError::LoadFailed(format!("PluginInstance::activate: {e}")))?;

    let processor = stopped
        .start_processing()
        .map_err(|e| HostError::LoadFailed(format!("start_processing: {e}")))?;

    let main_id = *next_id;
    *next_id = next_id.checked_add(1).unwrap_or(1);
    instances.insert(main_id, instance);

    Ok(LoadOk { name: plugin_name, processor, main_id })
}

// ── Public load entry point ─────────────────────────────────────────────────

/// Typed loader: returns a concrete [`ClapInstrument`] alongside the
/// human-readable plugin name. Used directly by tests; the public
/// dispatch entry point [`load`] erases this to `InstrumentBox`.
fn load_typed(path: &Path, sample_rate: u32) -> Result<(ClapInstrument, String), HostError> {
    let (reply_tx, reply_rx) = unbounded::<Result<LoadOk, HostError>>();
    clap_main_sender()
        .send(ClapCommand::Load {
            path: path.to_path_buf(),
            sample_rate,
            reply: reply_tx,
        })
        .map_err(|e| HostError::LoadFailed(format!("clap-main channel send: {e}")))?;

    let LoadOk { name, processor, main_id } = reply_rx
        .recv()
        .map_err(|e| HostError::LoadFailed(format!("clap-main reply recv: {e}")))??;

    Ok((ClapInstrument::new(processor, main_id), name))
}

/// Load a `.clap` plugin from `path` and return an [`InstrumentBox`] the
/// audio engine can swap in. Blocks the calling thread on the round-trip
/// to `clap-main`; expected to be called from `tokio::task::spawn_blocking`.
pub(super) fn load(path: &Path, sample_rate: u32) -> Result<(InstrumentBox, String), HostError> {
    let (inst, name) = load_typed(path, sample_rate)?;
    Ok((Box::new(inst), name))
}

// ── Audio-thread instrument ─────────────────────────────────────────────────

/// `Instrument` impl backed by a started CLAP audio processor. Owns all
/// the scratch buffers process() needs so the audio thread never allocates.
pub(crate) struct ClapInstrument {
    processor:    StartedPluginAudioProcessor<CadenzaClapHost>,
    /// Identity on the `clap-main` thread; sent in an `Unload` command on
    /// `Drop` so the matching `PluginInstance` is freed.
    main_id:      ClapMainId,

    // Pre-allocated host-side audio buffers. CLAP works in de-interleaved
    // float channels; we allocate `MAX_FRAMES_PER_BUFFER * CHANNELS_PER_PORT`
    // floats per port and reslice each callback to the actual frame count.
    input_ports:  AudioPorts,
    output_ports: AudioPorts,
    /// Flat backing storage for the input port's `CHANNELS_PER_PORT`
    /// channels. Layout: `[ch0_frames..., ch1_frames...]`.
    input_storage:  Box<[f32]>,
    /// Flat backing storage for the output port. Same layout.
    output_storage: Box<[f32]>,

    /// Reusable note-event staging buffer. `clear()`ed at the top of every
    /// `render_with_events` and re-populated from the supplied event slice.
    event_buffer:   EventBuffer,

    /// Steady-time counter for `process()`. Required by some plugins for
    /// internal scheduling.
    steady_counter: u64,
}

// SAFETY: `StartedPluginAudioProcessor` is `Send + !Sync` (asserted in
// clack-host's own tests). We never share `&ClapInstrument` between threads;
// the audio thread takes exclusive ownership at swap-in time. The
// `AudioPorts` type is also explicitly `unsafe impl Send` upstream.
//
// `EventBuffer`, `Box<[f32]>`, and `u64` are trivially `Send`.
unsafe impl Send for ClapInstrument {}

impl ClapInstrument {
    fn new(
        processor: StartedPluginAudioProcessor<CadenzaClapHost>,
        main_id:   ClapMainId,
    ) -> Self {
        let total = MAX_FRAMES_PER_BUFFER * CHANNELS_PER_PORT;
        Self {
            processor,
            main_id,
            // 1 port, CHANNELS_PER_PORT channels each — these capacities
            // match what we hand `with_input_buffers` / `with_output_buffers`
            // every callback so reallocation never happens.
            input_ports:    AudioPorts::with_capacity(CHANNELS_PER_PORT, 1),
            output_ports:   AudioPorts::with_capacity(CHANNELS_PER_PORT, 1),
            input_storage:  vec![0.0f32; total].into_boxed_slice(),
            output_storage: vec![0.0f32; total].into_boxed_slice(),
            event_buffer:   EventBuffer::with_capacity(EVENT_BUFFER_CAPACITY),
            steady_counter: 0,
        }
    }

    /// Pre-allocated host-thread helper used by the in-module
    /// `clap-host-tests` smoke test to drive a single `process()` call
    /// without going through the audio engine. The `inputs` slice is
    /// treated as interleaved stereo and copied into the input port
    /// verbatim; the returned `Vec` is interleaved stereo output.
    #[cfg(all(test, feature = "clap-host-tests"))]
    pub(crate) fn process_block_for_test(
        &mut self,
        inputs: &[f32],
        frames: usize,
    ) -> Result<Vec<f32>, String> {
        if frames > MAX_FRAMES_PER_BUFFER {
            return Err(format!("frames {frames} exceeds MAX_FRAMES_PER_BUFFER {MAX_FRAMES_PER_BUFFER}"));
        }
        if inputs.len() != frames * CHANNELS_PER_PORT {
            return Err(format!("inputs.len() {} != frames * channels {}", inputs.len(), frames * CHANNELS_PER_PORT));
        }

        // De-interleave caller-provided input into our channel storage.
        for ch in 0..CHANNELS_PER_PORT {
            for f in 0..frames {
                self.input_storage[ch * MAX_FRAMES_PER_BUFFER + f] = inputs[f * CHANNELS_PER_PORT + ch];
            }
        }
        // Zero the output region we're about to write into.
        for ch in 0..CHANNELS_PER_PORT {
            for f in 0..frames {
                self.output_storage[ch * MAX_FRAMES_PER_BUFFER + f] = 0.0;
            }
        }

        let process_result = self.run_process(frames);
        process_result.map_err(|e| format!("process failed: {e}"))?;

        // Re-interleave output channels.
        let mut out = vec![0.0f32; frames * CHANNELS_PER_PORT];
        for ch in 0..CHANNELS_PER_PORT {
            for f in 0..frames {
                out[f * CHANNELS_PER_PORT + ch] = self.output_storage[ch * MAX_FRAMES_PER_BUFFER + f];
            }
        }
        Ok(out)
    }

    /// The shared `process()` driver used by both the audio-thread render
    /// path and the smoke-test helper. Wraps the pre-allocated channel
    /// storage in clack-host's port descriptors and runs one process call.
    /// Never allocates as long as `frames <= MAX_FRAMES_PER_BUFFER`.
    fn run_process(&mut self, frames: usize) -> Result<(), PluginInstanceError> {
        // Borrow each port's channel slices. We split the flat storage
        // into per-channel sub-slices via chunks_exact_mut, taking only
        // the first `frames` of each channel chunk.
        let mut in_iter = self
            .input_storage
            .chunks_exact_mut(MAX_FRAMES_PER_BUFFER)
            .map(|c| InputChannel::variable(&mut c[..frames]));
        let in_buffers = self.input_ports.with_input_buffers([AudioPortBuffer {
            latency:  0,
            channels: AudioPortBufferType::f32_input_only((&mut in_iter).take(CHANNELS_PER_PORT)),
        }]);

        let mut out_iter = self
            .output_storage
            .chunks_exact_mut(MAX_FRAMES_PER_BUFFER)
            .map(|c| &mut c[..frames]);
        let mut out_buffers = self.output_ports.with_output_buffers([AudioPortBuffer {
            latency:  0,
            channels: AudioPortBufferType::f32_output_only((&mut out_iter).take(CHANNELS_PER_PORT)),
        }]);

        let in_events = InputEvents::from_buffer(&self.event_buffer);
        let mut out_events_buf = EventBuffer::new();
        let mut out_events = OutputEvents::from_buffer(&mut out_events_buf);

        self.processor.process(
            &in_buffers,
            &mut out_buffers,
            &in_events,
            &mut out_events,
            Some(self.steady_counter),
            None,
        )?;

        self.steady_counter += frames as u64;
        Ok(())
    }
}

impl Instrument for ClapInstrument {
    fn note_on(&mut self, _pitch: u8, _velocity: u8) {
        // Note events arrive through `render_with_events` for sample-
        // accurate dispatch; this method is unused on the audio path.
    }
    fn note_off(&mut self, _pitch: u8) {}
    fn all_notes_off(&mut self) {
        // Best-effort: clear any staged events. The plugin will receive
        // a fresh empty buffer on its next `process()` and continue
        // ringing out any sustained voices internally — same behavior as
        // PolySynth, which also has no hard "all off" today.
        self.event_buffer.clear();
    }

    fn render_with_events(
        &mut self,
        out:       &mut [f32],
        channels:  usize,
        events:    &[(u32, AudioCmd)],
    ) {
        // Stage CLAP events from the audio engine's note slice. Push
        // happens against the pre-allocated `event_buffer`; we cap at
        // EVENT_BUFFER_CAPACITY to avoid any chance of growth.
        self.event_buffer.clear();
        for (offset, cmd) in events.iter().take(EVENT_BUFFER_CAPACITY) {
            match cmd {
                AudioCmd::NoteOn { pitch, velocity } => {
                    let ev = NoteOnEvent::new(
                        *offset,
                        Pckn::new(0u16, 0u16, u16::from(*pitch), u32::MAX),
                        f64::from(*velocity) / 127.0,
                    );
                    self.event_buffer.push(&ev);
                }
                AudioCmd::NoteOff { pitch } => {
                    let ev = NoteOffEvent::new(
                        *offset,
                        Pckn::new(0u16, 0u16, u16::from(*pitch), u32::MAX),
                        0.0,
                    );
                    self.event_buffer.push(&ev);
                }
                AudioCmd::AllNotesOff => {
                    // Express as note-off-everything. CLAP's "all sound
                    // off" is a per-channel meta-event; the cheapest
                    // portable approximation is to clear staged events.
                }
            }
        }

        // Compute frame count from the interleaved cpal buffer the engine
        // gave us. Cap to MAX_FRAMES_PER_BUFFER and zero the tail if
        // somehow exceeded — same defensive policy as `audio.rs`.
        let frames = (out.len() / channels.max(1)).min(MAX_FRAMES_PER_BUFFER);
        if frames == 0 {
            return;
        }

        // Plugin inputs are silence; the daemon doesn't have an upstream
        // audio source. Zero just the leading `frames` of each channel.
        for ch in 0..CHANNELS_PER_PORT {
            let base = ch * MAX_FRAMES_PER_BUFFER;
            for s in &mut self.input_storage[base..base + frames] {
                *s = 0.0;
            }
        }
        for ch in 0..CHANNELS_PER_PORT {
            let base = ch * MAX_FRAMES_PER_BUFFER;
            for s in &mut self.output_storage[base..base + frames] {
                *s = 0.0;
            }
        }

        if let Err(e) = self.run_process(frames) {
            // Allocation-free error reporting: tracing::warn! is not
            // strictly RT-safe but matches the rest of audio.rs and only
            // fires on plugin failure. Worst case is a single audio glitch.
            tracing::warn!("CLAP plugin process failed: {e}");
            // Output left as zeros from the prior fill.
        }

        // Mux de-interleaved plugin output back into the cpal buffer.
        // We support two cases:
        //   - channels >= 2: write the first two plugin channels to the
        //     first two cpal channels per frame, copy ch1 into any extras
        //   - channels == 1: down-mix to mono by averaging
        let out_frames = (out.len() / channels.max(1)).min(frames);
        if channels >= CHANNELS_PER_PORT {
            for (f, frame) in out
                .chunks_exact_mut(channels)
                .enumerate()
                .take(out_frames)
            {
                for (ch, slot) in frame.iter_mut().enumerate().take(CHANNELS_PER_PORT) {
                    *slot = self.output_storage[ch * MAX_FRAMES_PER_BUFFER + f];
                }
                // Replicate the rightmost CLAP channel into any extra
                // device channels (e.g. surround). Cheap and harmless.
                let last =
                    self.output_storage[(CHANNELS_PER_PORT - 1) * MAX_FRAMES_PER_BUFFER + f];
                for slot in frame.iter_mut().skip(CHANNELS_PER_PORT) {
                    *slot = last;
                }
            }
        } else {
            // Mono device: average the stereo plugin output.
            for (f, slot) in out.iter_mut().enumerate().take(out_frames) {
                let l = self.output_storage[f];
                let r = self.output_storage[MAX_FRAMES_PER_BUFFER + f];
                *slot = 0.5 * (l + r);
            }
        }

        // Zero any tail beyond what we processed (e.g. cpal handed us a
        // buffer larger than MAX_FRAMES_PER_BUFFER).
        for s in out[out_frames * channels..].iter_mut() {
            *s = 0.0;
        }
    }
}

impl Drop for ClapInstrument {
    fn drop(&mut self) {
        // Tell `clap-main` to release the matching PluginInstance. The
        // command channel is unbounded so this `send` does not block.
        // If the channel is closed (clap-main panicked) the leak is the
        // least of our problems.
        let _ = clap_main_sender().send(ClapCommand::Unload { main_id: self.main_id });
    }
}

// ── End-to-end smoke test against a committed CLAP plugin ───────────────────
//
// Loads `tests/fixtures/gain.clap` (the nih-plug `gain` example, built and
// committed by Phase 5b) through the *real* `load()` path and verifies the
// plugin loads, activates, starts processing, and produces output for a
// constant input. This covers the entire pipeline:
// PluginEntry::load → factory → PluginInstance::new → activate →
// start_processing → process → de-interleave / mux.
//
// Gated behind the `clap-host-tests` feature so that running
// `cargo test -p cadenza-daemon` on a machine without the fixture (or
// without permission to dlopen unsigned binaries) does not fail. Run with:
//   cargo test -p cadenza-daemon --features clap-host-tests
//
// Why "process" not "note response": gain is an *effect* — it multiplies
// stereo input by a gain control. It does not respond to NoteOn at all.
// We therefore feed a non-zero constant input and assert non-zero output.
// When we add an instrument fixture in a follow-up turn we'll add a
// sibling test that asserts NoteOn → audio.
#[cfg(feature = "clap-host-tests")]
#[cfg(test)]
mod smoke {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path() -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests");
        p.push("fixtures");
        p.push("gain.clap");
        p
    }

    #[test]
    fn gain_clap_loads_activates_and_processes() {
        let path = fixture_path();
        assert!(
            path.exists(),
            "test fixture missing at {}; rebuild with the README instructions",
            path.display()
        );

        // Drive the load through the typed helper so we can keep the
        // returned ClapInstrument concrete and call the white-box
        // `process_block_for_test`. The public `load()` (which erases
        // to `InstrumentBox`) is a one-line wrapper around this same
        // path, so we still cover the full clap-main thread round-trip
        // and the load_on_main → start_processing chain.
        let (mut clap_inst, name) = load_typed(&path, 48_000).expect("load_typed");
        assert_eq!(name, "Gain");

        // Feed a constant 0.5 stereo input across 256 frames.
        const FRAMES: usize = 256;
        let inputs = vec![0.5f32; FRAMES * 2];
        let output = clap_inst
            .process_block_for_test(&inputs, FRAMES)
            .expect("process_block_for_test");
        assert_eq!(output.len(), FRAMES * 2);

        // Sanity: every output sample is finite and bounded.
        for (i, s) in output.iter().enumerate() {
            assert!(s.is_finite(), "sample {i} = {s} is not finite");
            assert!(s.abs() <= 4.0, "sample {i} = {s} is suspiciously large");
        }
        // Pipeline is alive: at least one sample emerged non-zero.
        assert!(
            output.iter().any(|s| s.abs() > 1e-6),
            "expected gain to pass through some non-zero samples; entire output was silent"
        );

        // Re-render to verify processor state survives a second call.
        let output2 = clap_inst
            .process_block_for_test(&inputs, FRAMES)
            .expect("second process_block_for_test");
        assert!(output2.iter().any(|s| s.abs() > 1e-6));
    }
}
