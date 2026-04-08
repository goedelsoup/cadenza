//! Cadenza shell — Tauri 2 native wrapper around `apps/cadenza-web` that
//! spawns and supervises `cadenza-daemon` for the user.
//!
//! ## What this does
//!
//! - Renders the SvelteKit static build at `apps/cadenza-web/build/` inside
//!   a single native window via the platform webview.
//! - Spawns `cadenza-daemon` as a child process at startup, stores its
//!   `Child` handle in the Tauri app state, and kills it on window close.
//! - **Restarts the daemon automatically if it crashes**, with exponential
//!   backoff (1s → 2s → 4s → 8s → 30s cap) and a rate limit of 5 restarts
//!   inside any 5-minute window. After a continuous uptime of 60s the
//!   backoff resets to 1s. State machine and tests live in
//!   [`supervisor`].
//! - Adds a system-tray / menu-bar icon with a daemon-status text label
//!   that polls the supervisor every 1s and reports `running (pid N)` /
//!   `restarting (attempt K, backoff Ks)` / `failing repeatedly` /
//!   `not started`.
//!
//! ## What this is NOT
//!
//! - Not an auto-updater. Bundle distribution is `cargo tauri build`
//!   on the user's own machine. Notarization, code-signing, sparkle, etc.
//!   are explicit Phase 5c work and are scoped out of the supervisor.
//! - Not a custom IPC layer. The web app continues to talk to the daemon
//!   over its WebSocket bridge (`ws://127.0.0.1:7878`), unchanged. The
//!   shell is purely additive — `nx run cadenza-web:dev` standalone still
//!   works exactly as before. The bridge in
//!   `packages/cadenza-api/src/daemon-bridge.ts` already does
//!   exponential-backoff reconnect on `onclose`, so when the supervisor
//!   respawns the daemon the frontend's header status indicator flips
//!   from `disconnected` back to `connected` automatically.
//! - Not a health-checked supervisor. We only observe the OS process
//!   state (`Child::try_wait`); there is no daemon-side IPC ping.
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
//! If none of the above resolves, the supervisor's spawner returns an
//! `io::Error`, which the supervisor treats as a crash and rate-limits
//! into `Failing`. The user can launch the daemon manually via
//! `mise run daemon` and the web app's existing WebSocket bridge will
//! pick it up — running daemons are entirely transparent to the shell.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod supervisor;

use std::io;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use tracing_subscriber::EnvFilter;

use supervisor::{DaemonSupervisor, Spawner, SupervisorConfig};

/// Walks the discovery order described in the module docs and returns
/// the first existing path. `None` means the user didn't build the
/// daemon yet — `default_spawner` translates that to an `io::Error` so
/// the supervisor's restart loop handles it the same way it would a
/// crash (rate-limited, surfaced in the tray label).
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
        // separately.
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

/// Production spawner: locate the binary, then `Command::new(...).spawn()`.
/// A missing binary is reported as `NotFound` so the supervisor records
/// it as a failed attempt rather than silently transitioning to a
/// "running" state with no child.
fn default_spawner() -> Spawner {
    Arc::new(|| -> io::Result<Child> {
        match locate_daemon_binary() {
            Some(path) => {
                tracing::info!("spawning cadenza-daemon from {}", path.display());
                Command::new(&path).spawn()
            }
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cadenza-daemon binary not found; build it with `cargo build -p cadenza-daemon` \
                 or set CADENZA_DAEMON_BIN",
            )),
        }
    })
}

/// Refresh the tray menu's status item with the supervisor's current
/// view. Cheap; called once on startup and again from the 1s polling
/// task immediately after `tick()`.
fn update_tray_status(app: &AppHandle) {
    let supervisor = app.state::<DaemonSupervisor>();
    let label = supervisor.status_label();

    // Tauri 2's MenuItem doesn't expose live text mutation directly —
    // we re-build the menu and assign it to the tray. The menu is small
    // (2 items) so this is cheap relative to the 1s tick rate.
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
        .manage(DaemonSupervisor::new(default_spawner(), SupervisorConfig::default()))
        .setup(|app| {
            // Spawn the daemon child first so the web app's WebSocket
            // bridge has a target by the time the window finishes loading.
            // If `start()` returns false, the supervisor has already
            // transitioned to `Restarting` (or `Failing`) and the tray
            // label will reflect that on the next tick.
            let supervisor = app.state::<DaemonSupervisor>();
            if !supervisor.start() {
                tracing::warn!(
                    "initial cadenza-daemon spawn failed; supervisor will retry with backoff. \
                     Build the daemon with `cargo build -p cadenza-daemon` or set CADENZA_DAEMON_BIN."
                );
            }

            // Build the tray icon with the initial status. The status
            // string updates from the polling task below; the menu items
            // ids are stable so the click handler can match on them.
            let initial_status = supervisor.status_label();
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

            // 1s tick to drive the supervisor state machine and refresh
            // the tray label. Spawn on a plain OS thread; we don't have
            // tokio in this crate and don't want to pull it in just for
            // a single sleep loop.
            let app_handle = app.handle().clone();
            std::thread::Builder::new()
                .name("shell-supervisor-poll".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(Duration::from_secs(1));
                        // tick() advances the state machine: observes
                        // crashes, schedules restarts, performs respawns
                        // when the backoff deadline has been reached.
                        app_handle.state::<DaemonSupervisor>().tick();
                        update_tray_status(&app_handle);
                    }
                })
                .ok();

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                // The user closed the main window. Reap the daemon
                // before Tauri tears down the app handle. shutdown()
                // sets state to NotStarted so the polling thread won't
                // try to respawn after this point.
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
