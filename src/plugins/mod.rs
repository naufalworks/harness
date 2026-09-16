//! P18-T02: bounded, digest-pinned extensions -- provider adapters, tool plugins
//! and benchmark packs.
//!
//! Trust model, decided by the owner on 2026-09-16: an extension is admitted
//! because its manifest bytes match a checked-in **digest pin**, not because it
//! carries a signature. There are no third-party extensions yet, so a signature
//! would only prove "signed by the single key this repo holds" -- ceremony, not a
//! control. A pin delivers the property that matters today: the bytes admitted
//! are exactly the bytes reviewed. `MANIFEST_SCHEMA` is carried in every manifest
//! so a future `harness.extension/v2` can add a signature block without
//! reinterpreting a v1 manifest.
//!
//! Admission is fail-closed. Every refusal below is a lane that a plausible
//! extension would otherwise use to widen its own reach: an unpinned manifest, a
//! repinned-but-unreviewed manifest, an undeclared capability, a host tool the
//! registry does not offer, a permission mode wider than the host's, a network
//! grant, a payload path that escapes the repo, or a review that attests some
//! other set of files. `admit` returns the first refusal with a stable code, so
//! callers and tests name the reason rather than a message.

// This module is the extension contract, exercised by its own tests and by
// scripts/check_extensions.py against the checked-in corpus. Nothing in the
// running server calls `admit` yet, because no extension is loaded at runtime in
// this build -- admitting one would mean a loader, and a loader with no
// extensions to load would be untested surface. In a binary crate that reads as
// dead code under `-D warnings`, so it is allowed here deliberately and
// narrowly, following the same pattern as `src/tools/mod.rs`. Remove this when a
// loader calls `admit`.
#![allow(dead_code)]

use anyhow::{bail, Result};
use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Manifests declare the contract version they were written against.
pub const MANIFEST_SCHEMA: &str = "harness.extension/v1";

