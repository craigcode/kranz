//! Kranz Mission Control desktop shell.
//!
//! On startup the app embeds `kranz_server::serve` on a runtime-chosen free
//! localhost port, then builds the main window programmatically so it can
//! carry an initialization script announcing that port to the frontend
//! (`window.__KRANZ_SERVER__` — the UI reads only that global; the
//! `get_server_url` / `get_repo_root` commands exist as an IPC fallback).

use std::net::TcpListener;
use std::path::PathBuf;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// Managed state exposed to the frontend via IPC commands.
struct ServerInfo {
    url: String,
    repo_root: PathBuf,
}

/// Base URL of the embedded kranz server, e.g. `http://127.0.0.1:52341`.
#[tauri::command]
fn get_server_url(state: tauri::State<'_, ServerInfo>) -> String {
    state.url.clone()
}

/// Absolute path of the kranz repo the embedded server is reading.
#[tauri::command]
fn get_repo_root(state: tauri::State<'_, ServerInfo>) -> String {
    state.repo_root.display().to_string()
}

/// Repo root resolution order: `KRANZ_REPO` env var, first non-flag CLI arg,
/// current working directory.
fn resolve_repo_root() -> PathBuf {
    let raw = std::env::var("KRANZ_REPO")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::args()
                .nth(1)
                .filter(|a| !a.starts_with('-'))
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| {
            std::env::current_dir().expect("cannot resolve current working directory")
        });
    // Best-effort absolutization so the server and `get_repo_root` report a
    // stable path regardless of where the app was launched from.
    std::fs::canonicalize(&raw).unwrap_or(raw)
}

/// Bind 127.0.0.1:0 to let the OS pick a free port, then release it so the
/// embedded server can bind it a moment later.
fn pick_free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("failed to bind 127.0.0.1:0");
    let port = listener
        .local_addr()
        .expect("listener has no local addr")
        .port();
    drop(listener);
    port
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_server_url, get_repo_root])
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            let repo_root = resolve_repo_root();
            let port = pick_free_port();
            let url = format!("http://127.0.0.1:{port}");
            // Per-launch mutation token (protocol "Authority: mutation
            // token"): every POST /api/... must carry it, so other local
            // processes cannot steer missions through our embedded server.
            let token = kranz_server::generate_token();
            log::info!(
                "starting embedded kranz server on {url} (repo root: {})",
                repo_root.display()
            );

            // Embedded REST/WS server (docs/protocol.md). `static_dir: None`:
            // the webview loads the bundled frontend itself and talks to the
            // server for /api only.
            {
                let repo_root = repo_root.clone();
                let token = token.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = kranz_server::serve(repo_root, port, None, Some(token)).await
                    {
                        log::error!("embedded kranz server exited: {err:#}");
                    }
                });
            }

            app.manage(ServerInfo {
                url: url.clone(),
                repo_root,
            });

            // The port is only known at runtime, so the main window is built
            // here (not in tauri.conf.json) to attach an initialization
            // script that runs before any frontend code.
            let init_script = format!(
                "window.__KRANZ_SERVER__ = {}; window.__KRANZ_TOKEN__ = {};",
                serde_json::to_string(&url).expect("a string always serializes"),
                serde_json::to_string(&token).expect("a string always serializes")
            );
            WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("Kranz Mission Control")
                .inner_size(1280.0, 800.0)
                .min_inner_size(1000.0, 640.0)
                .initialization_script(&init_script)
                .build()?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
