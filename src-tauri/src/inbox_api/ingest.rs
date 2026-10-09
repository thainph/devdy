//! Pure parsing of an incoming capture payload — a Slack thread or a web page,
//! sent as a raw `.md` or a `.zip` holding one `.md` plus attachments. No DB or filesystem access here — the
//! result is handed to `store::ingest` — so every guard is unit-testable.

use std::io::{Cursor, Read};

/// Max accepted request body / imported file size (compressed zip or raw md).
pub const MAX_BODY_BYTES: usize = 50 * 1024 * 1024;
/// Max number of entries (files + directories) inside a zip.
pub const MAX_ZIP_ENTRIES: usize = 200;
/// Max total uncompressed bytes extracted from one zip (zip-bomb guard).
pub const MAX_UNCOMPRESSED_BYTES: u64 = 200 * 1024 * 1024;
/// Max length (in chars) of a title derived from the body's first line.
const TITLE_FALLBACK_MAX_CHARS: usize = 120;

/// Why a payload was rejected. Maps 1:1 onto the HTTP status the API returns.
#[derive(Debug, PartialEq, Eq)]
pub enum IngestError {
    /// 400 — malformed payload (bad zip layout, zip-slip, invalid UTF-8 …).
    BadRequest(String),
    /// 413 — body or uncompressed content too large.
    TooLarge(String),
    /// 500 — storage failure.
    Internal(String),
}

impl IngestError {
    pub fn message(&self) -> &str {
        match self {
            IngestError::BadRequest(m) | IngestError::TooLarge(m) | IngestError::Internal(m) => m,
        }
    }
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

fn bad(msg: impl Into<String>) -> IngestError {
    IngestError::BadRequest(msg.into())
}

/// One attachment pulled out of a zip: its relative path inside the archive
/// (for display) and its bytes.
#[derive(Debug, Clone)]
pub struct ParsedAttachment {
    pub name: String,
    pub data: Vec<u8>,
}

/// What a capture is. Stored in `captures.kind`; also picks the files dir,
/// the default title and the notification text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureKind {
    #[default]
    Slack,
    Web,
}

impl CaptureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CaptureKind::Slack => "slack",
            CaptureKind::Web => "web",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "slack" => Some(CaptureKind::Slack),
            "web" => Some(CaptureKind::Web),
            _ => None,
        }
    }

    /// Subdirectory of `<app_data>` holding one folder per capture.
    pub fn dir_name(&self) -> &'static str {
        match self {
            CaptureKind::Slack => "slack-threads",
            CaptureKind::Web => "web-pages",
        }
    }

    pub fn default_title(&self) -> &'static str {
        match self {
            CaptureKind::Slack => "Slack thread",
            CaptureKind::Web => "Web page",
        }
    }

    /// OS notification text for an API-received capture.
    pub fn received_message(&self, title: &str) -> String {
        match self {
            CaptureKind::Slack => format!("Slack thread received: {}", title),
            CaptureKind::Web => format!("Web page received: {}", title),
        }
    }
}

/// A fully parsed capture, ready to be upserted. Slack-only and web-only
/// fields stay `None` for the other kind.
#[derive(Debug, Clone, Default)]
pub struct ParsedCapture {
    pub kind: CaptureKind,
    pub title: String,
    /// Markdown body with the front matter stripped.
    pub content: String,
    /// Export/capture time (`exported_at` for slack, `captured_at` for web).
    pub exported_at: Option<String>,
    // slack
    pub workspace: Option<String>,
    pub channel: Option<String>,
    pub thread_url: Option<String>,
    pub thread_ts: Option<String>,
    // web
    pub source_url: Option<String>,
    /// Normalized `source_url` (dedup key); `None` when absent/unparseable.
    pub url_key: Option<String>,
    pub site_name: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<String>,
    pub description: Option<String>,
    /// Only the user's selection was captured (never deduped).
    pub selection: bool,
    pub attachments: Vec<ParsedAttachment>,
}

/// Payload kind, decided from the request Content-Type or the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    Markdown,
    Zip,
}

impl PayloadKind {
    /// Map a Content-Type header value (parameters such as `charset` ignored).
    pub fn from_content_type(ct: &str) -> Option<Self> {
        let base = ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match base.as_str() {
            "text/markdown" | "text/x-markdown" | "text/plain" => Some(PayloadKind::Markdown),
            "application/zip" | "application/x-zip-compressed" => Some(PayloadKind::Zip),
            _ => None,
        }
    }

