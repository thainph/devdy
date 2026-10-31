//! Secret notes: notes no AI engine, MCP tool, log, backup reader or remote
//! controller can read.
//!
//! Nothing about them is in SQLite (`sidecar-mcp` and any agent reading
//! `data.db` see nothing). The consolidated Keychain item (`crate::secrets`)
//! holds only sealed data:
//! - the vault header: a random data key (DEK) wrapped under an Argon2id key
//!   derived from the vault password, and again under a recovery key shown
//!   once at setup;
//! - each note's title + content sealed with the DEK (XChaCha20-Poly1305, the
//!   AAD binds vault id + note id). Project, position and dates stay in clear.
//!
//! So reading the Keychain (an agent running `security`, a sync peer, a backup
//! file) yields ciphertext. The DEK only lives in this process while the vault
//! is unlocked and is wiped on lock: explicit, after `AUTO_LOCK` idle, or when
//! the header it belongs to is replaced. Idle time is wall-clock, so time
//! asleep counts.
//!
//! Sync: header and notes travel as Keychain items (`secret_vault`,
//! `secret_notes`). When two devices each set up a vault, the header is last
//! writer wins and the replaced one is kept machine-locally (`old_headers`), so
//! the notes sealed under it can be merged into the current vault with the old
//! password or recovery key ([`merge_old`]). A backup import of another vault
//! lands there too. Changing the password rotates everything ([`rotate`]): a
//! new vault (id, DEK, recovery key) re-seals the notes, so an old header or
//! recovery key (in a backup, on a peer) opens nothing current.
//!
//! Never log titles, contents, passwords or keys; errors are bare codes.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use dryoc::classic::crypto_aead_xchacha20poly1305_ietf::{
    crypto_aead_xchacha20poly1305_ietf_decrypt, crypto_aead_xchacha20poly1305_ietf_encrypt, Nonce,
};
use dryoc::classic::crypto_pwhash::{crypto_pwhash, PasswordHashAlgorithm};
use dryoc::rng::copy_randombytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};
use zeroize::Zeroizing;

type Key = [u8; 32];

const KDF_NAME: &str = "argon2id13";
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const SALT_LEN: usize = 16;
pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_TITLE_LEN: usize = 512;
pub const MAX_CONTENT_LEN: usize = 64 * 1024;
/// Idle time after which an unlocked vault locks itself.
pub const AUTO_LOCK: Duration = Duration::from_secs(5 * 60);

// Error codes the UI translates.
pub const E_LOCKED: &str = "vault_locked";
pub const E_MISSING: &str = "vault_missing";
pub const E_EXISTS: &str = "vault_exists";
pub const E_WRONG_PASSWORD: &str = "wrong_password";
pub const E_WRONG_RECOVERY: &str = "wrong_recovery_key";
pub const E_WEAK_PASSWORD: &str = "weak_password";
pub const E_THROTTLED: &str = "vault_throttled";
pub const E_NOT_FOUND: &str = "note_not_found";
pub const E_TOO_LARGE: &str = "too_large";
pub const E_CORRUPT: &str = "vault_corrupt";

/// Argon2id cost. Tests use a cheap one.
#[derive(Clone, Copy)]
pub struct KdfParams {
    pub opslimit: u64,
    pub memlimit: usize,
}

pub const DEFAULT_KDF: KdfParams = KdfParams { opslimit: 3, memlimit: 64 * 1024 * 1024 };

