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
//! - Plugin GUIs (`IPlugView` from `IEditController::createView`) — we
//!   load the controller for parameter discovery and automation but
//!   never instantiate its UI. Out of scope for v1.
//! - `IComponentHandler` callbacks for plugin-initiated parameter changes
//!   (the controller's "I changed a value, please pass it on" notification).
//!   Without this, plugin internal state changes won't propagate back to
//!   the host. Out of scope for v1.
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

use cadenza_ipc::PluginParam;
use crossbeam_channel::{Sender, unbounded};
use libloading::Library;
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::thread;

use vst3::{Class, ComPtr, ComWrapper, Interface, Steinberg::Vst::*, Steinberg::*};

/// Largest buffer the audio thread will ever ask us to render in one call.
/// Mirrors the CLAP backend cap so cpal sample-format scratch sizing in
/// `audio.rs::build_stream` is sufficient for both backends.
const MAX_FRAMES_PER_BUFFER: usize = 8192;

/// Stereo I/O. See module-level "Layout assumptions for v1" docs.
const CHANNELS_PER_PORT: usize = 2;

/// Cap on the number of parameters we'll enumerate from a single plugin.
/// Mirrors `clap_backend::MAX_PARAMS_PER_PLUGIN` so the two backends behave
/// the same in pathological cases.
const MAX_PARAMS_PER_PLUGIN: u32 = 4096;

/// Maximum number of distinct parameter queues that can be staged in a
/// single `process()` call. Practical plugin automation never needs more
/// than a handful of params changing per block; 64 leaves comfortable
/// headroom while keeping the per-instrument footprint bounded.
const VST3_MAX_PARAM_QUEUES: usize = 64;

/// Maximum number of `(sampleOffset, value)` points per parameter queue
/// in a single block. The audio engine's pending event queue caps at 512
/// total events; in the worst case those could all target one parameter,
/// but realistic automation scatters across params at a much lower density
/// than 64 per block per param. Sized to keep the per-queue array small.
const VST3_MAX_POINTS_PER_QUEUE: usize = 64;

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
    /// Parameter list discovered from the plugin's `IEditController` on
    /// the `vst3-main` thread before the audio processor was started.
    /// Mirrors `clap_backend::LoadOk::params` so the wire shape matches.
    params:    Vec<PluginParam>,
}

/// Per-plugin lifecycle state retained on the `vst3-main` thread.
/// `library` is held to keep the dlopen'd binary alive — dropping it
/// would unload the bundle and invalidate every COM pointer derived from
/// it. `component` owns the IComponent reference, which is queried for
/// IAudioProcessor at load time. `edit_controller` owns the
/// `IEditController` reference (separate object for dual-component plugins,
/// the same object as `component` for single-component plugins).
struct LoadedPlugin {
    /// Keeps the bundle's dynamic library mapped for as long as the
    /// component lives. Dropped *after* the component and controller
    /// (drop order is reverse declaration order, so order matters).
    edit_controller: Option<ComPtr<IEditController>>,
    /// `true` when `edit_controller` was created via a separate
    /// `factory.createInstance` call (dual-component plugin). `false`
    /// when the IComponent itself implements IEditController and we
    /// just cast to it. Determines whether `terminate()` should be
    /// called on the controller separately at unload.
    controller_is_separate: bool,
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

