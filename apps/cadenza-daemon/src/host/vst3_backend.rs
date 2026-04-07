//! Real VST3 plugin hosting via the [`vst3`](https://crates.io/crates/vst3)
//! crate (coupler-rs/vst3-rs). Compiled when the `vst3-host` cargo feature
//! is enabled (the default).
//!
//! ## Why a dedicated thread
//!
//! VST3 has a strict main-thread / audio-thread split per the spec:
//!
//! - `IPluginBase::initialize`, `IComponent::setActive`,
//!   `IComponent::activateBus`, `IAudioProcessor::setBusArrangements`,
//!   and `IAudioProcessor::setupProcessing` are documented as main-thread
//!   only. Calling them from arbitrary threads triggers undefined
//!   behavior in real-world plugins.
//!
//! - `IAudioProcessor::process` is the *only* call allowed on the audio
//!   thread, and only after `setProcessing(true)` has been issued (also
//!   from the main thread).
//!
//! Cadenza's daemon dispatches plugin loads from `tokio::task::spawn_blocking`
//! whose worker pool can hop OS threads between tasks. Storing a
//! `ComPtr<IComponent>` directly in `PluginHost` would let any subsequent
//! tokio-blocking task observe a plugin from a different OS thread than
//! the one that loaded it — a spec violation.
//!
//! The solution mirrors how `audio.rs` handles cpal's `!Send` `Stream`
//! and how `clap_backend.rs` handles clack-host's `!Send` `PluginInstance`:
//! a dedicated, parked OS thread (`vst3-main`) owns every loaded
//! `IComponent` for its entire lifetime. The control side talks to it
//! through a `crossbeam_channel` of [`Vst3Command`]s. Only the
//! `ComPtr<IAudioProcessor>` (which is `Send` because `vst3` declares
//! `unsafe impl Send for IAudioProcessor`) crosses back over the
//! channel and into the audio thread via the existing swap ringbuf.
//!
//! Note: vst3-rs's `ComPtr<I>` is itself `Send` whenever its inner
//! interface is, so the type system would let us share these freely.
//! The `vst3-main` thread is therefore *contractual* discipline, not a
//! borrow-checker requirement.
//!
//! ## Layout assumptions for v1
//!
//! Hardcoded 1-input + 1-output stereo audio bus configuration via
//! `setBusArrangements(&mut [kStereo], 1, &mut [kStereo], 1)`. Covers the
//! nih-plug `gain` test fixture and every common stereo effect /
//! instrument plugin we care about for the Phase 5b smoke test. Querying
//! the plugin's actual bus layout via `IComponent::getBusInfo` is
//! deferred to Phase 5c.
//!
//! ## What we *don't* implement (deferred to Phase 5c)
//!
//! - `IHostApplication` — we pass a null context to
//!   `IPluginBase::initialize`. nih-plug's `gain` example tolerates this;
//!   plugins that don't will fail at `initialize` and be reported as
//!   `LoadFailed`. Many commercial plugins require a real
//!   `IHostApplication` implementing `getName` and `createInstance(IMessage)`.
//! - `IComponentHandler` — host-side parameter automation callbacks. Not
//!   needed for load+process; required for plugins that report parameter
//!   changes upstream.
//! - `IConnectionPoint` between IComponent and IEditController — the
//!   "controller" side of the VST3 split. We don't currently load the
//!   IEditController at all, which means no parameter access and no GUI.
//!   Most simple effects work without it; some plugins (especially older
//!   commercial ones) refuse to instantiate the IComponent without it.
//! - VST3 events (note on/off via `IEventList`) — the v1
//!   `render_with_events` ignores incoming `AudioCmd` note events. The
//!   gain fixture is an effect with no event input, so this is fine for
//!   smoke-testing the audio pipeline. Adding NoteOn/NoteOff dispatch is
//!   straightforward but requires implementing an `IEventList`-shaped
//!   wrapper, which is more code than the smoke test motivates.
//! - Per-platform bundle entry points — currently macOS-only. Linux/Windows
//!   bundle layouts and entry symbols are flagged with `cfg(target_os)`
//!   blocks but only the macOS path is implemented end-to-end.
//!
//! ## Audio-thread guarantees
//!
//! [`Vst3Instrument::render_with_events`] performs no allocation. The
//! input/output channel `Vec<f32>` storage, the `[*mut f32; 2]` channel
//! pointer arrays, and the [`AudioBusBuffers`] structs are all allocated
//! once in the constructor. The cpal callback never touches the
//! allocator, never takes a lock, and never blocks.

