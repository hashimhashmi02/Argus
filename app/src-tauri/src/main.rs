//! The Argus desktop UI.
//!
//! A client of `argus-engine`, nothing more. It owns no sessions: every agent
//! lives in the engine, so this window can be closed, crash, or be reopened
//! without any agent noticing. If the engine is not running, the UI offers to
//! start it -- detached, so it keeps running after the UI is gone.
//!
//! The webview polls for the session list and the tiled screens. The one pane
//! you are working in is streamed instead: a `watch` connection per zoomed
//! session, its events passed to the webview over a Tauri channel.

// No console window behind the app in release builds. Debug builds keep one,
// which is where Rust panics and logs show up during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod engine;

use argus_control_server::{default_pipe_name, pipe_path};

fn main() {
    // `ARGUS_PIPE` points the UI at a non-default engine, for running a test
    // engine alongside a real one. Scrubbed from agents by the `ARGUS_` prefix
    // rule like every other Argus variable.
    let pipe = std::env::var("ARGUS_PIPE")
        .map(|name| pipe_path(&name))
        .unwrap_or_else(|_| default_pipe_name());

    let launcher = engine::EngineLauncher::locate().expect("could not find this executable's path");

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(commands::Pipe(pipe))
        .manage(commands::Watches::default())
        .manage(launcher)
        .invoke_handler(tauri::generate_handler![
            commands::list_sessions,
            commands::list_agents,
            commands::read_screen,
            commands::spawn_session,
            commands::send_line,
            commands::send_text,
            commands::send_key,
            commands::resize_session,
            commands::respond,
            commands::kill_session,
            commands::resume_session,
            commands::watch_session,
            commands::unwatch_session,
            commands::start_engine,
            commands::stop_engine,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Argus UI");
}
