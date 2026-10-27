//! Direct launch for `npx`-based stdio MCP servers.
//!
//! A server configured as `npx [-y] <pkg>[@ver] …` costs every run an extra
//! `npm exec` wrapper process (~90–120 MB RSS) that does nothing but sit between
//! the engine and the real server for the whole session, and `@latest` makes npm
//! hit the registry on every spawn. Devdy instead installs the package once into
//! `<app data>/mcp-packages/<spec>/` and launches its bin directly.
//!
//! Never blocks or breaks a run: when the package isn't installed yet the
//! original `npx` command is used unchanged and an install starts in the
//! background, so the NEXT run gets the direct launch. Floating specs (no
//! version, a dist-tag like `latest`, or a range) are re-installed in the
//! background once the install is older than `REFRESH_AFTER`; the current
//! install keeps being used meanwhile. Anything this module doesn't fully
//! understand (unknown npx flags, `-p`, git/path specs) is left to `npx`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

/// Re-install a floating spec in the background once its install is this old.
const REFRESH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Written after a successful install; its mtime is the install time.
const MARKER: &str = ".devdy-installed";

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Specs with an install in flight, so concurrent runs don't race npm.
static INSTALLING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// Set once at startup to `<app data>/mcp-packages`.
pub fn set_root(dir: PathBuf) {
    let _ = ROOT.set(dir);
}

/// An `npx` invocation this module can launch directly.
#[derive(Debug, PartialEq, Eq)]
struct NpxCall {
    /// Package spec as written, e.g. `@playwright/mcp@latest`.
    spec: String,
    /// Package name without the version, e.g. `@playwright/mcp`.
    name: String,
    /// Whether the spec can resolve to a newer version over time.
    floating: bool,
    /// Arguments passed through to the package's bin.
    args: Vec<String>,
}

/// Parse `npx` + args into a launchable call. None when the command isn't npx
/// or uses anything beyond `-y`/`--yes`/`-q`/`--quiet`/`--` before the package.
fn parse_npx(command: &str, args: &[String]) -> Option<NpxCall> {
    let file = Path::new(command.trim()).file_name()?.to_str()?;
    if file != "npx" {
        return None;
    }
    let mut it = args.iter();
    let spec = loop {
        let a = it.next()?;
        match a.as_str() {
            "-y" | "--yes" | "-q" | "--quiet" | "--" => continue,
            s if s.starts_with('-') => return None,
            s => break s.to_string(),
        }
    };
    let (name, version) = split_spec(&spec)?;
    let floating = !version.as_deref().is_some_and(is_exact_version);
    Some(NpxCall {
        name: name.to_string(),
        spec,
        floating,
        args: it.cloned().collect(),
    })
}

/// Split `name[@version]` (scoped or not). None for paths, URLs, git specs.
fn split_spec(spec: &str) -> Option<(&str, Option<String>)> {
    if spec.is_empty() || spec.contains(':') || spec.starts_with('.') || spec.starts_with('/') {
        return None;
    }
    let (scope_len, rest) = match spec.strip_prefix('@') {
        Some(r) => {
            let slash = r.find('/')?;
            (slash + 2, &r[slash + 1..])
        }
        None => (0, spec),
    };
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    match rest.find('@') {
        Some(at) => {
            let end = scope_len + at;
            let version = &spec[end + 1..];
            if version.is_empty() {
                return None;
            }
            Some((&spec[..end], Some(version.to_string())))
        }
        None => Some((spec, None)),
    }
}