#![allow(non_snake_case)] // VST3 binding names are camelCase per the C++ headers.

use crate::audio::AudioCmd;
use crate::instrument::{Instrument, InstrumentBox};

use super::HostError;

use crossbeam_channel::{Sender, unbounded};
use libloading::Library;
use std::collections::HashMap;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;

use vst3::{ComPtr, Interface, Steinberg::Vst::*, Steinberg::*};

/// Largest buffer the audio thread will ever ask us to render in one call.
/// Mirrors the CLAP backend cap so cpal sample-format scratch sizing in
/// `audio.rs::build_stream` is sufficient for both backends.
const MAX_FRAMES_PER_BUFFER: usize = 8192;

/// Stereo I/O. See module-level "Layout assumptions for v1" docs.
const CHANNELS_PER_PORT: usize = 2;

/// Identifier the `vst3-main` thread uses to track a loaded plugin
/// independently of `cadenza-ipc::PluginId`. Decoupled the same way as
/// the CLAP backend's `ClapMainId`.
type Vst3MainId = u64;

// ── ComPtr Send wrapper for the audio thread ────────────────────────────────

/// `ComPtr<IAudioProcessor>` is `Send` because vst3-rs declares
/// `unsafe impl Send for IAudioProcessor`. We re-state that here as a
/// type alias for clarity, since the underlying object's reference count
/// is the only shared state and COM `release()` is thread-safe.
type AudioProcessorPtr = ComPtr<IAudioProcessor>;

// ── vst3-main thread command channel ────────────────────────────────────────

/// Reply payload for a successful [`Vst3Command::Load`]. The audio
/// processor pointer is `Send` and crosses back to the control task to
/// be wrapped in a [`Vst3Instrument`] and handed to the audio engine.
struct LoadOk {
    name:      String,
    processor: AudioProcessorPtr,
    main_id:   Vst3MainId,
}

/// Per-plugin lifecycle state retained on the `vst3-main` thread.
/// `library` is held to keep the dlopen'd binary alive — dropping it
/// would unload the bundle and invalidate every COM pointer derived from
/// it. `component` owns the IComponent reference, which is queried for
/// IAudioProcessor at load time.
struct LoadedPlugin {
    /// Keeps the bundle's dynamic library mapped for as long as the
    /// component lives. Dropped *after* the component (drop order is
    /// declaration order in Rust).
    component: ComPtr<IComponent>,
    #[allow(dead_code)]
    library:   Library,
}

enum Vst3Command {
    Load {
        path:        PathBuf,
        sample_rate: u32,
        reply:       Sender<Result<LoadOk, HostError>>,
    },
    Unload {
        main_id: Vst3MainId,
    },
}

fn vst3_main_sender() -> &'static Sender<Vst3Command> {
    static SENDER: OnceLock<Sender<Vst3Command>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = unbounded::<Vst3Command>();
        thread::Builder::new()
            .name("vst3-main".into())
            .spawn(move || run_vst3_main(rx))
            .expect("failed to spawn vst3-main thread");
        tx
    })
}

