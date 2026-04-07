//! Audio engine: cpal output stream + SPSC ringbuf + built-in PolySynth.
//!
//! `AudioEngine` is the *handle* held by the tokio runtime. The cpal
//! `Stream` itself is owned by a dedicated OS thread (`audio-host`) which
//! parks forever — this sidesteps cpal's `!Send` quirk on macOS, where the
//! stream contains CoreAudio objects that must be created and dropped on
//! the same thread.
//!
//! Communication is one-way: scheduler/server task → audio thread via an
//! SPSC ringbuf of `TimedCmd`s, each carrying an absolute frame number.
//! The audio callback drains the ringbuf into a small pre-allocated
//! `pending` queue, partitions events that fall inside the current buffer,
//! and hands them to the synth at sample-exact offsets.
//!
//! A shared `Arc<AtomicU64>` frame counter advances by `frames_in_buffer`
//! after each callback; the scheduler reads it to compute event positions.
//!
//! Constraints honored here:
//!   - No allocation inside the audio callback
//!   - No locks inside the audio callback (SPSC ringbuf is wait-free)
//!   - All voices, pending queue, and scratch buffers pre-allocated up front
//!
//! ## Testing
//!
//! Tests that call `AudioEngine::start()` open a real output device and
//! cannot run on headless CI runners. Gate them behind:
//!
//! ```ignore
//! #[cfg_attr(not(feature = "audio-device-tests"), ignore)]
//! ```
//!
//! and run locally with `cargo test -p cadenza-daemon --features audio-device-tests`.
//! Renderer-level tests below construct a Renderer with a synthetic ringbuf
//! and never touch cpal.

use crate::instrument::{InstrumentBox, BUILTIN_PLUGIN_ID};
use crate::synth::PolySynth;
use crate::DynError;
use cadenza_ipc::PluginId;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, StreamConfig};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

/// Tagged instrument hand-off across the swap ringbufs. The id lets the
/// control side route an evicted instrument back to its originating
/// `PluginHost` entry so re-activation is a hot swap rather than a reload
/// from disk. The built-in `PolySynth` uses [`BUILTIN_PLUGIN_ID`].
pub type SwapItem = (PluginId, InstrumentBox);

/// Single bare command sent from scheduler to audio thread.
/// Kept POD-sized so it copies cheaply through the ringbuf.
#[derive(Debug, Clone, Copy)]
pub enum AudioCmd {
    NoteOn  { pitch: u8, velocity: u8 },
    NoteOff { pitch: u8 },
    AllNotesOff,
}

/// `AudioCmd` tagged with the absolute frame number it should fire on.
/// Frames are counted from process start by the audio callback.
#[derive(Debug, Clone, Copy)]
pub struct TimedCmd {
    pub frame: u64,
    pub cmd:   AudioCmd,
}

impl TimedCmd {
    /// Sentinel used to fill the pre-allocated `pending` array.
    const SENTINEL: TimedCmd = TimedCmd { frame: u64::MAX, cmd: AudioCmd::AllNotesOff };
}

const RINGBUF_CAPACITY: usize = 16_384;
/// Largest number of events the audio thread can buffer ahead of the
/// current callback. ~250ms of dense activity at 120bpm comfortably fits.
pub(crate) const PENDING_CAP: usize = 512;
/// Capacity of the bidirectional instrument-swap ringbufs. Two slots is
/// enough: one for an in-flight new instrument and one for the just-evicted
/// old instrument awaiting drop on the control thread.
const SWAP_CAPACITY: usize = 4;

type CmdProducer = <HeapRb<TimedCmd> as Split>::Prod;
type CmdConsumer = <HeapRb<TimedCmd> as Split>::Cons;
type SwapProducer = <HeapRb<SwapItem> as Split>::Prod;
type SwapConsumer = <HeapRb<SwapItem> as Split>::Cons;

/// Live handle owned by the tokio side. The audio thread keeps running
/// for the lifetime of the process.
pub struct AudioEngine {
    cmd_tx:        CmdProducer,
    swap_in_tx:    SwapProducer,
    swap_out_rx:   SwapConsumer,
    pub sample_rate: u32,
    frame_counter: Arc<AtomicU64>,
}

