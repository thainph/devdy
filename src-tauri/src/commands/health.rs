#[tauri::command]
pub async fn health_check() -> Result<String, String> {
    Ok("OK".to_string())
}
