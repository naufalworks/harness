//! The concrete continuation-packet shape.
//!
//! Docs describe this only in prose: `docs/ROADMAP.md#p18-optional-platform-evolution` promises
//! "portable continuation packets across machines", the P15 line promises "portable
//! import/export", the parking-lot entry in `docs/TASKS.md` calls it a "portable continuation
//! packet export", and `docs/design/causal-observability.md` uses "continuation anchor" for the
//! opaque cursor a client must pass back unchanged. This module makes the packet concrete while
//! keeping every one of those promises:
//!
//! * **portable** — one self-describing JSON document with an explicit `format_version`, no
//!   dependence on local file paths, and no database rowids;
//! * **selective** — it carries exactly the reviewed items the operator chose;
//! * **traceable** — every item carries the [`Citation`] shape search returns, so a packet read
//!   on another machine can still say where each piece came from;
//! * **verifiable** — a per-item checksum plus a bundle checksum over the canonical serialization;
//! * **opaque where the design says opaque** — `anchor` is passed back unchanged, never parsed.
//!
//! What a packet deliberately is *not*: it is not a database backup, it carries no exact original
//! bytes (`src/archive/` owns those, encrypted), and it makes no claim that importing it
//! reproduces any past answer. It is reviewed sanitized evidence plus the identifiers needed to
//! merge that evidence without duplicating or silently overwriting it.

use super::review::{self, Citation};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only packet format version this build writes or accepts.
pub const FORMAT_VERSION: u32 = 1;

/// What a bundle is for. `MemorySelection` ports reviewed memories; `ContinuationPacket` also
/// carries sanitized history so work can continue elsewhere with its context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketKind {
    MemorySelection,
    ContinuationPacket,
}

impl PacketKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MemorySelection => "memory_selection",
            Self::ContinuationPacket => "continuation_packet",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "memory_selection" => Some(Self::MemorySelection),
            "continuation_packet" => Some(Self::ContinuationPacket),
            _ => None,
        }
    }
}

/// One exported thing. `stable_id` and `revision` together are the merge key: the importer uses
/// them to decide created / unchanged / advanced / stale without ever minting a new identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PacketItem {
    /// `memory` or `history`.
    pub kind: String,
    /// The identifier this item already has in the exporting database.
    pub stable_id: String,
    pub revision: i64,
    /// The reviewed sanitized payload. Shape depends on `kind`; both carry `citation`.
    pub payload: Value,
    pub citation: Citation,
    pub content_sha256: String,
}

/// The portable document.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContinuationPacket {
    pub format_version: u32,
    /// The exporting bundle's identifier. Reused on import as the idempotency key, so importing
    /// the same packet twice is observably the same decision set rather than a second copy.
    pub bundle_id: String,
    pub kind: PacketKind,
    pub scope: String,
    /// Who this was reviewed for. Carried so a receiving operator can see the audience the
    /// review was made against rather than assuming their own.
    pub audience: String,
    /// The sanitizer that accepted every body in this packet.
    pub sanitizer: String,
    pub created_at: String,
    pub reviewed_at: String,
    /// Opaque continuation anchor, per `docs/design/causal-observability.md`: a client passes it
    /// back unchanged and never constructs or interprets it.
    pub anchor: Option<String>,
    pub items: Vec<PacketItem>,
    /// SHA-256 over the canonical serialization of everything above.
    pub content_sha256: String,
}

impl ContinuationPacket {
    /// The bytes a checksum is taken over: the packet with `content_sha256` blanked, serialized
    /// by `serde_json` with sorted keys. Defining it once means the writer and every later
    /// verifier hash the same thing.
    fn canonical(&self) -> Result<String> {
        let mut value = serde_json::to_value(self)?;
        if let Some(object) = value.as_object_mut() {
            object.remove("content_sha256");
        }
        Ok(canonical_json(&value))
    }

    /// Stamp the checksum over the current contents.
    pub fn seal(mut self) -> Result<Self> {
        self.content_sha256 = review::checksum(&self.canonical()?);
        Ok(self)
    }

