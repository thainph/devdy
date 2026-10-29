//! Remote desktop, controlling side: serve the controlled machine's files to a
//! remote-desktop window as URLs, and save them to this machine's Downloads.
//!
//! `convertFileSrc(path)` must turn a path into a URL synchronously, and in a
//! remote-desktop window `path` lives on the other machine. Its URLs use the
//! `rdfile` scheme registered here; each request is handed to the webview that
//! made it (`rdfile://request`), whose shim (src/lib/ipc/native.ts) fetches the
//! bytes over the relay (`remote_fs_read`) and answers with `rdfile_respond`.
//! Being a real protocol, it works for every consumer of an asset URL — `<img>`,
//! `<video>` / `<audio>` (Range requests), `<iframe>` with relative
//! sub-resources, `fetch` (pdf.js) — without touching any call site.
//!
//! Requests made before the webview's shim is listening wait here and are
//! handed over by `rdfile_register`, which also re-delivers everything still
//! unanswered when a reloaded page registers again. Unanswered requests fail
//! with 504 after [`REQUEST_TIMEOUT`].
//!
//! The download commands only ever create and append to fresh files in the
//! Downloads folder, under a sanitized base name.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::http::{header, Response, StatusCode};
use tauri::utils::mime_type::MimeType;
use tauri::{Emitter, Manager, Runtime, UriSchemeContext, UriSchemeResponder};
use tokio::sync::oneshot;

/// URL scheme of a controlled-machine file.
pub const SCHEME: &str = "rdfile";
/// Event (to the requesting webview) carrying a [`FileRequest`].
const REQUEST_EVENT: &str = "rdfile://request";
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// A requested byte range, as in the `Range` header (single range only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ByteRange {
    /// First byte; `None` for a suffix range (`bytes=-N`).
    pub start: Option<u64>,
    /// Last byte (inclusive), or the suffix length when `start` is `None`.
    pub end: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileRequest {
    pub id: String,
    pub path: String,
    pub range: Option<ByteRange>,
}

/// The shim's answer: `status` 200/206 with the bytes from `start` of a file of
/// `total` bytes, or an error status with an empty body.
#[derive(Debug)]
pub struct FileAnswer {
    pub status: u16,
    pub start: u64,
    pub total: u64,
    pub body: Vec<u8>,
}

struct Pending {
    label: String,
    request: FileRequest,
    reply: oneshot::Sender<FileAnswer>,
}

#[derive(Default)]
struct Inner {
    /// Webviews whose shim is listening for requests.
    registered: HashSet<String>,
    pending: HashMap<String, Pending>,
    /// Files created by `rd_download_create` (the only ones it may append to).
    downloads: HashSet<PathBuf>,
}

#[derive(Clone, Default)]
pub struct FileProxy(Arc<Mutex<Inner>>);

