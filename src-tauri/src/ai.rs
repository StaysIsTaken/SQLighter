use crate::error::AppResult;
use crate::model::AiProviderConfig;
pub fn validate_provider(_p: &AiProviderConfig) -> AppResult<()> { Ok(()) }
#[tauri::command] pub async fn ai_chat() -> AppResult<()> { Ok(()) }
#[tauri::command] pub async fn ai_cancel() -> AppResult<()> { Ok(()) }
#[tauri::command] pub async fn ai_list_models() -> AppResult<()> { Ok(()) }
#[tauri::command] pub async fn detect_claude() -> AppResult<()> { Ok(()) }