/// Everything the Keychain holds for the vault. Sealed data only.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct VaultData {
    #[serde(default)]
    pub header: Option<VaultHeader>,
    /// Headers replaced by sync or met in a backup import, kept until their
    /// notes are merged. Machine-local (never synced).
    #[serde(default)]
    pub old_headers: Vec<VaultHeader>,
    #[serde(default)]
    pub notes: HashMap<String, SealedNote>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultHeader {
    pub vault_id: String,
    pub kdf: String,
    pub opslimit: u64,
    pub memlimit: usize,
    pub salt: String,
    pub nonce: String,
    /// The DEK sealed under the password key.
    pub wrapped: String,
    #[serde(default)]
    pub recovery_nonce: Option<String>,
    /// The DEK sealed under the recovery key.
    #[serde(default)]
    pub recovery_wrapped: Option<String>,
    pub created_at: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SealedNote {
    pub vault_id: String,
    pub nonce: String,
    /// `{"title", "content"}` sealed with the DEK.
    pub ciphertext: String,
    #[serde(default)]
    pub project_id: Option<String>,
    pub position: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// A note's sealed part. No `Debug`: it holds the secret.
#[derive(Serialize, Deserialize)]
struct NotePlain {
    title: String,
    content: String,
}

/// What the note list shows (no content).
#[derive(Debug, Clone, Serialize)]
pub struct SecretNoteMeta {
    pub id: String,
    pub title: String,
    pub project_id: Option<String>,
    pub position: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl VaultData {
    /// A header arriving from a sync peer. A different vault replaces the
    /// current one; the current header is kept while notes still need it.
    /// A header never changes under the same id (a new password is a new
    /// vault), so a different one with this id is refused.
    pub fn apply_synced_header(&mut self, incoming: VaultHeader) {
        if let Some(cur) = self.header.take() {
            if cur.vault_id == incoming.vault_id {
                self.header = Some(cur);
                return;
            }
            self.keep_old(cur);
        }
        self.old_headers.retain(|h| h.vault_id != incoming.vault_id);
        self.header = Some(incoming);
        self.drop_unused_old_headers();
    }

    fn keep_old(&mut self, h: VaultHeader) {
        if !self.old_headers.iter().any(|o| o.vault_id == h.vault_id) {
            self.old_headers.push(h);
        }
    }

    fn current_id(&self) -> Option<&str> {
        self.header.as_ref().map(|h| h.vault_id.as_str())
    }

    /// Notes sealed under an old header (mergeable with its password).
    pub fn legacy_count(&self) -> usize {
        self.notes
            .values()
            .filter(|n| self.old_headers.iter().any(|h| h.vault_id == n.vault_id))
            .count()
    }

    /// Notes sealed under a header this device does not have (yet).
    pub fn pending_count(&self) -> usize {
        self.notes
            .values()
            .filter(|n| self.current_id() != Some(n.vault_id.as_str()))
            .filter(|n| !self.old_headers.iter().any(|h| h.vault_id == n.vault_id))
            .count()
    }

    pub fn drop_unused_old_headers(&mut self) {
        let notes = &self.notes;
        self.old_headers.retain(|h| notes.values().any(|n| n.vault_id == h.vault_id));
    }
}

// ---- Crypto ------------------------------------------------------------------

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn derive(password: &str, salt: &[u8], opslimit: u64, memlimit: usize) -> Result<Zeroizing<Key>, String> {
    let mut key = Zeroizing::new([0u8; 32]);
    crypto_pwhash(key.as_mut(), password.as_bytes(), salt, opslimit, memlimit, PasswordHashAlgorithm::Argon2id13)
        .map_err(|_| E_CORRUPT.to_string())?;
    Ok(key)
}

fn seal(key: &Key, plain: &[u8], aad: &[u8]) -> Result<(String, String), String> {
    let mut nonce: Nonce = [0u8; NONCE_LEN];
    copy_randombytes(&mut nonce);
    let mut cipher = vec![0u8; plain.len() + TAG_LEN];
    crypto_aead_xchacha20poly1305_ietf_encrypt(&mut cipher, plain, Some(aad), &nonce, key)
        .map_err(|_| E_CORRUPT.to_string())?;
    Ok((B64.encode(nonce), B64.encode(cipher)))
}

/// `None` when the key is wrong or the data was tampered with.
fn open(key: &Key, nonce: &str, cipher: &str, aad: &[u8]) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    let nonce: Nonce = B64
        .decode(nonce)
        .ok()
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| E_CORRUPT.to_string())?;
    let cipher = B64.decode(cipher).map_err(|_| E_CORRUPT.to_string())?;
    if cipher.len() < TAG_LEN {
        return Err(E_CORRUPT.into());
    }
    let mut plain = Zeroizing::new(vec![0u8; cipher.len() - TAG_LEN]);
    match crypto_aead_xchacha20poly1305_ietf_decrypt(&mut plain, &cipher, Some(aad), &nonce, key) {
        Ok(()) => Ok(Some(plain)),
        Err(_) => Ok(None),
    }
}

fn header_aad(vault_id: &str) -> Vec<u8> {
    format!("devdy-secret-vault:v1:{vault_id}").into_bytes()
}

fn recovery_aad(vault_id: &str) -> Vec<u8> {
    format!("devdy-secret-vault-recovery:v1:{vault_id}").into_bytes()
}

fn note_aad(vault_id: &str, note_id: &str) -> Vec<u8> {
    format!("devdy-secret-note:v1:{vault_id}:{note_id}").into_bytes()
}

fn as_key(bytes: &[u8]) -> Result<Zeroizing<Key>, String> {
    let mut key = Zeroizing::new([0u8; 32]);
    if bytes.len() != 32 {
        return Err(E_CORRUPT.into());
    }
    key.copy_from_slice(bytes);
    Ok(key)
}

/// Recovery key as shown to the user: 64 hex digits in groups of 8.
fn format_recovery(key: &Key) -> String {
    hex::encode_upper(key)
        .as_bytes()
        .chunks(8)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

fn parse_recovery(text: &str) -> Option<Zeroizing<Key>> {
    let digits: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let bytes = Zeroizing::new(hex::decode(digits).ok()?);
    as_key(&bytes).ok()
}

/// Bound the KDF cost a header may ask for: headers arrive by sync and backups.
fn check_params(h: &VaultHeader) -> Result<(), String> {
    if h.kdf != KDF_NAME || !(1..=10).contains(&h.opslimit) || !(8 * 1024..=1024 * 1024 * 1024).contains(&h.memlimit) {
        return Err(E_CORRUPT.into());
    }
    Ok(())
}

/// Seal `dek` under `password` into `h` (fresh salt and nonce).
fn wrap_password(h: &mut VaultHeader, dek: &Key, password: &str, params: KdfParams) -> Result<(), String> {
    let mut salt = [0u8; SALT_LEN];
    copy_randombytes(&mut salt);
    let kek = derive(password, &salt, params.opslimit, params.memlimit)?;
    let (nonce, wrapped) = seal(&kek, dek, &header_aad(&h.vault_id))?;
    h.kdf = KDF_NAME.into();
    h.opslimit = params.opslimit;
    h.memlimit = params.memlimit;
    h.salt = B64.encode(salt);
    h.nonce = nonce;
    h.wrapped = wrapped;
    Ok(())
}

/// A new vault: its header, its DEK and the recovery key to show once.
pub fn new_header(password: &str, params: KdfParams) -> Result<(VaultHeader, Zeroizing<Key>, String), String> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(E_WEAK_PASSWORD.into());
    }
    let mut dek = Zeroizing::new([0u8; 32]);
    copy_randombytes(dek.as_mut());
    let mut recovery = Zeroizing::new([0u8; 32]);
    copy_randombytes(recovery.as_mut());
    let vault_id = uuid::Uuid::new_v4().simple().to_string();
    let (recovery_nonce, recovery_wrapped) = seal(&recovery, dek.as_ref(), &recovery_aad(&vault_id))?;
    let mut h = VaultHeader {
        vault_id,
        kdf: KDF_NAME.into(),
        opslimit: 0,
        memlimit: 0,
        salt: String::new(),
        nonce: String::new(),
        wrapped: String::new(),
        recovery_nonce: Some(recovery_nonce),
        recovery_wrapped: Some(recovery_wrapped),
        created_at: now(),
    };
    wrap_password(&mut h, &dek, password, params)?;
    Ok((h, dek, format_recovery(&recovery)))
}

