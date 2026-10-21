//! Grammar check — proofreads a short text on the shared warm translation
//! sidecar (`TranslateState`), so it reuses the same engine / model / account
//! settings and never pays a separate cold start. Used by the Inbox API
//! (`POST /v1/grammar`).
//!
//! The model answers with a JSON object (`corrected` + `issues`), parsed by
//! `parse_result`; explanations are written in `explain_lang` (defaults to the
//! user's translate target language).

use crate::commands::translate::{self, TranslateState, TurnEvent};
use crate::db::Db;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tokio::sync::mpsc::UnboundedReceiver;

/// One problem found in the text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GrammarIssue {
    /// The erroneous fragment, as written in the input.
    #[serde(default)]
    pub original: String,
    /// Its replacement.
    #[serde(default)]
    pub suggestion: String,
    /// Short reason, in the explanation language.
    #[serde(default)]
    pub explanation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GrammarResult {
    /// The full text with every error fixed (equal to the input when clean).
    pub corrected: String,
    #[serde(default)]
    pub issues: Vec<GrammarIssue>,
}

fn build_prompt(source: &str, explain_name: &str) -> String {
    format!(
        "You are a meticulous proofreader. Check the grammar, spelling, punctuation and word \
         choice of the text delimited by <<<>>>, in whatever language it is written. Do NOT \
         translate it.\n\
         Reply with ONLY a JSON object — no markdown fences, no preamble, no notes:\n\
         {{\"corrected\": \"<the full text with every error fixed; identical to the input if \
         there are none>\", \"issues\": [{{\"original\": \"<the erroneous fragment exactly as \
         in the input>\", \"suggestion\": \"<its replacement>\", \"explanation\": \"<short \
         reason, written in {explain_name}>\"}}]}}\n\
         Keep the author's meaning, tone and formatting (markdown, code blocks, line breaks); \
         do not rewrite sentences that are already correct. Leave code, identifiers and proper \
         nouns unchanged. \"issues\" is [] when the text has no errors.\n\n\
         <<<\n{source}\n>>>",
    )
}

/// Submit a grammar check of `text` as one turn on `state`'s warm sidecar.
/// Returns `(turn id, event channel when want_events, resolved explanation
/// language code)`. The turn's `Done` text is the raw model answer — feed it
/// to `parse_result`. Does not truncate — callers enforce their own limit.
pub async fn submit(
    app: &AppHandle,
    db: &Db,
    state: &TranslateState,
    text: String,
    explain_lang: Option<String>,
    want_events: bool,
) -> Result<(u64, Option<UnboundedReceiver<TurnEvent>>, String), String> {
    let source = text.trim();
    if source.is_empty() {
        return Err(translate::ERR_NOTHING_TO_TRANSLATE.to_string());
    }
    let explain = match explain_lang
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(l) => l,
        None => translate::load_default_target(db).await?,
    };
    let prompt = build_prompt(source, &translate::target_language_name(&explain));
    let (turn, rx) = translate::submit_prompt(app, db, state, prompt, want_events).await?;
    Ok((turn, rx, explain))
}

/// Parse the model's answer: tolerates surrounding prose / code fences by
/// taking the outermost `{…}`. Drops empty and no-op issues. `None` when no
/// usable JSON object (or an empty `corrected`) is found.
pub fn parse_result(raw: &str) -> Option<GrammarResult> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end < start {
        return None;
    }
    let mut r: GrammarResult = serde_json::from_str(&raw[start..=end]).ok()?;
    if r.corrected.trim().is_empty() {
        return None;
    }
    r.issues
        .retain(|i| !i.original.trim().is_empty() && i.original != i.suggestion);
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_fenced_json() {
        let plain = r#"{"corrected":"I am here.","issues":[{"original":"is","suggestion":"am","explanation":"chủ ngữ I đi với am"}]}"#;
        let r = parse_result(plain).unwrap();
        assert_eq!(r.corrected, "I am here.");
        assert_eq!(r.issues.len(), 1);
        assert_eq!(r.issues[0].suggestion, "am");

        let fenced = format!("Here you go:\n```json\n{plain}\n```");
        assert_eq!(parse_result(&fenced).unwrap(), r);
    }

    #[test]
    fn missing_issues_and_noop_issues_are_tolerated() {
        let r = parse_result(r#"{"corrected":"Fine."}"#).unwrap();
        assert!(r.issues.is_empty());

        let r = parse_result(
            r#"{"corrected":"Fine.","issues":[{"original":"Fine","suggestion":"Fine"},{"original":"","suggestion":"x"},{"original":"a","suggestion":"b"}]}"#,
        )
        .unwrap();
        assert_eq!(r.issues.len(), 1);
        assert_eq!(r.issues[0].explanation, "");
    }

    #[test]
    fn rejects_unusable_answers() {
        assert!(parse_result("no json here").is_none());
        assert!(parse_result("} {").is_none());
        assert!(parse_result(r#"{"issues":[]}"#).is_none());
        assert!(parse_result(r#"{"corrected":"   "}"#).is_none());
    }

    #[test]
    fn prompt_names_the_explanation_language_and_wraps_the_text() {
        let p = build_prompt("He go home", "Vietnamese");
        assert!(p.contains("written in Vietnamese"));
        assert!(p.contains("<<<\nHe go home\n>>>"));
        assert!(p.contains("Do NOT translate"));
    }
}