fn is_exact_version(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The bin `npx <name>` runs, from the installed package.json: a string `bin`
/// is named after the package; a map uses its only entry, else the entry named
/// after the package (as npx does).
fn bin_name(name: &str, manifest: &serde_json::Value) -> Option<String> {
    let unscoped = name.rsplit('/').next().unwrap_or(name);
    match manifest.get("bin")? {
        serde_json::Value::String(_) => Some(unscoped.to_string()),
        serde_json::Value::Object(map) if map.len() == 1 => map.keys().next().cloned(),
        serde_json::Value::Object(map) => map.contains_key(unscoped).then(|| unscoped.to_string()),
        _ => None,
    }
}

/// Install directory for a spec (`/` in scoped names flattened).
fn install_dir(root: &Path, spec: &str) -> PathBuf {
    root.join(spec.replace('/', "+"))
}

/// The installed bin for `call`, if present.
fn installed_bin(dir: &Path, call: &NpxCall) -> Option<PathBuf> {
    if !dir.join(MARKER).exists() {
        return None;
    }
    let manifest_path = dir
        .join("node_modules")
        .join(&call.name)
        .join("package.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(manifest_path).ok()?).ok()?;
    let bin = dir
        .join("node_modules")
        .join(".bin")
        .join(bin_name(&call.name, &manifest)?);
    bin.exists().then_some(bin)
}

fn is_stale(dir: &Path) -> bool {
    std::fs::metadata(dir.join(MARKER))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_none_or(|age| age > REFRESH_AFTER)
}

/// Rewrite an `npx` stdio server to launch its installed bin directly.
/// Returns None (keep the configured command) when not applicable or not
/// installed yet; schedules a background install / refresh as needed.
pub fn direct_launch(command: &str, args: &[String]) -> Option<(String, Vec<String>)> {
    let call = parse_npx(command, args)?;
    let root = ROOT.get()?;
    let dir = install_dir(root, &call.spec);
    let bin = installed_bin(&dir, &call);
    if bin.is_none() || (call.floating && is_stale(&dir)) {
        schedule_install(call.spec.clone(), dir);
    }
    let bin = bin?;
    Some((bin.to_string_lossy().into_owned(), call.args))
}

/// Install `spec` into `dir` in the background (once at a time per spec).
/// Failures are logged; runs keep using `npx` until an install succeeds.
fn schedule_install(spec: String, dir: PathBuf) {
    let installing = INSTALLING.get_or_init(Default::default);
    if !installing.lock().unwrap().insert(spec.clone()) {
        return;
    }
    tokio::spawn(async move {
        let result = install(&spec, &dir).await;
        installing.lock().unwrap().remove(&spec);
        match result {
            Ok(()) => tracing::info!(event = "mcp_package_installed", spec = %spec),
            Err(e) => {
                tracing::warn!(event = "mcp_package_install_failed", spec = %spec, error = %e)
            }
        }
    });
}

/// Install into a fresh staging dir, then swap it in, so a run spawning
/// mid-install never sees a half-written tree (it falls back to `npx` during the
/// brief swap). Servers already running keep their open files.
async fn install(spec: &str, dir: &Path) -> Result<(), String> {
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let sibling = |tag: &str| {
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        dir.with_file_name(format!("{name}.{tag}-{stamp}"))
    };
    let staging = sibling("staging");
    let result = install_into(spec, &staging).await.and_then(|()| {
        let old = sibling("old");
        if dir.exists() {
            std::fs::rename(dir, &old).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&staging, dir).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&old);
        Ok(())
    });
    let _ = std::fs::remove_dir_all(&staging);
    result
}

async fn install_into(spec: &str, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut cmd = tokio::process::Command::new("npm");
    cmd.arg("install")
        .arg("--prefix")
        .arg(dir)
        .args(["--no-audit", "--no-fund", "--omit=dev", "--loglevel=error"])
        .arg(spec)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    crate::runs::sidecar::augment_command_path(&mut cmd);
    let out = tokio::time::timeout(Duration::from_secs(300), cmd.output())
        .await
        .map_err(|_| "npm install timed out".to_string())?
        .map_err(|e| format!("npm not runnable: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let last = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(format!("npm install exited with {}: {}", out.status, last));
    }
    std::fs::write(dir.join(MARKER), spec).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_common_npx_forms() {
        let c = parse_npx("npx", &s(&["@playwright/mcp@latest"])).unwrap();
        assert_eq!(c.name, "@playwright/mcp");
        assert!(c.floating);
        assert!(c.args.is_empty());

        let c = parse_npx(
            "/opt/node/bin/npx",
            &s(&["-y", "slack-mcp-server@latest", "--transport", "stdio"]),
        )
        .unwrap();
        assert_eq!(c.spec, "slack-mcp-server@latest");
        assert_eq!(c.name, "slack-mcp-server");
        assert_eq!(c.args, s(&["--transport", "stdio"]));

        let c = parse_npx("npx", &s(&["--yes", "--", "@scope/pkg@1.2.3", "-v"])).unwrap();
        assert_eq!(c.name, "@scope/pkg");
        assert!(!c.floating);
        assert_eq!(c.args, s(&["-v"]));

        let c = parse_npx("npx", &s(&["chrome-devtools-mcp"])).unwrap();
        assert!(c.floating);
        assert!(parse_npx("npx", &s(&["pkg@^1.2.0"])).unwrap().floating);
    }

    #[test]
    fn leaves_unsupported_invocations_to_npx() {
        assert!(parse_npx("node", &s(&["server.js"])).is_none());
        assert!(parse_npx("npx", &s(&[])).is_none());
        assert!(parse_npx("npx", &s(&["-p", "pkg", "bin"])).is_none());
        assert!(parse_npx("npx", &s(&["--package=pkg", "bin"])).is_none());
        assert!(parse_npx("npx", &s(&["github:user/repo"])).is_none());
        assert!(parse_npx("npx", &s(&["./local"])).is_none());
        assert!(parse_npx("npx", &s(&["@scope"])).is_none());
        assert!(parse_npx("npx", &s(&["pkg@"])).is_none());
    }

    #[test]
    fn picks_the_bin_npx_would_run() {
        let one = serde_json::json!({ "bin": { "playwright-mcp": "cli.js" } });
        assert_eq!(
            bin_name("@playwright/mcp", &one).as_deref(),
            Some("playwright-mcp")
        );
        let string = serde_json::json!({ "bin": "index.js" });
        assert_eq!(bin_name("@scope/tool", &string).as_deref(), Some("tool"));
        let many = serde_json::json!({ "bin": { "a": "a.js", "tool": "t.js" } });
        assert_eq!(bin_name("tool", &many).as_deref(), Some("tool"));
        let ambiguous = serde_json::json!({ "bin": { "a": "a.js", "b": "b.js" } });
        assert_eq!(bin_name("tool", &ambiguous), None);
        assert_eq!(bin_name("tool", &serde_json::json!({})), None);
    }

    #[test]
    fn launches_installed_bin_with_passthrough_args() {
        let root = std::env::temp_dir().join(format!("devdy-mcp-pkg-{}", std::process::id()));
        let call = parse_npx("npx", &s(&["-y", "@scope/tool@1.0.0", "--port", "1"])).unwrap();
        let dir = install_dir(&root, &call.spec);
        assert!(dir.ends_with("@scope+tool@1.0.0"));
        assert!(installed_bin(&dir, &call).is_none());

        let pkg = dir.join("node_modules/@scope/tool");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), r#"{"bin":{"tool":"cli.js"}}"#).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
        std::fs::write(dir.join("node_modules/.bin/tool"), "").unwrap();
        // No marker yet: a half-finished install is not used.
        assert!(installed_bin(&dir, &call).is_none());
        std::fs::write(dir.join(MARKER), &call.spec).unwrap();
        assert_eq!(
            installed_bin(&dir, &call),
            Some(dir.join("node_modules/.bin/tool"))
        );
        assert!(!is_stale(&dir));

        let _ = std::fs::remove_dir_all(&root);
    }
}