/// The DEK of `h` from its password. Slow (Argon2id): call off the async runtime.
pub fn unwrap_password(h: &VaultHeader, password: &str) -> Result<Zeroizing<Key>, String> {
    check_params(h)?;
    let salt = B64.decode(&h.salt).map_err(|_| E_CORRUPT.to_string())?;
    let kek = derive(password, &salt, h.opslimit, h.memlimit)?;
    let dek = open(&kek, &h.nonce, &h.wrapped, &header_aad(&h.vault_id))?.ok_or_else(|| E_WRONG_PASSWORD.to_string())?;
    as_key(&dek)
}

pub fn unwrap_recovery(h: &VaultHeader, recovery: &str) -> Result<Zeroizing<Key>, String> {
    let (Some(nonce), Some(wrapped)) = (&h.recovery_nonce, &h.recovery_wrapped) else {
        return Err(E_WRONG_RECOVERY.into());
    };
    let key = parse_recovery(recovery).ok_or_else(|| E_WRONG_RECOVERY.to_string())?;
    let dek = open(&key, nonce, wrapped, &recovery_aad(&h.vault_id))?.ok_or_else(|| E_WRONG_RECOVERY.to_string())?;
    as_key(&dek)
}

fn seal_note(dek: &Key, vault_id: &str, note_id: &str, title: &str, content: &str) -> Result<(String, String), String> {
    let plain = Zeroizing::new(
        serde_json::to_vec(&NotePlain { title: title.into(), content: content.into() }).map_err(|_| E_CORRUPT.to_string())?,
    );
    seal(dek, &plain, &note_aad(vault_id, note_id))
}

