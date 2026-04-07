//! cadenza-daemon — native audio/VST host.
//!
//! Three threads:
//!   1. tokio runtime  — WebSocket server, plugin scanning, control plane
//!   2. scheduler      — converts `Phrase` events to timed `AudioCmd`s
//!   3. cpal callback  — drains the SPSC ringbuf and renders audio
//!
//! The audio thread never allocates and never blocks. All buffers are
//! pre-allocated at startup. The scheduler is the only producer for the
//! ringbuf; the audio callback is the only consumer.

mod audio;
mod host;
mod instrument;
mod scheduler;
mod server;
mod synth;

use std::error::Error;
use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;

pub type DynError = Box<dyn Error + Send + Sync>;

const DEFAULT_ADDR: &str = "127.0.0.1:7878";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), DynError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "cadenza_daemon=info".into()),
        )
        .init();

    let addr: SocketAddr = std::env::var("CADENZA_DAEMON_ADDR")
        .unwrap_or_else(|_| DEFAULT_ADDR.into())
        .parse()?;

    // Boot the audio engine first so the WebSocket can immediately accept
    // PlayPhrase commands. If audio fails to come up, we still serve the
    // WebSocket and report the error to clients on connect.
    let engine = match audio::AudioEngine::start() {
        Ok(e) => Some(e),
        Err(e) => {
            tracing::error!("audio engine failed to start: {e}");
            None
        }
    };

    server::serve(addr, engine).await
}
