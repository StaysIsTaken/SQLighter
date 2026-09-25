//! SQLighter - a modern, minimal and secure SQL client.

pub mod ai;
mod commands;
pub mod db;
pub mod error;
pub mod io;
pub mod mcp;
pub mod model;
pub mod sql;
pub mod sqlgen;
pub mod ssh;
pub mod state;
pub mod store;
pub mod tls;

use std::sync::Arc;

use tauri::Manager;

pub use state::AppState;

pub fn state(app: &tauri::AppHandle) -> Arc<AppState> {
    app.state::<Arc<AppState>>().inner().clone()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tls::install_default_provider();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let store = store::Store::open(dir)?;
            let state = Arc::new(AppState::new(app.handle().clone(), store));
            app.manage(state.clone());
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
                .title("SQLighter")
                .inner_size(1440.0, 900.0)
                .min_inner_size(900.0, 560.0)
                // HTML5 drag & drop is used by the connection tree.
                .disable_drag_drop_handler()
                // The web view may never navigate away from the bundled UI.
                .on_navigation(|url| {
                    matches!(url.scheme(), "tauri" | "asset") || matches!(url.host_str(), Some("localhost") | Some("tauri.localhost") | Some("127.0.0.1"))
                })
                .build()?;
            tauri::async_runtime::spawn(async move {
                if let Err(e) = mcp::apply_settings(state).await {
                    log::error!("MCP server: {e:#}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::set_ai_api_key,
            commands::get_tree,
            commands::save_connection,
            commands::delete_connection,
            commands::duplicate_connection,
            commands::save_folder,
            commands::delete_folder,
            commands::move_item,
            commands::test_connection,
            commands::fetch_server_cert,
            commands::answer_host_key,
            commands::answer_approval,
            commands::connect,
            commands::disconnect,
            commands::connected_ids,
            commands::schema_summary,
            commands::execute,
            commands::cancel_query,
            commands::commit,
            commands::rollback,
            commands::set_auto_commit,
            commands::list_objects,
            commands::describe_table,
            commands::get_ddl,
            commands::table_data,
            commands::apply_changes,
            commands::get_history,
            commands::clear_history,
            commands::pick_open_file,
            commands::pick_save_file,
            commands::read_text_file,
            commands::write_text_file,
            io::export_data,
            io::import_preview,
            io::import_data,
            io::transfer_data,
            io::backup,
            io::restore,
            io::cancel_task,
            io::native_tools,
            ai::ai_chat,
            ai::ai_cancel,
            ai::ai_list_models,
            ai::detect_claude,
            mcp::mcp_info,
            mcp::mcp_regenerate_token,
        ])
        .run(tauri::generate_context!())
        .expect("error while running SQLighter");
}
