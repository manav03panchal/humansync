mod commands;

use std::path::PathBuf;

use tokio::sync::Mutex;

pub use commands::*;

/// Managed Tauri state holding the HumanSync node.
pub struct AppState {
    pub node: Mutex<Option<std::sync::Arc<humansync::HumanSync>>>,
    pub data_dir: PathBuf,
}

/// Build and run the Tauri application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt::init();

    // Allow overriding data dir for multi-instance testing:
    //   HUMANDOCS_DATA_DIR=/tmp/device2 cargo run -p humandocs
    let data_dir = std::env::var("HUMANDOCS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("humandocs")
        });

    std::fs::create_dir_all(&data_dir).expect("failed to create humandocs data directory");

    let app_state = AppState {
        node: Mutex::new(None),
        data_dir,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::init_app,
            commands::is_paired,
            commands::list_documents,
            commands::create_document,
            commands::open_document,
            commands::save_document,
            commands::delete_document,
            commands::add_attachment,
            commands::get_attachment,
            commands::get_sync_status,
            commands::sync_now,
            commands::update_cursor,
            commands::get_cursors,
            commands::get_device_id,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Humandocs");
}