                        // Terminate the edit controller before the
                        // component when it's a separate object (dual-
                        // component plugins). For single-component plugins
                        // the controller is the same instance as the
                        // component and `terminate()` would be a
                        // double-call; skip in that case.
                        if plugin.controller_is_separate {
                            if let Some(ref controller) = plugin.edit_controller {
                                let _ = controller.terminate();
                            }
                        }
                        let _ = plugin.component.terminate();
                    }
                    // Dropping `plugin` here releases the controller and
                    // component ComPtrs (decrementing their refcounts —
                    // the audio thread's processor ComPtr was already
                    // dropped before the Unload command was sent). Then
                    // drops the library, which unmaps the bundle.
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

    // Resolve the IEditController. Two cases per the VST3 spec:
    //
    //   1. Single-component plugin: the IComponent itself implements
    //      IEditController. A simple `cast::<IEditController>()` succeeds
    //      and we reuse the existing IPluginBase initialization.
    //
    //   2. Dual-component plugin: the IComponent reports a separate
    //      controller class id via `getControllerClassId()`, and we
    //      instantiate it through the same factory and initialize it
    //      independently. nih-plug-based plugins (including the gain
    //      fixture) follow this path.
    //
    // Either case is allowed to fail gracefully — the rest of the load
    // continues without param discovery, matching how `clap_backend.rs`
    // treats a plugin with no `params` extension. The audio path works
    // either way; only parameter automation is affected.
    let (edit_controller, controller_is_separate) =
        match component.cast::<IEditController>() {
            Some(c) => (Some(c), false),
            None => match instantiate_separate_controller(&factory, &component) {
                Ok(Some(c)) => (Some(c), true),
                Ok(None) => (None, false),
                Err(e) => {
                    tracing::warn!("vst3: separate edit controller setup failed: {e}; continuing without parameter discovery");
                    (None, false)
                }
            },
        };

    // Discover parameters from the controller if we have one. Failures
    // here are non-fatal — an empty list is reported and the plugin still
    // loads.
    let params = match edit_controller.as_ref() {
        Some(controller) => discover_vst3_params(controller),
        None => Vec::new(),
    };

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
    plugins.insert(
        main_id,
        LoadedPlugin {
            component,
            edit_controller,
            controller_is_separate,
            library,
        },
    );

    Ok(LoadOk { name: plugin_name, processor, main_id, params })
}

/// Instantiates a separate `IEditController` for a dual-component plugin.
///
/// Returns `Ok(None)` if the component reports `kNoInterface` for the
/// controller class id (the spec-compliant signal that this plugin uses
/// the single-component model and the cast we already attempted should
/// have succeeded). Returns `Ok(Some)` on a fully initialized controller,
/// or `Err` if any step of the dual-component setup fails.
///
/// On success the controller has been:
///   1. Created via `factory.createInstance(controller_cid, IEditController)`.
///   2. Initialized via `IPluginBase::initialize(null host context)`.
///   3. Connected to the IComponent via `IConnectionPoint` (best-effort —
///      a plugin that doesn't expose IConnectionPoint on either side is
///      tolerated, since the gain fixture and most simple plugins work
///      without it).
fn instantiate_separate_controller(
    factory:   &ComPtr<IPluginFactory>,
    component: &ComPtr<IComponent>,
) -> Result<Option<ComPtr<IEditController>>, String> {
    // Ask the IComponent for its paired controller's class id.
    let mut controller_cid: TUID = [0; 16];
    let r = unsafe { component.getControllerClassId(&mut controller_cid as *mut TUID) };
    if r != kResultOk {
        // The plugin declines to expose a separate controller class. This
        // is normal for single-component plugins; the caller will fall
        // through to "no controller, empty params".
        return Ok(None);
    }

    // Instantiate the controller class through the same factory.
    let controller: ComPtr<IEditController> = unsafe {
        let mut obj: *mut std::ffi::c_void = std::ptr::null_mut();
        let r = factory.createInstance(
            controller_cid.as_ptr() as FIDString,
            IEditController::IID.as_ptr() as FIDString,
            &mut obj,
        );
        if r != kResultOk || obj.is_null() {
            return Err(format!("createInstance(IEditController) failed: tresult={r}"));
        }
        ComPtr::from_raw_unchecked(obj as *mut IEditController)
    };

    // Initialize the controller. Same null host context as the component;
    // see module-level docs for the IHostApplication TODO.
    unsafe {
        let r = controller.initialize(std::ptr::null_mut());
        if r != kResultOk {
            return Err(format!("IEditController::initialize failed: tresult={r}"));
        }
    }

    // Connect IComponent <-> IEditController via IConnectionPoint when
    // both sides expose it. nih-plug's gain doesn't actually need this
    // for parameter discovery to work, but it's the spec-compliant
    // setup for dual-component plugins and several commercial plugins
    // refuse to function without it. Best-effort: if either side lacks
    // IConnectionPoint we silently skip the connection.
    if let (Some(comp_cp), Some(ctrl_cp)) = (
        component.cast::<IConnectionPoint>(),
        controller.cast::<IConnectionPoint>(),
    ) {
        unsafe {
            let r1 = comp_cp.connect(ctrl_cp.as_ptr());
            let r2 = ctrl_cp.connect(comp_cp.as_ptr());
            if r1 != kResultOk || r2 != kResultOk {
                tracing::warn!(
                    "vst3: IConnectionPoint::connect returned non-OK \
                     (component->controller={r1}, controller->component={r2}); continuing"
                );
            }
        }
    }

    Ok(Some(controller))
}