impl FileProxy {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The controlled-machine path in an `rdfile` URL: the percent-decoded URL
/// path without its leading `/` (as Tauri's asset protocol does).
pub fn path_from_uri_path(uri_path: &str) -> String {
    let raw = uri_path.strip_prefix('/').unwrap_or(uri_path).as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(raw[i + 1]), hex(raw[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a `Range` header. Multi-range and malformed values are ignored
/// (the whole file is served), as servers may.
pub fn parse_range(value: &str) -> Option<ByteRange> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    let num = |s: &str| if s.is_empty() { Ok(None) } else { s.parse::<u64>().map(Some) };
    let (start, end) = (num(a).ok()?, num(b).ok()?);
    match (start, end) {
        (None, None) => None,
        (None, Some(0)) => None,
        (Some(s), Some(e)) if e < s => None,
        _ => Some(ByteRange { start, end }),
    }
}

/// Content type by extension (what the previews need), else sniffed from the
/// first bytes when they are the start of the file.
pub fn content_type(path: &str, body: &[u8], start: u64) -> String {
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let known = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/vnd.microsoft.icon",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "html" | "htm" => "text/html",
        "xhtml" => "application/xhtml+xml",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "txt" | "md" => "text/plain",
        _ => "",
    };
    if !known.is_empty() {
        known.to_string()
    } else if start == 0 && !body.is_empty() {
        MimeType::parse_with_fallback(body, path, MimeType::OctetStream)
    } else {
        "application/octet-stream".to_string()
    }
}

/// Build the HTTP response for an answer to a request for `path`.
pub fn build_response(path: &str, answer: FileAnswer) -> Response<Vec<u8>> {
    let base = Response::builder()
        .header("Access-Control-Allow-Origin", "*")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::ACCEPT_RANGES, "bytes");
    let res = match answer.status {
        200 | 206 => {
            let ct = content_type(path, &answer.body, answer.start);
            let b = base
                .status(answer.status)
                .header(header::CONTENT_TYPE, ct)
                .header(header::CONTENT_LENGTH, answer.body.len());
            let b = if answer.status == 206 {
                let last = (answer.start + answer.body.len() as u64).saturating_sub(1);
                b.header(header::CONTENT_RANGE, format!("bytes {}-{}/{}", answer.start, last, answer.total))
            } else {
                b
            };
            b.body(answer.body)
        }
        416 => base
            .status(416)
            .header(header::CONTENT_RANGE, format!("bytes */{}", answer.total))
            .body(Vec::new()),
        s => base
            .status(StatusCode::from_u16(s).unwrap_or(StatusCode::BAD_GATEWAY))
            .body(Vec::new()),
    };
    res.unwrap_or_else(|_| Response::builder().status(500).body(Vec::new()).expect("static response"))
}

/// The `rdfile` protocol handler (see module docs).
pub fn handle<R: Runtime>(ctx: UriSchemeContext<'_, R>, request: tauri::http::Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = ctx.app_handle().clone();
    let label = ctx.webview_label().to_string();
    let path = path_from_uri_path(request.uri().path());
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_range);
    let Some(proxy) = app.try_state::<FileProxy>().map(|s| s.inner().clone()) else {
        responder.respond(build_response(&path, FileAnswer { status: 503, start: 0, total: 0, body: Vec::new() }));
        return;
    };
    let id = uuid::Uuid::new_v4().to_string();
    let req = FileRequest { id: id.clone(), path: path.clone(), range };
    let (tx, rx) = oneshot::channel();
    let deliver = {
        let mut inner = proxy.lock();
        let registered = inner.registered.contains(&label);
        inner.pending.insert(id.clone(), Pending { label: label.clone(), request: req.clone(), reply: tx });
        registered
    };
    if deliver {
        let _ = app.emit_to(label.as_str(), REQUEST_EVENT, &req);
    }
    tauri::async_runtime::spawn(async move {
        let answer = match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(answer)) => answer,
            _ => {
                proxy.lock().pending.remove(&id);
                FileAnswer { status: 504, start: 0, total: 0, body: Vec::new() }
            }
        };
        responder.respond(build_response(&path, answer));
    });
}

/// A base name safe to create in the Downloads folder.
pub fn sanitize_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    if cleaned.is_empty() {
        "download".to_string()
    } else {
        cleaned
    }
}

/// `name`, then `stem (1).ext`, `stem (2).ext`… for the n-th collision.
pub fn numbered_name(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_string();
    }
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} ({n}){}", &name[..i], &name[i..]),
        _ => format!("{name} ({n})"),
    }
}

fn header_str<'a>(request: &'a tauri::ipc::Request<'_>, name: &str) -> Option<&'a str> {
    request.headers().get(name).and_then(|v| v.to_str().ok())
}

fn raw_body(request: &tauri::ipc::Request<'_>) -> Vec<u8> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.clone(),
        tauri::ipc::InvokeBody::Json(_) => Vec::new(),
    }
}

// ---- Tauri commands (local only: called with the REAL invoke) ----

/// The calling webview's shim is listening: deliver what is waiting for it.
#[tauri::command]
pub fn rdfile_register(webview: tauri::Webview, state: tauri::State<'_, FileProxy>) -> Vec<FileRequest> {
    let label = webview.label().to_string();
    let mut inner = state.lock();
    inner.registered.insert(label.clone());
    inner
        .pending
        .values()
        .filter(|p| p.label == label)
        .map(|p| p.request.clone())
        .collect()
}

/// Answer a request. Raw body = the bytes; headers `x-rd-id`, `x-rd-status`,
/// `x-rd-start`, `x-rd-total`.
#[tauri::command]
pub fn rdfile_respond(request: tauri::ipc::Request<'_>, state: tauri::State<'_, FileProxy>) -> Result<(), String> {
    let id = header_str(&request, "x-rd-id").ok_or("missing x-rd-id")?.to_string();
    let num = |name: &str| header_str(&request, name).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let answer = FileAnswer {
        status: u16::try_from(num("x-rd-status")).unwrap_or(502),
        start: num("x-rd-start"),
        total: num("x-rd-total"),
        body: raw_body(&request),
    };
    // Unknown ids timed out already (or were answered by an older page).
    if let Some(p) = state.lock().pending.remove(&id) {
        let _ = p.reply.send(answer);
    }
    Ok(())
}

