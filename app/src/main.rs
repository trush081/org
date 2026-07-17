//! `org` desktop app — Tauri wiring only. Opens the same database as the CLI
//! ($ORG_DB, else ~/.org/org.db) and exposes org-core over IPC. All logic
//! lives in core; this crate is the GUI counterpart of cli/src/main.rs.

// Hide the console window on Windows release builds (no-op on macOS/Linux).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use org_core::{Config, Db};
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // One Db for the app's lifetime, opened before any command can
            // fire. setup() is sync, so block on the async open here — this is
            // the one place the app waits on the database.
            let db = tauri::async_runtime::block_on(Db::open(Config::resolve(None)))?;
            // manage() puts the Db in Tauri's state map; commands borrow it
            // via State<'_, Db> instead of a global.
            app.manage(db);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search,
            commands::who,
            commands::tree,
        ])
        .run(tauri::generate_context!())
        .expect("error while running org app");
}
