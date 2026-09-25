use std::sync::Arc;
use crate::error::AppResult;
#[derive(Default)] pub struct McpState {}
pub async fn apply_settings(_s: Arc<crate::AppState>) -> anyhow::Result<()> { Ok(()) }
#[tauri::command] pub async fn mcp_info() -> AppResult<()> { Ok(()) }
#[tauri::command] pub async fn mcp_regenerate_token() -> AppResult<()> { Ok(()) }