fn run_vst3_main(rx: crossbeam_channel::Receiver<Vst3Command>) {
    let mut plugins: HashMap<Vst3MainId, LoadedPlugin> = HashMap::new();
    let mut next_id: Vst3MainId = 1;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Vst3Command::Load { path, sample_rate, reply } => {
                let result = load_on_main(&path, sample_rate, &mut plugins, &mut next_id);
                let _ = reply.send(result);
            }
            Vst3Command::Unload { main_id } => {
                if let Some(plugin) = plugins.remove(&main_id) {
                    // SAFETY: every call below is on the main thread per
                    // the VST3 spec; the IComponent we own here was
                    // obtained on this same thread via load_on_main.
                    unsafe {
                        // setProcessing(false) is technically supposed to
                        // come from the audio thread, but the VST3 spec
                        // also says it must be called before setActive(false),
                        // and most plugins accept it from the main thread
                        // here at teardown when no audio thread will touch
                        // the processor again. Best-effort.
                        if let Some(processor) = plugin.component.cast::<IAudioProcessor>() {
                            let _ = processor.setProcessing(0);
                        }
                        let _ = plugin.component.setActive(0);
                        let _ = plugin.component.terminate();
                    }
                    // Dropping `plugin` here releases the IComponent
                    // ComPtr, which decrements its refcount to zero (the
                    // audio thread's processor ComPtr was already dropped
                    // before the Unload command was sent). Then drops the
                    // library, which unmaps the bundle.
                    drop(plugin);
                    tracing::debug!("vst3-main: unloaded plugin id {main_id}");
                }
            }
        }
    }
}

// ── Bundle loading per-platform ─────────────────────────────────────────────

/// Resolves the platform-specific path to the executable inside a `.vst3`
/// bundle. Currently macOS-only; Linux/Windows return an error so the
/// failure is explicit rather than silently producing silence.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
fn resolve_bundle_binary(bundle_path: &Path) -> Result<PathBuf, HostError> {
    #[cfg(target_os = "macos")]
    {
        // macOS VST3 bundles are directories: Foo.vst3/Contents/MacOS/Foo.
        // The binary's filename is the bundle name without the .vst3
        // extension.
        let stem = bundle_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| HostError::LoadFailed(format!("bundle path has no stem: {}", bundle_path.display())))?;
        let binary = bundle_path.join("Contents").join("MacOS").join(stem);
        if !binary.exists() {
            return Err(HostError::LoadFailed(format!(
                "bundle binary missing at {}",
                binary.display()
            )));
        }
        Ok(binary)
    }
    #[cfg(target_os = "linux")]
    {
        // Linux VST3 bundles: Foo.vst3/Contents/x86_64-linux/Foo.so
        // Not implemented for v1 — the smoke fixture is macOS-only.
        Err(HostError::LoadFailed(
            "VST3 bundle loading on Linux not implemented in Phase 5b v1".into(),
        ))
    }
    #[cfg(target_os = "windows")]
    {
        // Windows VST3 bundles: Foo.vst3/Contents/x86_64-win/Foo.vst3
        // Not implemented for v1.
        Err(HostError::LoadFailed(
            "VST3 bundle loading on Windows not implemented in Phase 5b v1".into(),
        ))
    }
}

/// The signature of `GetPluginFactory` exported by every VST3 binary.
type GetPluginFactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;

/// The signature of the per-platform module entry callback.
/// `bundleExit` exists too but we never call it — the v1 backend keeps
/// every loaded library mapped for the life of the process (the Library
/// handle is owned by `LoadedPlugin` and dropped on Unload, which
/// implicitly tears down the bundle).
#[cfg(target_os = "macos")]
type ModuleEntryFn = unsafe extern "system" fn(*mut std::ffi::c_void) -> bool;

#[cfg(target_os = "macos")]
const MODULE_ENTRY_SYM: &[u8] = b"bundleEntry";

// ── Synchronous load logic running on vst3-main ─────────────────────────────

