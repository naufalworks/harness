//! P20-T01 full internal recovery snapshots.
//!
//! This is intentionally separate from `export`: exports are reviewed knowledge packets,
//! backups are exact recovery artifacts.

use anyhow::{bail, Context, Result};
use ring::digest::{digest, SHA256};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupManifest {
    pub schema_version: u32,
    pub created_at: String,
    pub database_schema_version: i64,
    pub memories: i64,
    pub memory_revisions: i64,
    pub memory_embeddings: i64,
    pub sessions: i64,
    pub checksum_sha256: String,
}

pub fn validate_restore(
    snapshot: &Path,
    manifest: &BackupManifest,
    expected_schema: i64,
) -> Result<()> {
    if manifest.schema_version != SCHEMA_VERSION {
        bail!(
            "unsupported backup manifest schema {}",
            manifest.schema_version
        );
    }
    if manifest.database_schema_version != expected_schema {
        bail!(
            "database schema mismatch: backup={} expected={}",
            manifest.database_schema_version,
            expected_schema
        );
    }
    verify_snapshot(snapshot, manifest)
}

pub fn restore_snapshot(
    snapshot: &Path,
    manifest: &BackupManifest,
    destination: &Path,
    expected_schema: i64,
) -> Result<()> {
    validate_restore(snapshot, manifest, expected_schema)?;
    if destination.exists() {
        bail!("restore destination already exists");
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    // Publish only a verified, complete copy; hard_link refuses an existing target.
    let temporary = destination.with_extension(format!("restore-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        fs::copy(snapshot, &temporary)?;
        verify_snapshot(&temporary, manifest)?;
        fs::hard_link(&temporary, destination).context("publishing restored database")?;
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result?;
    Ok(())
}

pub fn create_snapshot(
    database: &Path,
    destination: &Path,
    created_at: String,
) -> Result<BackupManifest> {
    if !database.exists() {
        bail!("database does not exist: {}", database.display());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if destination.exists() || manifest_path(destination).exists() {
        bail!("backup destination or manifest already exists");
    }
    // Filesystem copying a live SQLite file omits WAL commits. VACUUM INTO
    // instead produces a transactionally consistent standalone database.
    let result = (|| -> Result<BackupManifest> {
        let source =
            Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source
            .execute("VACUUM INTO ?1", [destination.to_string_lossy().as_ref()])
            .with_context(|| format!("snapshotting database to {}", destination.display()))?;
        drop(source);
        let conn =
            Connection::open_with_flags(destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let manifest = manifest_for(&conn, created_at, checksum(destination)?)?;
        verify_snapshot(destination, &manifest)?;
        fs::write(
            manifest_path(destination),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        Ok(manifest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

pub fn verify_snapshot(snapshot: &Path, manifest: &BackupManifest) -> Result<()> {
    if manifest.schema_version != SCHEMA_VERSION {
        bail!(
            "unsupported backup manifest schema {}",
            manifest.schema_version
        );
    }
    let actual = checksum(snapshot)?;
    if actual != manifest.checksum_sha256 {
        bail!("backup checksum mismatch");
    }
    let conn = Connection::open_with_flags(snapshot, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        bail!("backup database integrity check failed: {integrity}");
    }
    let checked = manifest_for(&conn, manifest.created_at.clone(), actual)?;
    if checked != *manifest {
        bail!("backup manifest verification failed");
    }
    Ok(())
}

fn manifest_for(
    conn: &Connection,
    created_at: String,
    checksum_sha256: String,
) -> Result<BackupManifest> {
    Ok(BackupManifest {
        schema_version: SCHEMA_VERSION,
        created_at,
        database_schema_version: conn.query_row("PRAGMA user_version", [], |r| r.get(0))?,
        memories: count(conn, "memories")?,
        memory_revisions: count(conn, "memory_revisions")?,
        memory_embeddings: count(conn, "memory_embeddings")?,
        sessions: count(conn, "sessions")?,
        checksum_sha256,
    })
}

fn count(conn: &Connection, table: &str) -> Result<i64> {
    Ok(conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
}

fn checksum(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    let hash = digest(&SHA256, &bytes);
    Ok(hash.as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

fn manifest_path(snapshot: &Path) -> PathBuf {
    let mut path = snapshot.to_path_buf();
    path.set_extension("manifest.json");
    path
}

fn load_manifest(snapshot: &Path) -> Result<BackupManifest> {
    let path = manifest_path(snapshot);
    let bytes =
        fs::read(&path).with_context(|| format!("reading backup manifest {}", path.display()))?;
    serde_json::from_slice(&bytes).context("parsing backup manifest")
}

/// Production/operator hook for P20 recovery snapshots. This deliberately lives on the
/// executable instead of the HTTP surface: backup paths are privileged local filesystem
/// choices and should never become remotely supplied API arguments.
///
/// Usage:
///   harness backup create  <database> <snapshot>
///   harness backup verify  <snapshot>
///   harness backup restore <snapshot> <new-database>
pub fn maybe_run_cli() -> Option<Result<()>> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("backup") {
        return None;
    }
    let result = (|| -> Result<()> {
        let command = args.next().context(
            "usage: harness backup <create|verify|restore> <database/snapshot> [snapshot/destination]",
        )?;
        match command.as_str() {
            "create" => {
                let database =
                    PathBuf::from(args.next().context("backup create requires database")?);
                let snapshot =
                    PathBuf::from(args.next().context("backup create requires snapshot")?);
                if args.next().is_some() {
                    bail!("backup create received unexpected arguments");
                }
                let manifest =
                    create_snapshot(&database, &snapshot, chrono::Utc::now().to_rfc3339())?;
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            }
            "verify" => {
                let snapshot =
                    PathBuf::from(args.next().context("backup verify requires snapshot")?);
                if args.next().is_some() {
                    bail!("backup verify received unexpected arguments");
                }
                let manifest = load_manifest(&snapshot)?;
                verify_snapshot(&snapshot, &manifest)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "verified":true,
                        "snapshot":snapshot,
                        "manifest":manifest
                    }))?
                );
            }
            "restore" => {
                let snapshot =
                    PathBuf::from(args.next().context("backup restore requires snapshot")?);
                let destination =
                    PathBuf::from(args.next().context("backup restore requires destination")?);
                if args.next().is_some() {
                    bail!("backup restore received unexpected arguments");
                }
                let manifest = load_manifest(&snapshot)?;
                restore_snapshot(
                    &snapshot,
                    &manifest,
                    &destination,
                    crate::storage::CURRENT_DATABASE_SCHEMA_VERSION,
                )?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "restored":true,
                        "snapshot":snapshot,
                        "destination":destination,
                        "database_schema_version":manifest.database_schema_version
                    }))?
                );
            }
            _ => bail!("unknown backup command {command:?}"),
        }
        Ok(())
    })();
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backup_roundtrip_preserves_manifest() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("harness-backup-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir)?;
        let db = dir.join("db.sqlite");
        let conn = Connection::open(&db)?;
        conn.execute_batch("CREATE TABLE memories(id INTEGER); CREATE TABLE memory_revisions(id INTEGER); CREATE TABLE memory_embeddings(id INTEGER); CREATE TABLE sessions(id INTEGER);")?;
        let backup = dir.join("backup.sqlite");
        let manifest = create_snapshot(&db, &backup, "test".into())?;
        verify_snapshot(&backup, &manifest)?;
        let restored = dir.join("restored.sqlite");
        restore_snapshot(
            &backup,
            &manifest,
            &restored,
            manifest.database_schema_version,
        )?;
        let err = restore_snapshot(
            &backup,
            &manifest,
            &restored,
            manifest.database_schema_version,
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn rejects_corrupt_backup_and_incompatible_migration() -> Result<()> {
        let dir =
            std::env::temp_dir().join(format!("harness-backup-negative-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir)?;
        let db = dir.join("source.sqlite");
        let conn = Connection::open(&db)?;
        conn.execute_batch("PRAGMA user_version=7; CREATE TABLE memories(id INTEGER); CREATE TABLE memory_revisions(id INTEGER); CREATE TABLE memory_embeddings(id INTEGER); CREATE TABLE sessions(id INTEGER); INSERT INTO memories VALUES (1);")?;
        let snapshot = dir.join("snapshot.sqlite");
        let manifest = create_snapshot(&db, &snapshot, "test".into())?;
        let target = dir.join("restored.sqlite");
        assert!(validate_restore(&snapshot, &manifest, 8)
            .unwrap_err()
            .to_string()
            .contains("schema mismatch"));
        assert!(!target.exists());
        let mut unsupported = manifest.clone();
        unsupported.schema_version += 1;
        assert!(validate_restore(&snapshot, &unsupported, 7).is_err());
        let mut mismatch = manifest.clone();
        mismatch.memories += 1;
        assert!(verify_snapshot(&snapshot, &mismatch).is_err());
        let mut bytes = fs::read(&snapshot)?;
        bytes[100] ^= 1;
        fs::write(&snapshot, bytes)?;
        assert!(restore_snapshot(&snapshot, &manifest, &target, 7)
            .unwrap_err()
            .to_string()
            .contains("checksum"));
        assert!(!target.exists());
        fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn snapshot_includes_committed_wal_pages() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("harness-backup-wal-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir)?;
        let db = dir.join("source.sqlite");
        let conn = Connection::open(&db)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE memories(id INTEGER); CREATE TABLE memory_revisions(id INTEGER); CREATE TABLE memory_embeddings(id INTEGER); CREATE TABLE sessions(id INTEGER); INSERT INTO memories VALUES (1);")?;
        let snapshot = dir.join("snapshot.sqlite");
        let manifest = create_snapshot(&db, &snapshot, "test".into())?;
        assert_eq!(manifest.memories, 1);
        verify_snapshot(&snapshot, &manifest)?;
        drop(conn);
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
