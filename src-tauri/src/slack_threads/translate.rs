//! Batch translation of Slack messages into Vietnamese.
//!
//! Pure parts (batching, prompt, output parsing) are unit-tested; the engine
//! (warm sidecar) and the DB writes are injected via `TranslateEngine` /
//! `TranslateSink`.

use super::mrkdwn::{only_placeholders, placeholders, protect, restore};
use async_trait::async_trait;
use std::collections::HashMap;

/// Max chars of one batch (blocks + tags) = `translate::MAX_INPUT_CHARS`.
pub const BATCH_MAX_CHARS: usize = crate::commands::translate::MAX_INPUT_CHARS;
/// Messages longer than this are split into several blocks.
pub const SPLIT_MAX_CHARS: usize = 7_500;

/// One message to translate.
#[derive(Debug, Clone)]
pub struct MsgInput {
    pub ts: String,
    pub seq: i64,
    pub text_raw: String,
}

/// A message after protection + splitting.
#[derive(Debug, Clone)]
struct Prepared {
    ts: String,
    seq: i64,
    slots: Vec<String>,
    parts: Vec<String>,
    /// `seps[k]` sits between `parts[k]` and `parts[k + 1]`.
    seps: Vec<String>,
}

impl Prepared {
    fn block_id(&self, k: usize) -> String {
        if self.parts.len() == 1 {
            self.seq.to_string()
        } else {
            format!("{}.{}", self.seq, k + 1)
        }
    }

    fn blocks(&self) -> Vec<Block> {
        (0..self.parts.len())
            .map(|k| Block {
                id: self.block_id(k),
                text: self.parts[k].clone(),
            })
            .collect()
    }
}

/// One `<m id="…">` block of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: String,
    /// Masked text.
    pub text: String,
}

pub fn format_block(b: &Block) -> String {
    format!("<m id=\"{}\">\n{}\n</m>\n", b.id, b.text)
}

/// Byte index of the `n`-th char (or the end).
fn char_idx(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len())
}

/// Split `text` into parts of at most `max` chars, cutting at `\n\n`, then
/// `\n`, then whitespace (never inside a `⟦n⟧` placeholder). Returns the parts
/// and the separators removed between them, so `p0 + s0 + p1 + …` == `text`.
pub fn split_long(text: &str, max: usize) -> (Vec<String>, Vec<String>) {
    let mut parts = Vec::new();
    let mut seps = Vec::new();
    let mut rest = text;
    while rest.chars().count() > max {
        let window = &rest[..char_idx(rest, max)];
        // (cut index, separator is a newline run)
        let cut = window
            .rfind("\n\n")
            .filter(|&i| i > 0)
            .map(|i| (i, true))
            .or_else(|| window.rfind('\n').filter(|&i| i > 0).map(|i| (i, true)))
            .or_else(|| {
                window
                    .rfind(char::is_whitespace)
                    .filter(|&i| i > 0)
                    .map(|i| (i, false))
            });
        match cut {
            Some((i, newline)) => {
                let sep_len: usize = rest[i..]
                    .chars()
                    .take_while(|&c| {
                        if newline {
                            c == '\n'
                        } else {
                            c.is_whitespace() && c != '\n'
                        }
                    })
                    .map(char::len_utf8)
                    .sum();
                parts.push(rest[..i].to_string());
                seps.push(rest[i..i + sep_len].to_string());
                rest = &rest[i + sep_len..];
            }
            None => {
                // No whitespace at all: hard cut, but not inside a placeholder.
                let mut i = window.len();
                if let Some(open) = window.rfind('⟦') {
                    if !window[open..].contains('⟧') && open > 0 {
                        i = open;
                    }
                }
                parts.push(rest[..i].to_string());
                seps.push(String::new());
                rest = &rest[i..];
            }
        }
    }
    parts.push(rest.to_string());
    (parts, seps)
}