// VST3 enum constants are exposed by the `vst3` crate as different
// integer widths per platform (some targets use `u32`, others `i32`).
// The `as i32` casts below are required on macOS but become no-ops on
// Windows; suppressing the unnecessary_cast lint there keeps the code
// portable without per-call cfg gates.
#[cfg_attr(not(target_os = "macos"), allow(clippy::unnecessary_cast))]
fn load_on_main(
    path:        &Path,
    sample_rate: u32,
    plugins:     &mut HashMap<Vst3MainId, LoadedPlugin>,
    next_id:     &mut Vst3MainId,
) -> Result<LoadOk, HostError> {
    let binary_path = resolve_bundle_binary(path)?;

    // SAFETY: dlopen executes initialization code from a user-selected
    // dynamic library. We trust the user the same way any DAW trusts a
    // user-selected plugin path.
    let library = unsafe { Library::new(&binary_path) }
        .map_err(|e| HostError::LoadFailed(format!("dlopen {}: {e}", binary_path.display())))?;

    // Call the bundle's entry point so the plugin can perform any
    // one-time initialization. Many simple plugins don't need it but
    // calling it is required by the VST3 spec. Only macOS is wired up
    // in v1 — `resolve_bundle_binary` already errors out on Linux/Windows
    // before we get here, so this block is unreachable on those targets.
    #[cfg(target_os = "macos")]
    unsafe {
        if let Ok(entry) = library.get::<ModuleEntryFn>(MODULE_ENTRY_SYM) {
            // Argument is a CFBundleRef on macOS; passing null is the
            // documented "I don't have one" sentinel. nih-plug's gain
            // accepts null; some plugins require a real bundle handle.
            entry(std::ptr::null_mut());
        }
    }

    // Look up GetPluginFactory and call it.
    let factory_raw: *mut IPluginFactory = unsafe {
        let get_factory = library
            .get::<GetPluginFactoryFn>(b"GetPluginFactory")
            .map_err(|e| HostError::LoadFailed(format!("GetPluginFactory missing: {e}")))?;
        get_factory()
    };
    if factory_raw.is_null() {
        return Err(HostError::LoadFailed("GetPluginFactory returned null".into()));
    }
    // SAFETY: GetPluginFactory follows the VST3 SDK convention of
    // returning a new reference owned by the caller; from_raw_unchecked
    // takes ownership and Drop will release it.
    let factory: ComPtr<IPluginFactory> = unsafe { ComPtr::from_raw_unchecked(factory_raw) };

    // Walk the factory's classes looking for the first audio module class.
    let class_count = unsafe { factory.countClasses() };
    if class_count <= 0 {
        return Err(HostError::LoadFailed(format!(
            "factory exposes {class_count} classes"
        )));
    }

    let mut chosen: Option<(TUID, String)> = None;
    for index in 0..class_count {
        let mut info = PClassInfo {
            cid:         [0; 16],
            cardinality: 0,
            category:    [0; 32],
            name:        [0; 64],
        };
        let result = unsafe { factory.getClassInfo(index, &mut info) };
        if result != kResultOk {
            continue;
        }
        // Category is a fixed-size [c_char; 32] holding a NUL-terminated
        // ASCII string. The VST3 audio category constant is "Audio Module Class".
        let category = unsafe { CStr::from_ptr(info.category.as_ptr()) }
            .to_str()
            .unwrap_or("");
        if category != "Audio Module Class" {
            continue;
        }
        let name = unsafe { CStr::from_ptr(info.name.as_ptr()) }
            .to_str()
            .unwrap_or("unknown")
            .to_string();
        chosen = Some((info.cid, name));
        break;
    }

    let (cid, plugin_name) = chosen
        .ok_or_else(|| HostError::LoadFailed("no Audio Module Class in factory".into()))?;

    // Instantiate the chosen class as IComponent. createInstance returns
    // a new reference; from_raw takes ownership.
    let component: ComPtr<IComponent> = unsafe {
        let mut obj: *mut std::ffi::c_void = std::ptr::null_mut();
        let r = factory.createInstance(
            cid.as_ptr() as FIDString,
            IComponent::IID.as_ptr() as FIDString,
            &mut obj,
        );
        if r != kResultOk || obj.is_null() {
            return Err(HostError::LoadFailed(format!(
                "createInstance(IComponent) failed: tresult={r}"
            )));
        }
        ComPtr::from_raw_unchecked(obj as *mut IComponent)
    };

    // Initialize with a null host context. nih-plug accepts this; many
    // commercial plugins require a real IHostApplication. See module
    // doc comments for the deferred Phase 5c work.
    unsafe {
        let r = component.initialize(std::ptr::null_mut());
        if r != kResultOk {
            return Err(HostError::LoadFailed(format!(
                "IComponent::initialize failed: tresult={r}"
            )));
        }
    }

    // Query the same object as IAudioProcessor — VST3 components must
    // expose both interfaces simultaneously per the spec.
    let processor: AudioProcessorPtr = component
        .cast::<IAudioProcessor>()
        .ok_or_else(|| HostError::LoadFailed("component does not expose IAudioProcessor".into()))?;

    // Negotiate stereo I/O on the single input + output bus. Plugins
    // that don't support stereo will reject this; v1 documentation says
    // we don't try to be smart about layout.
    unsafe {
        let mut input_arr:  [SpeakerArrangement; 1] = [SpeakerArr::kStereo];
        let mut output_arr: [SpeakerArrangement; 1] = [SpeakerArr::kStereo];
        let r = processor.setBusArrangements(
            input_arr.as_mut_ptr(),
            1,
            output_arr.as_mut_ptr(),
            1,
        );
        if r != kResultOk {
            return Err(HostError::LoadFailed(format!(
                "setBusArrangements stereo,stereo failed: tresult={r}"
            )));
        }
    }

    // setupProcessing: Realtime mode, 32-bit float, our fixed max block.
    unsafe {
        let mut setup = ProcessSetup {
            processMode:        ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: MAX_FRAMES_PER_BUFFER as i32,
            sampleRate:         sample_rate as f64,
        };
        let r = processor.setupProcessing(&mut setup);
        if r != kResultOk {
            return Err(HostError::LoadFailed(format!(
                "setupProcessing failed: tresult={r}"
            )));
        }
    }

    // Activate the input + output audio buses. VST3 buses default to
    // inactive; the plugin will produce silence (or refuse to process)
    // if the buses aren't explicitly activated.
    unsafe {
        let r_in = component.activateBus(
            MediaTypes_::kAudio as i32,
            BusDirections_::kInput as i32,
            0,
            1,
        );
        if r_in != kResultOk {
            return Err(HostError::LoadFailed(format!(
                "activateBus(input 0) failed: tresult={r_in}"
            )));
        }
        let r_out = component.activateBus(
            MediaTypes_::kAudio as i32,
            BusDirections_::kOutput as i32,
            0,
            1,
        );
        if r_out != kResultOk {
            return Err(HostError::LoadFailed(format!(
                "activateBus(output 0) failed: tresult={r_out}"
            )));
        }
    }

    // setActive(true) — moves the component to the "processing-ready"
    // state. After this point setupProcessing cannot be called again.
    unsafe {
        let r = component.setActive(1);
        if r != kResultOk {
            return Err(HostError::LoadFailed(format!(
                "setActive(true) failed: tresult={r}"
            )));
        }
    }

    // Final transition into processing mode. The audio thread can now
    // call processor.process() until we issue setProcessing(false).
    unsafe {
        let r = processor.setProcessing(1);
        if r != kResultOk {
            return Err(HostError::LoadFailed(format!(
                "setProcessing(true) failed: tresult={r}"
            )));
        }
    }

    let main_id = *next_id;
    *next_id = next_id.checked_add(1).unwrap_or(1);
    plugins.insert(main_id, LoadedPlugin { component, library });

    Ok(LoadOk { name: plugin_name, processor, main_id })
}

