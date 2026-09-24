//! P20-T01 full internal recovery snapshots.
//!
//! This is intentionally separate from `export`: exports are reviewed knowledge packets,
//! backups are exact recovery artifacts.

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use ring::digest::{digest, SHA256};
use std::{fs, path::{Path, PathBuf}};

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


pub fn validate_restore(snapshot: &Path, manifest: &BackupManifest, expected_schema: i64) -> Result<()> {
    if manifest.schema_version != SCHEMA_VERSION {
        bail!("unsupported backup manifest schema {}", manifest.schema_version);
    }
    if manifest.database_schema_version != expected_schema {
        bail!("database schema mismatch: backup={} expected={}", manifest.database_schema_version, expected_schema);
    }
    verify_snapshot(snapshot, manifest)
}

pub fn restore_snapshot(snapshot: &Path, manifest: &BackupManifest, destination: &Path, expected_schema: i64) -> Result<()> {
    validate_restore(snapshot, manifest, expected_schema)?;
    if destination.exists() {
        bail!("restore destination already exists");
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(snapshot, destination)?;
    let conn = Connection::open(destination)?;
    let restored = manifest_for(&conn, manifest.created_at.clone(), checksum(destination)?)?;
    if restored != *manifest {
        bail!("restored database integrity mismatch");
    }
    Ok(())
}

pub fn create_snapshot(database: &Path, destination: &Path, created_at: String) -> Result<BackupManifest> {
    if !database.exists() {
        bail!("database does not exist: {}", database.display());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(database, destination)
        .with_context(|| format!("copying database to {}", destination.display()))?;

    let conn = Connection::open(destination)?;
    let manifest = manifest_for(&conn, created_at, checksum(destination)?)?;
    let manifest_path = manifest_path(destination);
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    Ok(manifest)
}

pub fn verify_snapshot(snapshot: &Path, manifest: &BackupManifest) -> Result<()> {
    let actual = checksum(snapshot)?;
    if actual != manifest.checksum_sha256 {
        bail!("backup checksum mismatch");
    }
    let conn = Connection::open(snapshot)?;
    let checked = manifest_for(&conn, manifest.created_at.clone(), actual)?;
    if checked.memories != manifest.memories
        || checked.memory_revisions != manifest.memory_revisions
        || checked.memory_embeddings != manifest.memory_embeddings
        || checked.sessions != manifest.sessions
    {
        bail!("backup count verification failed");
    }
    Ok(())
}

fn manifest_for(conn: &Connection, created_at: String, checksum_sha256: String) -> Result<BackupManifest> {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backup_roundtrip_preserves_manifest() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("harness-backup-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir)?;
        let db = dir.join("db.sqlite");
        let conn = Connection::open(&db)?;
        conn.execute_batch("CREATE TABLE memories(id INTEGER); CREATE TABLE memory_revisions(id INTEGER); CREATE TABLE memory_embeddings(id INTEGER); CREATE TABLE sessions(id INTEGER);")?;
        let backup = dir.join("backup.sqlite");
        let manifest = create_snapshot(&db, &backup, "test".into())?;
        verify_snapshot(&backup, &manifest)?;
        let restored = dir.join("restored.sqlite");
        restore_snapshot(&backup, &manifest, &restored, manifest.database_schema_version)?;
        let err = restore_snapshot(&backup, &manifest, &restored, manifest.database_schema_version).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