    /// Map a file path by extension (`.md`/`.markdown` or `.zip`).
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
        match ext.as_str() {
            "md" | "markdown" => Some(PayloadKind::Markdown),
            "zip" => Some(PayloadKind::Zip),
            _ => None,
        }
    }
}

/// Parse a payload of the given format as a capture of `kind`.
pub fn parse_payload(
    kind: CaptureKind,
    format: PayloadKind,
    bytes: &[u8],
) -> Result<ParsedCapture, IngestError> {
    if bytes.len() > MAX_BODY_BYTES {
        return Err(IngestError::TooLarge(format!(
            "payload exceeds {} MB",
            MAX_BODY_BYTES / 1024 / 1024
        )));
    }
    match format {
        PayloadKind::Markdown => parse_markdown(kind, bytes),
        PayloadKind::Zip => parse_zip(kind, bytes),
    }
}

/// Parse a raw markdown document (optional YAML front matter + body).
pub fn parse_markdown(kind: CaptureKind, bytes: &[u8]) -> Result<ParsedCapture, IngestError> {
    let text = std::str::from_utf8(bytes).map_err(|_| bad("markdown is not valid UTF-8"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (front, body) = split_front_matter(text);

    let mut thread = ParsedCapture {
        kind,
        content: body.to_string(),
        ..Default::default()
    };
    let mut title = None;
    if let Some(front) = front {
        let value: serde_yaml::Value = if front.trim().is_empty() {
            serde_yaml::Value::Null
        } else {
            serde_yaml::from_str(front).map_err(|e| bad(format!("invalid front matter: {}", e)))?
        };
        if let serde_yaml::Value::Mapping(map) = value {
            let get = |key: &str| map.get(key).and_then(yaml_scalar);
            title = get("title");
            match kind {
                CaptureKind::Slack => {
                    thread.workspace = get("workspace");
                    thread.channel = get("channel");
                    thread.thread_url = get("thread_url");
                    thread.thread_ts = get("thread_ts");
                    thread.exported_at = get("exported_at");
                }
                CaptureKind::Web => {
                    thread.source_url = get("url");
                    thread.url_key = thread.source_url.as_deref().and_then(normalize_url);
                    thread.site_name = get("site_name");
                    thread.author = get("author");
                    thread.published_at = get("published_at");
                    thread.exported_at = get("captured_at").or_else(|| get("exported_at"));
                    thread.description = get("description");
                    thread.selection = map.get("selection").map(yaml_truthy).unwrap_or(false);
                }
            }
        } else if !value.is_null() {
            return Err(bad("invalid front matter: expected a YAML mapping"));
        }
    }
    thread.title = title.unwrap_or_else(|| {
        first_line_title(&thread.content)
            .or_else(|| thread.source_url.as_deref().and_then(url_domain))
            .unwrap_or_else(|| kind.default_title().to_string())
    });
    Ok(thread)
}

/// Split a leading `---` … `---` (or `...`) YAML block off the document.
/// Returns `(Some(yaml), body)` when present, else `(None, whole_text)`.
fn split_front_matter(text: &str) -> (Option<&str>, &str) {
    let first_line_end = match text.find('\n') {
        Some(i) => i,
        None => return (None, text),
    };
    if text[..first_line_end].trim_end_matches('\r') != "---" {
        return (None, text);
    }
    let rest = &text[first_line_end + 1..];
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed == "---" || trimmed == "..." {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return (Some(yaml), body);
        }
        offset += line.len();
    }
    // No closing fence: not front matter after all.
    (None, text)
}

/// Read a YAML scalar as a trimmed, non-empty string. Numbers are accepted
/// (an unquoted `thread_ts: 1700000000.123456` must not be dropped).
fn yaml_scalar(v: &serde_yaml::Value) -> Option<String> {
    let s = match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        _ => return None,
    };
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `true`/`"true"`/`"yes"`/`1` → true; anything else → false.
fn yaml_truthy(v: &serde_yaml::Value) -> bool {
    match v {
        serde_yaml::Value::Bool(b) => *b,
        serde_yaml::Value::Number(n) => n.as_i64() == Some(1),
        serde_yaml::Value::String(s) => {
            matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1")
        }
        _ => false,
    }
}

/// First non-empty body line stripped of markdown heading `#`s, capped at 120
/// chars; `None` when the body has no usable line.
pub fn first_line_title(body: &str) -> Option<String> {
    body.lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(TITLE_FALLBACK_MAX_CHARS).collect())
}