// ── Public load entry points ────────────────────────────────────────────────

/// Typed loader: returns a concrete [`Vst3Instrument`] alongside the
/// human-readable plugin name. Used directly by the smoke test; the
/// public dispatch entry point [`load`] erases this to `InstrumentBox`.
fn load_typed(path: &Path, sample_rate: u32) -> Result<(Vst3Instrument, String), HostError> {
    let (reply_tx, reply_rx) = unbounded::<Result<LoadOk, HostError>>();
    vst3_main_sender()
        .send(Vst3Command::Load {
            path: path.to_path_buf(),
            sample_rate,
            reply: reply_tx,
        })
        .map_err(|e| HostError::LoadFailed(format!("vst3-main channel send: {e}")))?;

    let LoadOk { name, processor, main_id } = reply_rx
        .recv()
        .map_err(|e| HostError::LoadFailed(format!("vst3-main reply recv: {e}")))??;

    Ok((Vst3Instrument::new(processor, main_id), name))
}

/// Load a `.vst3` plugin from `path` and return an [`InstrumentBox`] the
/// audio engine can swap in. Blocks the calling thread on the round-trip
/// to `vst3-main`; expected to be called from `tokio::task::spawn_blocking`.
pub(super) fn load(path: &Path, sample_rate: u32) -> Result<(InstrumentBox, String), HostError> {
    let (inst, name) = load_typed(path, sample_rate)?;
    Ok((Box::new(inst), name))
}

