//! P17-T02 isolated treatment snapshots.
//!
//! A treatment is an immutable child of a validated capsule. The source database is read only and
//! the source checkout is inspected only; project files are checked out into a detached temporary
//! worktree and exact approved memory revisions are copied into a separate immutable SQLite file.

use super::capsule::{MemoryRef, ProjectState, ReplayMode, RunCapsule};
use crate::export::packet::canonical_json;
use crate::export::review;
use anyhow::{bail, Context, Result};
use ring::digest::{digest, SHA256};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const FORMAT: &str = "harness-treatment-v1";
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TreatmentKind {
    Baseline,
    RemoveOne { stable_id: String },
    NoMemory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenMemory {
    pub stable_id: String,
    pub revision: i64,
    pub scope: String,
    pub key: String,
    pub value: String,
    pub branch: String,
    pub category: String,
    pub content_sha256: String,
}

impl FrozenMemory {
    fn canonical_without_hash(&self) -> Result<String> {
        let mut value = serde_json::to_value(self)?;
        value
            .as_object_mut()
            .context("frozen memory must serialize as an object")?
            .remove("content_sha256");
        Ok(canonical_json(&value))
    }

    pub fn seal(mut self) -> Result<Self> {
        self.content_sha256 = review::checksum(&self.canonical_without_hash()?);
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> Result<()> {
        bounded("memory stable id", &self.stable_id, 1, 128)?;
        if self.revision < 1 {
            bail!("frozen memory revision must be positive");
        }
        bounded("memory scope", &self.scope, 1, 256)?;
        bounded("memory key", &self.key, 1, 256)?;
        bounded("memory value", &self.value, 1, 64 * 1024)?;
        bounded("memory branch", &self.branch, 1, 60)?;
        if !matches!(
            self.category.as_str(),
            "preference"
                | "fact"
                | "project"
                | "rule"
                | "skill"
                | "decision"
                | "episodic"
                | "procedural"
        ) {
            bail!("frozen memory has unsupported category");
        }
        hex_digest("frozen memory", &self.content_sha256)?;
        if self.content_sha256 != review::checksum(&self.canonical_without_hash()?) {
            bail!("frozen memory content hash mismatch");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreatmentManifest {
    pub format: String,
    pub treatment_id: String,
    pub parent_capsule_id: String,
    pub kind: TreatmentKind,
    pub project: ProjectState,
    pub source_memory_package_sha256: String,
    pub source_memory_ids: Vec<String>,
    pub memory_package_sha256: String,
    pub memories: Vec<FrozenMemory>,
    pub removed_memory_ids: Vec<String>,
}

impl TreatmentManifest {
    pub fn derive(
        capsule: &RunCapsule,
        kind: TreatmentKind,
        source_memories: &[FrozenMemory],
    ) -> Result<Self> {
        capsule.validate()?;
        if capsule.replay_mode != ReplayMode::Strict {
            bail!("isolated M1 treatments require a strict parent capsule");
        }
        let declared: Vec<&str> = capsule
            .body
            .memory
            .memories
            .iter()
            .map(|item| item.stable_id.as_str())
            .collect();
        let frozen: Vec<&str> = source_memories
            .iter()
            .map(|item| item.stable_id.as_str())
            .collect();
        if declared != frozen {
            bail!("frozen memories do not exactly match capsule order");
        }
        for (reference, memory) in capsule.body.memory.memories.iter().zip(source_memories) {
            memory.validate()?;
            if reference.revision != memory.revision
                || reference.content_sha256 != memory.content_sha256
            {
                bail!("frozen memory does not match the capsule revision and hash");
            }
        }

        let source_memory_ids: Vec<String> = source_memories
            .iter()
            .map(|item| item.stable_id.clone())
            .collect();
        let (memories, removed_memory_ids) = match &kind {
            TreatmentKind::Baseline => (source_memories.to_vec(), vec![]),
            TreatmentKind::RemoveOne { stable_id } => {
                bounded("removed memory id", stable_id, 1, 128)?;
                if !source_memory_ids.contains(stable_id) {
                    bail!("remove-one target is not present in the parent capsule");
                }
                (
                    source_memories
                        .iter()
                        .filter(|item| item.stable_id != *stable_id)
                        .cloned()
                        .collect(),
                    vec![stable_id.clone()],
                )
            }
            TreatmentKind::NoMemory => (vec![], source_memory_ids.clone()),
        };
        let mut manifest = Self {
            format: FORMAT.into(),
            treatment_id: String::new(),
            parent_capsule_id: capsule.capsule_id.clone(),
            kind,
            project: capsule.body.project.clone(),
            source_memory_package_sha256: capsule.body.memory.package_sha256.clone(),
            source_memory_ids,
            memory_package_sha256: memory_package_sha256(&memories)?,
            memories,
            removed_memory_ids,
        };
        manifest.treatment_id = review::checksum(&manifest.canonical_without_id()?);
        manifest.validate()?;
        Ok(manifest)
    }

    fn canonical_without_id(&self) -> Result<String> {
        let mut value = serde_json::to_value(self)?;
        value
            .as_object_mut()
            .context("treatment must serialize as an object")?
            .remove("treatment_id");
        Ok(canonical_json(&value))
    }

    pub fn canonical_json(&self) -> Result<String> {
        self.validate()?;
        let raw = canonical_json(&serde_json::to_value(self)?);
        if raw.len() > MAX_MANIFEST_BYTES {
            bail!("treatment manifest exceeds the 1 MiB bound");
        }
        Ok(raw)
    }

    pub fn validate(&self) -> Result<()> {
        if self.format != FORMAT {
            bail!("unsupported treatment format");
        }
        hex_digest("treatment id", &self.treatment_id)?;
        hex_digest("parent capsule id", &self.parent_capsule_id)?;
        hex_digest("source memory package", &self.source_memory_package_sha256)?;
        hex_digest("treatment memory package", &self.memory_package_sha256)?;
        if self.treatment_id != review::checksum(&self.canonical_without_id()?) {
            bail!("treatment content address mismatch");
        }
        if !self.project.clean {
            bail!("treatment project must remain a clean frozen snapshot");
        }
        let mut source = BTreeSet::new();
        for id in &self.source_memory_ids {
            bounded("source memory id", id, 1, 128)?;
            if !source.insert(id.as_str()) {
                bail!("duplicate source memory id");
            }
        }
        let mut kept = BTreeSet::new();
        for memory in &self.memories {
            memory.validate()?;
            if !source.contains(memory.stable_id.as_str())
                || !kept.insert(memory.stable_id.as_str())
            {
                bail!("treatment memory is duplicate or absent from its parent");
            }
        }
        if self.memory_package_sha256 != memory_package_sha256(&self.memories)? {
            bail!("treatment memory package hash mismatch");
        }
        let expected_removed: Vec<&str> = self
            .source_memory_ids
            .iter()
            .filter(|id| !kept.contains(id.as_str()))
            .map(String::as_str)
            .collect();
        let declared_removed: Vec<&str> =
            self.removed_memory_ids.iter().map(String::as_str).collect();
        if declared_removed != expected_removed {
            bail!("removed memory list does not match the treatment contents");
        }
        match &self.kind {
            TreatmentKind::Baseline if !self.removed_memory_ids.is_empty() => {
                bail!("baseline cannot remove memories")
            }
            TreatmentKind::RemoveOne { stable_id }
                if self.removed_memory_ids.len() != 1
                    || self.removed_memory_ids[0] != *stable_id =>
            {
                bail!("remove-one treatment must remove exactly its named memory")
            }
            TreatmentKind::NoMemory if !self.memories.is_empty() => {
                bail!("no-memory treatment must have an empty store")
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenTreatment {
    pub manifest: TreatmentManifest,
    pub root: PathBuf,
    pub project: PathBuf,
    pub memory_store: PathBuf,
}

pub fn load_declared_memories(
    source: &Connection,
    references: &[MemoryRef],
) -> Result<Vec<FrozenMemory>> {
    let mut memories = Vec::with_capacity(references.len());
    for reference in references {
        let row: Option<(String, String, String, String, String, String, i64)> = source
            .query_row(
                "SELECT scope,key,value,branch,category,status,revision FROM memories WHERE id=?1",
                [&reference.stable_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;
        let (scope, key, value, branch, category, status, revision) =
            row.with_context(|| format!("declared memory {} is missing", reference.stable_id))?;
        if status != "active" || revision != reference.revision {
            bail!("declared memory is no longer the active captured revision");
        }
        let frozen = FrozenMemory {
            stable_id: reference.stable_id.clone(),
            revision,
            scope,
            key,
            value,
            branch,
            category,
            content_sha256: String::new(),
        }
        .seal()?;
        if frozen.content_sha256 != reference.content_sha256 {
            bail!("declared memory bytes no longer match the capsule");
        }
        memories.push(frozen);
    }
    Ok(memories)
}

pub fn project_tree_sha256(repo: &Path, revision: &str) -> Result<String> {
    let output = git(repo, &["ls-tree", "-r", "-z", "--full-tree", revision])?;
    Ok(hex_bytes(output.stdout.as_slice()))
}

pub fn freeze_treatment(
    source_repo: &Path,
    source_db: &Connection,
    output_root: &Path,
    capsule: &RunCapsule,
    kind: TreatmentKind,
) -> Result<FrozenTreatment> {
    capsule.validate()?;
    if capsule.replay_mode != ReplayMode::Strict {
        bail!("only strict capsules can be frozen into M1 treatments");
    }
    if output_root == source_repo || output_root.starts_with(source_repo) {
        bail!("treatment output must be outside the source worktree");
    }
    let source_before = source_fingerprint(source_repo)?;
    if !source_before.clean {
        bail!("source project must be clean before freezing");
    }
    if source_before.head != capsule.body.project.reference {
        bail!("source HEAD does not match the capsule project reference");
    }
    if source_before.tree_sha256 != capsule.body.project.content_sha256 {
        bail!("source tree does not match the capsule project hash");
    }
    let memories = load_declared_memories(source_db, &capsule.body.memory.memories)?;
    let manifest = TreatmentManifest::derive(capsule, kind, &memories)?;

    fs::create_dir_all(output_root)?;
    let root = output_root.join(&manifest.treatment_id);
    if root.exists() {
        bail!("immutable treatment destination already exists");
    }
    fs::create_dir(&root)?;
    let project = root.join("project");
    let memory_store = root.join("memory.sqlite3");
    let result = (|| -> Result<()> {
        let project_text = project.to_str().context("non-UTF8 treatment path")?;
        let output = git(
            source_repo,
            &[
                "worktree",
                "add",
                "--detach",
                project_text,
                &capsule.body.project.reference,
            ],
        )?;
        if !output.status.success() {
            bail!("git worktree add failed");
        }
        let frozen_project = source_fingerprint(&project)?;
        if !frozen_project.clean
            || frozen_project.head != source_before.head
            || frozen_project.tree_sha256 != source_before.tree_sha256
        {
            bail!("temporary project did not reproduce the frozen source tree");
        }
        write_isolated_store(&memory_store, &manifest)?;
        fs::write(root.join("manifest.json"), manifest.canonical_json()?)?;
        make_read_only(&memory_store)?;
        make_read_only(&root.join("manifest.json"))?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = Command::new("git")
            .arg("-C")
            .arg(source_repo)
            .args(["worktree", "remove", "--force"])
            .arg(&project)
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("GIT_CONFIG_SYSTEM")
            .output();
        let _ = fs::remove_dir_all(&root);
        return Err(error);
    }
    if source_fingerprint(source_repo)? != source_before {
        bail!("source project changed while treatment was frozen");
    }
    Ok(FrozenTreatment {
        manifest,
        root,
        project,
        memory_store,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceFingerprint {
    head: String,
    tree_sha256: String,
    clean: bool,
}

fn source_fingerprint(repo: &Path) -> Result<SourceFingerprint> {
    let head = text(git(repo, &["rev-parse", "HEAD"])?);
    let status = git(
        repo,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    Ok(SourceFingerprint {
        tree_sha256: project_tree_sha256(repo, &head)?,
        head,
        clean: status.stdout.is_empty(),
    })
}

fn write_isolated_store(path: &Path, manifest: &TreatmentManifest) -> Result<()> {
    let mut connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA foreign_keys=ON;
         CREATE TABLE treatment(id TEXT PRIMARY KEY, parent_capsule_id TEXT NOT NULL, kind TEXT NOT NULL, manifest_json TEXT NOT NULL CHECK(json_valid(manifest_json)), CHECK(length(id)=64));
         CREATE TABLE memories(stable_id TEXT PRIMARY KEY, revision INTEGER NOT NULL CHECK(revision>0), scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, branch TEXT NOT NULL, category TEXT NOT NULL, content_sha256 TEXT NOT NULL);
         CREATE TABLE sealed(one INTEGER PRIMARY KEY CHECK(one=1));
         CREATE TRIGGER treatment_no_insert BEFORE INSERT ON treatment WHEN EXISTS(SELECT 1 FROM sealed) BEGIN SELECT RAISE(ABORT,'treatment is sealed'); END;
         CREATE TRIGGER treatment_no_update BEFORE UPDATE ON treatment BEGIN SELECT RAISE(ABORT,'treatment is immutable'); END;
         CREATE TRIGGER treatment_no_delete BEFORE DELETE ON treatment BEGIN SELECT RAISE(ABORT,'treatment is immutable'); END;
         CREATE TRIGGER treatment_memories_no_insert BEFORE INSERT ON memories WHEN EXISTS(SELECT 1 FROM sealed) BEGIN SELECT RAISE(ABORT,'treatment memories are sealed'); END;
         CREATE TRIGGER treatment_memories_no_update BEFORE UPDATE ON memories BEGIN SELECT RAISE(ABORT,'treatment memories are immutable'); END;
         CREATE TRIGGER treatment_memories_no_delete BEFORE DELETE ON memories BEGIN SELECT RAISE(ABORT,'treatment memories are immutable'); END;
         CREATE TRIGGER treatment_seal_no_update BEFORE UPDATE ON sealed BEGIN SELECT RAISE(ABORT,'treatment seal is immutable'); END;
         CREATE TRIGGER treatment_seal_no_delete BEFORE DELETE ON sealed BEGIN SELECT RAISE(ABORT,'treatment seal is immutable'); END;",
    )?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO treatment(id,parent_capsule_id,kind,manifest_json) VALUES(?1,?2,?3,?4)",
        params![
            &manifest.treatment_id,
            &manifest.parent_capsule_id,
            treatment_kind_name(&manifest.kind),
            manifest.canonical_json()?
        ],
    )?;
    for memory in &manifest.memories {
        transaction.execute(
            "INSERT INTO memories(stable_id,revision,scope,key,value,branch,category,content_sha256) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![memory.stable_id, memory.revision, memory.scope, memory.key, memory.value, memory.branch, memory.category, memory.content_sha256],
        )?;
    }
    transaction.execute("INSERT INTO sealed(one) VALUES(1)", [])?;
    transaction.commit()?;
    Ok(())
}

fn treatment_kind_name(kind: &TreatmentKind) -> &'static str {
    match kind {
        TreatmentKind::Baseline => "baseline",
        TreatmentKind::RemoveOne { .. } => "remove_one",
        TreatmentKind::NoMemory => "no_memory",
    }
}

fn memory_package_sha256(memories: &[FrozenMemory]) -> Result<String> {
    Ok(review::checksum(&canonical_json(&serde_json::to_value(
        memories,
    )?)))
}

fn git(repo: &Path, args: &[&str]) -> Result<Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        .output()
        .context("failed to execute git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

fn text(output: Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn hex_bytes(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn bounded(name: &str, value: &str, min: usize, max: usize) -> Result<()> {
    if value.len() < min || value.len() > max || value.trim() != value {
        bail!("{name} must be trimmed and {min}..={max} bytes");
    }
    Ok(())
}

fn hex_digest(name: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{name} must be a lowercase SHA-256 digest");
    }
    Ok(())
}

fn make_read_only(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experiments::capsule::{
        Assertion, Boundary, BoundaryState, CapsuleBody, ContextState, EvidenceRef, MemoryState,
        ModelState, ToolState, FORMAT as CAPSULE_FORMAT, VALIDATOR,
    };
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("harness-{name}-{nonce}"))
    }

    fn memory(id: &str, value: &str) -> FrozenMemory {
        FrozenMemory {
            stable_id: id.into(),
            revision: 1,
            scope: "project".into(),
            key: format!("key-{id}"),
            value: value.into(),
            branch: "main".into(),
            category: "rule".into(),
            content_sha256: String::new(),
        }
        .seal()
        .unwrap()
    }

    fn capsule(project: ProjectState, memories: &[FrozenMemory]) -> RunCapsule {
        let refs: Vec<MemoryRef> = memories
            .iter()
            .map(|item| MemoryRef {
                stable_id: item.stable_id.clone(),
                revision: item.revision,
                content_sha256: item.content_sha256.clone(),
            })
            .collect();
        RunCapsule {
            format: CAPSULE_FORMAT.into(),
            validator: VALIDATOR.into(),
            capsule_id: String::new(),
            source_request_id: "request-1".into(),
            replay_mode: ReplayMode::Strict,
            body: CapsuleBody {
                task: EvidenceRef {
                    content_sha256: review::checksum("task"),
                    sanitizer: review::SANITIZER.into(),
                },
                session_history: EvidenceRef {
                    content_sha256: review::checksum("history"),
                    sanitizer: review::SANITIZER.into(),
                },
                project,
                model: ModelState {
                    provider: "fixture".into(),
                    model: "fixture".into(),
                    parameters: json!({"temperature":0}),
                    request_schema_sha256: review::checksum("schema"),
                },
                tools: ToolState {
                    schemas_sha256: review::checksum("tools"),
                    permission_mode: "ask".into(),
                    results_complete: true,
                    results: vec![],
                },
                memory: MemoryState {
                    package_sha256: review::checksum(&canonical_json(
                        &serde_json::to_value(&refs).unwrap(),
                    )),
                    memories: refs,
                },
                context: ContextState {
                    receipt_id: "context-1".into(),
                    content_sha256: review::checksum("context"),
                },
                assertions: vec![Assertion::NoUnauthorizedActions],
                boundaries: ["clock", "randomness", "provider", "tools"]
                    .into_iter()
                    .map(|name| Boundary {
                        name: name.into(),
                        state: BoundaryState::Frozen,
                        content_sha256: Some(review::checksum(name)),
                        reason: None,
                    })
                    .collect(),
                unavailable_evidence: vec![],
                strict_prefix_sha256: None,
                first_live_boundary: None,
            },
        }
        .seal()
        .unwrap()
    }

    #[test]
    fn treatment_children_change_only_the_memory_condition() {
        let memories = vec![memory("m1", "one"), memory("m2", "two")];
        let parent = capsule(
            ProjectState {
                kind: "git_commit".into(),
                reference: "a".repeat(40),
                content_sha256: review::checksum("tree"),
                clean: true,
            },
            &memories,
        );
        let baseline =
            TreatmentManifest::derive(&parent, TreatmentKind::Baseline, &memories).unwrap();
        let removed = TreatmentManifest::derive(
            &parent,
            TreatmentKind::RemoveOne {
                stable_id: "m1".into(),
            },
            &memories,
        )
        .unwrap();
        let empty = TreatmentManifest::derive(&parent, TreatmentKind::NoMemory, &memories).unwrap();
        assert_eq!(baseline.project, removed.project);
        assert_eq!(removed.project, empty.project);
        assert_eq!(baseline.memories.len(), 2);
        assert_eq!(
            removed
                .memories
                .iter()
                .map(|item| item.stable_id.as_str())
                .collect::<Vec<_>>(),
            ["m2"]
        );
        assert!(empty.memories.is_empty());
        assert_eq!(removed.removed_memory_ids, ["m1"]);
        assert_eq!(empty.removed_memory_ids, ["m1", "m2"]);
        assert!(baseline.validate().is_ok());
        let store_path = temp("sealed-treatment-store");
        write_isolated_store(&store_path, &baseline).unwrap();
        let store = Connection::open(&store_path).unwrap();
        assert!(store.execute("INSERT INTO memories(stable_id,revision,scope,key,value,branch,category,content_sha256) VALUES('m3',1,'project','key','value','main','rule',?1)", [review::checksum("forged")]).is_err());
        assert!(store
            .execute(
                "UPDATE memories SET value='changed' WHERE stable_id='m1'",
                []
            )
            .is_err());
        assert!(store.execute("DELETE FROM treatment", []).is_err());
        drop(store);
        fs::remove_file(store_path).unwrap();
        let mut tampered = removed;
        tampered.memories[0].value.push_str(" changed");
        assert!(tampered.validate().is_err());
    }

    #[test]
    fn treatment_freeze_leaves_source_db_and_worktree_unchanged() {
        let repo = temp("treatment-source");
        let output = temp("treatment-output");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]).unwrap();
        git(&repo, &["config", "user.email", "fixture@example.invalid"]).unwrap();
        git(&repo, &["config", "user.name", "Fixture"]).unwrap();
        fs::write(repo.join("main.rs"), "fn main() {}\n").unwrap();
        git(&repo, &["add", "main.rs"]).unwrap();
        git(&repo, &["commit", "-q", "-m", "fixture"]).unwrap();
        let before = source_fingerprint(&repo).unwrap();

        let source = Connection::open_in_memory().unwrap();
        source.execute_batch("CREATE TABLE memories(id TEXT PRIMARY KEY,scope TEXT,key TEXT,value TEXT,branch TEXT,category TEXT,status TEXT,revision INTEGER);").unwrap();
        let frozen = memory("m1", "prefer explicit errors");
        source
            .execute(
                "INSERT INTO memories VALUES(?1,?2,?3,?4,?5,?6,'active',?7)",
                params![
                    frozen.stable_id,
                    frozen.scope,
                    frozen.key,
                    frozen.value,
                    frozen.branch,
                    frozen.category,
                    frozen.revision
                ],
            )
            .unwrap();
        let parent = capsule(
            ProjectState {
                kind: "git_commit".into(),
                reference: before.head.clone(),
                content_sha256: before.tree_sha256.clone(),
                clean: true,
            },
            std::slice::from_ref(&frozen),
        );
        let source_changes = source.total_changes();
        let child =
            freeze_treatment(&repo, &source, &output, &parent, TreatmentKind::NoMemory).unwrap();
        assert_eq!(source.total_changes(), source_changes);
        assert_eq!(source_fingerprint(&repo).unwrap(), before);
        assert_eq!(
            fs::read_to_string(child.project.join("main.rs")).unwrap(),
            "fn main() {}\n"
        );
        let isolated = Connection::open_with_flags(
            &child.memory_store,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            isolated
                .query_row("SELECT count(*) FROM memories", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            isolated
                .query_row("SELECT parent_capsule_id FROM treatment", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            parent.capsule_id
        );

        git(
            &repo,
            &[
                "worktree",
                "remove",
                "--force",
                child.project.to_str().unwrap(),
            ],
        )
        .unwrap();
        fs::remove_dir_all(&output).unwrap();
        fs::remove_dir_all(&repo).unwrap();
    }
}