/// Walks `IEditController::getParameterCount() / getParameterInfo()` and
/// returns a `Vec<PluginParam>` shaped like the CLAP backend's output.
///
/// VST3 stores parameter values normalized to `[0.0, 1.0]` internally.
/// We use `normalizedParamToPlain` to report `min` / `max` / `default` in
/// the plugin's *plain* (display) units so the wire shape matches CLAP's
/// plain-value convention.
///
/// Failures are localized: any single bad parameter is logged and skipped
/// without aborting the rest of the discovery.
// `ParameterFlags_::kIsHidden` etc. are `int32` (== `i32`) on macOS but
// `u32` on other targets, so the `as i32` casts below are necessary on
// some targets and a no-op on macOS — same pattern as `load_on_main`.
#[cfg_attr(target_os = "macos", allow(clippy::unnecessary_cast))]
fn discover_vst3_params(controller: &ComPtr<IEditController>) -> Vec<PluginParam> {
    let count_signed = unsafe { controller.getParameterCount() };
    if count_signed <= 0 {
        return Vec::new();
    }
    let count = (count_signed as u32).min(MAX_PARAMS_PER_PLUGIN);
    if (count_signed as u32) > MAX_PARAMS_PER_PLUGIN {
        tracing::warn!(
            "VST3 plugin reports {} params; truncating to {}",
            count_signed, MAX_PARAMS_PER_PLUGIN
        );
    }

    let mut out = Vec::with_capacity(count as usize);
    for index in 0..count as i32 {
        // ParameterInfo is `#[repr(C)] Copy` with all-integer / all-array
        // fields, so zero-init is well-defined and the plugin will fully
        // overwrite it on success.
        let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
        let r = unsafe { controller.getParameterInfo(index, &mut info) };
        if r != kResultOk {
            tracing::warn!("VST3 plugin returned no info for param index {index}; skipping");
            continue;
        }
        // Skip hidden / read-only / program-change params — they're not
        // interesting to surface in the cadenza UI.
        if (info.flags & ParameterInfo_::ParameterFlags_::kIsHidden as i32) != 0 {
            continue;
        }

        let name  = string128_to_string(&info.title);
        let units = string128_to_string(&info.units);

        // Convert normalized [0,1] bounds to plain values via the
        // controller's mapping function. nih-plug exposes a sensible
        // plain range (e.g. -30..30 dB for the gain example) so the
        // resulting numbers are meaningful for UI display.
        let plain_min = unsafe { controller.normalizedParamToPlain(info.id, 0.0) } as f32;
        let plain_max = unsafe { controller.normalizedParamToPlain(info.id, 1.0) } as f32;
        let plain_default = unsafe {
            controller.normalizedParamToPlain(info.id, info.defaultNormalizedValue)
        } as f32;

        out.push(PluginParam {
            id: info.id,
            name,
            min:     plain_min,
            max:     plain_max,
            default: plain_default,
            units,
            // VST3 stepCount is 0 for continuous params, N>=1 for stepped
            // (e.g. 1 for a toggle, where the only non-zero plain value
            // is "on"). Negative values aren't allowed.
            step_count:  info.stepCount.max(0) as u32,
            automatable: (info.flags & ParameterInfo_::ParameterFlags_::kCanAutomate as i32) != 0,
            // VST3 has no equivalent of CLAP's IS_MODULATABLE.
            modulatable: false,
        });
    }
    out
}

/// Convert a NUL-terminated UTF-16 `String128` (used pervasively by VST3
/// for human-readable strings) into an owned Rust `String`. Lossy on
/// invalid surrogates, matching `String::from_utf16_lossy`.
fn string128_to_string(s: &[u16; 128]) -> String {
    let len = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..len])
}

// ── Public load entry points ────────────────────────────────────────────────