// ── Audio-thread instrument ─────────────────────────────────────────────────

pub(crate) struct Vst3Instrument {
    processor: AudioProcessorPtr,
    /// Identity on the `vst3-main` thread; sent in an `Unload` command on
    /// `Drop` so the matching IComponent is finalized and its library
    /// unmapped.
    main_id:   Vst3MainId,

    /// De-interleaved channel storage for the input bus. `Box<[f32]>`
    /// allocated once. Each channel occupies `MAX_FRAMES_PER_BUFFER`
    /// floats laid out contiguously: `[ch0..., ch1...]`.
    input_storage:  Box<[f32]>,
    /// De-interleaved channel storage for the output bus.
    output_storage: Box<[f32]>,
    /// Per-channel pointer arrays VST3 expects in `AudioBusBuffers`.
    /// Updated each callback to point at the current frame slice base
    /// inside `*_storage`. Boxed so the pointers are stable across moves.
    input_channel_ptrs:  Box<[*mut f32]>,
    output_channel_ptrs: Box<[*mut f32]>,

    /// One input AudioBusBuffers (1 bus). Updated each callback.
    input_bus:  AudioBusBuffers,
    /// One output AudioBusBuffers (1 bus). Updated each callback.
    output_bus: AudioBusBuffers,
}

// SAFETY: Vst3Instrument owns ComPtr<IAudioProcessor>, which is `Send`
// per vst3-rs (`unsafe impl Send for IAudioProcessor`). The remaining
// fields are plain owned bytes. The audio thread takes exclusive
// ownership at swap-in time; we never share `&Vst3Instrument` between
// threads. The internal pointer arrays point into our own boxed storage,
// not into shared memory.
unsafe impl Send for Vst3Instrument {}