/// Query params dropped from the dedup key (tracking noise).
fn is_tracking_param(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_")
        || matches!(
            key.as_str(),
            "fbclid" | "gclid" | "mc_cid" | "mc_eid" | "ref" | "ref_src"
        )
}

/// Split `scheme://authority/path?query#frag` into lowercase scheme, authority,
/// path, query (fragment dropped). `None` if there is no `scheme://` or host.
fn split_url(raw: &str) -> Option<(String, &str, &str, &str)> {
    let raw = raw.trim();
    let (scheme, rest) = raw.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let rest = rest.split('#').next().unwrap_or("");
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    if authority.is_empty() {
        return None;
    }
    let tail = &rest[auth_end..];
    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p, q),
        None => (tail, ""),
    };
    Some((scheme.to_ascii_lowercase(), authority, path, query))
}

/// Lowercased host (+ port unless default) of an authority; userinfo kept.
fn normalize_authority(scheme: &str, authority: &str) -> String {
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    // Port separator: last ':' that is not inside an IPv6 literal.
    let (host, port) = match hostport.rfind(':') {
        Some(i) if !hostport[i..].contains(']') => (&hostport[..i], Some(&hostport[i + 1..])),
        _ => (hostport, None),
    };
    let default_port = match scheme {
        "http" | "ws" => Some("80"),
        "https" | "wss" => Some("443"),
        "ftp" => Some("21"),
        _ => None,
    };
    let mut out = String::new();
    if let Some(u) = userinfo {
        out.push_str(u);
        out.push('@');
    }
    out.push_str(&host.to_ascii_lowercase());
    if let Some(p) = port.filter(|p| !p.is_empty() && Some(*p) != default_port) {
        out.push(':');
        out.push_str(p);
    }
    out
}

/// Dedup key for a web page URL: lowercase scheme + host, default port and
/// `#fragment` dropped, tracking params (`utm_*`, `fbclid`, `gclid`, `mc_cid`,
/// `mc_eid`, `ref`, `ref_src`) removed, remaining params sorted, one trailing
/// `/` stripped from the path (root `/` kept). `None` when not a URL.
pub fn normalize_url(raw: &str) -> Option<String> {
    let (scheme, authority, path, query) = split_url(raw)?;
    let authority = normalize_authority(&scheme, authority);
    let mut path = if path.is_empty() { "/" } else { path };
    if path.len() > 1 && path.ends_with('/') {
        path = &path[..path.len() - 1];
    }
    let mut params: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| !is_tracking_param(p.split('=').next().unwrap_or("")))
        .collect();
    params.sort_unstable();
    let mut out = format!("{}://{}{}", scheme, authority, path);
    if !params.is_empty() {
        out.push('?');
        out.push_str(&params.join("&"));
    }
    Some(out)
}