/// Typed loader: returns a concrete [`Vst3Instrument`] alongside the
/// human-readable plugin name and discovered parameter list. Used
/// directly by the smoke tests; the public dispatch entry point [`load`]
/// erases this to `InstrumentBox`.
fn load_typed(
    path: &Path,
    sample_rate: u32,
) -> Result<(Vst3Instrument, String, Vec<PluginParam>), HostError> {
    let (reply_tx, reply_rx) = unbounded::<Result<LoadOk, HostError>>();
    vst3_main_sender()
        .send(Vst3Command::Load {
            path: path.to_path_buf(),
            sample_rate,
            reply: reply_tx,
        })
        .map_err(|e| HostError::LoadFailed(format!("vst3-main channel send: {e}")))?;

    let LoadOk { name, processor, main_id, params } = reply_rx
        .recv()
        .map_err(|e| HostError::LoadFailed(format!("vst3-main reply recv: {e}")))??;

    Ok((Vst3Instrument::new(processor, main_id), name, params))
}

/// Load a `.vst3` plugin from `path` and return an [`InstrumentBox`] the
/// audio engine can swap in. Blocks the calling thread on the round-trip
/// to `vst3-main`; expected to be called from `tokio::task::spawn_blocking`.
pub(super) fn load(
    path: &Path,
    sample_rate: u32,
) -> Result<(InstrumentBox, String, Vec<PluginParam>), HostError> {
    let (inst, name, params) = load_typed(path, sample_rate)?;
    Ok((Box::new(inst), name, params))
}

// ── Host-side IParameterChanges / IParamValueQueue implementations ──────────
//
// VST3 delivers parameter automation to a plugin through a host-implemented
// `IParameterChanges` containing one `IParamValueQueue` per parameter that
// changes during the block. CLAP, by contrast, embeds parameter events in
// the same event list as note on/off events; the CLAP backend therefore
// just appends `ParamValueEvent`s to its `EventBuffer` and lets clack-host
// hand them to the plugin.
//
// We have to provide concrete COM objects implementing both interfaces.
// Both are pre-allocated at `Vst3Instrument` construction so the audio
// thread never allocates: a fixed pool of `VST3_MAX_PARAM_QUEUES` queues
// hangs off a single `Vst3ParamChangesImpl`, and `render_with_events`
// stages each `ParamSet` event by reusing slots from that pool.
//
// **Concurrency.** All access happens on the audio thread between the
// `clear()` at the top of `render_with_events` and the end of `process()`.
// We use atomics for the count fields so the trait methods can take
// `&self` without `RefCell`, and `UnsafeCell` for the point arrays
// because the `Class`-derived COM wrappers wrap the impl in `Arc` and
// `Arc<C>` requires `C: Sync` to be `Send`. The `unsafe impl Sync` on
// `Vst3ParamQueueImpl` is sound because we never share a reference to a
// queue across threads — only the audio thread (which exclusively owns
// the `Vst3Instrument`) ever observes it.
//
// **Normalization.** VST3 wants normalized `[0, 1]` values for parameter
// changes. The wire `AudioCmd::ParamSet { value }` is treated as already-
// normalized for v1 (see scope notes). When the UI grows real param
// controls, the right place to normalize from a plain value is in the
// control task before sending the AudioCmd, not on the audio thread.

/// Host-side `IParamValueQueue`. One per parameter being changed in a
/// block. The point arrays are pre-allocated to `VST3_MAX_POINTS_PER_QUEUE`
/// and indexed by `point_count`.
struct Vst3ParamQueueImpl {
    /// VST3 ParamID of the parameter this queue is currently representing.
    /// Reset to 0 between blocks. The id is meaningful only when this
    /// queue is among the `queue_count` active queues in its parent
    /// `Vst3ParamChangesImpl`.
    param_id:       AtomicU32,
    /// Number of `(sample_offset, value)` points currently staged in
    /// this queue. Cleared at the top of every block.
    point_count:    AtomicI32,
    /// Per-point sample offsets. Single-threaded audio access only;
    /// `UnsafeCell` because the COM trait methods take `&self`.
    sample_offsets: UnsafeCell<[i32; VST3_MAX_POINTS_PER_QUEUE]>,
    /// Per-point parameter values, normalized to `[0, 1]` per the VST3
    /// spec. Same single-threaded access as `sample_offsets`.
    values:         UnsafeCell<[f64; VST3_MAX_POINTS_PER_QUEUE]>,
}

