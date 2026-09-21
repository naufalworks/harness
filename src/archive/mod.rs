//! Opt-in encrypted storage for exact imported source bytes.
//!
//! The ordinary database remains sanitized. This subsystem is invoked explicitly and
//! stores only authenticated ciphertext plus non-secret metadata in SQLite.
//!
//! Delivered by P13-T02; reachable from authenticated HTTP routes since P13-T02b. The
//! on-disk format is mirrored by `scripts/backup.py`, and migration 007 ships the
//! `exact_archives` / `privacy_events` tables.
//!
//! The subsystem stays opt-in: `open_from_env` returns `None` when no archive
//! setting is present; partial configuration fails closed. A deployment that never
//! configures a key keeps an entirely sanitized database and its archive routes refuse
//! explicitly instead of half-working.
//!
//! The `&Connection` methods are the primitives. The `*_via` wrappers are the only path the
//! HTTP layer uses, so every archive write runs inside a `DbStore` closure on the serialized
//! writer instead of opening a second connection to the same database file.
use crate::storage::{guard_fence, DbStore, Lease};
use anyhow::{anyhow, bail, Context, Result};
use ring::{
    aead, digest,
    rand::{SecureRandom, SystemRandom},
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

const MAGIC: &[u8] = b"HARNESS-EXACT\0";
const VERSION: u8 = 1;
const ALGORITHM: &str = "AES-256-GCM";
const MAX_HEADER: usize = 64 * 1024;

#[derive(Clone)]
struct ArchiveKey {
    id: String,
    bytes: [u8; 32],
}

#[derive(Clone)]
pub struct KeyRing {
    current: ArchiveKey,
    previous: Option<ArchiveKey>,
}

impl KeyRing {
    pub fn from_files(current: &Path, previous: Option<&Path>) -> Result<Self> {
        let current = read_key(current)?;
        let previous = previous.map(read_key).transpose()?;
        if previous.as_ref().is_some_and(|key| key.id == current.id) {
            bail!("previous archive key must differ from current key");
        }
        Ok(Self { current, previous })
    }

    /// P13-T02b: the `?` this once used on `previous` returned `None` for every lookup when no
    /// rotation key was configured, so a single-key deployment could write archives it could
    /// never read back. `flatten` makes the previous key optional instead of required.
    fn by_id(&self, id: &str) -> Option<&ArchiveKey> {
        [Some(&self.current), self.previous.as_ref()]
            .into_iter()
            .flatten()
            .find(|key| key.id == id)
    }
}

#[cfg(unix)]
fn require_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
        bail!("archive key file must be owner-only (chmod 600)");
    }
    Ok(())
}
#[cfg(not(unix))]
fn require_owner_only(_path: &Path) -> Result<()> {
    Ok(())
}

fn read_key(path: &Path) -> Result<ArchiveKey> {
    require_owner_only(path)?;
    let raw = fs::read(path).with_context(|| format!("read archive key {}", path.display()))?;
    let bytes = if raw.len() == 32 {
        raw
    } else {
        let text = std::str::from_utf8(&raw)?.trim();
        if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("archive key must be 32 raw bytes or 64 hexadecimal characters");
        }
        (0..32)
            .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16))
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow!("archive key must be 256 bits"))?;
    let id = hex(digest::digest(&digest::SHA256, &bytes).as_ref())[..16].to_owned();
    Ok(ArchiveKey { id, bytes })
}

#[derive(Serialize, Deserialize)]
struct Header {
    version: u8,
    algorithm: String,
    key_id: String,
    nonce: String,
    source_id: String,
    plaintext_sha256: String,
    created_at: String,
}

pub struct ArchiveStore {
    root: PathBuf,
    keys: KeyRing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyAction {
    Forget,
    DeleteSource,
    PurgeIndex,
}

impl PrivacyAction {
    fn name(self) -> &'static str {
        match self {
            Self::Forget => "forget",
            Self::DeleteSource => "delete_source",
            Self::PurgeIndex => "purge_index",
        }
    }
    /// Parse the wire name. Returning `Option` keeps an unknown action a 400 at the edge
    /// rather than a silently mis-recorded privacy event.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "forget" => Some(Self::Forget),
            "delete_source" => Some(Self::DeleteSource),
            "purge_index" => Some(Self::PurgeIndex),
            _ => None,
        }
    }
    fn column(self) -> &'static str {
        match self {
            Self::Forget => "forgotten_at",
            Self::DeleteSource => "source_deleted_at",
            Self::PurgeIndex => "index_purged_at",
        }
    }
}