/// Domain of a URL for display/title fallback (lowercased, `www.` stripped).
pub fn url_domain(raw: &str) -> Option<String> {
    let (scheme, authority, _, _) = split_url(raw)?;
    let auth = normalize_authority(&scheme, authority);
    let host = auth.rsplit('@').next().unwrap_or(&auth);
    let host = host.split(':').next().unwrap_or(host);
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// True for zip noise we silently skip (macOS resource forks / Finder files).
fn is_ignored(path: &str) -> bool {
    path.starts_with("__MACOSX/")
        || path.split('/').any(|seg| seg == "__MACOSX")
        || path.rsplit('/').next() == Some(".DS_Store")
}

/// Normalize a zip entry name and reject anything that could escape the
/// extraction dir (absolute paths, drive letters, `..` segments).
fn safe_entry_path(raw: &str) -> Result<String, IngestError> {
    let name = raw.replace('\\', "/");
    if name.starts_with('/') || name.as_bytes().get(1) == Some(&b':') {
        return Err(bad(format!("zip entry has an absolute path: {}", raw)));
    }
    let mut parts = Vec::new();
    for seg in name.split('/') {
        match seg {
            "" | "." => continue,
            ".." => return Err(bad(format!("zip entry escapes the archive: {}", raw))),
            s => parts.push(s),
        }
    }
    Ok(parts.join("/"))
}

/// Parse a zip archive: exactly one conversation `.md` plus attachments.
pub fn parse_zip(kind: CaptureKind, bytes: &[u8]) -> Result<ParsedCapture, IngestError> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| bad(format!("invalid zip: {}", e)))?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(bad(format!(
            "zip has {} entries (max {})",
            archive.len(),
            MAX_ZIP_ENTRIES
        )));
    }

    // Extract every regular file, enforcing path safety and the total
    // uncompressed budget on the bytes actually read (headers can lie).
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| bad(format!("invalid zip entry: {}", e)))?;
        let raw = entry.name().to_string();
        let path = safe_entry_path(&raw)?;
        if entry.enclosed_name().is_none() {
            return Err(bad(format!("zip entry escapes the archive: {}", raw)));
        }
        if entry.is_dir() || raw.ends_with('/') || path.is_empty() || is_ignored(&path) {
            continue;
        }
        let remaining = MAX_UNCOMPRESSED_BYTES - total;
        let mut data = Vec::new();
        (&mut entry)
            .take(remaining + 1)
            .read_to_end(&mut data)
            .map_err(|e| bad(format!("failed to read zip entry {}: {}", raw, e)))?;
        total += data.len() as u64;
        if total > MAX_UNCOMPRESSED_BYTES {
            return Err(IngestError::TooLarge(format!(
                "zip uncompressed content exceeds {} MB",
                MAX_UNCOMPRESSED_BYTES / 1024 / 1024
            )));
        }
        files.push((path, data));
    }

    // Pick the conversation: the single root `.md`; else the only `.md` anywhere.
    let is_md = |p: &str| p.to_ascii_lowercase().ends_with(".md");
    let root_md: Vec<usize> = (0..files.len())
        .filter(|&i| is_md(&files[i].0) && !files[i].0.contains('/'))
        .collect();
    let any_md: Vec<usize> = (0..files.len()).filter(|&i| is_md(&files[i].0)).collect();
    let md_index = match (root_md.len(), any_md.len()) {
        (1, _) => root_md[0],
        (0, 1) => any_md[0],
        (0, 0) => return Err(bad("zip contains no .md file")),
        _ => {
            return Err(bad(
                "zip contains several candidate .md files; expected exactly one",
            ))
        }
    };

    let (_, md_bytes) = files.remove(md_index);
    let mut thread = parse_markdown(kind, &md_bytes)?;
    thread.attachments = files
        .into_iter()
        .map(|(name, data)| ParsedAttachment { name, data })
        .collect();
    Ok(thread)
}

/// Make a flat, filesystem-safe file name from an attachment's zip path,
/// unique (case-insensitively) within `taken`.
pub fn safe_file_name(rel_path: &str, taken: &mut std::collections::HashSet<String>) -> String {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let mut cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    cleaned = cleaned.trim_start_matches('.').to_string();
    if cleaned.chars().count() > 100 {
        // Keep the extension when truncating.
        let (stem, ext) = split_ext(&cleaned);
        let stem: String = stem.chars().take(90).collect();
        cleaned = format!("{}{}", stem, ext.chars().take(10).collect::<String>());
    }
    if cleaned.is_empty() {
        cleaned = "file".to_string();
    }
    let (stem, ext) = split_ext(&cleaned);
    let (stem, ext) = (stem.to_string(), ext.to_string());
    let mut candidate = cleaned;
    let mut n = 1;
    while taken.contains(&candidate.to_lowercase()) {
        candidate = format!("{}-{}{}", stem, n, ext);
        n += 1;
    }
    taken.insert(candidate.to_lowercase());
    candidate
}