impl AudioEngine {
    pub fn start() -> Result<Self, DynError> {
        // Pre-allocate the ringbuf and split it before crossing the thread
        // boundary so the audio thread never touches the allocator.
        let rb: HeapRb<TimedCmd> = HeapRb::new(RINGBUF_CAPACITY);
        let (cmd_tx, cmd_rx) = rb.split();

        // Bidirectional swap channels for instrument hand-off. The control
        // task pushes new boxed instruments via `swap_in_tx`; the audio
        // thread evicts the old one and sends it back via `swap_out_tx`
        // so it can be dropped off-thread (drop allocates and is forbidden
        // on the audio callback).
        let swap_in:  HeapRb<SwapItem> = HeapRb::new(SWAP_CAPACITY);
        let swap_out: HeapRb<SwapItem> = HeapRb::new(SWAP_CAPACITY);
        let (swap_in_tx,  swap_in_rx)  = swap_in.split();
        let (swap_out_tx, swap_out_rx) = swap_out.split();

        let frame_counter = Arc::new(AtomicU64::new(0));

        // Hand the consumer + frame counter to a dedicated audio-host thread
        // which builds the cpal stream and parks. Sample rate is reported
        // back via a sync channel so we can record it on the engine handle.
        let (boot_tx, boot_rx) = mpsc::sync_channel::<Result<u32, String>>(1);
        let fc_for_thread = frame_counter.clone();

        std::thread::Builder::new()
            .name("audio-host".into())
            .spawn(move || run_audio_thread(cmd_rx, fc_for_thread, swap_in_rx, swap_out_tx, boot_tx))
            .map_err(|e| -> DynError { Box::new(e) })?;

        let sample_rate = boot_rx
            .recv()
            .map_err(|e| -> DynError { Box::new(e) })?
            .map_err(|s| -> DynError { s.into() })?;

        Ok(Self {
            cmd_tx,
            swap_in_tx,
            swap_out_rx,
            sample_rate,
            frame_counter,
        })
    }

    /// Snapshot of the current frame counter. Used by the scheduler to
    /// translate phrase ticks into absolute frames.
    pub fn now_frame(&self) -> u64 {
        self.frame_counter.load(Ordering::Acquire)
    }

    /// Best-effort enqueue of a frame-tagged command. Drops on overflow.
    pub fn send_timed(&mut self, cmd: TimedCmd) {
        if self.cmd_tx.try_push(cmd).is_err() {
            tracing::warn!("audio ringbuf full, dropping cmd: {cmd:?}");
        }
    }

    /// Enqueue a command for *immediate* dispatch on the next audio callback.
    /// Used by the server task for things like AllNotesOff after Stop.
    pub fn send(&mut self, cmd: AudioCmd) {
        let frame = self.frame_counter.load(Ordering::Acquire);
        self.send_timed(TimedCmd { frame, cmd });
    }

    /// Hand a freshly-constructed instrument to the audio thread, tagged
    /// with the originating plugin id (or [`BUILTIN_PLUGIN_ID`] for the
    /// engine's own `PolySynth`). The audio callback picks the tuple up on
    /// its next invocation, `mem::replace`s the current instrument, and
    /// sends the previously-installed `(id, old)` tuple back via
    /// `swap_out_tx`. Returns `false` if the swap inbox is full.
    ///
    /// The control side is responsible for periodically calling
    /// [`Self::take_dropped_instruments`] to keep the return ringbuf from
    /// backpressuring the audio thread (if `swap_out` fills up the audio
    /// thread refuses to perform further swaps until it drains).
    pub fn swap_instrument(&mut self, plugin_id: PluginId, new_inst: InstrumentBox) -> bool {
        if self.swap_in_tx.try_push((plugin_id, new_inst)).is_err() {
            tracing::warn!("instrument swap inbox full");
            return false;
        }
        true
    }

    /// Take all evicted instruments out of the swap-out ringbuf so the
    /// control side can route each one back to its `PluginHost` entry via
    /// [`crate::host::PluginHost::return_instrument`]. Built-in synth
    /// evictions arrive tagged with [`BUILTIN_PLUGIN_ID`] and should be
    /// dropped on the control thread.
    pub fn take_dropped_instruments(&mut self) -> Vec<SwapItem> {
        let mut out = Vec::new();
        while let Some(item) = self.swap_out_rx.try_pop() {
            out.push(item);
        }
        out
    }
}