impl ArchiveStore {
    pub fn open(root: &Path, current_key: &Path, previous_key: Option<&Path>) -> Result<Self> {
        fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root: root.canonicalize()?,
            keys: KeyRing::from_files(current_key, previous_key)?,
        })
    }

    /// Opt-in construction from the environment. `None` means no archive was configured, which
    /// is deliberately distinct from a bad configuration: a key that is present but unreadable,
    /// wrongly sized or world-readable fails at startup rather than at the first request.
    pub fn open_from_env() -> Result<Option<Self>> {
        fn setting(name: &str) -> Result<Option<String>> {
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(_) => bail!("invalid archive configuration"),
            }
        }
        let root = setting("HARNESS_ARCHIVE_ROOT")?;
        let current = setting("HARNESS_ARCHIVE_KEY")?;
        let previous = setting("HARNESS_ARCHIVE_KEY_PREVIOUS")?;
        Self::open_configured(root.as_deref(), current.as_deref(), previous.as_deref())
    }

    /// Partial configuration must never silently downgrade exact capture to disabled.
    fn open_configured(
        root: Option<&str>,
        current: Option<&str>,
        previous: Option<&str>,
    ) -> Result<Option<Self>> {
        match (root, current, previous) {
            (None, None, None) => Ok(None),
            (Some(root), Some(current), previous)
                if !root.is_empty()
                    && !current.is_empty()
                    && !previous.is_some_and(str::is_empty) =>
            {
                Self::open(Path::new(root), Path::new(current), previous.map(Path::new)).map(Some)
            }
            _ => bail!("incomplete archive configuration"),
        }
    }

    /// Archive bytes through the shared writer. The ciphertext file is published before the
    /// metadata row and removed again if that insert fails, so a readable archive file without
    /// a row is never left behind.
    pub async fn archive_exact_via(
        self: &Arc<Self>,
        db: &DbStore,
        source_id: String,
        exact: Vec<u8>,
    ) -> Result<String> {
        let store = Arc::clone(self);
        db.run(move |c| store.archive_exact(c, &source_id, &exact))
            .await
    }

    pub async fn read_exact_via(
        self: &Arc<Self>,
        db: &DbStore,
        archive_id: String,
    ) -> Result<Vec<u8>> {
        let store = Arc::clone(self);
        db.read(move |c| store.read_exact(c, &archive_id)).await
    }

    /// The state row and its `privacy_events` entry share one transaction, so the audit trail
    /// can never disagree with the state it is supposed to explain.
    pub async fn record_action_via(
        self: &Arc<Self>,
        db: &DbStore,
        source_id: String,
        action: PrivacyAction,
    ) -> Result<()> {
        let store = Arc::clone(self);
        db.run(move |c| store.record_action(c, &source_id, action))
            .await
    }

    pub async fn delete_archive_via(
        self: &Arc<Self>,
        db: &DbStore,
        archive_id: String,
    ) -> Result<()> {
        let store = Arc::clone(self);
        db.run(move |c| store.delete_archive(c, &archive_id)).await
    }

    /// P18-T04: turn-owned archive deletion is a filesystem side effect, so callers that execute
    /// it inside a durable turn can require the same held lease before recording intent and outcome.
    #[allow(dead_code)]
    pub async fn delete_archive_via_lease(
        self: &Arc<Self>,
        db: &DbStore,
        archive_id: String,
        lease: Lease,
    ) -> Result<()> {
        let store = Arc::clone(self);
        db.run(move |c| store.delete_archive_with_lease(c, &archive_id, &lease))
            .await
    }

    pub fn archive_exact(
        &self,
        conn: &Connection,
        source_id: &str,
        exact: &[u8],
    ) -> Result<String> {
        if source_id.is_empty() {
            bail!("source id is required");
        }
        let archive_id = Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        let plaintext_sha256 = hex(digest::digest(&digest::SHA256, exact).as_ref());
        let mut nonce = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| anyhow!("secure archive randomness unavailable"))?;
        let header = Header {
            version: VERSION,
            algorithm: ALGORITHM.into(),
            key_id: self.keys.current.id.clone(),
            nonce: hex(&nonce),
            source_id: source_id.into(),
            plaintext_sha256: plaintext_sha256.clone(),
            created_at: created_at.clone(),
        };
        let encoded = serde_json::to_vec(&header)?;
        if encoded.len() > MAX_HEADER {
            bail!("archive header too large");
        }
        let mut prefix = Vec::with_capacity(MAGIC.len() + 4 + encoded.len());
        prefix.extend_from_slice(MAGIC);
        // The MAX_HEADER check above already bounds this, but the on-disk format is a 4-byte
        // length: a checked conversion keeps a future change to that check from silently
        // writing a truncated header length.
        let header_len =
            u32::try_from(encoded.len()).map_err(|_| anyhow!("archive header too large"))?;
        prefix.extend_from_slice(&header_len.to_be_bytes());
        prefix.extend_from_slice(&encoded);
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &self.keys.current.bytes)
                .map_err(|_| anyhow!("invalid archive key"))?,
        );
        let mut ciphertext = exact.to_vec();
        key.seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(&prefix),
            &mut ciphertext,
        )
        .map_err(|_| anyhow!("archive encryption failed"))?;
        let relative = format!("{archive_id}.har");
        atomic_write(&self.root.join(&relative), &[prefix, ciphertext].concat())?;
        if let Err(error) = conn.execute("INSERT INTO exact_archives(id,source_id,relative_path,format_version,algorithm,key_id,plaintext_sha256,byte_length,created_at) VALUES(?,?,?,?,?,?,?,?,?)", params![archive_id, source_id, relative, VERSION, ALGORITHM, self.keys.current.id, plaintext_sha256, exact.len() as i64, created_at]) {
            fs::remove_file(self.root.join(&relative)).ok();
            return Err(error.into());
        }
        Ok(archive_id)
    }

    pub fn read_exact(&self, conn: &Connection, archive_id: &str) -> Result<Vec<u8>> {
        let row: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT relative_path,deleted_at FROM exact_archives WHERE id=?",
                [archive_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (relative, deleted_at) = row.ok_or_else(|| anyhow!("exact archive not found"))?;
        if deleted_at.is_some() {
            bail!("exact archive was deleted");
        }
        let payload = fs::read(self.root.join(relative))?;
        let (header, prefix_len) = parse_header(&payload)?;
        let key_material = self
            .keys
            .by_id(&header.key_id)
            .ok_or_else(|| anyhow!("archive key is unavailable"))?;
        let nonce = decode_fixed::<12>(&header.nonce)?;
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, &key_material.bytes)
                .map_err(|_| anyhow!("invalid archive key"))?,
        );
        let mut ciphertext = payload[prefix_len..].to_vec();
        let plaintext = key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(&payload[..prefix_len]),
                &mut ciphertext,
            )
            .map_err(|_| anyhow!("archive authentication failed"))?;
        if hex(digest::digest(&digest::SHA256, plaintext).as_ref()) != header.plaintext_sha256 {
            bail!("archive checksum failed");
        }
        Ok(plaintext.to_vec())
    }

    pub fn record_action(
        &self,
        conn: &Connection,
        source_id: &str,
        action: PrivacyAction,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let sql = format!("INSERT INTO source_privacy_state(source_id,{0},updated_at) VALUES(?1,?2,?2) ON CONFLICT(source_id) DO UPDATE SET {0}=excluded.{0},updated_at=excluded.updated_at", action.column());
        let tx = conn.unchecked_transaction()?;
        tx.execute(&sql, params![source_id, now])?;
        tx.execute(
            "INSERT INTO privacy_events(id,source_id,action,created_at) VALUES(?,?,?,?)",
            params![Uuid::new_v4().to_string(), source_id, action.name(), now],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_archive(&self, conn: &Connection, archive_id: &str) -> Result<()> {
        self.delete_archive_inner(conn, archive_id, None)
    }

    /// P18-T04: lease-guarded primitive for any future recorded-turn archive deletion path.
    #[allow(dead_code)]
    pub fn delete_archive_with_lease(
        &self,
        conn: &Connection,
        archive_id: &str,
        lease: &Lease,
    ) -> Result<()> {
        self.delete_archive_inner(conn, archive_id, Some(lease))
    }

    fn delete_archive_inner(
        &self,
        conn: &Connection,
        archive_id: &str,
        lease: Option<&Lease>,
    ) -> Result<()> {
        let row: (String, String, Option<String>) = conn.query_row(
            "SELECT source_id,relative_path,deleted_at FROM exact_archives WHERE id=?",
            [archive_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if row.2.is_some() {
            return Ok(());
        }
        let now = chrono::Utc::now().to_rfc3339();
        {
            let tx = conn.unchecked_transaction()?;
            if let Some(lease) = lease {
                guard_fence(&tx, lease)?;
            }
            tx.execute("INSERT INTO privacy_events(id,source_id,archive_id,action,created_at) VALUES(?,?,?,?,?)", params![Uuid::new_v4().to_string(), row.0, archive_id, "delete_archive_intent", now])?;
            tx.commit()?;
        }
        let path = self.root.join(&row.1);
        let removed = match fs::remove_file(&path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error).with_context(|| format!("delete archive {}", path.display()))
            }
        };
        let finished_at = chrono::Utc::now().to_rfc3339();
        let tx = conn.unchecked_transaction()?;
        if let Some(lease) = lease {
            guard_fence(&tx, lease)?;
        }
        tx.execute(
            "UPDATE exact_archives SET deleted_at=? WHERE id=? AND deleted_at IS NULL",
            params![finished_at, archive_id],
        )?;
        let outcome = if removed {
            "delete_archive_succeeded"
        } else {
            "delete_archive_missing"
        };
        tx.execute("INSERT INTO privacy_events(id,source_id,archive_id,action,created_at) VALUES(?,?,?,?,?)", params![Uuid::new_v4().to_string(), row.0, archive_id, outcome, finished_at])?;
        tx.execute("INSERT INTO privacy_events(id,source_id,archive_id,action,created_at) VALUES(?,?,?,?,?)", params![Uuid::new_v4().to_string(), row.0, archive_id, "delete_archive", now])?;
        tx.commit()?;
        Ok(())
    }
}

fn parse_header(payload: &[u8]) -> Result<(Header, usize)> {
    if !payload.starts_with(MAGIC) || payload.len() < MAGIC.len() + 4 {
        bail!("not a Harness exact archive");
    }
    let start = MAGIC.len();
    let size = u32::from_be_bytes(payload[start..start + 4].try_into()?) as usize;
    if size == 0 || size > MAX_HEADER || payload.len() <= start + 4 + size {
        bail!("invalid exact archive header");
    }
    let end = start + 4 + size;
    let header: Header = serde_json::from_slice(&payload[start + 4..end])?;
    if header.version != VERSION || header.algorithm != ALGORITHM {
        bail!("unsupported exact archive format");
    }
    Ok((header, end))
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("partial-{}", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    if let Err(error) = (|| -> Result<()> {
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })() {
        fs::remove_file(&temporary).ok();
        return Err(error);
    }
    Ok(())
}

fn decode_fixed<const N: usize>(text: &str) -> Result<[u8; N]> {
    if text.len() != N * 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid archive nonce");
    }
    let bytes = (0..N)
        .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    bytes
        .try_into()
        .map_err(|_| anyhow!("invalid archive nonce"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn external_privacy_archive_configuration_is_explicit_and_fail_closed() {
        assert!(ArchiveStore::open_configured(None, None, None)
            .unwrap()
            .is_none());
        for (root, key, previous) in [
            (Some("/unused"), None, None),
            (None, Some("/unused"), None),
            (None, None, Some("/unused")),
            (Some(""), Some("/unused"), None),
            (Some("/unused"), Some(""), None),
            (Some("/unused"), Some("/unused"), Some("")),
        ] {
            let error = ArchiveStore::open_configured(root, key, previous)
                .err()
                .unwrap();
            assert_eq!(error.to_string(), "incomplete archive configuration");
        }
    }

    fn fixture() -> Result<(PathBuf, Connection, PathBuf, PathBuf)> {
        let dir = std::env::temp_dir().join(format!("harness-archive-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir)?;
        let current = dir.join("current.key");
        let previous = dir.join("previous.key");
        fs::write(
            &current,
            b"1111111111111111111111111111111111111111111111111111111111111111\n",
        )?;
        fs::write(
            &previous,
            b"2222222222222222222222222222222222222222222222222222222222222222\n",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&current, fs::Permissions::from_mode(0o600))?;
            fs::set_permissions(&previous, fs::Permissions::from_mode(0o600))?;
        }
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(include_str!("../../migrations/007_privacy_archive.sql"))?;
        conn.execute_batch(include_str!(
            "../../migrations/020_archive_delete_outcomes.sql"
        ))?;
        Ok((dir, conn, current, previous))
    }

    #[test]
    fn archive_round_trip_tamper_and_previous_key_rotation() -> Result<()> {
        let (dir, conn, current, previous) = fixture()?;
        let root = dir.join("archive");
        let old = ArchiveStore::open(&root, &previous, None)?;
        let id = old.archive_exact(&conn, "source-1", b"exact secret bytes")?;
        let rotated = ArchiveStore::open(&root, &current, Some(&previous))?;
        assert_eq!(rotated.read_exact(&conn, &id)?, b"exact secret bytes");
        let path: String = conn.query_row(
            "SELECT relative_path FROM exact_archives WHERE id=?",
            [&id],
            |row| row.get(0),
        )?;
        let mut payload = fs::read(root.join(&path))?;
        *payload.last_mut().unwrap() ^= 1;
        fs::write(root.join(&path), payload)?;
        assert!(rotated
            .read_exact(&conn, &id)
            .unwrap_err()
            .to_string()
            .contains("authentication"));
        Ok(())
    }

    #[test]
    fn privacy_actions_are_distinct_append_only_and_archive_delete_is_idempotent() -> Result<()> {
        let (dir, conn, current, _) = fixture()?;
        let store = ArchiveStore::open(&dir.join("archive"), &current, None)?;
        let id = store.archive_exact(&conn, "source-2", b"original")?;
        store.record_action(&conn, "source-2", PrivacyAction::Forget)?;
        store.record_action(&conn, "source-2", PrivacyAction::DeleteSource)?;
        store.record_action(&conn, "source-2", PrivacyAction::PurgeIndex)?;
        store.delete_archive(&conn, &id)?;
        store.delete_archive(&conn, &id)?;
        let actions: String = conn.query_row(
            "SELECT group_concat(action,',') FROM privacy_events ORDER BY seq",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            actions,
            "forget,delete_source,purge_index,delete_archive_intent,delete_archive_succeeded,delete_archive"
        );
        let state: (Option<String>, Option<String>, Option<String>) = conn.query_row("SELECT forgotten_at,source_deleted_at,index_purged_at FROM source_privacy_state WHERE source_id='source-2'", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        assert!(state.0.is_some() && state.1.is_some() && state.2.is_some());
        assert!(store
            .read_exact(&conn, &id)
            .unwrap_err()
            .to_string()
            .contains("deleted"));
        assert!(conn.execute("DELETE FROM privacy_events", []).is_err());
        Ok(())
    }

    #[test]
    fn archive_delete_records_missing_ciphertext_truthfully() -> Result<()> {
        let (dir, conn, current, _) = fixture()?;
        let store = ArchiveStore::open(&dir.join("archive"), &current, None)?;
        let id = store.archive_exact(&conn, "source-missing", b"original")?;
        let path: String = conn.query_row(
            "SELECT relative_path FROM exact_archives WHERE id=?",
            [&id],
            |row| row.get(0),
        )?;
        fs::remove_file(dir.join("archive").join(path))?;
        store.delete_archive(&conn, &id)?;
        let actions: String = conn.query_row(
            "SELECT group_concat(action,',') FROM privacy_events ORDER BY seq",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            actions,
            "delete_archive_intent,delete_archive_missing,delete_archive"
        );
        Ok(())
    }

    #[test]
    fn refuses_world_readable_or_duplicate_rotation_keys() -> Result<()> {
        let (dir, _conn, current, _) = fixture()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&current, fs::Permissions::from_mode(0o644))?;
            assert!(ArchiveStore::open(&dir.join("a"), &current, None).is_err());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&current, fs::Permissions::from_mode(0o600))?;
        }
        assert!(ArchiveStore::open(&dir.join("b"), &current, Some(&current)).is_err());
        Ok(())
    }
}