fn open_note(dek: &Key, id: &str, n: &SealedNote) -> Result<NotePlain, String> {
    let plain = open(dek, &n.nonce, &n.ciphertext, &note_aad(&n.vault_id, id))?.ok_or_else(|| E_CORRUPT.to_string())?;
    serde_json::from_slice(&plain).map_err(|_| E_CORRUPT.to_string())
}

fn check_sizes(title: &str, content: &str) -> Result<(), String> {
    if title.chars().count() > MAX_TITLE_LEN || content.len() > MAX_CONTENT_LEN {
        return Err(E_TOO_LARGE.into());
    }
    Ok(())
}

// ---- Note operations over vault data (pure, given the DEK) -------------------

fn meta(id: &str, n: &SealedNote, title: String) -> SecretNoteMeta {
    SecretNoteMeta {
        id: id.into(),
        title,
        project_id: n.project_id.clone(),
        position: n.position,
        created_at: n.created_at.clone(),
        updated_at: n.updated_at.clone(),
    }
}

/// The current vault's notes, top first. Notes of other vaults are counted by
/// the status, not listed.
pub fn list(data: &VaultData, dek: &Key) -> Result<Vec<SecretNoteMeta>, String> {
    let Some(vault_id) = data.current_id() else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    // A note that does not open (damaged in sync, crafted import) is left out
    // rather than hiding every other one.
    for (id, n) in data.notes.iter().filter(|(_, n)| n.vault_id == vault_id) {
        if let Ok(plain) = open_note(dek, id, n) {
            out.push(meta(id, n, plain.title));
        }
    }
    out.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| b.updated_at.cmp(&a.updated_at)));
    Ok(out)
}

fn current_note<'a>(data: &'a VaultData, id: &str) -> Result<&'a SealedNote, String> {
    data.notes
        .get(id)
        .filter(|n| data.current_id() == Some(n.vault_id.as_str()))
        .ok_or_else(|| E_NOT_FOUND.to_string())
}

pub fn reveal(data: &VaultData, dek: &Key, id: &str) -> Result<String, String> {
    Ok(open_note(dek, id, current_note(data, id)?)?.content)
}

pub fn create(
    data: &mut VaultData,
    dek: &Key,
    title: &str,
    content: &str,
    project_id: Option<String>,
) -> Result<SecretNoteMeta, String> {
    check_sizes(title, content)?;
    let vault_id = data.current_id().ok_or_else(|| E_MISSING.to_string())?.to_string();
    let id = uuid::Uuid::new_v4().to_string();
    let (nonce, ciphertext) = seal_note(dek, &vault_id, &id, title, content)?;
    let position = data.notes.values().map(|n| n.position).min().map_or(0, |p| p - 1);
    let ts = now();
    let note = SealedNote { vault_id, nonce, ciphertext, project_id, position, created_at: ts.clone(), updated_at: ts };
    let out = meta(&id, &note, title.into());
    data.notes.insert(id, note);
    Ok(out)
}

pub fn update(data: &mut VaultData, dek: &Key, id: &str, title: &str, content: &str) -> Result<SecretNoteMeta, String> {
    check_sizes(title, content)?;
    let mut note = current_note(data, id)?.clone();
    let (nonce, ciphertext) = seal_note(dek, &note.vault_id, id, title, content)?;
    note.nonce = nonce;
    note.ciphertext = ciphertext;
    note.updated_at = now();
    let out = meta(id, &note, title.into());
    data.notes.insert(id.into(), note);
    Ok(out)
}

pub fn set_project(data: &mut VaultData, id: &str, project_id: Option<String>) -> Result<(), String> {
    let mut note = current_note(data, id)?.clone();
    note.project_id = project_id;
    note.updated_at = now();
    data.notes.insert(id.into(), note);
    Ok(())
}

pub fn delete(data: &mut VaultData, id: &str) -> Result<(), String> {
    current_note(data, id)?;
    data.notes.remove(id);
    Ok(())
}

pub fn reorder(data: &mut VaultData, ids: &[String]) -> Result<(), String> {
    for (i, id) in ids.iter().enumerate() {
        if let Some(n) = data.notes.get_mut(id) {
            n.position = i as i64;
        }
    }
    Ok(())
}