// SAFETY: only the audio thread accesses the `UnsafeCell` fields, and
// only between `clear()` and the end of `process()`. We never share
// `&Vst3ParamQueueImpl` across threads — `Vst3Instrument` is moved to
// the audio thread at swap-in time and stays there. The `unsafe impl
// Sync` is needed only to satisfy `ComWrapper`'s `Send` bound on the
// parent `Vst3ParamChangesImpl`.
unsafe impl Sync for Vst3ParamQueueImpl {}

impl Vst3ParamQueueImpl {
    fn new() -> Self {
        Self {
            param_id:       AtomicU32::new(0),
            point_count:    AtomicI32::new(0),
            sample_offsets: UnsafeCell::new([0; VST3_MAX_POINTS_PER_QUEUE]),
            values:         UnsafeCell::new([0.0; VST3_MAX_POINTS_PER_QUEUE]),
        }
    }
}

impl Class for Vst3ParamQueueImpl {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for Vst3ParamQueueImpl {
    unsafe fn getParameterId(&self) -> ParamID {
        self.param_id.load(Ordering::Relaxed)
    }

    unsafe fn getPointCount(&self) -> i32 {
        self.point_count.load(Ordering::Relaxed)
    }

    unsafe fn getPoint(
        &self,
        index:        i32,
        sampleOffset: *mut i32,
        value:        *mut ParamValue,
    ) -> tresult {
        let n = self.point_count.load(Ordering::Relaxed);
        if index < 0 || index >= n {
            return kInvalidArgument;
        }
        // SAFETY: single-threaded audio access; `index < n <= cap`.
        let offsets = &*self.sample_offsets.get();
        let values  = &*self.values.get();
        if !sampleOffset.is_null() {
            *sampleOffset = offsets[index as usize];
        }
        if !value.is_null() {
            *value = values[index as usize];
        }
        kResultOk
    }

    unsafe fn addPoint(
        &self,
        sampleOffset: i32,
        value:        ParamValue,
        index:        *mut i32,
    ) -> tresult {
        let n = self.point_count.load(Ordering::Relaxed);
        if (n as usize) >= VST3_MAX_POINTS_PER_QUEUE {
            return kResultFalse;
        }
        // SAFETY: single-threaded audio access.
        let offsets = &mut *self.sample_offsets.get();
        let values  = &mut *self.values.get();
        offsets[n as usize] = sampleOffset;
        values[n as usize]  = value;
        self.point_count.store(n + 1, Ordering::Relaxed);
        if !index.is_null() {
            *index = n;
        }
        kResultOk
    }
}

/// Host-side `IParameterChanges`. Owns a fixed pool of
/// `VST3_MAX_PARAM_QUEUES` queues; the active subset (the first
/// `queue_count`) is what the plugin sees per `process()` call.
struct Vst3ParamChangesImpl {
    /// Number of currently-active queues. Reset by `clear()` at the top
    /// of every block. Atomic so the COM trait methods can mutate it
    /// through `&self`.
    queue_count: AtomicI32,
    /// Owning references to the queue impls. Indexing into this slice
    /// directly is how the host stages events; the plugin sees the
    /// queues only through the IParamValueQueue COM pointers in
    /// `queue_com_ptrs`.
    queues:        Vec<ComWrapper<Vst3ParamQueueImpl>>,
    /// Cached COM pointers to the same queue objects, returned to the
    /// plugin from `getParameterData` / `addParameterData`. Pre-computed
    /// so the audio path performs no allocation. Each `ComPtr` holds a
    /// refcount on its queue for the life of the parent.
    queue_com_ptrs: Vec<ComPtr<IParamValueQueue>>,
}

// SAFETY: `Vec<ComPtr<IParamValueQueue>>` is auto Send+Sync because the
// underlying interface is declared `unsafe impl Send + Sync` upstream.
// `Vec<ComWrapper<Vst3ParamQueueImpl>>` is Send+Sync because
// `Vst3ParamQueueImpl: Send + Sync` (atomics + the unsafe Sync above).
// Atomic count is Send+Sync. The struct is therefore auto Send+Sync but
// we re-state the impls for clarity since it lives inside another COM
// wrapper.
unsafe impl Send for Vst3ParamChangesImpl {}
unsafe impl Sync for Vst3ParamChangesImpl {}

impl Vst3ParamChangesImpl {
    fn new() -> Self {
        let mut queues = Vec::with_capacity(VST3_MAX_PARAM_QUEUES);
        let mut queue_com_ptrs = Vec::with_capacity(VST3_MAX_PARAM_QUEUES);
        for _ in 0..VST3_MAX_PARAM_QUEUES {
            let wrapper = ComWrapper::new(Vst3ParamQueueImpl::new());
            // `to_com_ptr` cannot fail here because `IParamValueQueue` is
            // listed in the impl's `Class::Interfaces`. Treating a None
            // as an unrecoverable construction error keeps the audio
            // thread free of `Option` checks later.
            let com_ptr = wrapper
                .to_com_ptr::<IParamValueQueue>()
                .expect("Vst3ParamQueueImpl declares IParamValueQueue in its interface list");
            queues.push(wrapper);
            queue_com_ptrs.push(com_ptr);
        }
        Self {
            queue_count: AtomicI32::new(0),
            queues,
            queue_com_ptrs,
        }
    }

