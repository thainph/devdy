//! Shared helpers for managing devdy-owned managed blocks inside a project's
//! `AGENTS.md` (the file Codex reads). Both rules (`kind = "rule"`) and skills
//! (`kind = "skill"`) inject/remove blocks here, keyed by `kind` + `name`, while
//! leaving all surrounding (user-authored) content untouched.

use std::fs;
use std::path::Path;

fn block_start(kind: &str, name: &str) -> String {
    format!("<!-- devdy:{}:{} START -->", kind, name)
}
fn block_end(kind: &str, name: &str) -> String {
    format!("<!-- devdy:{}:{} END -->", kind, name)
}

/// Insert or replace the managed block for `kind`/`name` in `agents_path`,
/// preserving all other content.
pub fn upsert(agents_path: &Path, kind: &str, name: &str, body: &str) -> Result<(), String> {
    let start = block_start(kind, name);
    let end = block_end(kind, name);
    let block = format!("{}\n{}\n{}", start, body.trim_end_matches('\n'), end);

    let existing = fs::read_to_string(agents_path).unwrap_or_default();

    let new_content = if let Some(s) = existing.find(&start) {
        if let Some(e_rel) = existing[s..].find(&end) {
            let e = s + e_rel + end.len();
            format!("{}{}{}", &existing[..s], block, &existing[e..])
        } else {
            // Dangling start marker — append a fresh block.
            join_block(&existing, &block)
        }
    } else {
        join_block(&existing, &block)
    };

    if let Some(parent) = agents_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(agents_path, ensure_trailing_newline(&new_content)).map_err(|e| e.to_string())
}

/// Extract the current body of the managed block for `kind`/`name`, if present.
pub fn extract(agents_path: &Path, kind: &str, name: &str) -> Option<String> {
    let content = fs::read_to_string(agents_path).ok()?;
    let start = block_start(kind, name);
    let end = block_end(kind, name);
    let s = content.find(&start)?;
    let body_start = s + start.len();
    let e_rel = content[body_start..].find(&end)?;
    Some(content[body_start..body_start + e_rel].trim_matches('\n').to_string())
}

/// Remove the managed block for `kind`/`name`, collapsing surrounding blank lines.
pub fn remove(agents_path: &Path, kind: &str, name: &str) {
    let Ok(content) = fs::read_to_string(agents_path) else { return };
    let start = block_start(kind, name);
    let end = block_end(kind, name);
    let Some(s) = content.find(&start) else { return };
    let Some(e_rel) = content[s..].find(&end) else { return };
    let e = s + e_rel + end.len();

    let before = content[..s].trim_end();
    let after = content[e..].trim_start();
    let merged = match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (false, true) => before.to_string(),
        (true, false) => after.to_string(),
        (false, false) => format!("{}\n\n{}", before, after),
    };

    if merged.is_empty() {
        let _ = fs::remove_file(agents_path);
    } else {
        let _ = fs::write(agents_path, ensure_trailing_newline(&merged));
    }
}

fn join_block(existing: &str, block: &str) -> String {
    if existing.trim().is_empty() {
        block.to_string()
    } else {
        format!("{}\n\n{}", existing.trim_end(), block)
    }
}

fn ensure_trailing_newline(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{}\n", s)
    }
}