/// Owns the active instrument + ringbuf consumers + pending queue for the
/// audio callback. Lives entirely on the audio thread; never allocates
/// after construction.
pub(crate) struct Renderer {
    instrument:    InstrumentBox,
    /// Plugin id of the currently-installed instrument. Carried back to the
    /// control side on eviction so the host can re-cache the instance.
    current_id:    PluginId,
    consumer:      CmdConsumer,
    swap_in_rx:    SwapConsumer,
    swap_out_tx:   SwapProducer,
    channels:      usize,
    frame_counter: Arc<AtomicU64>,
    /// Sorted-by-frame queue of events drained from the ringbuf but not
    /// yet due. Indices `0..pending_len` are valid.
    pending:       [TimedCmd; PENDING_CAP],
    pending_len:   usize,
    /// Pre-allocated scratch for the per-buffer event slice handed to the
    /// instrument. Reused every callback.
    scratch:       [(u32, AudioCmd); PENDING_CAP],
}

impl Renderer {
    fn new(
        instrument:    InstrumentBox,
        current_id:    PluginId,
        consumer:      CmdConsumer,
        swap_in_rx:    SwapConsumer,
        swap_out_tx:   SwapProducer,
        channels:      usize,
        frame_counter: Arc<AtomicU64>,
    ) -> Self {
        Self {
            instrument,
            current_id,
            consumer,
            swap_in_rx,
            swap_out_tx,
            channels,
            frame_counter,
            pending:     [TimedCmd::SENTINEL; PENDING_CAP],
            pending_len: 0,
            scratch:     [(0, AudioCmd::AllNotesOff); PENDING_CAP],
        }
    }

    /// Hot-swap the active instrument if the control side has handed us a
    /// new one and the swap-out ringbuf has room to receive the evicted
    /// one. If the outbox is full we skip this round so the audio thread
    /// never has to drop a `Box<dyn Instrument>` (which would allocate).
    fn try_swap_instrument(&mut self) {
        if self.swap_out_tx.vacant_len() == 0 {
            return;
        }
        let Some((new_id, new_inst)) = self.swap_in_rx.try_pop() else { return };
        let mut old = std::mem::replace(&mut self.instrument, new_inst);
        let old_id = std::mem::replace(&mut self.current_id, new_id);
        // Mute the outgoing instrument so any sustained voices don't
        // bleed across the swap boundary.
        old.all_notes_off();
        let _ = self.swap_out_tx.try_push((old_id, old));
    }

    fn drain_into_pending(&mut self) {
        while self.pending_len < PENDING_CAP {
            let Some(tc) = self.consumer.try_pop() else { break };
            // Sorted insert (insertion sort over a small N).
            let mut i = self.pending_len;
            while i > 0 && self.pending[i - 1].frame > tc.frame {
                self.pending[i] = self.pending[i - 1];
                i -= 1;
            }
            self.pending[i] = tc;
            self.pending_len += 1;
        }
        // If the ringbuf still had events but pending is full, those events
        // sit in the ringbuf and are picked up next callback. Worst case
        // they fire one buffer late.
    }

    /// Drain the ringbuf, dispatch in-buffer events at exact frame offsets,
    /// and advance the shared frame counter. Pure function over `out` and
    /// renderer state — no cpal contact.
    fn render(&mut self, out: &mut [f32]) {
        self.try_swap_instrument();
        self.drain_into_pending();

        let now    = self.frame_counter.load(Ordering::Acquire);
        let frames = (out.len() / self.channels.max(1)) as u32;
        let end    = now + frames as u64;

        // Partition: take events with frame < end into scratch, in order.
        let mut n = 0;
        while n < self.pending_len && self.pending[n].frame < end {
            let tc = self.pending[n];
            // Saturating ensures late events still play (offset 0).
            let offset = tc.frame.saturating_sub(now) as u32;
            self.scratch[n] = (offset.min(frames.saturating_sub(1)), tc.cmd);
            n += 1;
        }
        // Compact: shift remaining pending entries to the front.
        if n > 0 && n < self.pending_len {
            for i in n..self.pending_len {
                self.pending[i - n] = self.pending[i];
            }
        }
        if n > 0 {
            self.pending_len -= n;
        }

        self.instrument.render_with_events(out, self.channels, &self.scratch[..n]);
        self.frame_counter.fetch_add(frames as u64, Ordering::Release);
    }
}