    /// Reset all queues to "empty" for a new block. O(active queues).
    fn clear(&self) {
        let n = self.queue_count.load(Ordering::Relaxed);
        for i in 0..n as usize {
            self.queues[i].point_count.store(0, Ordering::Relaxed);
            self.queues[i].param_id.store(0, Ordering::Relaxed);
        }
        self.queue_count.store(0, Ordering::Relaxed);
    }

    /// Append a single `(param_id, sample_offset, value)` automation
    /// point. Reuses an existing queue for the same `param_id` when
    /// possible (the spec wants one queue per param per block) and
    /// allocates a fresh slot from the pool otherwise. Drops events
    /// silently when either the queue pool or the per-queue point
    /// budget is exhausted — same defensive policy as the CLAP backend's
    /// `EVENT_BUFFER_CAPACITY` cap.
    fn stage(&self, param_id: u32, sample_offset: i32, value: f64) {
        let n = self.queue_count.load(Ordering::Relaxed);

        // Linear search for an existing queue with this id. With
        // VST3_MAX_PARAM_QUEUES = 64 this is fine on the audio thread;
        // typical blocks touch ~1-3 distinct params anyway.
        let mut idx: Option<usize> = None;
        for i in 0..n as usize {
            if self.queues[i].param_id.load(Ordering::Relaxed) == param_id {
                idx = Some(i);
                break;
            }
        }

        let queue_idx = match idx {
            Some(i) => i,
            None => {
                if (n as usize) >= self.queues.len() {
                    return;
                }
                let i = n as usize;
                self.queues[i].param_id.store(param_id, Ordering::Relaxed);
                self.queues[i].point_count.store(0, Ordering::Relaxed);
                self.queue_count.store(n + 1, Ordering::Relaxed);
                i
            }
        };

        let q  = &self.queues[queue_idx];
        let pc = q.point_count.load(Ordering::Relaxed);
        if (pc as usize) >= VST3_MAX_POINTS_PER_QUEUE {
            return;
        }
        // SAFETY: single-threaded audio access between `clear()` and
        // the end of `process()`.
        unsafe {
            (*q.sample_offsets.get())[pc as usize] = sample_offset;
            (*q.values.get())[pc as usize] = value;
        }
        q.point_count.store(pc + 1, Ordering::Relaxed);
    }
}

impl Class for Vst3ParamChangesImpl {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for Vst3ParamChangesImpl {
    unsafe fn getParameterCount(&self) -> i32 {
        self.queue_count.load(Ordering::Relaxed)
    }

    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        let n = self.queue_count.load(Ordering::Relaxed);
        if index < 0 || index >= n {
            return std::ptr::null_mut();
        }
        self.queue_com_ptrs[index as usize].as_ptr()
    }