    /// Verify the packet is exactly what was sealed, that its format is understood, and that
    /// every body in it is still what the shared sanitizer would emit.
    ///
    /// The sanitizer re-check is not redundant: a packet arrives from outside this machine, so
    /// "it was reviewed there" is a claim, and the only thing that makes it safe to index here
    /// is running the local gate over it again.
    pub fn verify(&self) -> Result<()> {
        if self.format_version != FORMAT_VERSION {
            bail!(
                "unsupported continuation packet format version {}",
                self.format_version
            );
        }
        if self.content_sha256 != review::checksum(&self.canonical()?) {
            bail!("continuation packet checksum does not match its contents");
        }
        for item in &self.items {
            if item.revision <= 0 {
                bail!("packet item {} has no usable revision", item.stable_id);
            }
            let body = item_body(&item.payload);
            if !review::is_sanitized(&body) {
                bail!(
                    "packet item {} carries content the local sanitizer refuses",
                    item.stable_id
                );
            }
            if review::checksum(&body) != item.content_sha256 {
                bail!("packet item {} checksum does not match", item.stable_id);
            }
        }
        Ok(())
    }
}

/// The one field of an item payload that holds free text. Both payload shapes put their
/// sanitized body in `body`, so the gate has one place to look.
pub fn item_body(payload: &Value) -> String {
    payload
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Deterministic JSON: object keys sorted, no incidental whitespace. `serde_json::Map` preserves
/// insertion order by default, so two packets with identical content could otherwise hash
/// differently purely because of field order.
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body = keys
                .iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        canonical_json(&Value::String((*key).clone())),
                        canonical_json(&map[*key])
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(body: &str) -> PacketItem {
        PacketItem {
            kind: "memory".into(),
            stable_id: "11111111-1111-4111-8111-111111111111".into(),
            revision: 2,
            payload: json!({"key":"language","body":body}),
            citation: Citation {
                id: "11111111-1111-4111-8111-111111111111".into(),
                kind: "memory".into(),
                source_id: "cand".into(),
                revision: 2,
                scope: "proj".into(),
                session_id: None,
                timestamp: "2026-09-15T00:00:00+00:00".into(),
                content_sha256: review::checksum(body),
                sanitizer: review::SANITIZER.into(),
            },
            content_sha256: review::checksum(body),
        }
    }

    fn packet(body: &str) -> ContinuationPacket {
        ContinuationPacket {
            format_version: FORMAT_VERSION,
            bundle_id: "22222222-2222-4222-8222-222222222222".into(),
            kind: PacketKind::ContinuationPacket,
            scope: "proj".into(),
            audience: "self".into(),
            sanitizer: review::SANITIZER.into(),
            created_at: "2026-09-15T00:00:00+00:00".into(),
            reviewed_at: "2026-09-15T00:01:00+00:00".into(),
            anchor: None,
            items: vec![item(body)],
            content_sha256: String::new(),
        }
        .seal()
        .unwrap()
    }

    #[test]
    fn a_sealed_packet_verifies_and_survives_a_json_round_trip() {
        let sealed = packet("we chose SQLite");
        sealed.verify().unwrap();
        let text = serde_json::to_string(&sealed).unwrap();
        let parsed: ContinuationPacket = serde_json::from_str(&text).unwrap();
        parsed.verify().unwrap();
        assert_eq!(parsed.content_sha256, sealed.content_sha256);
    }

    #[test]
    fn canonical_json_ignores_field_order() {
        let a = json!({"a":1,"b":{"c":2,"d":3}});
        let b: Value = serde_json::from_str(r#"{"b":{"d":3,"c":2},"a":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn tampering_with_a_packet_is_detected() {
        // Body edited after sealing: both the item checksum and the bundle checksum move.
        let mut tampered = packet("we chose SQLite");
        tampered.items[0].payload = json!({"key":"language","body":"we chose Postgres"});
        assert!(tampered.verify().is_err());

        // Revision bumped to look newer than it is.
        let mut relabelled = packet("we chose SQLite");
        relabelled.items[0].revision = 99;
        assert!(relabelled.verify().is_err());

        // A packet whose body would not pass the local sanitizer is refused even if its own
        // checksums are internally consistent.
        let mut unsanitized = packet("we chose SQLite");
        unsanitized.items[0].payload = json!({"key":"language","body":"bearer abc123def456"});
        unsanitized.items[0].content_sha256 = review::checksum("bearer abc123def456");
        let unsanitized = unsanitized.seal().unwrap();
        let error = unsanitized.verify().unwrap_err().to_string();
        assert!(error.contains("sanitizer refuses"), "{error}");

        // An unknown format version is refused rather than best-effort parsed.
        let mut future = packet("we chose SQLite");
        future.format_version = FORMAT_VERSION + 1;
        assert!(future.verify().is_err());
    }
}
