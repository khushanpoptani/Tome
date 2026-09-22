mod storage;

use storage::ClientStorage;
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_http::init())
        .setup(|app| {
            let root = app.path().app_data_dir()?;
            app.manage(ClientStorage::open(root)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            storage::bootstrap,
            storage::create_chat,
            storage::get_chat,
            storage::save_chat,
            storage::rename_chat,
            storage::delete_chat,
            storage::search_chats,
            storage::import_chat,
            storage::export_chat,
            storage::export_chat_file,
            storage::save_settings,
            storage::upsert_profile,
            storage::delete_profile,
            storage::set_event_cursor,
            storage::save_partial_response,
            storage::load_partial_responses,
            storage::clear_partial_response,
            storage::write_interaction_export,
            storage::store_attachment,
            storage::delete_all_preview,
            storage::delete_all_data,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Tome client");
}