/// Re-seal under the current vault every note of the old headers `secret`
/// (their password or recovery key) opens. Slow (Argon2id per old header).
/// Returns how many notes moved; a note that does not open stays where it is.
pub fn merge_old(data: &mut VaultData, dek: &Key, secret: &str) -> Result<usize, String> {
    let vault_id = data.current_id().ok_or_else(|| E_MISSING.to_string())?.to_string();
    let mut merged = 0;
    let mut opened_any = false;
    for old in data.old_headers.clone() {
        let Ok(old_dek) = unwrap_recovery(&old, secret).or_else(|_| unwrap_password(&old, secret)) else { continue };
        opened_any = true;
        let ids: Vec<String> =
            data.notes.iter().filter(|(_, n)| n.vault_id == old.vault_id).map(|(id, _)| id.clone()).collect();
        for id in ids {
            let mut note = data.notes[&id].clone();
            let Ok(plain) = open_note(&old_dek, &id, &note) else { continue };
            let (nonce, ciphertext) = seal_note(dek, &vault_id, &id, &plain.title, &plain.content)?;
            note.vault_id = vault_id.clone();
            note.nonce = nonce;
            note.ciphertext = ciphertext;
            data.notes.insert(id, note);
            merged += 1;
        }
    }
    if !opened_any {
        return Err(E_WRONG_PASSWORD.into());
    }
    data.drop_unused_old_headers();
    Ok(merged)
}

/// A new password: a new vault (id, DEK, recovery key) with every current note
/// re-sealed under it. The old header is dropped once nothing uses it (notes
/// changed on a peer meanwhile keep it, and merge with the old password).
/// Returns the new DEK and recovery key. Slow (Argon2id).
pub fn rotate(data: &mut VaultData, dek: &Key, password: &str, params: KdfParams) -> Result<(Zeroizing<Key>, String), String> {
    let old = data.header.clone().ok_or_else(|| E_MISSING.to_string())?;
    let (header, new_dek, recovery) = new_header(password, params)?;
    let ids: Vec<String> =
        data.notes.iter().filter(|(_, n)| n.vault_id == old.vault_id).map(|(id, _)| id.clone()).collect();
    for id in ids {
        let mut note = data.notes[&id].clone();
        let Ok(plain) = open_note(dek, &id, &note) else { continue };
        let (nonce, ciphertext) = seal_note(&new_dek, &header.vault_id, &id, &plain.title, &plain.content)?;
        note.vault_id = header.vault_id.clone();
        note.nonce = nonce;
        note.ciphertext = ciphertext;
        data.notes.insert(id, note);
    }
    data.keep_old(old);
    data.header = Some(header);
    data.drop_unused_old_headers();
    Ok((new_dek, recovery))
}

/// A vault from a backup file: kept as-is when this device has none, merged
/// note by note (newer wins) when it is the same vault, else parked as an old
/// header (merge later with that vault's password). Returns the notes taken.
pub fn import(data: &mut VaultData, incoming: VaultData) -> usize {
    let Some(header) = incoming.header else { return 0 };
    match data.current_id() {
        None => data.header = Some(header),
        Some(id) if id == header.vault_id => {}
        Some(_) => data.keep_old(header),
    }
    for h in incoming.old_headers {
        if data.current_id() != Some(h.vault_id.as_str()) {
            data.keep_old(h);
        }
    }
    let mut taken = 0;
    for (id, note) in incoming.notes {
        let newer = data.notes.get(&id).is_none_or(|local| local.updated_at < note.updated_at);
        if newer {
            data.notes.insert(id, note);
            taken += 1;
        }
    }
    data.drop_unused_old_headers();
    taken
}

// ---- Session: the unlocked DEK --------------------------------------------------

struct Unlocked {
    vault_id: String,
    dek: Zeroizing<Key>,
    /// Wall clock: time asleep counts as idle.
    last_used: SystemTime,
    /// Opened with the recovery key: a new password needs no current one.
    via_recovery: bool,
}

static SESSION: Mutex<Option<Unlocked>> = Mutex::new(None);

/// The session, even after a panic elsewhere poisoned the lock (it must still lock).
fn session() -> std::sync::MutexGuard<'static, Option<Unlocked>> {
    SESSION.lock().unwrap_or_else(|e| e.into_inner())
}

/// Failed unlocks in a row, and when the next attempt is allowed.
static ATTEMPTS: Mutex<(u32, Option<Instant>)> = Mutex::new((0, None));