impl Vst3Instrument {
    fn new(processor: AudioProcessorPtr, main_id: Vst3MainId) -> Self {
        let total = MAX_FRAMES_PER_BUFFER * CHANNELS_PER_PORT;
        let input_storage  = vec![0.0f32; total].into_boxed_slice();
        let output_storage = vec![0.0f32; total].into_boxed_slice();
        let input_channel_ptrs:  Box<[*mut f32]> = vec![std::ptr::null_mut(); CHANNELS_PER_PORT].into_boxed_slice();
        let output_channel_ptrs: Box<[*mut f32]> = vec![std::ptr::null_mut(); CHANNELS_PER_PORT].into_boxed_slice();

        // Wire the channel pointer arrays at construction so they're
        // stable for the life of the instrument; render_with_events only
        // touches the f32 storage they reference, never the pointer arrays.
        let mut me = Self {
            processor,
            main_id,
            input_storage,
            output_storage,
            input_channel_ptrs,
            output_channel_ptrs,
            input_bus: AudioBusBuffers {
                numChannels:  CHANNELS_PER_PORT as i32,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: std::ptr::null_mut(),
                },
            },
            output_bus: AudioBusBuffers {
                numChannels:  CHANNELS_PER_PORT as i32,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: std::ptr::null_mut(),
                },
            },
        };
        // Populate the channel pointer arrays now that the boxes are in
        // their final memory location, then point the bus structs at them.
        for ch in 0..CHANNELS_PER_PORT {
            me.input_channel_ptrs[ch]  = unsafe { me.input_storage.as_mut_ptr().add(ch * MAX_FRAMES_PER_BUFFER) };
            me.output_channel_ptrs[ch] = unsafe { me.output_storage.as_mut_ptr().add(ch * MAX_FRAMES_PER_BUFFER) };
        }
        me.input_bus.__field0.channelBuffers32  = me.input_channel_ptrs.as_mut_ptr();
        me.output_bus.__field0.channelBuffers32 = me.output_channel_ptrs.as_mut_ptr();
        me
    }

    /// White-box helper used by the `vst3-host-tests` smoke test. Feeds
    /// an interleaved-stereo input through `process()` and returns the
    /// interleaved-stereo output. Allocates only the return `Vec`; the
    /// process() call itself is RT-safe.
    #[cfg(all(test, feature = "vst3-host-tests"))]
    pub(crate) fn process_block_for_test(
        &mut self,
        inputs: &[f32],
        frames: usize,
    ) -> Result<Vec<f32>, String> {
        if frames > MAX_FRAMES_PER_BUFFER {
            return Err(format!("frames {frames} > MAX_FRAMES_PER_BUFFER {MAX_FRAMES_PER_BUFFER}"));
        }
        if inputs.len() != frames * CHANNELS_PER_PORT {
            return Err(format!("inputs.len() {} != frames * channels {}", inputs.len(), frames * CHANNELS_PER_PORT));
        }
        // De-interleave the test input into our channel storage.
        for ch in 0..CHANNELS_PER_PORT {
            for f in 0..frames {
                self.input_storage[ch * MAX_FRAMES_PER_BUFFER + f] = inputs[f * CHANNELS_PER_PORT + ch];
            }
        }
        // Zero the leading `frames` of the output region.
        for ch in 0..CHANNELS_PER_PORT {
            for f in 0..frames {
                self.output_storage[ch * MAX_FRAMES_PER_BUFFER + f] = 0.0;
            }
        }
        self.run_process(frames)?;
        let mut out = vec![0.0f32; frames * CHANNELS_PER_PORT];
        for ch in 0..CHANNELS_PER_PORT {
            for f in 0..frames {
                out[f * CHANNELS_PER_PORT + ch] = self.output_storage[ch * MAX_FRAMES_PER_BUFFER + f];
            }
        }
        Ok(out)
    }

    /// Build a `ProcessData` against the pre-allocated bus structs and
    /// invoke the plugin's `process()`. Returns a stringified error on
    /// non-OK tresult — the smoke test surfaces it; the audio path
    /// downgrades it to a `tracing::warn` and silence.
    // See note on `load_on_main` re: VST3 enum cast portability.
    #[cfg_attr(not(target_os = "macos"), allow(clippy::unnecessary_cast))]
    fn run_process(&mut self, frames: usize) -> Result<(), String> {
        let mut data = ProcessData {
            processMode:        ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples:         frames as i32,
            numInputs:          1,
            numOutputs:         1,
            inputs:             &mut self.input_bus,
            outputs:            &mut self.output_bus,
            inputParameterChanges:  std::ptr::null_mut(),
            outputParameterChanges: std::ptr::null_mut(),
            inputEvents:            std::ptr::null_mut(),
            outputEvents:           std::ptr::null_mut(),
            processContext:         std::ptr::null_mut(),
        };
        // SAFETY: data references our boxed storage which lives for the
        // duration of this call; the plugin contract says process() must
        // not retain pointers from ProcessData past the call's return.
        let r = unsafe { self.processor.process(&mut data) };
        if r != kResultOk {
            return Err(format!("IAudioProcessor::process tresult={r}"));
        }
        Ok(())
    }
}

impl Instrument for Vst3Instrument {
    fn note_on(&mut self, _pitch: u8, _velocity: u8) {}
    fn note_off(&mut self, _pitch: u8) {}
    fn all_notes_off(&mut self) {
        // No-op for v1 — VST3 events would require an IEventList wrapper.
        // Effects (like the gain fixture) ignore note events anyway.
    }

    fn render_with_events(
        &mut self,
        out:      &mut [f32],
        channels: usize,
        _events:  &[(u32, AudioCmd)],
    ) {
        // v1 ignores incoming AudioCmd events. See module docs for the
        // IEventList work needed to wire NoteOn/NoteOff through.
        let frames = (out.len() / channels.max(1)).min(MAX_FRAMES_PER_BUFFER);
        if frames == 0 {
            return;
        }

        // Zero the leading `frames` of each input channel (no upstream
        // audio source) and the matching output region.
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
            tracing::warn!("VST3 plugin process failed: {e}");
            // Output left as zeros from the prior fill.
        }

