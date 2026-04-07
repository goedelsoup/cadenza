//! WebSocket control plane.
//!
//! Each connected client gets its own task that owns:
//!   - a clone of the shared `AudioEngine` handle (via Arc<Mutex<>>)
//!   - an optional `JoinHandle` for the currently-playing phrase
//!
//! Messages are JSON-encoded `DaemonMessage` values. The same enum is
//! used in both directions, mirrored in `cadenza-api/src/daemon-bridge.ts`.

use crate::audio::{AudioCmd, AudioEngine, TimedCmd};
use crate::host::PluginHost;
use crate::instrument::{InstrumentBox, BUILTIN_PLUGIN_ID};
use crate::scheduler;
use crate::synth::PolySynth;
use crate::DynError;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use cadenza_ipc::DaemonMessage;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Clone)]
struct AppState {
    engine: Option<Arc<Mutex<AudioEngine>>>,
    host:   Arc<Mutex<PluginHost>>,
}

pub async fn serve(addr: SocketAddr, engine: Option<AudioEngine>) -> Result<(), DynError> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("cadenza-daemon listening on ws://{addr}");
    axum::serve(listener, app(engine)).await?;
    Ok(())
}

/// Build the axum router with the given (possibly absent) audio engine.
/// Exposed so integration tests can mount the same routes against a
/// stubbed engine and an ephemeral port.
///
/// When an engine is present this also spawns a tokio task that ticks
/// every 100ms, drains any instruments the audio thread has evicted, and
/// returns each one to its [`PluginHost`] entry so re-activation is a hot
/// swap rather than a reload from disk. Built-in synth evictions arrive
/// tagged with [`BUILTIN_PLUGIN_ID`] and are dropped on the control thread.
pub fn app(engine: Option<AudioEngine>) -> Router {
    let state = AppState {
        engine: engine.map(|e| Arc::new(Mutex::new(e))),
        host:   Arc::new(Mutex::new(PluginHost::new())),
    };
    if let Some(engine) = state.engine.clone() {
        let host = state.host.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
            loop {
                tick.tick().await;
                let dropped = {
                    let mut e = engine.lock().await;
                    e.take_dropped_instruments()
                };
                if dropped.is_empty() {
                    continue;
                }
                let mut h = host.lock().await;
                for (id, inst) in dropped {
                    if id == BUILTIN_PLUGIN_ID {
                        // The built-in synth has no host entry; just drop
                        // it here on the control thread (drop allocates,
                        // which is forbidden on the audio thread).
                        drop(inst);
                    } else {
                        h.return_instrument(id, inst);
                    }
                }
            }
        });
    }
    Router::new().route("/", get(ws_handler)).with_state(state)
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut tx, mut rx) = socket.split();
    let mut current_playback: Option<JoinHandle<()>> = None;

    while let Some(Ok(msg)) = rx.next().await {
        let text = match msg {
            Message::Text(t)  => t,
            Message::Close(_) => break,
            _ => continue,
        };

        let parsed = match DaemonMessage::from_json(&text) {
            Ok(m) => m,
            Err(e) => {
                let _ = tx
                    .send(Message::Text(reply(DaemonMessage::Error(format!("bad message: {e}")))))
                    .await;
                continue;
            }
        };

        let response: Option<DaemonMessage> = match parsed {
            DaemonMessage::Ping => Some(DaemonMessage::Pong),

            DaemonMessage::PlayPhrase { phrase, plugin_id: _ } => {
                let Some(engine) = state.engine.clone() else {
                    let _ = tx
                        .send(Message::Text(reply(DaemonMessage::Error("audio engine unavailable".into()))))
                        .await;
                    continue;
                };
                // Cancel any in-flight playback before starting a new one.
                if let Some(handle) = current_playback.take() {
                    handle.abort();
                    engine.lock().await.send(AudioCmd::AllNotesOff);
                }
                let engine_for_task = engine.clone();
                current_playback = Some(tokio::spawn(async move {
                    scheduler::play_phrase(engine_for_task, phrase).await;
                }));
                Some(DaemonMessage::PlaybackStarted)
            }

            DaemonMessage::Stop => {
                if let Some(handle) = current_playback.take() { handle.abort(); }
                if let Some(engine) = &state.engine {
                    engine.lock().await.send(AudioCmd::AllNotesOff);
                }
                Some(DaemonMessage::PlaybackStopped)
            }

            DaemonMessage::SetTempo(_) => {
                // Tempo applies per-phrase today. A global override would
                // live on the engine handle in Phase 5b.
                None
            }

            DaemonMessage::ScanPlugins { dir } => {
                // Filesystem walk runs on the blocking pool — never on the
                // tokio reactor and never on the audio thread.
                let host = state.host.clone();
                let join = tokio::task::spawn_blocking(move || {
                    let h = host.blocking_lock();
                    h.scan(std::path::Path::new(&dir))
                }).await;
                Some(match join {
                    Ok(Ok(paths)) => DaemonMessage::ScannedPlugins { paths },
                    Ok(Err(e))    => DaemonMessage::Error(e.to_string()),
                    Err(join_err) => DaemonMessage::Error(format!("scan task: {join_err}")),
                })
            }

            DaemonMessage::LoadPlugin { path } => {
                let Some(engine) = state.engine.clone() else {
                    let _ = tx
                        .send(Message::Text(reply(DaemonMessage::Error("audio engine unavailable".into()))))
                        .await;
                    continue;
                };
                let sample_rate = engine.lock().await.sample_rate;
                let host = state.host.clone();
                let join = tokio::task::spawn_blocking(move || {
                    let mut h = host.blocking_lock();
                    h.load(&path, sample_rate)
                }).await;
                Some(match join {
                    Ok(Ok(p))     => DaemonMessage::PluginLoaded {
                        id: p.id, name: p.name, params: p.params,
                    },
                    Ok(Err(e))    => DaemonMessage::Error(e.to_string()),
                    Err(join_err) => DaemonMessage::Error(format!("load task: {join_err}")),
                })
            }

            DaemonMessage::UnloadPlugin { id } => {
                let mut host = state.host.lock().await;
                match host.unload(id) {
                    Ok(()) => Some(DaemonMessage::PluginUnloaded { id }),
                    Err(e) => Some(DaemonMessage::Error(e.to_string())),
                }
            }

            DaemonMessage::SetInstrument { plugin_id } => {
                let Some(engine) = state.engine.clone() else {
                    let _ = tx
                        .send(Message::Text(reply(DaemonMessage::Error("audio engine unavailable".into()))))
                        .await;
                    continue;
                };
                // Take the boxed instrument from the host on the control side.
                let inst_opt = {
                    let mut host = state.host.lock().await;
                    host.take_instrument(plugin_id)
                };
                let Some(inst) = inst_opt else {
                    let _ = tx
                        .send(Message::Text(reply(DaemonMessage::Error(format!(
                            "plugin {plugin_id} not loaded or already active"
                        )))))
                        .await;
                    continue;
                };
                if engine.lock().await.swap_instrument(plugin_id, inst) {
                    Some(DaemonMessage::PluginActivated { id: plugin_id })
                } else {
                    Some(DaemonMessage::Error("instrument swap failed".into()))
                }
            }

            DaemonMessage::UseBuiltinSynth => {
                let Some(engine) = state.engine.clone() else {
                    let _ = tx
                        .send(Message::Text(reply(DaemonMessage::Error("audio engine unavailable".into()))))
                        .await;
                    continue;
                };
                let mut e = engine.lock().await;
                let inst: InstrumentBox = Box::new(PolySynth::new(e.sample_rate as f32));
                if e.swap_instrument(BUILTIN_PLUGIN_ID, inst) {
                    Some(DaemonMessage::BuiltinSynthActivated)
                } else {
                    Some(DaemonMessage::Error("instrument swap failed".into()))
                }
            }

            DaemonMessage::SetParam { plugin_id: _, param_id, value } => {
                // Parameter automation rides the same SPSC ringbuf as note
                // events. The active instrument is responsible for routing
                // the `ParamSet` to its backend's native event format —
                // CLAP wraps it in a `ParamValueEvent`; PolySynth and the
                // current VST3 backend silently ignore it.
                //
                // Why no `plugin_id` validation: the audio thread always
                // dispatches events to whichever instrument is currently
                // installed. If the client sends a `SetParam` for a plugin
                // that isn't active, the event still flows through but the
                // installed instrument has no matching `param_id` and
                // ignores it. Adding a server-side check against
                // `current_active_id` would tighten this but requires
                // tracking that state across messages — a follow-up.
                let Some(engine) = state.engine.clone() else {
                    let _ = tx
                        .send(Message::Text(reply(DaemonMessage::Error("audio engine unavailable".into()))))
                        .await;
                    continue;
                };
                let mut e = engine.lock().await;
                let frame = e.now_frame();
                e.send_timed(TimedCmd {
                    frame,
                    cmd: AudioCmd::ParamSet { param_id, value },
                });
                // Fire-and-forget on success — no reply expected.
                None
            }

            // Outbound-only variants — silently ignore if a client sends them.
            DaemonMessage::ScannedPlugins { .. }
            | DaemonMessage::PluginLoaded { .. }
            | DaemonMessage::PluginUnloaded { .. }
            | DaemonMessage::PluginActivated { .. }
            | DaemonMessage::BuiltinSynthActivated
            | DaemonMessage::PlaybackStarted
            | DaemonMessage::PlaybackStopped
            | DaemonMessage::Pong
            | DaemonMessage::Error(_) => None,
        };

        if let Some(out) = response {
            if tx.send(Message::Text(reply(out))).await.is_err() {
                break;
            }
        }
    }

    if let Some(handle) = current_playback.take() { handle.abort(); }
    if let Some(engine) = &state.engine {
        engine.lock().await.send(AudioCmd::AllNotesOff);
    }
}