pub fn lock() {
    *session() = None;
    clipboard_clear_pending();
}

pub fn set_unlocked(vault_id: &str, dek: Zeroizing<Key>, via_recovery: bool) {
    *session() = Some(Unlocked { vault_id: vault_id.into(), dek, last_used: SystemTime::now(), via_recovery });
}

/// Whether the session was opened with the recovery key.
pub fn unlocked_via_recovery() -> bool {
    session().as_ref().is_some_and(|u| u.via_recovery)
}

/// Whether the session's DEK belongs to `vault_id` (a replaced header locks).
pub fn is_unlocked(vault_id: Option<&str>) -> bool {
    let mut s = session();
    match (s.as_ref(), vault_id) {
        (Some(u), Some(id)) if u.vault_id == id => true,
        (Some(_), _) => {
            *s = None;
            false
        }
        _ => false,
    }
}

/// The DEK for the current vault `vault_id`, refreshing the idle timer.
pub fn session_dek(vault_id: Option<&str>) -> Result<Zeroizing<Key>, String> {
    let mut s = session();
    if s.as_ref().is_some_and(idle) {
        *s = None;
    }
    match (s.as_mut(), vault_id) {
        (Some(u), Some(id)) if u.vault_id == id => {
            u.last_used = SystemTime::now();
            Ok(u.dek.clone())
        }
        _ => {
            *s = None;
            Err(E_LOCKED.into())
        }
    }
}

/// Idle for `AUTO_LOCK` (a clock set back counts as idle).
fn idle(u: &Unlocked) -> bool {
    u.last_used.elapsed().map_or(true, |d| d >= AUTO_LOCK)
}

/// Lock when idle for `AUTO_LOCK`. Returns whether it locked.
pub fn lock_if_idle() -> bool {
    let mut s = session();
    if s.as_ref().is_some_and(idle) {
        *s = None;
        drop(s);
        clipboard_clear_pending();
        return true;
    }
    false
}

/// Lock now if unlocked (screen locked, machine woke up). Returns whether it locked.
pub fn lock_if_unlocked() -> bool {
    let was = session().take().is_some();
    if was {
        clipboard_clear_pending();
    }
    was
}

/// Refuse an unlock attempt while backing off after failures.
pub fn check_throttle() -> Result<(), String> {
    let a = ATTEMPTS.lock().map_err(|_| E_THROTTLED.to_string())?;
    if let Some(until) = a.1 {
        let now = Instant::now();
        if now < until {
            return Err(format!("{E_THROTTLED}:{}", (until - now).as_secs() + 1));
        }
    }
    Ok(())
}

/// Record an unlock outcome: 3 free tries, then 2, 4, 8… seconds (max 5 min).
pub fn record_attempt(ok: bool) {
    let Ok(mut a) = ATTEMPTS.lock() else { return };
    if ok {
        *a = (0, None);
        return;
    }
    a.0 += 1;
    if a.0 >= 3 {
        let secs = 2u64.saturating_pow(a.0 - 2).min(300);
        a.1 = Some(Instant::now() + Duration::from_secs(secs));
    }
}

// ---- Clipboard ----------------------------------------------------------------

/// How long a copied secret stays on the pasteboard.
pub const CLIPBOARD_CLEAR: Duration = Duration::from_secs(30);

/// Change count of the secret this process last put on the pasteboard, until
/// it is cleared: locking the vault clears it at once.
static CLIPBOARD_PENDING: Mutex<Option<isize>> = Mutex::new(None);

pub fn clipboard_remember(change_count: isize) {
    *CLIPBOARD_PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(change_count);
}

/// Clear the pasteboard if it still holds the secret copied last.
pub fn clipboard_clear_pending() {
    if let Some(count) = CLIPBOARD_PENDING.lock().unwrap_or_else(|e| e.into_inner()).take() {
        clipboard_clear_if(count);
    }
}

/// Put `text` on the general pasteboard for this Mac only (no Universal
/// Clipboard), marked concealed so clipboard managers skip it. Returns the
/// pasteboard's change count, to clear it later only if nothing replaced it.
#[cfg(target_os = "macos")]
pub fn clipboard_put(text: &str) -> Result<isize, String> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardContentsOptions, NSPasteboardTypeString};
    use objc2_foundation::NSString;
    let pb = NSPasteboard::generalPasteboard();
    pb.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
    let ok = pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    // nspasteboard.org convention honoured by clipboard managers.
    pb.setString_forType(&NSString::from_str(""), &NSString::from_str("org.nspasteboard.ConcealedType"));
    if !ok {
        return Err("clipboard_failed".into());
    }
    Ok(pb.changeCount())
}