        // Mux the de-interleaved plugin output back into the interleaved
        // cpal buffer. Same shape as ClapInstrument::render_with_events.
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
                let last =
                    self.output_storage[(CHANNELS_PER_PORT - 1) * MAX_FRAMES_PER_BUFFER + f];
                for slot in frame.iter_mut().skip(CHANNELS_PER_PORT) {
                    *slot = last;
                }
            }
        } else {
            for (f, slot) in out.iter_mut().enumerate().take(out_frames) {
                let l = self.output_storage[f];
                let r = self.output_storage[MAX_FRAMES_PER_BUFFER + f];
                *slot = 0.5 * (l + r);
            }
        }

        // Zero the tail beyond what we processed.
        for s in out[out_frames * channels..].iter_mut() {
            *s = 0.0;
        }
    }
}

impl Drop for Vst3Instrument {
    fn drop(&mut self) {
        // Tell `vst3-main` to release the matching IComponent. The
        // command channel is unbounded so this `send` does not block.
        let _ = vst3_main_sender().send(Vst3Command::Unload { main_id: self.main_id });
    }
}

// ── End-to-end smoke test against a committed VST3 plugin ───────────────────
//
// Loads `tests/fixtures/gain.vst3` (the nih-plug `gain` example, built
// and committed by Phase 5b) through the *real* `load()` path and
// verifies the plugin loads, instantiates, sets up processing, and
// produces output for a constant input. This covers the entire pipeline:
// dlopen bundle binary → bundleEntry → GetPluginFactory → enumerate
// classes → createInstance(IComponent) → query IAudioProcessor →
// initialize → setBusArrangements → setupProcessing → activateBus ×2 →
// setActive → setProcessing → process.
//
// Gated behind the `vst3-host-tests` feature so that running
// `cargo test -p cadenza-daemon` on a machine without the fixture (or
// without permission to dlopen unsigned binaries) does not fail. Run with:
//   cargo test -p cadenza-daemon --features vst3-host-tests
//
// Why "process" not "note response": the gain fixture is a stereo *effect*
// — it multiplies stereo input by a gain control. It does not respond to
// VST3 events at all (and our v1 backend ignores incoming AudioCmd events
// anyway). We feed a constant 0.5 stereo input and assert non-zero output.
#[cfg(feature = "vst3-host-tests")]
#[cfg(test)]
mod smoke {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path() -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests");
        p.push("fixtures");
        p.push("gain.vst3");
        p
    }

    #[test]
    fn gain_vst3_loads_setups_and_processes() {
        let path = fixture_path();
        assert!(
            path.exists(),
            "test fixture missing at {}; rebuild with the README instructions",
            path.display()
        );

        // Drive the load through the typed helper so we can keep the
        // returned Vst3Instrument concrete and call the white-box
        // `process_block_for_test`. The public `load()` is a one-line
        // wrapper around this same path.
        let (mut inst, name) = load_typed(&path, 48_000).expect("load_typed");
        // nih-plug's gain example has class name "Gain" — same as the
        // CLAP variant. Asserting catches accidental fixture swaps.
        assert_eq!(name, "Gain");

        // Feed a constant 0.5 stereo input across 256 frames.
        const FRAMES: usize = 256;
        let inputs = vec![0.5f32; FRAMES * 2];
        let output = inst
            .process_block_for_test(&inputs, FRAMES)
            .expect("process_block_for_test");
        assert_eq!(output.len(), FRAMES * 2);

        for (i, s) in output.iter().enumerate() {
            assert!(s.is_finite(), "sample {i} = {s} is not finite");
            assert!(s.abs() <= 4.0, "sample {i} = {s} is suspiciously large");
        }
        assert!(
            output.iter().any(|s| s.abs() > 1e-6),
            "expected gain to pass through some non-zero samples; entire output was silent"
        );

        // Re-render to verify processor state survives a second call.
        let output2 = inst
            .process_block_for_test(&inputs, FRAMES)
            .expect("second process_block_for_test");
        assert!(output2.iter().any(|s| s.abs() > 1e-6));
    }
}
