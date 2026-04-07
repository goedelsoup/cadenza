//! Cadenza shell — Tauri 2 native wrapper around `apps/cadenza-web` that
//! spawns and supervises `cadenza-daemon` for the user.
//!
//! ## What this does
//!
//! - Renders the SvelteKit static build at `apps/cadenza-web/build/` inside
//!   a single native window via the platform webview.
//! - Spawns `cadenza-daemon` as a child process at startup, stores its
//!   `Child` handle in the Tauri app state, and kills it on window close.
//! - Adds a system-tray / menu-bar icon with a daemon-status text label
//!   that polls the child every 1s and reports `running` / `stopped` /
//!   `crashed (exit N)`.
//!
//! ## What this is NOT
//!
//! - Not an auto-updater. Bundle distribution is `cargo tauri build`
//!   on the user's own machine. Notarization, code-signing, sparkle, etc.
//!   are explicit Phase 5c work.
//! - Not a custom IPC layer. The web app continues to talk to the daemon
//!   over its WebSocket bridge (`ws://127.0.0.1:7878`), unchanged. The
//!   shell is purely additive — `nx run cadenza-web:dev` standalone still
//!   works exactly as before.
//! - Not a daemon-restart manager. If the daemon crashes the tray label
//!   updates but the shell doesn't try to relaunch it. Restart-on-crash
//!   is Phase 5c.
//!
//! ## Daemon binary discovery
//!
//! Walked in this order, first match wins:
//! 1. `CADENZA_DAEMON_BIN` env var (absolute path).
//! 2. `<exe_dir>/cadenza-daemon[.exe]` — the bundled-app case after
//!    `cargo tauri build`. Tauri places sidecar binaries next to the
//!    shell binary inside `Contents/MacOS/` (macOS) or the install dir
//!    on Linux/Windows.
//! 3. `<workspace>/target/{debug,release}/cadenza-daemon[.exe]` — the
//!    development case, where you run `cargo run -p cadenza-shell` and
//!    the daemon was previously built via `cargo build -p cadenza-daemon`.
//!    Workspace root is detected by walking up from the exe directory
//!    looking for the `target/` directory.
//!
//! If none of the above resolves, the shell logs a warning and continues
//! without spawning a daemon. The web app's existing Tone.js fallback
//! kicks in automatically. The user can launch the daemon manually via
//! `mise run daemon` and the shell will pick it up via the existing
//! WebSocket bridge — running daemons are entirely transparent to the
//! shell, since the shell doesn't track WebSocket state.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::Duration;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tracing_subscriber::EnvFilter;

/// Wraps the supervised child so we can mutate it from multiple Tauri
/// callbacks (setup, tray menu click, window event, polling task) without
/// fighting Tauri's `state()` borrow rules.
struct DaemonSupervisor {
    child: Mutex<Option<Child>>,
}

impl DaemonSupervisor {
    fn new() -> Self {
        Self { child: Mutex::new(None) }
    }

    /// Best-effort liveness check. Returns one of:
    /// - `"not started"` — `spawn_daemon` was never called or failed.
    /// - `"running"`     — child still has no exit status.
    /// - `"exited (N)"`  — child finished cleanly with code N.
    /// - `"signaled"`    — child finished by signal (no exit code on Unix).
    fn status(&self) -> String {
        let mut guard = match self.child.lock() {
            Ok(g) => g,
            Err(_) => return "lock poisoned".to_string(),
        };
        match guard.as_mut() {
            None => "not started".to_string(),
            Some(child) => match child.try_wait() {
                Ok(None) => "running".to_string(),
                Ok(Some(status)) => match status.code() {
                    Some(code) => format!("exited ({code})"),
                    None       => "signaled".to_string(),
                },
                Err(e) => format!("query failed: {e}"),
            },
        }
    }

    /// Tear down the supervised child. Idempotent — safe to call from
    /// both the window-close handler and the global RunEvent::Exit hook
    /// without double-killing.
    fn shutdown(&self) {
        let mut guard = match self.child.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(mut child) = guard.take() {
            // Send SIGKILL on Unix / TerminateProcess on Windows. The
            // daemon's tokio runtime doesn't install any signal handlers
            // so a graceful SIGTERM wouldn't be observed anyway.
            let _ = child.kill();
            let _ = child.wait();
            tracing::info!("cadenza-daemon child reaped");
        }
    }
}