/// Clear the pasteboard if it still holds what `clipboard_put` wrote.
#[cfg(target_os = "macos")]
pub fn clipboard_clear_if(change_count: isize) {
    use objc2_app_kit::NSPasteboard;
    let pb = NSPasteboard::generalPasteboard();
    if pb.changeCount() == change_count {
        pb.clearContents();
    }
}

/// Whether the login session's screen is locked (`CGSSessionScreenIsLocked`
/// is only present while it is).
#[cfg(target_os = "macos")]
pub fn screen_locked() -> bool {
    use objc2_core_foundation::CFString;
    let Some(dict) = objc2_core_graphics::CGSessionCopyCurrentDictionary() else { return false };
    let key = CFString::from_static_str("CGSSessionScreenIsLocked");
    // SAFETY: a CFString key looked up in a live CFDictionary; only the pointer is checked.
    !unsafe { dict.value((&*key as *const CFString).cast()) }.is_null()
}

#[cfg(not(target_os = "macos"))]
pub fn screen_locked() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn clipboard_put(_text: &str) -> Result<isize, String> {
    Err("clipboard_unsupported".into())
}

#[cfg(not(target_os = "macos"))]
pub fn clipboard_clear_if(_change_count: isize) {}

#[cfg(test)]
mod tests {
    use super::*;

    const CHEAP: KdfParams = KdfParams { opslimit: 1, memlimit: 64 * 1024 };

    fn vault(password: &str) -> (VaultData, Zeroizing<Key>, String) {
        let (h, dek, recovery) = new_header(password, CHEAP).unwrap();
        (VaultData { header: Some(h), ..Default::default() }, dek, recovery)
    }

    #[test]
    fn notes_round_trip_and_nothing_is_in_clear() {
        let (mut data, dek, _) = vault("correct horse");
        let m = create(&mut data, &dek, "AWS root", "AKIA-very-secret", Some("p1".into())).unwrap();
        assert_eq!(list(&data, &dek).unwrap()[0].title, "AWS root");
        assert_eq!(reveal(&data, &dek, &m.id).unwrap(), "AKIA-very-secret");
        let stored = serde_json::to_string(&data).unwrap();
        assert!(!stored.contains("AWS root") && !stored.contains("very-secret"));
        update(&mut data, &dek, &m.id, "AWS", "rotated").unwrap();
        assert_eq!(reveal(&data, &dek, &m.id).unwrap(), "rotated");
        delete(&mut data, &m.id).unwrap();
        assert!(list(&data, &dek).unwrap().is_empty());
    }

    #[test]
    fn password_and_recovery_open_the_vault() {
        let (data, dek, recovery) = vault("correct horse");
        let h = data.header.unwrap();
        assert_eq!(unwrap_password(&h, "correct horse").unwrap().as_ref(), dek.as_ref());
        assert_eq!(unwrap_password(&h, "wrong horse").err().as_deref(), Some(E_WRONG_PASSWORD));
        assert_eq!(unwrap_recovery(&h, &recovery.to_lowercase()).unwrap().as_ref(), dek.as_ref());
        assert_eq!(unwrap_recovery(&h, "00").err().as_deref(), Some(E_WRONG_RECOVERY));
        assert_eq!(new_header("short", CHEAP).err().as_deref(), Some(E_WEAK_PASSWORD));
    }

    #[test]
    fn a_note_cannot_be_moved_to_another_id() {
        let (mut data, dek, _) = vault("correct horse");
        let m = create(&mut data, &dek, "t", "c", None).unwrap();
        let note = data.notes.remove(&m.id).unwrap();
        data.notes.insert("other".into(), note);
        assert_eq!(reveal(&data, &dek, "other").err().as_deref(), Some(E_CORRUPT));
    }

    #[test]
    fn hostile_kdf_params_are_refused() {
        let (data, _, _) = vault("correct horse");
        let mut h = data.header.unwrap();
        h.memlimit = usize::MAX;
        assert_eq!(unwrap_password(&h, "correct horse").err().as_deref(), Some(E_CORRUPT));
    }

