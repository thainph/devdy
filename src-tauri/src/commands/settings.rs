use crate::db::Db;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tauri::State;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppSettings {
    pub default_engine: String,
    pub claude_path: String,
    pub codex_path: String,
    /// Default model for Claude runs (SDK alias: ""=engine default, opus, sonnet, haiku).
    pub claude_model: String,
    /// Default model for Codex runs ("" = engine default, e.g. gpt-5-codex, gpt-5).
    pub codex_model: String,
    pub extra_args: String,
    pub theme: String,
    /// Named color palette: "default" | "ocean" | "forest" | "sunset" | "rose".
    pub color_theme: String,
    pub analyze_issue_prompt: String,
    pub review_pr_prompt: String,
    pub default_permission_mode: String,
    /// Terminal app to open a project folder with ("terminal" = macOS Terminal.app, "iterm").
    pub terminal_app: String,
    /// Context-window meter: warn threshold as a percent string, e.g. "80".
    pub context_warn_percent: String,
    /// Context-window limit override in tokens ("" = auto-resolve from model).
    pub context_limit_override: String,
    /// Global token budget period: "week" | "5h" (legacy "month" → treated as weekly).
    pub token_budget_period: String,
    /// Global token budget limit in tokens ("" = feature disabled).
    pub token_budget_limit: String,
    /// Budget warn threshold as a percent string, e.g. "80".
    pub budget_warn_percent: String,
}

#[tauri::command]
pub async fn get_settings(db: State<'_, Db>) -> Result<AppSettings, String> {
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let mut settings = AppSettings {
        default_engine: "claude".to_string(),
        claude_path: "claude".to_string(),
        codex_path: "codex".to_string(),
        claude_model: "".to_string(),
        codex_model: "".to_string(),
        extra_args: "".to_string(),
        theme: "system".to_string(),
        color_theme: "default".to_string(),
        analyze_issue_prompt: "Please analyze the GitHub issue described in the file and create a detailed implementation plan.".to_string(),
        review_pr_prompt: "Please review the pull request described in the file according to the configured skills.".to_string(),
        default_permission_mode: "default".to_string(),
        terminal_app: "terminal".to_string(),
        context_warn_percent: "80".to_string(),
        context_limit_override: "".to_string(),
        token_budget_period: "week".to_string(),
        token_budget_limit: "".to_string(),
        budget_warn_percent: "80".to_string(),
    };

    for row in rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "default_engine" => settings.default_engine = value,
            "claude_path" => settings.claude_path = value,
            "codex_path" => settings.codex_path = value,
            "claude_model" => settings.claude_model = value,
            "codex_model" => settings.codex_model = value,
            "extra_args" => settings.extra_args = value,
            "theme" => settings.theme = value,
            "color_theme" => settings.color_theme = value,
            "analyze_issue_prompt" => settings.analyze_issue_prompt = value,
            "review_pr_prompt" => settings.review_pr_prompt = value,
            "default_permission_mode" => settings.default_permission_mode = value,
            "terminal_app" => settings.terminal_app = value,
            "context_warn_percent" => settings.context_warn_percent = value,
            "context_limit_override" => settings.context_limit_override = value,
            "token_budget_period" => settings.token_budget_period = value,
            "token_budget_limit" => settings.token_budget_limit = value,
            "budget_warn_percent" => settings.budget_warn_percent = value,
            _ => {}
        }
    }

    Ok(settings)
}

/// Resolve the global default engine, falling back to "claude" when unset or
/// empty. Used by run-creation paths now that engine is no longer stored
/// per-project.
pub async fn resolve_default_engine(db: &Db) -> String {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = 'default_engine'")
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "claude".to_string())
}

#[tauri::command]
pub async fn update_setting(
    db: State<'_, Db>,
    key: String,
    value: String,
) -> Result<(), String> {
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(&key)
        .bind(&value)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
