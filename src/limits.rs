//! Shared bounds (P12-T04, "shared limits replace magic values").
//!
//! A bound belongs here when two modules must agree on it and nothing in the type system
//! forces them to. The verification pass is the live example: `memory_agents` *rejects* a
//! provider report whose fields exceed these caps, and `storage` *truncates* the same fields
//! when projecting a stored report for the API. If those two numbers drift apart, a report
//! that passed validation is silently shortened on read, or a field that the reader can
//! represent is rejected at the door. Neither failure surfaces as an error, so the only
//! defence is a single definition both sides read.
//!
//! Bounds that are local to one module stay in that module. Do not move a constant here
//! merely because it is a number.

/// Caps on the verification report, shared by the producer-side validator
/// (`memory_agents::parse_verification`), the evidence builder
/// (`agent_loop::verification`) and the read-side projection
/// (`storage::verification_projection`).
pub mod verification {
    /// Claims accepted per report, and projected per report.
    pub const MAX_CLAIMS: usize = 20;
    /// Evidence step ids accepted per claim, and projected per claim.
    pub const MAX_EVIDENCE_IDS_PER_CLAIM: usize = 8;
    /// Skipped-diagnostic strings accepted per report, and projected per report.
    pub const MAX_SKIPPED_DIAGNOSTICS: usize = 10;
    /// Characters in a claim string.
    pub const MAX_CLAIM_CHARS: usize = 500;
    /// Characters in a claim's `reason`.
    pub const MAX_REASON_CHARS: usize = 500;
    /// Characters in a skipped-diagnostic string.
    pub const MAX_DIAGNOSTIC_CHARS: usize = 240;
    /// Characters in an evidence step id, and in the projected model name.
    pub const MAX_IDENTIFIER_CHARS: usize = 128;
    /// Characters in an evidence tool summary or file path handed to the verifier.
    pub const MAX_EVIDENCE_SUMMARY_CHARS: usize = 500;
}