fn reply(msg: DaemonMessage) -> String {
    msg.to_json().unwrap_or_else(|e| format!(r#"{{"Error":"serialize failed: {e}"}}"#))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as WsMsg;

    /// Spin up the router on an ephemeral port and return its address.
    /// The server has no audio engine, so PlayPhrase will return an Error
    /// — but Ping/malformed cases exercise the protocol path.
    async fn spawn_server() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let app = app(None);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        addr
    }

    async fn connect(addr: SocketAddr) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
        let url = format!("ws://{addr}/");
        let (ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        ws
    }

    #[tokio::test]
    async fn ping_returns_pong() {
        let addr = spawn_server().await;
        let mut ws = connect(addr).await;
        ws.send(WsMsg::Text(DaemonMessage::Ping.to_json().unwrap()))
            .await
            .expect("send");
        let reply = ws.next().await.expect("some").expect("ok");
        let text = match reply {
            WsMsg::Text(t) => t,
            other => panic!("unexpected ws frame: {other:?}"),
        };
        let parsed = DaemonMessage::from_json(&text).expect("parse");
        assert!(matches!(parsed, DaemonMessage::Pong));
    }

    #[tokio::test]
    async fn malformed_message_returns_error() {
        let addr = spawn_server().await;
        let mut ws = connect(addr).await;
        ws.send(WsMsg::Text("{not valid json".into())).await.expect("send");
        let reply = ws.next().await.expect("some").expect("ok");
        let text = match reply {
            WsMsg::Text(t) => t,
            other => panic!("unexpected ws frame: {other:?}"),
        };
        let parsed = DaemonMessage::from_json(&text).expect("parse");
        match parsed {
            DaemonMessage::Error(msg) => assert!(msg.contains("bad message")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn play_phrase_without_engine_errors() {
        let addr = spawn_server().await;
        let mut ws = connect(addr).await;
        let phrase = cadenza_theory::phrase::Phrase::new(
            1,
            "t",
            cadenza_theory::rhythm::TimeSignature::four_four(),
            120,
        );
        let msg = DaemonMessage::PlayPhrase { phrase, plugin_id: None };
        ws.send(WsMsg::Text(msg.to_json().unwrap())).await.expect("send");
        let reply = ws.next().await.expect("some").expect("ok");
        let text = match reply {
            WsMsg::Text(t) => t,
            other => panic!("unexpected ws frame: {other:?}"),
        };
        let parsed = DaemonMessage::from_json(&text).expect("parse");
        assert!(matches!(parsed, DaemonMessage::Error(_)));
    }
}
