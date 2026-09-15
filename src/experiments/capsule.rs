//! Immutable, content-addressed run-capsule contract (P17-T01).
//!
//! A capsule says exactly which project, model, tools, memories and context formed a run. It also
//! records every nondeterministic boundary as frozen, deterministic, live, or explicitly
//! unavailable. Validation is fail-closed: an incomplete boundary is not a weaker capsule.

use crate::export::packet::canonical_json;
use crate::export::review;
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const FORMAT: &str = "harness-run-capsule-v1";
pub const VALIDATOR: &str = "harness-capsule-validator-v1";
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const REQUIRED_BOUNDARIES: [&str; 4] = ["clock", "randomness", "provider", "tools"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    Strict,
    Live,
    Hybrid,
}

impl ReplayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Live => "live",
            Self::Hybrid => "hybrid",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryState {
    Frozen,
    Deterministic,
    Live,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub content_sha256: String,
    pub sanitizer: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectState {
    /// `git_commit` or `snapshot`; never an unfrozen live path.
    pub kind: String,
    pub reference: String,
    pub content_sha256: String,
    pub clean: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelState {
    pub provider: String,
    pub model: String,
    /// Exact provider parameters. Object-only so null/list shorthand cannot hide a boundary.
    pub parameters: Value,
    pub request_schema_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultRef {
    pub sequence: u32,
    pub tool_name: String,
    pub request_sha256: String,
    pub result_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolState {
    pub schemas_sha256: String,
    pub permission_mode: String,
    pub results_complete: bool,
    pub results: Vec<ToolResultRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRef {
    pub stable_id: String,
    pub revision: i64,
    pub content_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryState {
    /// Hash of the ordered references, including the explicitly empty package.
    pub package_sha256: String,
    pub memories: Vec<MemoryRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextState {
    pub receipt_id: String,
    pub content_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Assertion {
    Command {
        command: String,
        expected_exit: i32,
    },
    FileSha256 {
        path: String,
        expected_sha256: String,
    },
    NoUnauthorizedActions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boundary {
    pub name: String,
    pub state: BoundaryState,
    /// Frozen/deterministic bytes have a digest; live/unavailable boundaries do not.
    pub content_sha256: Option<String>,
    /// Live/unavailable boundaries explain why exact bytes are not frozen.
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnavailableEvidence {
    pub boundary: String,
    pub reason: String,
    /// Always `unavailable`; downstream behavior must never be silently scored as zero.
    pub downstream_behavior: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleBody {
    pub task: EvidenceRef,
    pub session_history: EvidenceRef,
    pub project: ProjectState,
    pub model: ModelState,
    pub tools: ToolState,
    pub memory: MemoryState,
    pub context: ContextState,
    pub assertions: Vec<Assertion>,
    pub boundaries: Vec<Boundary>,
    pub unavailable_evidence: Vec<UnavailableEvidence>,
    pub strict_prefix_sha256: Option<String>,
    pub first_live_boundary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCapsule {
    pub format: String,
    pub validator: String,
    /// SHA-256 of canonical JSON after removing this field.
    pub capsule_id: String,
    pub source_request_id: String,
    pub replay_mode: ReplayMode,
    pub body: CapsuleBody,
}

impl RunCapsule {
    fn canonical_without_id(&self) -> Result<String> {
        let mut value = serde_json::to_value(self)?;
        value
            .as_object_mut()
            .context("capsule must serialize as an object")?
            .remove("capsule_id");
        Ok(canonical_json(&value))
    }

    pub fn seal(mut self) -> Result<Self> {
        self.capsule_id.clear();
        self.capsule_id = review::checksum(&self.canonical_without_id()?);
        self.validate()?;
        Ok(self)
    }

    pub fn from_json(raw: &str) -> Result<Self> {
        if raw.len() > MAX_MANIFEST_BYTES {
            bail!("capsule manifest exceeds the 1 MiB bound");
        }
        let capsule: Self = serde_json::from_str(raw).context("invalid capsule JSON")?;
        capsule.validate()?;
        Ok(capsule)
    }

    pub fn canonical_json(&self) -> Result<String> {
        self.validate()?;
        self.canonical_json_unchecked()
    }

    pub fn validate(&self) -> Result<()> {
        if self.format != FORMAT || self.validator != VALIDATOR {
            bail!("unsupported capsule format or validator");
        }
        bounded("source_request_id", &self.source_request_id, 1, 128)?;
        hex_digest("capsule_id", &self.capsule_id)?;
        if self.capsule_id != review::checksum(&self.canonical_without_id()?) {
            bail!("capsule content address does not match its manifest");
        }
        if self.canonical_json_unchecked()?.len() > MAX_MANIFEST_BYTES {
            bail!("capsule manifest exceeds the 1 MiB bound");
        }

        validate_evidence("task", &self.body.task)?;
        validate_evidence("session_history", &self.body.session_history)?;
        if !matches!(self.body.project.kind.as_str(), "git_commit" | "snapshot") {
            bail!("project kind must be git_commit or snapshot");
        }
        bounded("project reference", &self.body.project.reference, 1, 512)?;
        hex_digest("project content", &self.body.project.content_sha256)?;
        if !self.body.project.clean {
            bail!("project state must be a clean commit or sealed snapshot");
        }

        bounded("provider", &self.body.model.provider, 1, 128)?;
        bounded("model", &self.body.model.model, 1, 256)?;
        if !self.body.model.parameters.is_object() {
            bail!("model parameters must be an object");
        }
        hex_digest(
            "model request schema",
            &self.body.model.request_schema_sha256,
        )?;

        hex_digest("tool schemas", &self.body.tools.schemas_sha256)?;
        if !matches!(
            self.body.tools.permission_mode.as_str(),
            "ask" | "auto_edit" | "auto_all"
        ) {
            bail!("unsupported tool permission mode");
        }
        for (index, result) in self.body.tools.results.iter().enumerate() {
            if result.sequence as usize != index {
                bail!("tool results must have contiguous recorded order");
            }
            bounded("tool name", &result.tool_name, 1, 128)?;
            hex_digest("tool request", &result.request_sha256)?;
            hex_digest("tool result", &result.result_sha256)?;
        }

        hex_digest("memory package", &self.body.memory.package_sha256)?;
        let mut memory_ids = BTreeSet::new();
        for memory in &self.body.memory.memories {
            bounded("memory id", &memory.stable_id, 1, 128)?;
            if memory.revision < 1 || !memory_ids.insert(memory.stable_id.as_str()) {
                bail!("memory references require unique ids and positive revisions");
            }
            hex_digest("memory content", &memory.content_sha256)?;
        }
        let references = serde_json::to_value(&self.body.memory.memories)?;
        if self.body.memory.package_sha256 != review::checksum(&canonical_json(&references)) {
            bail!("memory package checksum does not match its ordered references");
        }

        bounded("context receipt", &self.body.context.receipt_id, 1, 128)?;
        hex_digest("context", &self.body.context.content_sha256)?;
        if self.body.assertions.is_empty() || self.body.assertions.len() > 64 {
            bail!("capsule requires 1 to 64 deterministic assertions");
        }
        for assertion in &self.body.assertions {
            match assertion {
                Assertion::Command {
                    command,
                    expected_exit,
                } => {
                    bounded("assertion command", command, 1, 512)?;
                    if !(0..=255).contains(expected_exit) {
                        bail!("assertion exit code must be in 0..=255");
                    }
                }
                Assertion::FileSha256 {
                    path,
                    expected_sha256,
                } => {
                    bounded("assertion path", path, 1, 512)?;
                    if path.starts_with('/') || path.split('/').any(|part| part == "..") {
                        bail!("assertion path must be relative and non-traversing");
                    }
                    hex_digest("assertion file", expected_sha256)?;
                }
                Assertion::NoUnauthorizedActions => {}
            }
        }

        let boundaries = boundary_map(&self.body.boundaries)?;
        let names: BTreeSet<&str> = boundaries.keys().copied().collect();
        let required: BTreeSet<&str> = REQUIRED_BOUNDARIES.into_iter().collect();
        if names != required {
            bail!("capsule must declare exactly clock, randomness, provider and tools boundaries");
        }
        for boundary in boundaries.values() {
            validate_boundary(boundary)?;
        }
        validate_unavailable(&boundaries, &self.body.unavailable_evidence)?;

        match self.replay_mode {
            ReplayMode::Strict => {
                if !self.body.tools.results_complete
                    || boundaries.values().any(|boundary| {
                        !matches!(
                            boundary.state,
                            BoundaryState::Frozen | BoundaryState::Deterministic
                        )
                    })
                {
                    bail!("strict replay requires complete frozen or deterministic boundaries");
                }
                if self.body.strict_prefix_sha256.is_some()
                    || self.body.first_live_boundary.is_some()
                {
                    bail!("strict replay cannot declare a hybrid live boundary");
                }
            }
            ReplayMode::Live => {
                if boundaries["provider"].state != BoundaryState::Live {
                    bail!("live replay requires an explicitly live provider boundary");
                }
                if self.body.strict_prefix_sha256.is_some()
                    || self.body.first_live_boundary.is_some()
                {
                    bail!("live replay cannot declare a hybrid strict prefix");
                }
            }
            ReplayMode::Hybrid => {
                let prefix = self
                    .body
                    .strict_prefix_sha256
                    .as_deref()
                    .context("hybrid replay requires strict_prefix_sha256")?;
                hex_digest("hybrid strict prefix", prefix)?;
                let first = self
                    .body
                    .first_live_boundary
                    .as_deref()
                    .context("hybrid replay requires first_live_boundary")?;
                if boundaries.get(first).map(|item| item.state) != Some(BoundaryState::Live) {
                    bail!("hybrid first_live_boundary must name a declared live boundary");
                }
            }
        }
        Ok(())
    }

    fn canonical_json_unchecked(&self) -> Result<String> {
        Ok(canonical_json(&serde_json::to_value(self)?))
    }

    /// Insert once. Repeating identical bytes is idempotent; a collision is rejected.
    pub fn persist(&self, connection: &Connection, created_at: &str) -> Result<bool> {
        self.validate()?;
        bounded("created_at", created_at, 1, 64)?;
        let manifest = self.canonical_json_unchecked()?;
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO run_capsules(id,format,validator,source_request_id,replay_mode,manifest_json,manifest_sha256,created_at) VALUES(?1,?2,?3,?4,?5,?6,?1,?7)",
            params![&self.capsule_id, &self.format, &self.validator, &self.source_request_id, self.replay_mode.as_str(), &manifest, created_at],
        )?;
        if inserted == 0 {
            let stored: Option<String> = connection
                .query_row(
                    "SELECT manifest_json FROM run_capsules WHERE id=?1",
                    [&self.capsule_id],
                    |row| row.get(0),
                )
                .optional()?;
            if stored.as_deref() != Some(manifest.as_str()) {
                bail!("capsule address already exists with different bytes");
            }
        }
        Ok(inserted == 1)
    }

    pub fn load(connection: &Connection, capsule_id: &str) -> Result<Option<Self>> {
        hex_digest("capsule id", capsule_id)?;
        let raw: Option<String> = connection
            .query_row(
                "SELECT manifest_json FROM run_capsules WHERE id=?1",
                [capsule_id],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|manifest| Self::from_json(&manifest)).transpose()
    }
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

fn validate_evidence(name: &str, evidence: &EvidenceRef) -> Result<()> {
    hex_digest(name, &evidence.content_sha256)?;
    if evidence.sanitizer != review::SANITIZER {
        bail!("{name} was not accepted by the current sanitizer");
    }
    Ok(())
}

fn boundary_map(boundaries: &[Boundary]) -> Result<BTreeMap<&str, &Boundary>> {
    if boundaries.len() != REQUIRED_BOUNDARIES.len() {
        bail!("capsule has an incomplete nondeterministic boundary set");
    }
    let mut map = BTreeMap::new();
    for boundary in boundaries {
        if map.insert(boundary.name.as_str(), boundary).is_some() {
            bail!("capsule has duplicate nondeterministic boundaries");
        }
    }
    Ok(map)
}

fn validate_boundary(boundary: &Boundary) -> Result<()> {
    match boundary.state {
        BoundaryState::Frozen | BoundaryState::Deterministic => {
            let digest = boundary
                .content_sha256
                .as_deref()
                .context("frozen/deterministic boundary requires a digest")?;
            hex_digest(&boundary.name, digest)?;
            if boundary.reason.is_some() {
                bail!("frozen/deterministic boundary cannot carry an unavailable reason");
            }
        }
        BoundaryState::Live | BoundaryState::Unavailable => {
            if boundary.content_sha256.is_some() {
                bail!("live/unavailable boundary cannot claim frozen bytes");
            }
            bounded(
                "live/unavailable boundary reason",
                boundary.reason.as_deref().unwrap_or_default(),
                1,
                512,
            )?;
        }
    }
    Ok(())
}

fn validate_unavailable(
    boundaries: &BTreeMap<&str, &Boundary>,
    evidence: &[UnavailableEvidence],
) -> Result<()> {
    let unavailable: BTreeSet<&str> = boundaries
        .values()
        .filter(|boundary| boundary.state == BoundaryState::Unavailable)
        .map(|boundary| boundary.name.as_str())
        .collect();
    let mut declared = BTreeSet::new();
    for item in evidence {
        if !declared.insert(item.boundary.as_str())
            || item.downstream_behavior != "unavailable"
            || boundaries
                .get(item.boundary.as_str())
                .and_then(|boundary| boundary.reason.as_deref())
                != Some(item.reason.as_str())
        {
            bail!("unavailable evidence must exactly match an unavailable boundary");
        }
    }
    if declared != unavailable {
        bail!("every unavailable boundary requires explicit unavailable evidence");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn digest(value: &str) -> String {
        review::checksum(value)
    }

    fn capsule(mode: ReplayMode) -> RunCapsule {
        let memories = vec![MemoryRef {
            stable_id: "memory-1".into(),
            revision: 2,
            content_sha256: digest("memory body"),
        }];
        let provider_state = if mode == ReplayMode::Strict {
            BoundaryState::Frozen
        } else {
            BoundaryState::Live
        };
        RunCapsule {
            format: FORMAT.into(),
            validator: VALIDATOR.into(),
            capsule_id: String::new(),
            source_request_id: "11111111-1111-4111-8111-111111111111".into(),
            replay_mode: mode,
            body: CapsuleBody {
                task: EvidenceRef {
                    content_sha256: digest("sanitized task"),
                    sanitizer: review::SANITIZER.into(),
                },
                session_history: EvidenceRef {
                    content_sha256: digest("sanitized history"),
                    sanitizer: review::SANITIZER.into(),
                },
                project: ProjectState {
                    kind: "git_commit".into(),
                    reference: "abcdef1234567890".into(),
                    content_sha256: digest("project tree"),
                    clean: true,
                },
                model: ModelState {
                    provider: "local-fixture".into(),
                    model: "deterministic-v1".into(),
                    parameters: json!({"temperature":0}),
                    request_schema_sha256: digest("provider request schema"),
                },
                tools: ToolState {
                    schemas_sha256: digest("tool schemas"),
                    permission_mode: "ask".into(),
                    results_complete: mode == ReplayMode::Strict,
                    results: vec![ToolResultRef {
                        sequence: 0,
                        tool_name: "read".into(),
                        request_sha256: digest("read request"),
                        result_sha256: digest("read result"),
                    }],
                },
                memory: MemoryState {
                    package_sha256: review::checksum(&canonical_json(
                        &serde_json::to_value(&memories).unwrap(),
                    )),
                    memories,
                },
                context: ContextState {
                    receipt_id: "context-1".into(),
                    content_sha256: digest("context receipt"),
                },
                assertions: vec![
                    Assertion::Command {
                        command: "cargo test --locked".into(),
                        expected_exit: 0,
                    },
                    Assertion::NoUnauthorizedActions,
                ],
                boundaries: REQUIRED_BOUNDARIES
                    .into_iter()
                    .map(|name| Boundary {
                        name: name.into(),
                        state: if name == "provider" {
                            provider_state
                        } else {
                            BoundaryState::Frozen
                        },
                        content_sha256: if name == "provider"
                            && provider_state == BoundaryState::Live
                        {
                            None
                        } else {
                            Some(digest(name))
                        },
                        reason: (name == "provider" && provider_state == BoundaryState::Live).then(
                            || "provider will be called under a separately bounded live run".into(),
                        ),
                    })
                    .collect(),
                unavailable_evidence: vec![],
                strict_prefix_sha256: None,
                first_live_boundary: None,
            },
        }
    }

    #[test]
    fn capsule_is_content_addressed_canonical_and_round_trips() {
        let capsule = capsule(ReplayMode::Strict).seal().unwrap();
        assert_eq!(capsule.capsule_id.len(), 64);
        let raw = capsule.canonical_json().unwrap();
        assert_eq!(RunCapsule::from_json(&raw).unwrap(), capsule);
        let mut changed = capsule.clone();
        changed.body.context.receipt_id = "other-context".into();
        assert!(changed.validate().is_err());
        assert_ne!(changed.seal().unwrap().capsule_id, capsule.capsule_id);
    }

    #[test]
    fn capsule_rejects_incomplete_or_falsely_deterministic_boundaries() {
        let mut missing = capsule(ReplayMode::Strict);
        missing.body.boundaries.pop();
        assert!(missing.seal().is_err());
        let mut live_strict = capsule(ReplayMode::Strict);
        live_strict.body.boundaries[2] = Boundary {
            name: "provider".into(),
            state: BoundaryState::Live,
            content_sha256: None,
            reason: Some("not frozen".into()),
        };
        assert!(live_strict.seal().is_err());
        let mut unavailable = capsule(ReplayMode::Live);
        unavailable.body.boundaries[0] = Boundary {
            name: "clock".into(),
            state: BoundaryState::Unavailable,
            content_sha256: None,
            reason: Some("source did not record a clock".into()),
        };
        assert!(unavailable.clone().seal().is_err());
        unavailable
            .body
            .unavailable_evidence
            .push(UnavailableEvidence {
                boundary: "clock".into(),
                reason: "source did not record a clock".into(),
                downstream_behavior: "unavailable".into(),
            });
        unavailable.seal().unwrap();
    }

    #[test]
    fn capsule_live_and_hybrid_semantics_are_explicit() {
        capsule(ReplayMode::Live).seal().unwrap();
        let mut hybrid = capsule(ReplayMode::Hybrid);
        hybrid.body.strict_prefix_sha256 = Some(digest("strict prefix"));
        hybrid.body.first_live_boundary = Some("provider".into());
        hybrid.seal().unwrap();
        let mut incomplete = capsule(ReplayMode::Hybrid);
        incomplete.body.first_live_boundary = Some("provider".into());
        assert!(incomplete.seal().is_err());
    }
}