/// The complete set of capabilities an extension may request. Anything outside
/// this list is refused rather than ignored: a capability the host does not
/// understand cannot be bounded, so it cannot be granted.
pub const ALLOWED_CAPABILITIES: &[&str] = &[
    "benchmark.capsule_replay",
    "packet.export_read",
    "provider.describe",
    "provider.translate_request",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionKind {
    ProviderAdapter,
    ToolPlugin,
    BenchmarkPack,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRef {
    pub path: String,
    pub sha256: String,
}

/// An audience review. It attests the payload *set* (see [`payload_digest`]),
/// never the manifest digest: a review stored inside the manifest cannot attest
/// the bytes that contain it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub audience: String,
    pub reviewed_by: String,
    pub payload_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: String,
    pub id: String,
    pub version: String,
    pub kind: ExtensionKind,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub files: Vec<FileRef>,
    #[serde(default)]
    pub review: Option<Review>,
}

/// What the host is willing to offer. Built from the live tool registry and the
/// session's own permission mode, so an extension can never be granted more than
/// the session it runs inside.
#[derive(Debug, Clone)]
pub struct HostPolicy {
    pub permission_mode: String,
    pub available_tools: BTreeSet<String>,
    /// extension id -> pinned manifest digest.
    pub pins: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Admitted {
    pub manifest: Manifest,
    pub manifest_sha256: String,
}

pub fn hex_sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Digest over the sorted `path sha256` lines of a payload set. Reordering the
/// files, swapping one digest, adding a file, or dropping one all change it,
/// which is what makes a review falsifiable.
pub fn payload_digest(files: &[FileRef]) -> String {
    let mut lines: Vec<String> = files
        .iter()
        .map(|f| format!("{} {}\n", f.path, f.sha256))
        .collect();
    lines.sort();
    hex_sha256(lines.concat().as_bytes())
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Ordering of permission modes. Mirrors `crate::tools::PermissionMode`; the test
/// `permission_mode_names_match_the_tool_registry` asserts the names stay in step
/// so this table cannot drift into granting a mode the registry cannot parse.
fn mode_rank(mode: &str) -> Option<u8> {
    match mode {
        "ask" => Some(0),
        "auto_edit" => Some(1),
        "auto_all" => Some(2),
        _ => None,
    }
}

/// Admit a manifest, or refuse with a stable `code: detail` reason.
pub fn admit(raw: &str, policy: &HostPolicy) -> Result<Admitted> {
    let manifest_sha256 = hex_sha256(raw.as_bytes());
    let manifest: Manifest = match serde_json::from_str(raw) {
        Ok(manifest) => manifest,
        Err(err) => bail!("malformed_manifest: {err}"),
    };

    if manifest.schema != MANIFEST_SCHEMA {
        bail!(
            "unsupported_schema: expected {MANIFEST_SCHEMA}, found {}",
            manifest.schema
        );
    }
    if manifest.id.is_empty()
        || !manifest
            .id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    {
        bail!(
            "invalid_id: {:?} must be lowercase alphanumeric with '-' or '.'",
            manifest.id
        );
    }
    if manifest.version.is_empty() {
        bail!("invalid_version: {:?} must not be empty", manifest.id);
    }

    // Pinning. An unpinned manifest is refused even when it looks harmless, and a
    // manifest whose bytes drifted from the pin is refused even though its id is
    // known -- that is the case where someone edited a reviewed extension.
    let Some(pinned) = policy.pins.get(&manifest.id) else {
        bail!("unpinned_extension: {} has no digest pin", manifest.id);
    };
    if pinned != &manifest_sha256 {
        bail!(
            "digest_mismatch: {} is pinned at {pinned} but these bytes are {manifest_sha256}",
            manifest.id
        );
    }

    if manifest.capabilities.is_empty() {
        bail!(
            "no_capability_declared: {} requests nothing and cannot be bounded",
            manifest.id
        );
    }
    for capability in &manifest.capabilities {
        if !ALLOWED_CAPABILITIES.contains(&capability.as_str()) {
            bail!("capability_not_allowed: {capability}");
        }
    }

    // Host tools. The extension may only name tools the live registry offers; it
    // cannot invent one and it cannot reach a tool the host withheld.
    for tool in &manifest.tools {
        if !policy.available_tools.contains(tool) {
            bail!("unknown_tool: {tool} is not offered by the host registry");
        }
    }

    // Permission mode. Equal or narrower than the host's, never wider.
    let Some(host_rank) = mode_rank(&policy.permission_mode) else {
        bail!(
            "invalid_host_mode: {:?} is not a permission mode",
            policy.permission_mode
        );
    };
    if let Some(requested) = &manifest.permission_mode {
        let Some(rank) = mode_rank(requested) else {
            bail!("invalid_permission_mode: {requested:?}");
        };
        if rank > host_rank {
            bail!(
                "permission_widened: {} requests {requested} above host {}",
                manifest.id,
                policy.permission_mode
            );
        }
    }

    if manifest.network {
        bail!(
            "network_not_permitted: {} requests network access",
            manifest.id
        );
    }

    // Payload paths stay inside the repo and carry real digests.
    for file in &manifest.files {
        if file.path.is_empty()
            || file.path.starts_with('/')
            || file.path.split('/').any(|part| part == "..")
        {
            bail!("invalid_payload_path: {:?}", file.path);
        }
        if !is_hex64(&file.sha256) {
            bail!("invalid_payload_digest: {} in {}", file.sha256, file.path);
        }
    }

    // Audience review. A benchmark pack ships evidence others will read, so it
    // must carry a review, and that review must cover this exact payload set.
    if manifest.kind == ExtensionKind::BenchmarkPack {
        let Some(review) = &manifest.review else {
            bail!(
                "review_missing: {} ships shareable evidence without a review",
                manifest.id
            );
        };
        if review.audience.is_empty() || review.reviewed_by.is_empty() {
            bail!(
                "review_incomplete: {} names no audience or reviewer",
                manifest.id
            );
        }
        let expected = payload_digest(&manifest.files);
        if review.payload_sha256 != expected {
            bail!(
                "review_digest_mismatch: review attests {} but the payload set is {expected}",
                review.payload_sha256
            );
        }
    }

    Ok(Admitted {
        manifest,
        manifest_sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{PermissionMode, Registry};

    fn pack_json(overrides: &[(&str, &str)]) -> String {
        let mut manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "id": "memory-recall-v1",
            "version": "1.0.0",
            "kind": "benchmark_pack",
            "capabilities": ["benchmark.capsule_replay"],
            "tools": [],
            "permission_mode": "ask",
            "network": false,
            "files": [{
                "path": "tests/capsules/strict_replay_requests.json",
                "sha256": "9a7e9e8aa29e2f040d60ed4f08b032b9e3042a68495608bc69ad9289c196c697"
            }],
        });
        let files: Vec<FileRef> =
            serde_json::from_value(manifest["files"].clone()).expect("files parse");
        manifest["review"] = serde_json::json!({
            "audience": "owner",
            "reviewed_by": "owner",
            "payload_sha256": payload_digest(&files),
        });
        for (pointer, value) in overrides {
            let parsed: serde_json::Value =
                serde_json::from_str(value).expect("override is valid JSON");
            manifest[*pointer] = parsed;
        }
        serde_json::to_string(&manifest).expect("serialize")
    }

    /// Pins the manifest we are about to test, so only the field under test can
    /// be the reason a case is refused.
    fn policy_for(raw: &str) -> HostPolicy {
        let mut pins = BTreeMap::new();
        let id: String = serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_owned))
            .unwrap_or_default();
        pins.insert(id, hex_sha256(raw.as_bytes()));
        HostPolicy {
            permission_mode: "auto_edit".to_owned(),
            available_tools: Registry::standard()
                .schemas()
                .expect("schemas")
                .iter()
                .filter_map(|s| s["name"].as_str().map(str::to_owned))
                .collect(),
            pins,
        }
    }

    fn refusal(raw: &str) -> String {
        let policy = policy_for(raw);
        admit(raw, &policy)
            .expect_err("manifest should have been refused")
            .to_string()
    }

    #[test]
    fn a_pinned_bounded_reviewed_pack_is_admitted() {
        let raw = pack_json(&[]);
        let policy = policy_for(&raw);
        let admitted = admit(&raw, &policy).expect("should be admitted");
        assert_eq!(admitted.manifest.id, "memory-recall-v1");
        assert_eq!(admitted.manifest.kind, ExtensionKind::BenchmarkPack);
        assert_eq!(admitted.manifest_sha256, hex_sha256(raw.as_bytes()));
    }

    #[test]
    fn an_unpinned_manifest_is_refused() {
        let raw = pack_json(&[]);
        let policy = HostPolicy {
            pins: BTreeMap::new(),
            ..policy_for(&raw)
        };
        let err = admit(&raw, &policy).expect_err("unpinned").to_string();
        assert!(err.starts_with("unpinned_extension:"), "{err}");
    }

    /// The case that matters most: a reviewed extension edited after the review.
    #[test]
    fn a_manifest_edited_after_pinning_is_refused() {
        let reviewed = pack_json(&[]);
        let policy = policy_for(&reviewed);
        let edited = pack_json(&[("version", "\"1.0.1\"")]);
        let err = admit(&edited, &policy)
            .expect_err("digest drift")
            .to_string();
        assert!(err.starts_with("digest_mismatch:"), "{err}");
    }

    #[test]
    fn an_undeclared_capability_is_refused() {
        let err = refusal(&pack_json(&[("capabilities", "[\"storage.write_all\"]")]));
        assert!(err.starts_with("capability_not_allowed:"), "{err}");
    }

    #[test]
    fn declaring_no_capability_is_refused() {
        let err = refusal(&pack_json(&[("capabilities", "[]")]));
        assert!(err.starts_with("no_capability_declared:"), "{err}");
    }

    #[test]
    fn a_tool_the_registry_does_not_offer_is_refused() {
        let err = refusal(&pack_json(&[("tools", "[\"deploy\"]")]));
        assert!(err.starts_with("unknown_tool:"), "{err}");
    }

    #[test]
    fn a_wider_permission_mode_is_refused() {
        let err = refusal(&pack_json(&[("permission_mode", "\"auto_all\"")]));
        assert!(err.starts_with("permission_widened:"), "{err}");
    }

    #[test]
    fn a_network_grant_is_refused() {
        let err = refusal(&pack_json(&[("network", "true")]));
        assert!(err.starts_with("network_not_permitted:"), "{err}");
    }

    #[test]
    fn a_payload_path_escaping_the_repo_is_refused() {
        let escaping = "[{\"path\": \"../../etc/passwd\", \"sha256\": \"9a7e9e8aa29e2f040d60ed4f08b032b9e3042a68495608bc69ad9289c196c697\"}]";
        let err = refusal(&pack_json(&[("files", escaping)]));
        assert!(err.starts_with("invalid_payload_path:"), "{err}");
    }

    #[test]
    fn a_benchmark_pack_without_a_review_is_refused() {
        let err = refusal(&pack_json(&[("review", "null")]));
        assert!(err.starts_with("review_missing:"), "{err}");
    }

    /// Swapping the payload after review keeps the review block intact, so only
    /// the payload-set digest can catch it.
    #[test]
    fn a_review_that_does_not_cover_the_payload_is_refused() {
        let swapped = "[{\"path\": \"tests/experiments/remote_runner.json\", \"sha256\": \"c3f22e259fd6ee0530f143b639d9553cae824c9dbc468388c070a89276a22961\"}]";
        let err = refusal(&pack_json(&[("files", swapped)]));
        assert!(err.starts_with("review_digest_mismatch:"), "{err}");
    }

    #[test]
    fn an_unsupported_schema_is_refused() {
        let err = refusal(&pack_json(&[("schema", "\"harness.extension/v2\"")]));
        assert!(err.starts_with("unsupported_schema:"), "{err}");
    }

    #[test]
    fn malformed_json_is_refused_rather_than_panicking() {
        let err = refusal("{not json");
        assert!(err.starts_with("malformed_manifest:"), "{err}");
    }

    /// The permission ranking here is a second copy of the registry's mode names.
    /// This test is what stops the copy from drifting into a mode the registry
    /// cannot parse, which would silently admit an unbounded extension.
    #[test]
    fn permission_mode_names_match_the_tool_registry() {
        for mode in [
            PermissionMode::Ask,
            PermissionMode::AutoEdit,
            PermissionMode::AutoAll,
        ] {
            assert!(
                mode_rank(mode.as_str()).is_some(),
                "registry mode {} is unknown to the extension policy",
                mode.as_str()
            );
        }
        assert_eq!(mode_rank("root"), None);
    }

    /// A provider adapter carries no review requirement, but it is still bounded
    /// by pin, capability allowlist and the network refusal.
    #[test]
    fn a_provider_adapter_is_admitted_without_a_review() {
        let raw = pack_json(&[
            ("kind", "\"provider_adapter\""),
            ("id", "\"acme-provider\""),
            ("capabilities", "[\"provider.describe\"]"),
            ("review", "null"),
            ("files", "[]"),
        ]);
        let policy = policy_for(&raw);
        let admitted = admit(&raw, &policy).expect("provider adapter should be admitted");
        assert_eq!(admitted.manifest.kind, ExtensionKind::ProviderAdapter);
    }

    /// The `kind` strings in benchmarks/pinned.json and in scripts/check_extensions.py
    /// are the serde names of this enum. Asserting both directions here is what stops
    /// a rename from silently desynchronising the checked-in pin ledger, which would
    /// otherwise surface as a pin that matches nothing.
    #[test]
    fn kind_names_match_the_checked_in_pin_ledger() {
        for (kind, name) in [
            (ExtensionKind::ProviderAdapter, "provider_adapter"),
            (ExtensionKind::ToolPlugin, "tool_plugin"),
            (ExtensionKind::BenchmarkPack, "benchmark_pack"),
        ] {
            assert_eq!(
                serde_json::to_value(kind).expect("serialize"),
                serde_json::json!(name)
            );
        }

        let ledger_path = concat!(env!("CARGO_MANIFEST_DIR"), "/benchmarks/pinned.json");
        let raw = std::fs::read_to_string(ledger_path).expect("pin ledger is readable");
        let ledger: serde_json::Value = serde_json::from_str(&raw).expect("pin ledger is JSON");
        let pins = ledger["pins"].as_object().expect("pins object");
        assert!(!pins.is_empty(), "pin ledger must not be empty");
        for (id, pin) in pins {
            let kind = pin["kind"].as_str().expect("pinned kind");
            serde_json::from_value::<ExtensionKind>(serde_json::json!(kind))
                .unwrap_or_else(|_| panic!("pin {id} names unknown kind {kind}"));
        }
    }
}