    unsafe fn addParameterData(
        &self,
        id:    *const ParamID,
        index: *mut i32,
    ) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        let id_val = *id;
        let n = self.queue_count.load(Ordering::Relaxed);
        // Existing queue?
        for i in 0..n {
            if self.queues[i as usize].param_id.load(Ordering::Relaxed) == id_val {
                if !index.is_null() {
                    *index = i;
                }
                return self.queue_com_ptrs[i as usize].as_ptr();
            }
        }
        if (n as usize) >= self.queues.len() {
            return std::ptr::null_mut();
        }
        let new_idx = n as usize;
        self.queues[new_idx].param_id.store(id_val, Ordering::Relaxed);
        self.queues[new_idx].point_count.store(0, Ordering::Relaxed);
        self.queue_count.store(n + 1, Ordering::Relaxed);
        if !index.is_null() {
            *index = n;
        }
        self.queue_com_ptrs[new_idx].as_ptr()
    }
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

    /// Owning handle to the host-implemented `IParameterChanges` object
    /// passed to `process()` in `inputParameterChanges`. Held as the
    /// concrete `ComWrapper` so the audio thread can call inherent
    /// helpers (`clear()` / `stage()`) without going through the COM
    /// vtable. The matching `*mut IParameterChanges` lives in
    /// `param_changes_ptr` to avoid an `as_ptr()` allocation guess on
    /// every call.
    param_changes:     ComWrapper<Vst3ParamChangesImpl>,
    /// Cached COM pointer to the same object as `param_changes`,
    /// re-fetched once at construction so `run_process` can stuff it
    /// into `ProcessData::inputParameterChanges` without touching the
    /// allocator.
    param_changes_ptr: ComPtr<IParameterChanges>,
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