/// Group blocks into batches whose formatted size stays ≤ `max_chars`.
pub fn build_batches(blocks: Vec<Block>, max_chars: usize) -> Vec<Vec<Block>> {
    let mut batches = Vec::new();
    let mut current: Vec<Block> = Vec::new();
    let mut size = 0;
    for b in blocks {
        let len = format_block(&b).chars().count();
        if !current.is_empty() && size + len > max_chars {
            batches.push(std::mem::take(&mut current));
            size = 0;
        }
        size += len;
        current.push(b);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

pub fn build_batch_prompt(blocks: &[Block], style_instruction: &str) -> String {
    let mut p = String::from(
        "You translate Slack messages into Vietnamese. The input is a list of <m id=\"…\">…</m> blocks.\n\
         Rules:\n\
         - Output exactly the same blocks with the same ids in the same order; each block contains only the Vietnamese translation of its own content. Output nothing outside the blocks.\n\
         - Tokens like ⟦3⟧ are placeholders for mentions, links, code and emoji: copy every placeholder unchanged, exactly once, at the natural position.\n\
         - Keep Slack formatting markers (*bold*, _italic_, ~strike~, leading \"> \" quotes, bullets, numbering, line breaks) and HTML entities (&lt; &gt; &amp;) unchanged.\n\
         - Keep person names, product names, file names, identifiers and URLs unchanged. If a block is already Vietnamese, copy it unchanged.\n",
    );
    p.push_str(style_instruction);
    p.push_str("\n\n");
    for b in blocks {
        p.push_str(&format_block(b));
    }
    p
}

fn is_block_id(id: &str) -> bool {
    let (a, b) = match id.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (id, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    digits(a) && b.is_none_or(digits)
}

/// Every `(id, content)` matching `<m id="([0-9]+(?:\.[0-9]+)?)">\n?(.*?)\n?</m>`.
fn scan_blocks(output: &str) -> Vec<(String, String)> {
    const OPEN: &str = "<m id=\"";
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(i) = output[pos..].find(OPEN) {
        let start = pos + i;
        let id_start = start + OPEN.len();
        let Some(id_len) = output[id_start..].find("\">") else {
            break;
        };
        let id = &output[id_start..id_start + id_len];
        if !is_block_id(id) {
            pos = id_start;
            continue;
        }
        let mut body = id_start + id_len + 2;
        if output[body..].starts_with('\n') {
            body += 1;
        }
        let Some(end_rel) = output[body..].find("</m>") else {
            break;
        };
        let mut content = &output[body..body + end_rel];
        content = content.strip_suffix('\n').unwrap_or(content);
        out.push((id.to_string(), content.to_string()));
        pos = body + end_rel + 4;
    }
    out
}

/// Validate a batch answer: each expected block must appear exactly once,
/// non-empty, with the same placeholder multiset as its source. Returns per
/// block id the translated (still masked) text or an error code
/// (`missing_block` / `placeholder_mismatch`). Unknown ids are ignored.
pub fn parse_batch_output(
    output: &str,
    blocks: &[Block],
) -> HashMap<String, Result<String, String>> {
    let mut found: HashMap<String, Vec<String>> = HashMap::new();
    for (id, content) in scan_blocks(output) {
        found.entry(id).or_default().push(content);
    }
    blocks
        .iter()
        .map(|b| {
            let r = match found.get(&b.id).map(Vec::as_slice) {
                Some([content]) if !content.trim().is_empty() => {
                    if placeholders(content) == placeholders(&b.text) {
                        Ok(content.clone())
                    } else {
                        Err("placeholder_mismatch".to_string())
                    }
                }
                _ => Err("missing_block".to_string()),
            };
            (b.id.clone(), r)
        })
        .collect()
}

/// Why an engine call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The engine could not start / accept the turn: abort the whole run.
    Unavailable(String),
    /// This turn failed (engine error, `timeout`).
    Failed(String),
}

#[async_trait]
pub trait TranslateEngine: Send + Sync {
    /// `style_instruction(translate_style)`.
    async fn style_instruction(&self) -> String;
    async fn translate(&self, prompt: String) -> Result<String, EngineError>;
}

/// Final outcome for one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsgOutcome {
    Done(String),
    /// Nothing to translate: `text_vi = text_raw`.
    Skipped(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgResult {
    pub ts: String,
    pub outcome: MsgOutcome,
}

/// The job stopped (thread deleted).
#[derive(Debug)]
pub struct Stop;

#[async_trait]
pub trait TranslateSink: Send {
    async fn progress(&mut self, done: usize, total: usize);
    async fn apply(&mut self, results: Vec<MsgResult>) -> Result<(), Stop>;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub batches: usize,
    pub translated: usize,
    pub failed: usize,
    /// Engine unavailable, or every batch failed.
    pub all_failed: bool,
}

/// Outcome of one engine call, per message ts.
async fn run_batch(
    engine: &dyn TranslateEngine,
    style: &str,
    blocks: &[Block],
) -> Result<HashMap<String, Result<String, String>>, EngineError> {
    let prompt = build_batch_prompt(blocks, style);
    match engine.translate(prompt).await {
        Ok(out) => Ok(parse_batch_output(&out, blocks)),
        Err(EngineError::Failed(e)) => Ok(blocks
            .iter()
            .map(|b| (b.id.clone(), Err(e.clone())))
            .collect()),
        Err(e) => Err(e),
    }
}

/// Fold block results into a message outcome (all parts must succeed).
fn assemble(
    p: &Prepared,
    results: &HashMap<String, Result<String, String>>,
) -> Result<String, String> {
    let mut joined = String::new();
    for k in 0..p.parts.len() {
        match results.get(&p.block_id(k)) {
            Some(Ok(t)) => joined.push_str(t),
            Some(Err(e)) => return Err(e.clone()),
            None => return Err("missing_block".to_string()),
        }
        if let Some(sep) = p.seps.get(k) {
            joined.push_str(sep);
        }
    }
    Ok(restore(&joined, &p.slots))
}

/// Translate `inputs` (already filtered to `pending|error`, by seq): batches
/// first, then one retry per failed message in its own batch(es).
pub async fn run_translation(
    engine: &dyn TranslateEngine,
    sink: &mut dyn TranslateSink,
    inputs: &[MsgInput],
) -> Result<RunSummary, Stop> {
    let mut summary = RunSummary::default();
    let mut skipped = Vec::new();
    let mut prepared: Vec<Prepared> = Vec::new();
    for m in inputs {
        let (masked, slots) = protect(&m.text_raw);
        if only_placeholders(&masked) {
            skipped.push(MsgResult {
                ts: m.ts.clone(),
                outcome: MsgOutcome::Skipped(m.text_raw.clone()),
            });
            continue;
        }
        let (parts, seps) = split_long(&masked, SPLIT_MAX_CHARS);
        prepared.push(Prepared {
            ts: m.ts.clone(),
            seq: m.seq,
            slots,
            parts,
            seps,
        });
    }
    if !skipped.is_empty() {
        sink.apply(skipped).await?;
    }
    let style = if prepared.is_empty() {
        String::new()
    } else {
        engine.style_instruction().await
    };

    let by_block: HashMap<String, usize> = prepared
        .iter()
        .enumerate()
        .flat_map(|(i, p)| (0..p.parts.len()).map(move |k| (p.block_id(k), i)))
        .collect();
    let batches = build_batches(
        prepared.iter().flat_map(Prepared::blocks).collect(),
        BATCH_MAX_CHARS,
    );
    let mut total = batches.len();
    let mut done = 0;
    let mut any_success = false;
    sink.progress(done, total).await;

    // Round 1: messages whose blocks are all in this batch are final when OK;
    // failures (and messages straddling batches) are settled afterwards.
    let mut block_results: HashMap<String, Result<String, String>> = HashMap::new();
    let mut unavailable: Option<String> = None;
    for batch in &batches {
        match run_batch(engine, &style, batch).await {
            Ok(r) => block_results.extend(r),
            Err(EngineError::Unavailable(e)) | Err(EngineError::Failed(e)) => {
                unavailable = Some(e);
                break;
            }
        }
        done += 1;
        sink.progress(done, total).await;
        // Publish messages that are now complete and OK.
        let mut ready = Vec::new();
        for b in batch {
            let p = &prepared[by_block[&b.id]];
            if b.id != p.block_id(p.parts.len() - 1) {
                continue;
            }
            if let Ok(text) = assemble(p, &block_results) {
                ready.push(MsgResult {
                    ts: p.ts.clone(),
                    outcome: MsgOutcome::Done(text),
                });
            }
        }
        if !ready.is_empty() {
            any_success = true;
            summary.translated += ready.len();
            sink.apply(ready).await?;
        }
    }
    summary.batches = done;

    if let Some(e) = unavailable {
        let failed: Vec<MsgResult> = prepared
            .iter()
            .filter(|p| assemble(p, &block_results).is_err())
            .map(|p| MsgResult {
                ts: p.ts.clone(),
                outcome: MsgOutcome::Error(e.clone()),
            })
            .collect();
        summary.failed = failed.len();
        summary.all_failed = true;
        sink.apply(failed).await?;
        return Ok(summary);
    }

    // Round 2: retry each failed message alone, once.
    let retry: Vec<&Prepared> = prepared
        .iter()
        .filter(|p| assemble(p, &block_results).is_err())
        .collect();
    let retry_batches: Vec<(usize, Vec<Vec<Block>>)> = retry
        .iter()
        .enumerate()
        .map(|(i, p)| (i, build_batches(p.blocks(), BATCH_MAX_CHARS)))
        .collect();
    total += retry_batches.iter().map(|(_, b)| b.len()).sum::<usize>();
    if !retry.is_empty() {
        sink.progress(done, total).await;
    }
    for (i, batches) in retry_batches {
        let p = retry[i];
        let mut results: HashMap<String, Result<String, String>> = HashMap::new();
        let mut aborted = None;
        for batch in &batches {
            match run_batch(engine, &style, batch).await {
                Ok(r) => results.extend(r),
                Err(EngineError::Unavailable(e)) | Err(EngineError::Failed(e)) => {
                    aborted = Some(e);
                }
            }
            done += 1;
            sink.progress(done, total).await;
            if aborted.is_some() {
                break;
            }
        }
        summary.batches = done;
        let outcome = match aborted {
            Some(e) => MsgOutcome::Error(e),
            None => match assemble(p, &results) {
                Ok(text) => MsgOutcome::Done(text),
                Err(e) => MsgOutcome::Error(e),
            },
        };
        match &outcome {
            MsgOutcome::Done(_) => {
                any_success = true;
                summary.translated += 1;
            }
            _ => summary.failed += 1,
        }
        sink.apply(vec![MsgResult {
            ts: p.ts.clone(),
            outcome,
        }])
        .await?;
    }
    summary.all_failed = !prepared.is_empty() && !any_success;
    Ok(summary)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn split_and_rejoin_long_message() {
        let para = "word ".repeat(400); // 2000 chars
        let text = format!("{p}\n\n{p}\n\n{p}\n{p}{p}", p = para.trim_end());
        assert!(text.chars().count() > 9_000);
        let (parts, seps) = split_long(&text, 7_500);
        assert!(parts.len() >= 2);
        assert!(parts.iter().all(|p| p.chars().count() <= 7_500));
        let mut joined = String::new();
        for (i, p) in parts.iter().enumerate() {
            joined.push_str(p);
            if let Some(s) = seps.get(i) {
                joined.push_str(s);
            }
        }
        assert_eq!(joined, text);
        assert_eq!(seps[0], "\n\n");

        // No whitespace: hard cut that never splits a placeholder.
        let text = format!("{}⟦12⟧{}", "a".repeat(7_498), "b".repeat(100));
        let (parts, seps) = split_long(&text, 7_500);
        assert_eq!(parts[0], "a".repeat(7_498));
        assert!(parts[1].starts_with("⟦12⟧"));
        assert_eq!(seps, vec![String::new()]);
    }

    #[test]
    fn batches_respect_the_limit() {
        let blocks: Vec<Block> = (0..20)
            .map(|i| Block {
                id: i.to_string(),
                text: "x".repeat(1_000),
            })
            .collect();
        let batches = build_batches(blocks, BATCH_MAX_CHARS);
        assert!(batches.len() > 1);
        for b in &batches {
            let size: usize = b.iter().map(|x| format_block(x).chars().count()).sum();
            assert!(size <= BATCH_MAX_CHARS, "{}", size);
        }
        assert_eq!(batches.iter().map(Vec::len).sum::<usize>(), 20);
    }

    #[test]
    fn prompt_has_rules_style_and_blocks() {
        let p = build_batch_prompt(
            &[Block {
                id: "3".into(),
                text: "hi ⟦0⟧".into(),
            }],
            "STYLE",
        );
        assert!(p.starts_with("You translate Slack messages into Vietnamese."));
        assert!(p.contains("copy every placeholder unchanged"));
        assert!(p.ends_with("STYLE\n\n<m id=\"3\">\nhi ⟦0⟧\n</m>\n"));
    }

    #[test]
    fn parse_output_accepts_only_valid_blocks() {
        let blocks = vec![
            Block {
                id: "1".into(),
                text: "a ⟦0⟧".into(),
            },
            Block {
                id: "2".into(),
                text: "b".into(),
            },
            Block {
                id: "3".into(),
                text: "c ⟦1⟧".into(),
            },
            Block {
                id: "4.1".into(),
                text: "d".into(),
            },
            Block {
                id: "5".into(),
                text: "e".into(),
            },
        ];
        // Reordered, text outside tags, block 3 lost its placeholder, block 5
        // missing, an unknown id and a duplicate of 2.
        let out =
            "Sure!\n<m id=\"4.1\">\nđ\n</m>\n<m id=\"2\">b-vi</m>\n<m id=\"1\">\nmột ⟦0⟧\n</m>\n\
                   <m id=\"3\">\nba\n</m>\n<m id=\"99\">\nx\n</m>\n<m id=\"x\">bad</m>";
        let r = parse_batch_output(out, &blocks);
        assert_eq!(r["1"], Ok("một ⟦0⟧".to_string()));
        assert_eq!(r["2"], Ok("b-vi".to_string()));
        assert_eq!(r["3"], Err("placeholder_mismatch".to_string()));
        assert_eq!(r["4.1"], Ok("đ".to_string()));
        assert_eq!(r["5"], Err("missing_block".to_string()));
        assert!(!r.contains_key("99"));
        let dup = format!("{out}\n<m id=\"2\">again</m>");
        assert_eq!(
            parse_batch_output(&dup, &blocks)["2"],
            Err("missing_block".to_string())
        );
        let empty = "<m id=\"5\">\n  \n</m>";
        assert_eq!(
            parse_batch_output(empty, &blocks)["5"],
            Err("missing_block".to_string())
        );
    }

    /// Engine double: answers each block with `VI:<text>` unless the block text
    /// contains a key from `drop` (answered without the block) or `strip`
    /// (answered without placeholders); `drop_once` only fails the first time.
    pub struct FakeEngine {
        pub prompts: Mutex<Vec<String>>,
        pub drop: Vec<String>,
        pub drop_once: Mutex<Vec<String>>,
        pub unavailable: bool,
    }

    impl FakeEngine {
        pub fn new() -> Self {
            FakeEngine {
                prompts: Mutex::new(Vec::new()),
                drop: Vec::new(),
                drop_once: Mutex::new(Vec::new()),
                unavailable: false,
            }
        }

        pub fn calls(&self) -> usize {
            self.prompts.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl TranslateEngine for FakeEngine {
        async fn style_instruction(&self) -> String {
            "STYLE".into()
        }

        async fn translate(&self, prompt: String) -> Result<String, EngineError> {
            if self.unavailable {
                return Err(EngineError::Unavailable("no engine".into()));
            }
            self.prompts.lock().unwrap().push(prompt.clone());
            let body = prompt.split_once("STYLE\n\n").map(|x| x.1).unwrap_or("");
            let mut out = String::new();
            for (id, text) in scan_blocks(body) {
                if self.drop.iter().any(|d| text.contains(d.as_str())) {
                    continue;
                }
                let mut once = self.drop_once.lock().unwrap();
                if let Some(i) = once.iter().position(|d| text.contains(d.as_str())) {
                    once.remove(i);
                    continue;
                }
                out.push_str(&format!("<m id=\"{id}\">\nVI:{text}\n</m>\n"));
            }
            Ok(out)
        }
    }

    #[derive(Default)]
    pub struct VecSink {
        pub results: Vec<MsgResult>,
        pub progress: Vec<(usize, usize)>,
    }

    #[async_trait]
    impl TranslateSink for VecSink {
        async fn progress(&mut self, done: usize, total: usize) {
            self.progress.push((done, total));
        }
        async fn apply(&mut self, results: Vec<MsgResult>) -> Result<(), Stop> {
            self.results.extend(results);
            Ok(())
        }
    }

    fn input(seq: i64, text: &str) -> MsgInput {
        MsgInput {
            ts: format!("1700000000.{:06}", seq),
            seq,
            text_raw: text.to_string(),
        }
    }

    fn outcome(sink: &VecSink, seq: i64) -> MsgOutcome {
        let ts = format!("1700000000.{:06}", seq);
        sink.results
            .iter()
            .rev()
            .find(|r| r.ts == ts)
            .unwrap()
            .outcome
            .clone()
    }

    #[tokio::test]
    async fn translation_round_trip_with_placeholders() {
        let engine = FakeEngine::new();
        let mut sink = VecSink::default();
        let inputs = vec![
            input(0, "Hi <@U1> `code` :smile:"),
            input(1, "<@U2> :tada:"),
        ];
        let s = run_translation(&engine, &mut sink, &inputs).await.unwrap();
        assert_eq!(
            outcome(&sink, 0),
            MsgOutcome::Done("VI:Hi <@U1> `code` :smile:".into())
        );
        assert_eq!(
            outcome(&sink, 1),
            MsgOutcome::Skipped("<@U2> :tada:".into())
        );
        assert_eq!(s.batches, 1);
        assert!(!s.all_failed);
        assert_eq!(sink.progress, vec![(0, 1), (1, 1)]);
    }

    #[tokio::test]
    async fn failed_block_is_retried_alone_once_then_errors() {
        let mut engine = FakeEngine::new();
        engine.drop = vec!["always-bad".into()];
        *engine.drop_once.lock().unwrap() = vec!["flaky".into()];
        let mut sink = VecSink::default();
        let inputs = vec![
            input(0, "ok one"),
            input(1, "flaky"),
            input(2, "always-bad"),
        ];
        let s = run_translation(&engine, &mut sink, &inputs).await.unwrap();
        assert_eq!(outcome(&sink, 0), MsgOutcome::Done("VI:ok one".into()));
        assert_eq!(outcome(&sink, 1), MsgOutcome::Done("VI:flaky".into()));
        assert_eq!(outcome(&sink, 2), MsgOutcome::Error("missing_block".into()));
        // 1 batch + 2 single-message retries.
        assert_eq!(engine.calls(), 3);
        assert_eq!(s.failed, 1);
        assert!(!s.all_failed);
        assert_eq!(sink.progress.last(), Some(&(3, 3)));
    }

    #[tokio::test]
    async fn long_thread_and_long_message_round_trip() {
        let engine = FakeEngine::new();
        let mut sink = VecSink::default();
        let long = format!(
            "{}\n\n{}",
            "a ".repeat(3_500).trim_end(),
            "b ".repeat(2_500)
        );
        assert!(long.chars().count() >= 12_000 - 1);
        let mut inputs: Vec<MsgInput> = (0..6).map(|i| input(i, &"x ".repeat(1_500))).collect();
        inputs.push(input(6, &long));
        let s = run_translation(&engine, &mut sink, &inputs).await.unwrap();
        assert!(s.batches > 1);
        for p in engine.prompts.lock().unwrap().iter() {
            let body = p.split_once("STYLE\n\n").unwrap().1;
            assert!(body.chars().count() <= BATCH_MAX_CHARS);
        }
        // The split message is reassembled with its separator.
        let MsgOutcome::Done(text) = outcome(&sink, 6) else {
            panic!("not translated");
        };
        assert!(text.starts_with("VI:a a"));
        assert!(text.contains("\n\nVI:b b"));
        assert_eq!(s.failed, 0);
    }

    #[tokio::test]
    async fn unavailable_engine_fails_everything() {
        let mut engine = FakeEngine::new();
        engine.unavailable = true;
        let mut sink = VecSink::default();
        let s = run_translation(&engine, &mut sink, &[input(0, "x"), input(1, "y")])
            .await
            .unwrap();
        assert!(s.all_failed);
        assert_eq!(outcome(&sink, 1), MsgOutcome::Error("no engine".into()));
        // Nothing to translate → no engine call, no batch.
        let mut sink = VecSink::default();
        let s = run_translation(&FakeEngine::new(), &mut sink, &[])
            .await
            .unwrap();
        assert_eq!(s, RunSummary::default());
        assert_eq!(sink.progress, vec![(0, 0)]);
    }
}