    #[test]
    fn a_synced_vault_keeps_the_replaced_one_until_merged() {
        let (mut mine, my_dek, _) = vault("mine password");
        create(&mut mine, &my_dek, "mine", "m", None).unwrap();
        let (mut theirs, their_dek, _) = vault("their password");
        let t = create(&mut theirs, &their_dek, "theirs", "t", None).unwrap();
        // Their header and note arrive by sync.
        mine.apply_synced_header(theirs.header.clone().unwrap());
        mine.notes.insert(t.id.clone(), theirs.notes[&t.id].clone());
        assert_eq!(mine.legacy_count(), 1);
        assert_eq!(list(&mine, &their_dek).unwrap().len(), 1);
        assert_eq!(merge_old(&mut mine, &their_dek, "nope nope").err().as_deref(), Some(E_WRONG_PASSWORD));
        assert_eq!(merge_old(&mut mine, &their_dek, "mine password").unwrap(), 1);
        assert_eq!(mine.legacy_count(), 0);
        assert!(mine.old_headers.is_empty());
        let titles: Vec<String> = list(&mine, &their_dek).unwrap().into_iter().map(|m| m.title).collect();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&"mine".to_string()));
    }

    #[test]
    fn rotate_re_seals_under_a_new_vault() {
        let (mut data, dek, old_recovery) = vault("correct horse");
        let m = create(&mut data, &dek, "t", "secret", None).unwrap();
        let old = data.header.clone().unwrap();
        let (new_dek, recovery) = rotate(&mut data, &dek, "battery staple", CHEAP).unwrap();
        let h = data.header.clone().unwrap();
        assert_ne!(h.vault_id, old.vault_id);
        assert_ne!(new_dek.as_ref(), dek.as_ref());
        assert_ne!(recovery, old_recovery);
        assert!(data.old_headers.is_empty());
        assert_eq!(reveal(&data, &new_dek, &m.id).unwrap(), "secret");
        assert!(unwrap_recovery(&h, &old_recovery).is_err());
        assert_eq!(unwrap_password(&h, "battery staple").unwrap().as_ref(), new_dek.as_ref());
    }

    #[test]
    fn a_same_id_header_is_not_replaced_and_bad_notes_are_skipped() {
        let (mut data, dek, _) = vault("correct horse");
        create(&mut data, &dek, "good", "g", None).unwrap();
        let bad = create(&mut data, &dek, "bad", "b", None).unwrap();
        data.notes.get_mut(&bad.id).unwrap().ciphertext = B64.encode([0u8; 40]);
        let titles: Vec<String> = list(&data, &dek).unwrap().into_iter().map(|m| m.title).collect();
        assert_eq!(titles, vec!["good".to_string()]);
        let mut tampered = data.header.clone().unwrap();
        tampered.wrapped = B64.encode([1u8; 48]);
        data.apply_synced_header(tampered);
        assert!(unwrap_password(data.header.as_ref().unwrap(), "correct horse").is_ok());
    }

    #[test]
    fn old_notes_merge_with_the_old_recovery_key() {
        let (mut mine, my_dek, _) = vault("mine password");
        let (mut old, old_dek, old_recovery) = vault("old password");
        create(&mut old, &old_dek, "o", "o", None).unwrap();
        import(&mut mine, old);
        assert_eq!(merge_old(&mut mine, &my_dek, &old_recovery).unwrap(), 1);
    }

    #[test]
    fn import_of_another_vault_parks_it() {
        let (mut mine, my_dek, _) = vault("mine password");
        let (mut other, other_dek, _) = vault("other password");
        create(&mut other, &other_dek, "o", "o", None).unwrap();
        assert_eq!(import(&mut mine, other.clone()), 1);
        assert_eq!(mine.legacy_count(), 1);
        assert_eq!(merge_old(&mut mine, &my_dek, "other password").unwrap(), 1);
        // Same vault again: nothing newer to take.
        let mut empty = VaultData::default();
        assert_eq!(import(&mut empty, other.clone()), 1);
        assert_eq!(import(&mut empty, other), 0);
    }

    #[test]
    fn session_locks_on_a_replaced_header() {
        let mut dek = Zeroizing::new([0u8; 32]);
        dek[0] = 7;
        set_unlocked("v1", dek, false);
        assert!(session_dek(Some("v1")).is_ok());
        assert!(is_unlocked(Some("v1")));
        assert_eq!(session_dek(Some("v2")).err().as_deref(), Some(E_LOCKED));
        assert!(!is_unlocked(Some("v1")));
    }
}
