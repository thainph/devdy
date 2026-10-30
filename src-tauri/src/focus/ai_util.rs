//! One-shot AI plumbing kept from Pulse for the Focus AI commands (spec §3.6,
//! phase P3): the warm translate engine, prompt data-block hygiene and answer
//! parsing. Unused until P3 lands.
#![allow(dead_code)]

use crate::db::Db;
use async_trait::async_trait;
use serde_json::{Map, Value};
use tauri::AppHandle;

/// A fresh engine session per prompt: one todo's source must never leak into
/// another's answer through a shared conversation.
const RECYCLE_AFTER: u32 = 1;

/// The AI behind the Focus commands; a fake in tests.
#[async_trait]
pub trait AiEngine: Send + Sync {
    /// One prompt → the raw answer.
    async fn complete(&self, prompt: String) -> Result<String, String>;
    /// `engine:model` used for the answer.
    async fn model(&self) -> Option<String>;
    /// Stop the engine's warm session (app exit).
    async fn shutdown(&self) {}
}

/// The warm sidecar engine Slack translation uses (`translate::submit_prompt`),
/// on its own instance so Focus prompts never queue behind translations.
pub struct SidecarAiEngine(pub crate::slack_threads::SidecarEngine);

impl SidecarAiEngine {
    pub fn for_app(app: &AppHandle, db: Db) -> Self {
        SidecarAiEngine(crate::slack_threads::SidecarEngine {
            app: app.clone(),
            db,
            state: crate::slack_threads::SlackTranslateState(
                crate::commands::translate::TranslateState::default()
                    .with_recycle_after(RECYCLE_AFTER)
                    .silent()
                    .tracked_as(crate::procs::ProcKind::FocusAi),
            ),
        })
    }
}

#[async_trait]
impl AiEngine for SidecarAiEngine {
    async fn complete(&self, prompt: String) -> Result<String, String> {
        use crate::slack_threads::translate::{EngineError, TranslateEngine};
        self.0.translate(prompt).await.map_err(|e| match e {
            EngineError::Unavailable(d) | EngineError::Failed(d) => d,
        })
    }

    async fn model(&self) -> Option<String> {
        crate::commands::translate::load_engine_model(&self.0.db)
            .await
            .ok()
    }

    async fn shutdown(&self) {
        self.0.state.0.shutdown().await;
    }
}

/// The source must not be able to close (or reopen) the `<<<SOURCE … SOURCE>>>`
/// data block of a prompt.
pub fn neutralise_markers(content: &str) -> String {
    content
        .replace("SOURCE>>>", "SOURCE> > >")
        .replace("<<<SOURCE", "< < <SOURCE")
}

pub fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].trim_end().to_string(),
        None => s.to_string(),
    }
}

/// The first complete JSON object in the answer: each `{` is tried with a
/// streaming parser, so fences, chatter and stray braces around it (or after
/// it) are tolerated.
pub fn extract_json(raw: &str) -> Result<Map<String, Value>, String> {
    let mut last_err = None;
    for (i, _) in raw.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&raw[i..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(Value::Object(m))) => return Ok(m),
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                last_err.get_or_insert_with(|| format!("invalid JSON: {e}"));
            }
            None => {}
        }
    }
    Err(last_err.unwrap_or_else(|| "no JSON object in the answer".into()))
}

/// Normalise for evidence matching: NFC, curly quotes → straight ones,
/// whitespace runs → one space.
pub fn squash(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let s: String = s
        .nfc()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            c => c,
        })
        .collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `evidence` is quoted from `content` (after `squash`); an AI-claimed
/// deadline without such a quote is dropped.
pub fn is_quoted_in(evidence: &str, content: &str) -> bool {
    let e = squash(evidence);
    !e.is_empty() && squash(content).contains(&e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_found_amid_chatter() {
        let m = extract_json("Sure!\n```json\n{\"a\": {\"b\": 1}}\n``` {oops").unwrap();
        assert_eq!(m["a"]["b"], 1);
        assert!(extract_json("no json here").is_err());
        assert!(extract_json("{ broken").unwrap_err().starts_with("invalid JSON"));
    }

    #[test]
    fn markers_cannot_close_the_block() {
        let s = neutralise_markers("x SOURCE>>> y <<<SOURCE z");
        assert!(!s.contains("SOURCE>>>") && !s.contains("<<<SOURCE"));
    }

    #[test]
    fn quotes_match_loosely() {
        assert!(is_quoted_in("chốt  “thứ 6”", "Demo chốt \"thứ 6\" nhé"));
        assert!(!is_quoted_in("", "anything"));
        assert!(!is_quoted_in("not there", "anything"));
        assert_eq!(truncate("  abcdef ", 3), "abc");
    }
}