        // Pre-allocate the parameter-changes COM object and its queue
        // pool exactly once. Both `ComWrapper::new` calls inside
        // `Vst3ParamChangesImpl::new` allocate, but they're confined to
        // construction here on the control thread; the audio thread that
        // later owns this instrument never allocates against this object.
        let param_changes = ComWrapper::new(Vst3ParamChangesImpl::new());
        let param_changes_ptr = param_changes
            .to_com_ptr::<IParameterChanges>()
            .expect("Vst3ParamChangesImpl declares IParameterChanges in its interface list");

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
            param_changes,
            param_changes_ptr,
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
        self.process_block_with_events_for_test(inputs, frames, &[])
    }

    /// Variant of [`process_block_for_test`] that lets a test stage
    /// `AudioCmd` events (currently only `ParamSet`) before the process
    /// call. Routes through the same staging path as `render_with_events`
    /// so the test exercises the production code, not a parallel one.
    /// Note events are accepted and ignored — VST3 IEventList is out of
    /// scope for v1; see the module-level docs.
    #[cfg(all(test, feature = "vst3-host-tests"))]
    pub(crate) fn process_block_with_events_for_test(
        &mut self,
        inputs: &[f32],
        frames: usize,
        events: &[(u32, AudioCmd)],
    ) -> Result<Vec<f32>, String> {
        if frames > MAX_FRAMES_PER_BUFFER {
            return Err(format!("frames {frames} > MAX_FRAMES_PER_BUFFER {MAX_FRAMES_PER_BUFFER}"));
        }
        if inputs.len() != frames * CHANNELS_PER_PORT {
            return Err(format!("inputs.len() {} != frames * channels {}", inputs.len(), frames * CHANNELS_PER_PORT));
        }
        // Stage parameter events through the same code the audio thread
        // uses, so the test exercises the production path.
        self.param_changes.clear();
        for (offset, cmd) in events {
            if let AudioCmd::ParamSet { param_id, value } = cmd {
                self.param_changes
                    .stage(*param_id, *offset as i32, f64::from(*value));
            }
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
            // Pass our host-implemented IParameterChanges. The plugin
            // will read it via getParameterCount/getParameterData/getPoint
            // during the call. Whatever was staged via
            // `param_changes.stage(...)` since the last `clear()` is
            // visible to the plugin here.
            inputParameterChanges:  self.param_changes_ptr.as_ptr(),
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
        events:   &[(u32, AudioCmd)],
    ) {
        // Stage parameter automation through the host-side
        // IParameterChanges. Note events are still ignored in v1; see
        // module-level "What we don't implement" docs for the IEventList
        // work needed to dispatch NoteOn/NoteOff. The CLAP backend handles
        // both in one pass; here we have to keep them separate because
        // VST3 puts them in different ProcessData fields.
        self.param_changes.clear();
        for (offset, cmd) in events {
            if let AudioCmd::ParamSet { param_id, value } = cmd {
                // Allocation-free: `stage` only writes into the
                // pre-allocated queue pool. Out-of-cap events are
                // dropped silently — same defensive policy as the CLAP
                // backend's EVENT_BUFFER_CAPACITY truncation.
                self.param_changes
                    .stage(*param_id, *offset as i32, f64::from(*value));
            }
        }

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
        let (mut inst, name, _params) = load_typed(&path, 48_000).expect("load_typed");
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

    /// End-to-end parameter automation against the gain.vst3 fixture.
    /// Sibling of `gain_clap_param_automation_changes_output_level` in
    /// the CLAP backend; same shape, same fixture (the nih-plug `gain`
    /// example built for both formats).
    ///
    /// Stages a `ParamSet` event for the gain plugin's gain parameter at
    /// the top of a process block driving it to the *normalized*
    /// minimum (0.0 — the wire format is normalized for VST3 in v1; see
    /// the module's "Normalization" docs in the param-changes section).
    /// nih-plug's gain plugin spreads parameter changes over a smoother;
    /// we render `SETTLE_BLOCKS` extra blocks for the level to converge,
    /// then assert the resulting output level dropped meaningfully.
    #[test]
    fn gain_vst3_param_automation_changes_output_level() {
        let path = fixture_path();
        assert!(path.exists(), "test fixture missing at {}", path.display());

        let (mut inst, _name, params) = load_typed(&path, 48_000).expect("load_typed");
        assert!(
            !params.is_empty(),
            "gain plugin should expose at least one param via IEditController"
        );
        // Find the gain control by name. Same loose matcher as the CLAP
        // sibling test — fires loudly if the fixture's parameter naming
        // changes so we update intentionally rather than guessing.
        let gain = params
            .iter()
            .find(|p| p.name.to_ascii_lowercase().contains("gain"))
            .unwrap_or_else(|| {
                panic!(
                    "no parameter named 'gain' in plugin params: {:?}",
                    params.iter().map(|p| &p.name).collect::<Vec<_>>()
                )
            });
        let gain_param_id = gain.id;

        const FRAMES: usize = 512;
        const SETTLE_BLOCKS: usize = 16;
        let inputs = vec![0.5f32; FRAMES * 2];

        let mean_abs = |buf: &[f32]| -> f32 {
            let sum: f32 = buf.iter().map(|s| s.abs()).sum();
            sum / buf.len().max(1) as f32
        };

        // 1. Baseline at the default value: render two blocks so any
        //    startup smoother settles, capture the second.
        let _warmup = inst
            .process_block_for_test(&inputs, FRAMES)
            .expect("warmup");
        let baseline_block = inst
            .process_block_for_test(&inputs, FRAMES)
            .expect("baseline");
        let baseline = mean_abs(&baseline_block);
        assert!(
            baseline > 1e-4,
            "baseline level should be audible, got {baseline}"
        );

        // 2. Stage a ParamSet at offset 0 driving the gain to its
        //    normalized minimum (0.0). VST3 wants normalized values; the
        //    plain min reported in `params[..].min` would map to the
        //    same point but the wire convention here is normalized in v1.
        let events = [(0u32, AudioCmd::ParamSet { param_id: gain_param_id, value: 0.0 })];
        inst.process_block_with_events_for_test(&inputs, FRAMES, &events)
            .expect("process with param event");

        // 3. Settle.
        let mut settled_block = Vec::new();
        for _ in 0..SETTLE_BLOCKS {
            settled_block = inst
                .process_block_for_test(&inputs, FRAMES)
                .expect("settle");
        }
        let settled = mean_abs(&settled_block);

        // 4. Loose bound — see the CLAP sibling for the rationale.
        assert!(
            settled < 0.5 * baseline,
            "expected settled level {settled} to be < 50% of baseline {baseline} \
             after driving gain param to normalized min=0.0"
        );
        for s in &settled_block {
            assert!(s.is_finite(), "non-finite sample after automation: {s}");
        }
    }
}