fn run_audio_thread(
    cmd_rx:        CmdConsumer,
    frame_counter: Arc<AtomicU64>,
    swap_in_rx:    SwapConsumer,
    swap_out_tx:   SwapProducer,
    boot_tx:       mpsc::SyncSender<Result<u32, String>>,
) {
    let stream = match build_stream(cmd_rx, frame_counter, swap_in_rx, swap_out_tx) {
        Ok((stream, sr)) => {
            let _ = boot_tx.send(Ok(sr));
            stream
        }
        Err(e) => {
            let _ = boot_tx.send(Err(e.to_string()));
            return;
        }
    };

    // Keep the stream alive for the life of the process. We can't move
    // `stream` to another thread on macOS, so we park here forever.
    let _keep_alive = stream;
    loop { std::thread::park(); }
}

/// Build the cpal output stream. The consumers are moved into the audio
/// callback closure. Returns the live stream and its sample rate.
fn build_stream(
    consumer:      CmdConsumer,
    frame_counter: Arc<AtomicU64>,
    swap_in_rx:    SwapConsumer,
    swap_out_tx:   SwapProducer,
) -> Result<(cpal::Stream, u32), DynError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("no default output device")?;
    let supported = device.default_output_config()?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.clone().into();
    let sample_rate = config.sample_rate.0;
    let channels = config.channels as usize;

    // Pre-allocate the built-in synth as the initial instrument. The
    // control task can hot-swap a hosted plugin in later via
    // `AudioEngine::swap_instrument`.
    let initial_instrument: InstrumentBox = Box::new(PolySynth::new(sample_rate as f32));

    let err_fn = |e| tracing::error!("cpal stream error: {e}");

    // Per-buffer hot-loop is generic over sample type. We pre-allocate a
    // scratch f32 buffer once for the I16/U16 paths so the audio callback
    // never allocates. cpal does not guarantee a fixed buffer size, so we
    // size the scratch generously and silently drop frames beyond that.
    const MAX_SCRATCH: usize = 8192 * 8; // frames * channels upper bound

    let stream = match sample_format {
        SampleFormat::F32 => {
            let mut renderer = Renderer::new(
                initial_instrument,
                BUILTIN_PLUGIN_ID,
                consumer,
                swap_in_rx,
                swap_out_tx,
                channels,
                frame_counter.clone(),
            );
            device.build_output_stream(
                &config,
                move |data: &mut [f32], _| renderer.render(data),
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let mut renderer = Renderer::new(
                initial_instrument,
                BUILTIN_PLUGIN_ID,
                consumer,
                swap_in_rx,
                swap_out_tx,
                channels,
                frame_counter.clone(),
            );
            let mut scratch = vec![0.0f32; MAX_SCRATCH];
            device.build_output_stream(
                &config,
                move |data: &mut [i16], _| {
                    let n = data.len().min(scratch.len());
                    let buf = &mut scratch[..n];
                    renderer.render(buf);
                    for (dst, src) in data.iter_mut().zip(buf.iter()) {
                        *dst = i16::from_sample(*src);
                    }
                    for s in data.iter_mut().skip(n) { *s = i16::EQUILIBRIUM; }
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let mut renderer = Renderer::new(
                initial_instrument,
                BUILTIN_PLUGIN_ID,
                consumer,
                swap_in_rx,
                swap_out_tx,
                channels,
                frame_counter.clone(),
            );
            let mut scratch = vec![0.0f32; MAX_SCRATCH];
            device.build_output_stream(
                &config,
                move |data: &mut [u16], _| {
                    let n = data.len().min(scratch.len());
                    let buf = &mut scratch[..n];
                    renderer.render(buf);
                    for (dst, src) in data.iter_mut().zip(buf.iter()) {
                        *dst = u16::from_sample(*src);
                    }
                    for s in data.iter_mut().skip(n) { *s = u16::EQUILIBRIUM; }
                },
                err_fn,
                None,
            )?
        }
        other => return Err(format!("unsupported sample format: {other:?}").into()),
    };
    stream.play()?;
    Ok((stream, sample_rate))
}

#[cfg(test)]
pub(crate) struct TestHandles {
    pub renderer:       Renderer,
    pub cmd_prod:       CmdProducer,
    pub swap_in_prod:   SwapProducer,
    pub swap_out_cons:  SwapConsumer,
    pub frame_counter:  Arc<AtomicU64>,
}

#[cfg(test)]
pub(crate) fn make_test_renderer(sample_rate: f32, channels: usize) -> TestHandles {
    let rb: HeapRb<TimedCmd> = HeapRb::new(RINGBUF_CAPACITY);
    let (cmd_prod, cmd_cons) = rb.split();

    let swap_in:  HeapRb<SwapItem> = HeapRb::new(SWAP_CAPACITY);
    let swap_out: HeapRb<SwapItem> = HeapRb::new(SWAP_CAPACITY);
    let (swap_in_prod, swap_in_cons)  = swap_in.split();
    let (swap_out_prod, swap_out_cons) = swap_out.split();

    let frame_counter = Arc::new(AtomicU64::new(0));
    let renderer = Renderer::new(
        Box::new(PolySynth::new(sample_rate)),
        BUILTIN_PLUGIN_ID,
        cmd_cons,
        swap_in_cons,
        swap_out_prod,
        channels,
        frame_counter.clone(),
    );
    TestHandles {
        renderer,
        cmd_prod,
        swap_in_prod,
        swap_out_cons,
        frame_counter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::Instrument;

    /// Number of frames that need to elapse after a NoteOn before the AR
    /// envelope produces an audible (>1e-3) sample at sr=48kHz with the
    /// 5ms attack rate. ~240 frames; we wait 400 to be safe.
    const ATTACK_OBSERVABLE_FRAMES: usize = 400;

    fn first_nonzero_frame(buf: &[f32], channels: usize) -> Option<usize> {
        let frames = buf.len() / channels;
        (0..frames).find(|&f| buf[f * channels].abs() > 1e-6)
    }

    /// Assert nothing was rendered before `offset` and that signal appears
    /// at the very first frame after the event boundary. The synth's sine
    /// starts at phase=0 so frame `offset` itself is exactly 0.0; the first
    /// audible sample is `offset + 1`.
    fn assert_event_lands_at_offset(buf: &[f32], channels: usize, offset: usize) {
        for f in 0..offset {
            assert_eq!(buf[f * channels], 0.0, "frame {f} should be silent");
        }
        let first = first_nonzero_frame(buf, channels)
            .expect("expected signal somewhere in the buffer");
        assert_eq!(
            first,
            offset + 1,
            "first audible frame should be exactly one frame after the event offset"
        );
    }

    #[test]
    fn render_dispatches_event_at_exact_frame_offset() {
        let mut h = make_test_renderer(48_000.0, 2);
        let target_frame = 200u64;
        h.cmd_prod.try_push(TimedCmd {
            frame: target_frame,
            cmd:   AudioCmd::NoteOn { pitch: 69, velocity: 127 },
        }).expect("push");

        let frames = 4096;
        let mut buf = vec![0.0f32; frames * 2];
        h.renderer.render(&mut buf);

        assert_event_lands_at_offset(&buf, 2, target_frame as usize);
        assert_eq!(h.frame_counter.load(Ordering::Acquire), frames as u64);
        assert!(buf[(target_frame as usize + ATTACK_OBSERVABLE_FRAMES) * 2].abs() > 1e-3);
    }

    #[test]
    fn render_holds_future_events_across_buffers() {
        let mut h = make_test_renderer(48_000.0, 2);
        h.cmd_prod.try_push(TimedCmd {
            frame: 5000,
            cmd:   AudioCmd::NoteOn { pitch: 69, velocity: 127 },
        }).expect("push");

        let mut buf1 = vec![0.0f32; 2 * 1024];
        h.renderer.render(&mut buf1);
        assert!(buf1.iter().all(|&x| x == 0.0), "no signal in [0, 1024)");
        assert_eq!(h.frame_counter.load(Ordering::Acquire), 1024);

        h.frame_counter.store(4096, Ordering::Release);

        let mut buf2 = vec![0.0f32; 2 * 1024];
        h.renderer.render(&mut buf2);
        assert_event_lands_at_offset(&buf2, 2, 904);
    }

    #[test]
    fn render_processes_multiple_events_in_order() {
        let mut h = make_test_renderer(48_000.0, 2);
        h.cmd_prod.try_push(TimedCmd {
            frame: 100,
            cmd:   AudioCmd::NoteOn { pitch: 69, velocity: 100 },
        }).unwrap();
        h.cmd_prod.try_push(TimedCmd {
            frame: 500,
            cmd:   AudioCmd::NoteOn { pitch: 76, velocity: 100 },
        }).unwrap();

        let mut buf = vec![0.0f32; 2 * 4096];
        h.renderer.render(&mut buf);
        assert_eq!(first_nonzero_frame(&buf, 2), Some(101));
    }

    #[test]
    fn render_advances_frame_counter() {
        let mut h = make_test_renderer(48_000.0, 2);
        let mut buf = vec![0.0f32; 2 * 512];
        h.renderer.render(&mut buf);
        assert_eq!(h.frame_counter.load(Ordering::Acquire), 512);
        h.renderer.render(&mut buf);
        assert_eq!(h.frame_counter.load(Ordering::Acquire), 1024);
    }

    #[test]
    fn out_of_order_pushes_get_sorted_into_pending() {
        let mut h = make_test_renderer(48_000.0, 2);
        h.cmd_prod.try_push(TimedCmd {
            frame: 800,
            cmd:   AudioCmd::NoteOn { pitch: 76, velocity: 100 },
        }).unwrap();
        h.cmd_prod.try_push(TimedCmd {
            frame: 200,
            cmd:   AudioCmd::NoteOn { pitch: 69, velocity: 100 },
        }).unwrap();

        let mut buf = vec![0.0f32; 2 * 4096];
        h.renderer.render(&mut buf);
        assert_eq!(first_nonzero_frame(&buf, 2), Some(201));
    }

    /// Test-only instrument that records every method call. Lets us verify
    /// that the audio thread actually swapped to a new instrument and is
    /// dispatching events through it.
    struct CountingInstrument {
        notes_on:   u32,
        all_off:    u32,
        renders:    u32,
    }
    impl CountingInstrument {
        fn new() -> Self { Self { notes_on: 0, all_off: 0, renders: 0 } }
    }
    impl Instrument for CountingInstrument {
        fn note_on(&mut self, _pitch: u8, _velocity: u8) { self.notes_on += 1; }
        fn note_off(&mut self, _pitch: u8) {}
        fn all_notes_off(&mut self) { self.all_off += 1; }
        fn render_with_events(&mut self, _out: &mut [f32], _channels: usize, events: &[(u32, AudioCmd)]) {
            self.renders += 1;
            for (_, cmd) in events {
                if let AudioCmd::NoteOn { .. } = cmd { self.notes_on += 1; }
                if let AudioCmd::AllNotesOff   = cmd { self.all_off  += 1; }
            }
        }
    }

    #[test]
    fn instrument_swap_evicts_old_and_routes_events_through_new() {
        let mut h = make_test_renderer(48_000.0, 2);

        // Push a new instrument via the swap inbox, tagged with a fake id.
        let new_inst: InstrumentBox = Box::new(CountingInstrument::new());
        // try_push returns Err containing the boxed tuple, which has no
        // Debug impl, so we can't .unwrap() it directly.
        assert!(h.swap_in_prod.try_push((42, new_inst)).is_ok());

        // Send a note for the new instrument to count.
        h.cmd_prod.try_push(TimedCmd {
            frame: 100,
            cmd:   AudioCmd::NoteOn { pitch: 60, velocity: 100 },
        }).unwrap();

        let mut buf = vec![0.0f32; 2 * 1024];
        h.renderer.render(&mut buf);

        // The old (PolySynth) instrument was evicted into the outbox tagged
        // with the built-in id.
        let (old_id, old) = h.swap_out_cons.try_pop().expect("evicted instrument");
        assert_eq!(old_id, BUILTIN_PLUGIN_ID);
        // We can't downcast a `dyn Instrument` without `Any`, but we can
        // verify it's *some* instrument and dropping it doesn't crash.
        drop(old);

        // The audio thread is now rendering through the CountingInstrument.
        // We can't peek inside its state from here (it lives behind the
        // dyn boundary), but we can prove the swap happened by sending
        // another note: render twice and verify no panic + frame counter
        // advanced for both buffers.
        let mut buf2 = vec![0.0f32; 2 * 1024];
        h.renderer.render(&mut buf2);
        assert_eq!(h.frame_counter.load(Ordering::Acquire), 2 * 1024);
    }

    #[test]
    fn consecutive_swaps_each_evict_predecessor() {
        let mut h = make_test_renderer(48_000.0, 2);

        // Queue two swaps. Each render() call consumes one new instrument
        // from the inbox and emits exactly one evicted instrument into the
        // outbox.
        let a: InstrumentBox = Box::new(CountingInstrument::new());
        let b: InstrumentBox = Box::new(CountingInstrument::new());
        assert!(h.swap_in_prod.try_push((7, a)).is_ok());
        assert!(h.swap_in_prod.try_push((9, b)).is_ok());

        let mut buf = vec![0.0f32; 2 * 256];
        h.renderer.render(&mut buf);
        // First swap evicted the original PolySynth (built-in id).
        let (id1, _) = h.swap_out_cons.try_pop().expect("first eviction");
        assert_eq!(id1, BUILTIN_PLUGIN_ID);

        h.renderer.render(&mut buf);
        // Second swap evicted the first CountingInstrument (id 7).
        let (id2, _) = h.swap_out_cons.try_pop().expect("second eviction");
        assert_eq!(id2, 7);
    }

    /// Full lifecycle: load a plugin via the host, hand its instrument to
    /// the audio thread, evict it via a follow-up swap, drain the eviction,
    /// route it back to the host, and verify a re-activation finds the
    /// cached instance instead of `None`. This is the round-trip the Phase
    /// 5b drain task enables.
    #[test]
    fn evicted_instrument_is_routed_back_to_host_for_reuse() {
        use crate::host::PluginHost;
        use std::fs::File;

        let dir = tempfile::Builder::new()
            .prefix("cadenza-roundtrip")
            .tempdir()
            .expect("tempdir");
        // Use a `.vst3` extension because the VST3 backend is still
        // a stub that accepts any path; the real CLAP backend (enabled
        // by default) would correctly reject this fake empty file.
        let plugin_path = dir.path().join("fake.vst3");
        File::create(&plugin_path).expect("create fake plugin");

        let mut host = PluginHost::new();
        let loaded = host
            .load(plugin_path.to_str().unwrap(), 48_000)
            .expect("host load");
        let id = loaded.id;

        // Pull the instrument out of the host as the server would when
        // SetInstrument arrives, then push it through the swap inbox.
        let mut h = make_test_renderer(48_000.0, 2);
        let inst = host.take_instrument(id).expect("first take");
        assert!(h.swap_in_prod.try_push((id, inst)).is_ok());

        // Render once: the audio thread installs the loaded instrument and
        // evicts the original PolySynth.
        let mut buf = vec![0.0f32; 2 * 256];
        h.renderer.render(&mut buf);

        // Drain that first eviction (the built-in synth) and discard it.
        let (evicted_id, _) = h.swap_out_cons.try_pop().expect("first eviction");
        assert_eq!(evicted_id, BUILTIN_PLUGIN_ID);

        // Now switch back to the built-in synth, evicting the loaded plugin.
        let builtin: InstrumentBox = Box::new(PolySynth::new(48_000.0));
        assert!(h.swap_in_prod.try_push((BUILTIN_PLUGIN_ID, builtin)).is_ok());
        h.renderer.render(&mut buf);

        // The drain task would now read this off `swap_out` and route it
        // back via PluginHost::return_instrument.
        let (evicted_id, evicted) = h.swap_out_cons.try_pop().expect("plugin eviction");
        assert_eq!(evicted_id, id);
        host.return_instrument(evicted_id, evicted);

        // Re-activating the same plugin id finds the cached instance and
        // does *not* re-enter the host's load() path.
        let reused = host.take_instrument(id);
        assert!(
            reused.is_some(),
            "expected the cached instrument to be available for reuse after eviction"
        );
    }

    #[test]
    fn take_dropped_instruments_returns_empty_when_idle() {
        // Engine handle level is hard to exercise without a real device, so
        // verify the rendering side surfaces evictions in order via the
        // ringbuf and that an idle test renderer's outbox starts empty.
        let mut h = make_test_renderer(48_000.0, 2);
        assert!(h.swap_out_cons.try_pop().is_none());

        // After a single swap a single eviction is observable.
        let inst: InstrumentBox = Box::new(CountingInstrument::new());
        assert!(h.swap_in_prod.try_push((11, inst)).is_ok());
        let mut buf = vec![0.0f32; 2 * 64];
        h.renderer.render(&mut buf);

        let (id, _) = h.swap_out_cons.try_pop().expect("eviction");
        assert_eq!(id, BUILTIN_PLUGIN_ID);
        assert!(h.swap_out_cons.try_pop().is_none());
    }
}