/// Walks the discovery order described in the module docs and returns
/// the first existing path. `None` means the user didn't build the
/// daemon yet — the shell logs and continues, the web app falls back to
/// Tone.js, and the user gets the same experience as `nx run cadenza-web:dev`
/// without `mise run daemon`.
fn locate_daemon_binary() -> Option<PathBuf> {
    // 1. Env var override.
    if let Some(p) = std::env::var_os("CADENZA_DAEMON_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
        tracing::warn!(
            "CADENZA_DAEMON_BIN points at {} but the file does not exist",
            path.display()
        );
    }

    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?.to_path_buf();
    let binary_name = if cfg!(windows) { "cadenza-daemon.exe" } else { "cadenza-daemon" };

    // 2. Sibling of the shell exe — bundled-app case.
    let sibling = exe_dir.join(binary_name);
    if sibling.exists() {
        return Some(sibling);
    }

    // 3. Workspace target/{debug,release}/ — dev case. Walk up from the
    //    exe directory looking for the workspace `target` dir. The exe
    //    typically lives inside `target/debug/` already, so the parent
    //    of `exe_dir` is `target/`.
    let mut search = Some(exe_dir.as_path());
    while let Some(dir) = search {
        // If we're inside `target/{profile}/...`, the daemon binary lives
        // alongside ours. cargo run -p cadenza-shell on a fresh checkout
        // does NOT also build cadenza-daemon, so this lookup will only
        // succeed when the user has run `cargo build -p cadenza-daemon`
        // separately (or our shell setup callback has done so on demand,
        // which is out of scope for v1).
        if dir.file_name().and_then(|n| n.to_str()) == Some("debug")
            || dir.file_name().and_then(|n| n.to_str()) == Some("release")
        {
            let candidate = dir.join(binary_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        search = dir.parent();
    }
    None
}

fn spawn_daemon() -> Option<Child> {
    let path = locate_daemon_binary()?;
    tracing::info!("spawning cadenza-daemon from {}", path.display());
    match Command::new(&path).spawn() {
        Ok(child) => {
            tracing::info!("cadenza-daemon child pid {}", child.id());
            Some(child)
        }
        Err(e) => {
            tracing::error!("failed to spawn cadenza-daemon at {}: {e}", path.display());
            None
        }
    }
}

/// Refresh the tray menu's status item with the supervisor's current
/// view. Cheap; called once on startup and again from a 1s polling task.
fn update_tray_status(app: &AppHandle) {
    let supervisor = app.state::<DaemonSupervisor>();
    let label = format!("daemon: {}", supervisor.status());

    // Tauri 2's MenuItem doesn't expose live text mutation directly —
    // we re-build the menu and assign it to the tray. The menu is small
    // (3 items) so this is cheap relative to the 1s tick rate.
    let status_item = MenuItemBuilder::with_id("status", &label).enabled(false).build(app);
    let quit_item   = MenuItemBuilder::with_id("quit", "Quit Cadenza").build(app);
    let (Ok(status_item), Ok(quit_item)) = (status_item, quit_item) else {
        tracing::warn!("failed to construct tray menu items for status update");
        return;
    };

    let menu = MenuBuilder::new(app).items(&[&status_item, &quit_item]).build();
    let Ok(menu) = menu else {
        tracing::warn!("failed to build tray menu for status update");
        return;
    };

    if let Some(tray) = app.tray_by_id("cadenza-tray") {
        let _ = tray.set_menu(Some(menu));
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "cadenza_shell=info".into()),
        )
        .init();

    tauri::Builder::default()
        .manage(DaemonSupervisor::new())
        .setup(|app| {
            // Spawn the daemon child first so the web app's WebSocket
            // bridge has a target by the time the window finishes loading.
            // `spawn_daemon` returns `Option<Child>`; if it failed to
            // locate or launch a binary the supervisor stays in its
            // "not started" state and the tray label says so.
            let child = spawn_daemon();
            if child.is_none() {
                tracing::warn!(
                    "no cadenza-daemon binary found; web app will fall back to Tone.js. \
                     Build the daemon with `cargo build -p cadenza-daemon` or set CADENZA_DAEMON_BIN."
                );
            }
            *app.state::<DaemonSupervisor>().child.lock().expect("supervisor lock") = child;

            // Build the tray icon with the initial status. The status
            // string updates from the polling task below; the menu items
            // ids are stable so the click handler can match on them.
            let initial_status = format!("daemon: {}", app.state::<DaemonSupervisor>().status());
            let status_item = MenuItemBuilder::with_id("status", &initial_status).enabled(false).build(app)?;
            let quit_item   = MenuItemBuilder::with_id("quit", "Quit Cadenza").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&status_item, &quit_item]).build()?;

            let _tray = TrayIconBuilder::with_id("cadenza-tray")
                .menu(&menu)
                .tooltip("Cadenza")
                .on_menu_event(|app, event| {
                    if event.id().as_ref() == "quit" {
                        // Tear the supervised daemon down explicitly
                        // before exiting so the user doesn't see an
                        // orphaned process in `ps`. The RunEvent::Exit
                        // hook would also do this, but the path through
                        // `app.exit(0)` doesn't always reach that hook
                        // before the process leaves the Tauri runtime.
                        app.state::<DaemonSupervisor>().shutdown();
                        app.exit(0);
                    }
                })
                .build(app)?;

            // 1s tick to refresh the tray label. Spawn on a plain OS
            // thread; we don't have tokio in this crate and don't want
            // to pull it in just for a single sleep loop.
            let app_handle = app.handle().clone();
            std::thread::Builder::new()
                .name("shell-status-poll".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(Duration::from_secs(1));
                        update_tray_status(&app_handle);
                    }
                })
                .ok();

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                // The user closed the main window. Reap the daemon
                // before Tauri tears down the app handle.
                window.app_handle().state::<DaemonSupervisor>().shutdown();
            }
        })
        .build(tauri::generate_context!())
        .expect("error building Tauri application")
        .run(|app_handle, event| {
            // Belt-and-braces: also reap on the global Exit hook in case
            // the user quit via Cmd-Q rather than closing the window.
            if let RunEvent::Exit = event {
                app_handle.state::<DaemonSupervisor>().shutdown();
            }
        });
}