/// Create a new, empty file named after `name` in the Downloads folder
/// (numbered on collision); returns its path.
#[tauri::command]
pub fn rd_download_create(app: tauri::AppHandle, state: tauri::State<'_, FileProxy>, name: String) -> Result<String, String> {
    let dir = app.path().download_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = sanitize_file_name(&name);
    for n in 0..1000 {
        let path = dir.join(numbered_name(&name, n));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => {
                state.lock().downloads.insert(path.clone());
                return Ok(path.to_string_lossy().into_owned());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("no free file name in Downloads".into())
}

/// Append the raw body to a file made by [`rd_download_create`] (header
/// `x-rd-path`, URI-encoded).
#[tauri::command]
pub fn rd_download_append(request: tauri::ipc::Request<'_>, state: tauri::State<'_, FileProxy>) -> Result<(), String> {
    let encoded = header_str(&request, "x-rd-path").ok_or("missing x-rd-path")?;
    let path = PathBuf::from(path_from_uri_path(encoded));
    if !state.lock().downloads.contains(&path) {
        return Err("not a download".into());
    }
    let mut file = std::fs::OpenOptions::new().append(true).open(&path).map_err(|e| e.to_string())?;
    file.write_all(&raw_body(&request)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_uri_paths_like_the_asset_protocol() {
        assert_eq!(path_from_uri_path("/%2FUsers%2Fme%2Fa%20b.png"), "/Users/me/a b.png");
        // pageUrl form: one encoded segment per component, extra leading slash.
        assert_eq!(path_from_uri_path("//Users/me/site/style.css"), "/Users/me/site/style.css");
        assert_eq!(path_from_uri_path("/C%3A%5Cx%5C%E1%BA%A3nh.png"), "C:\\x\\ảnh.png");
        assert_eq!(path_from_uri_path("/bad%zz%2"), "bad%zz%2");
    }

    #[test]
    fn parses_single_byte_ranges() {
        assert_eq!(parse_range("bytes=0-1"), Some(ByteRange { start: Some(0), end: Some(1) }));
        assert_eq!(parse_range("bytes=100-"), Some(ByteRange { start: Some(100), end: None }));
        assert_eq!(parse_range("bytes=-500"), Some(ByteRange { start: None, end: Some(500) }));
        assert_eq!(parse_range("bytes=5-1"), None);
        assert_eq!(parse_range("bytes=0-1,4-5"), None);
        assert_eq!(parse_range("items=0-1"), None);
        assert_eq!(parse_range("bytes=-"), None);
    }

    #[test]
    fn content_types_by_extension_then_sniffed() {
        assert_eq!(content_type("/a/B.PNG", b"", 0), "image/png");
        assert_eq!(content_type("/a/x.svg", b"<svg", 0), "image/svg+xml");
        assert_eq!(content_type("/a/noext", b"%PDF-1.7\n", 0), "application/pdf");
        assert_eq!(content_type("/a/noext", b"%PDF-1.7\n", 10), "application/octet-stream");
    }

    #[test]
    fn builds_full_partial_and_error_responses() {
        let full = build_response("/a.png", FileAnswer { status: 200, start: 0, total: 3, body: vec![1, 2, 3] });
        assert_eq!(full.status(), 200);
        assert_eq!(full.headers()[header::CONTENT_LENGTH], "3");
        assert_eq!(full.headers()[header::CONTENT_TYPE], "image/png");

        let part = build_response("/v.mp4", FileAnswer { status: 206, start: 10, total: 100, body: vec![0; 5] });
        assert_eq!(part.status(), 206);
        assert_eq!(part.headers()[header::CONTENT_RANGE], "bytes 10-14/100");

        let bad = build_response("/v.mp4", FileAnswer { status: 416, start: 0, total: 100, body: vec![] });
        assert_eq!(bad.headers()[header::CONTENT_RANGE], "bytes */100");
        let nf = build_response("/x", FileAnswer { status: 404, start: 0, total: 0, body: vec![] });
        assert_eq!(nf.status(), 404);
        assert_eq!(nf.headers()["Access-Control-Allow-Origin"], "*");
    }

    #[test]
    fn sanitizes_and_numbers_download_names() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("C:\\a\\b:c?.txt"), "b_c_.txt");
        assert_eq!(sanitize_file_name(".."), "download");
        assert_eq!(sanitize_file_name(".bashrc"), "bashrc");
        assert_eq!(numbered_name("a.tar.gz", 2), "a.tar (2).gz");
        assert_eq!(numbered_name("README", 1), "README (1)");
        assert_eq!(numbered_name("x.png", 0), "x.png");
    }
}