/// Split `name.ext` into (`name`, `.ext`); no extension → (`name`, "").
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Best-effort MIME type from the file extension.
pub fn guess_mime(name: &str) -> Option<String> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    if !name.contains('.') {
        return None;
    }
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "heic" => "image/heic",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "json" => "application/json",
        "xml" => "application/xml",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "yaml" | "yml" => "application/yaml",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => return None,
    };
    Some(mime.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// Build an in-memory zip from `(name, bytes)` entries; names ending in `/`
    /// become directory entries.
    pub fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        for (name, data) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, opts).unwrap();
            } else {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data).unwrap();
            }
        }
        zip.finish().unwrap().into_inner()
    }

    const MD: &str = "---\ntitle: \"Deploy failed on staging\"\nworkspace: malme\nchannel: \"#dev-backend\"\nthread_url: \"https://malme.slack.com/archives/C0123/p1700000000123456\"\nthread_ts: \"1700000000.123456\"\nexported_at: \"2026-10-08T09:00:00Z\"\nextra: ignored\n---\n# Hello\n\nbody text\n";

    #[test]
    fn front_matter_is_parsed_and_stripped() {
        let t = parse_markdown(CaptureKind::Slack, MD.as_bytes()).unwrap();
        assert_eq!(t.title, "Deploy failed on staging");
        assert_eq!(t.workspace.as_deref(), Some("malme"));
        assert_eq!(t.channel.as_deref(), Some("#dev-backend"));
        assert_eq!(
            t.thread_url.as_deref(),
            Some("https://malme.slack.com/archives/C0123/p1700000000123456")
        );
        assert_eq!(t.thread_ts.as_deref(), Some("1700000000.123456"));
        assert_eq!(t.exported_at.as_deref(), Some("2026-10-08T09:00:00Z"));
        assert_eq!(t.content, "# Hello\n\nbody text\n");
    }

    #[test]
    fn no_front_matter_keeps_whole_body_and_falls_back_title() {
        let t = parse_markdown(CaptureKind::Slack, b"\n\n## Incident report  \nmore").unwrap();
        assert_eq!(t.title, "Incident report");
        assert_eq!(t.content, "\n\n## Incident report  \nmore");
        assert!(t.channel.is_none());
    }

    #[test]
    fn crlf_front_matter_and_bom() {
        let md = "\u{feff}---\r\nchannel: general\r\nthread_ts: 1700000000.5\r\n---\r\nhi\r\n";
        let t = parse_markdown(CaptureKind::Slack, md.as_bytes()).unwrap();
        assert_eq!(t.channel.as_deref(), Some("general"));
        assert_eq!(t.thread_ts.as_deref(), Some("1700000000.5"));
        assert_eq!(t.title, "hi");
        assert_eq!(t.content, "hi\r\n");
    }

    #[test]
    fn empty_title_falls_back() {
        let t = parse_markdown(CaptureKind::Slack, b"---\ntitle: \"  \"\n---\n").unwrap();
        assert_eq!(t.title, "Slack thread");
        let long = format!("# {}", "x".repeat(300));
        assert_eq!(first_line_title(&long).unwrap().chars().count(), 120);
    }

    #[test]
    fn unclosed_fence_is_body() {
        let t = parse_markdown(CaptureKind::Slack, b"---\nnot front matter").unwrap();
        assert_eq!(t.content, "---\nnot front matter");
    }

    #[test]
    fn invalid_yaml_and_utf8_rejected() {
        assert!(matches!(
            parse_markdown(CaptureKind::Slack, b"---\ntitle: [unclosed\n---\nx"),
            Err(IngestError::BadRequest(_))
        ));
        assert!(matches!(
            parse_markdown(CaptureKind::Slack, &[0xff, 0xfe, 0x00]),
            Err(IngestError::BadRequest(_))
        ));
    }

    #[test]
    fn zip_root_md_and_attachments() {
        let z = make_zip(&[
            ("thread.md", MD.as_bytes()),
            ("attachments/", b""),
            ("attachments/screenshot.png", b"\x89PNG"),
            ("attachments/log.txt", b"log"),
            ("attachments/notes.md", b"nested md is an attachment"),
            ("__MACOSX/._thread.md", b"junk"),
            ("attachments/.DS_Store", b"junk"),
        ]);
        let t = parse_zip(CaptureKind::Slack, &z).unwrap();
        assert_eq!(t.title, "Deploy failed on staging");
        let names: Vec<_> = t.attachments.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "attachments/screenshot.png",
                "attachments/log.txt",
                "attachments/notes.md"
            ]
        );
    }

    #[test]
    fn zip_single_nested_md_is_conversation() {
        let z = make_zip(&[("export/thread.md", b"# Nested"), ("export/a.png", b"x")]);
        let t = parse_zip(CaptureKind::Slack, &z).unwrap();
        assert_eq!(t.title, "Nested");
        assert_eq!(t.attachments.len(), 1);
    }

    #[test]
    fn zip_ambiguous_or_missing_md_rejected() {
        let two_root = make_zip(&[("a.md", b"a"), ("b.md", b"b")]);
        assert!(matches!(
            parse_zip(CaptureKind::Slack, &two_root),
            Err(IngestError::BadRequest(_))
        ));
        let two_nested = make_zip(&[("x/a.md", b"a"), ("y/b.md", b"b")]);
        assert!(matches!(
            parse_zip(CaptureKind::Slack, &two_nested),
            Err(IngestError::BadRequest(_))
        ));
        let none = make_zip(&[("a.png", b"a")]);
        assert!(matches!(
            parse_zip(CaptureKind::Slack, &none),
            Err(IngestError::BadRequest(_))
        ));
    }

    #[test]
    fn zip_slip_rejected() {
        for evil in ["../x", "a/../../x", "/etc/passwd", "..\\x", "C:/x"] {
            let z = make_zip(&[("t.md", b"t"), (evil, b"pwn")]);
            assert!(
                matches!(
                    parse_zip(CaptureKind::Slack, &z),
                    Err(IngestError::BadRequest(_))
                ),
                "{} should be rejected",
                evil
            );
        }
    }

    #[test]
    fn zip_entry_count_limit() {
        let names: Vec<String> = (0..=MAX_ZIP_ENTRIES)
            .map(|i| format!("f{}.txt", i))
            .collect();
        let mut entries: Vec<(&str, &[u8])> =
            names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        entries.push(("t.md", b"t"));
        let z = make_zip(&entries);
        assert!(matches!(
            parse_zip(CaptureKind::Slack, &z),
            Err(IngestError::BadRequest(_))
        ));
    }

    #[test]
    fn zip_bomb_rejected() {
        // ~201 MB of zeros compresses to a few hundred KB with deflate.
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        zip.start_file("t.md", opts).unwrap();
        zip.write_all(b"t").unwrap();
        zip.start_file("bomb.bin", opts).unwrap();
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..201 {
            zip.write_all(&chunk).unwrap();
        }
        let z = zip.finish().unwrap().into_inner();
        assert!(z.len() < MAX_BODY_BYTES);
        assert!(matches!(
            parse_zip(CaptureKind::Slack, &z),
            Err(IngestError::TooLarge(_))
        ));
    }

    #[test]
    fn garbage_zip_rejected() {
        assert!(matches!(
            parse_zip(CaptureKind::Slack, b"not a zip"),
            Err(IngestError::BadRequest(_))
        ));
    }

    #[test]
    fn content_type_and_path_mapping() {
        assert_eq!(
            PayloadKind::from_content_type("text/markdown; charset=utf-8"),
            Some(PayloadKind::Markdown)
        );
        assert_eq!(
            PayloadKind::from_content_type("text/plain"),
            Some(PayloadKind::Markdown)
        );
        assert_eq!(
            PayloadKind::from_content_type("application/zip"),
            Some(PayloadKind::Zip)
        );
        assert_eq!(PayloadKind::from_content_type("application/json"), None);
        assert_eq!(
            PayloadKind::from_path(std::path::Path::new("/x/T.ZIP")),
            Some(PayloadKind::Zip)
        );
    }

    #[test]
    fn url_normalization() {
        let n = |u: &str| normalize_url(u);
        assert_eq!(
            n("HTTPS://Example.COM:443/Docs/Page/?utm_source=x&b=2&a=1&fbclid=z#frag").as_deref(),
            Some("https://example.com/Docs/Page?a=1&b=2")
        );
        assert_eq!(
            n("http://example.com:80").as_deref(),
            Some("http://example.com/")
        );
        assert_eq!(
            n("http://example.com/").as_deref(),
            Some("http://example.com/")
        );
        assert_eq!(
            n("http://example.com:8080/a//").as_deref(),
            Some("http://example.com:8080/a/")
        );
        assert_eq!(
            n("https://x.io/p?ref=hn&ref_src=t&gclid=1&mc_cid=2&mc_eid=3&UTM_Medium=m&q=1")
                .as_deref(),
            Some("https://x.io/p?q=1")
        );
        assert_eq!(
            n("https://x.io?utm_x=1#a").as_deref(),
            Some("https://x.io/")
        );
        assert_eq!(
            n("https://user@Host.io/a").as_deref(),
            Some("https://user@host.io/a")
        );
        assert_eq!(n("http://[::1]:80/x").as_deref(), Some("http://[::1]/x"));
        assert_eq!(
            n("http://[::1]:3000/x").as_deref(),
            Some("http://[::1]:3000/x")
        );
        // Same page, different noise → same key.
        assert_eq!(
            n("https://v2.tauri.app/security/capabilities/"),
            n("https://V2.tauri.app/security/capabilities?utm_source=x#frag")
        );
        assert_eq!(n("not a url"), None);
        assert_eq!(n("https://"), None);
        assert_eq!(
            url_domain("https://www.Tauri.app:443/x").as_deref(),
            Some("tauri.app")
        );
    }

    #[test]
    fn web_front_matter_and_title_fallback() {
        let md = "---\ntitle: \"Caps\"\nurl: \"https://v2.tauri.app/security/capabilities/?utm_source=x\"\nsite_name: Tauri\nauthor: A\npublished_at: \"2026-09-01\"\ncaptured_at: \"2026-10-09T10:00:00Z\"\ndescription: D\nselection: false\nchannel: ignored\n---\nbody";
        let t = parse_markdown(CaptureKind::Web, md.as_bytes()).unwrap();
        assert_eq!(t.kind, CaptureKind::Web);
        assert_eq!(t.title, "Caps");
        assert_eq!(
            t.source_url.as_deref(),
            Some("https://v2.tauri.app/security/capabilities/?utm_source=x")
        );
        assert_eq!(
            t.url_key.as_deref(),
            Some("https://v2.tauri.app/security/capabilities")
        );
        assert_eq!(t.site_name.as_deref(), Some("Tauri"));
        assert_eq!(t.author.as_deref(), Some("A"));
        assert_eq!(t.published_at.as_deref(), Some("2026-09-01"));
        assert_eq!(t.exported_at.as_deref(), Some("2026-10-09T10:00:00Z"));
        assert_eq!(t.description.as_deref(), Some("D"));
        assert!(!t.selection);
        assert!(t.channel.is_none());

        let sel = parse_markdown(CaptureKind::Web, b"---\nselection: true\n---\nx").unwrap();
        assert!(sel.selection);
        let sel = parse_markdown(CaptureKind::Web, b"---\nselection: \"yes\"\n---\nx").unwrap();
        assert!(sel.selection);

        // Title: body line → domain → default.
        let t = parse_markdown(
            CaptureKind::Web,
            b"---\nurl: https://www.example.com/a\n---\n\n",
        )
        .unwrap();
        assert_eq!(t.title, "example.com");
        let t = parse_markdown(CaptureKind::Web, b"").unwrap();
        assert_eq!(t.title, "Web page");
        assert!(t.url_key.is_none());
        let t = parse_markdown(CaptureKind::Slack, b"").unwrap();
        assert_eq!(t.title, "Slack thread");
        // Slack payloads ignore web keys.
        let t = parse_markdown(CaptureKind::Slack, b"---\nurl: https://a.b/\n---\nx").unwrap();
        assert!(t.source_url.is_none());
    }

    #[test]
    fn safe_names_are_flat_and_unique() {
        let mut taken = HashSet::new();
        assert_eq!(
            safe_file_name("a/b/shot one.png", &mut taken),
            "shot_one.png"
        );
        assert_eq!(
            safe_file_name("c/shot one.png", &mut taken),
            "shot_one-1.png"
        );
        assert_eq!(
            safe_file_name("d/SHOT_ONE.png", &mut taken),
            "SHOT_ONE-2.png"
        );
        assert_eq!(safe_file_name("e/.hidden", &mut taken), "hidden");
        assert_eq!(safe_file_name("f/...", &mut taken), "file");
        assert_eq!(guess_mime("a/b.PNG").as_deref(), Some("image/png"));
        assert_eq!(guess_mime("noext"), None);
    }
}
