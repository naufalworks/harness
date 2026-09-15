//! P15-T04: the one implementation of "may this text leave, and what does it cite".
//!
//! Search, export and import all need the same two answers: whether a piece of stored text is
//! sanitized enough to be indexed or shipped, and how a returned fragment is traced back to the
//! exact row and revision it came from. Writing that twice would guarantee drift — a search that
//! is stricter than an export, or a citation that names a revision the other side does not
//! record. So `sanitize` and [`Citation`] live here and every caller goes through them.
//!
//! This is deliberately conservative rather than complete: `safety::sensitive` is a best-effort
//! matcher, so a rejection is trustworthy while an acceptance only means "no known marker was
//! found". That is why an accepted document still records which sanitizer version accepted it.

use crate::safety;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Identity of the accepting sanitizer, stored per row. A later, stricter version must not be
/// able to claim it approved text it never saw.
pub const SANITIZER: &str = "harness-sanitize-v1";

/// The largest single document body indexed or exported. A bound that both sides share, so an
/// export cannot ship something search would have refused to hold.
pub const MAX_DOCUMENT_BYTES: usize = 32 * 1024;

/// Why the shared sanitizer refused text. Kept as an enum because "too large" and "contains a
/// secret marker" are different operator problems and must not collapse into one message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    Sensitive,
    TooLarge,
    Empty,
}

impl Rejection {
    pub fn code(self) -> &'static str {
        match self {
            Self::Sensitive => "sensitive_content",
            Self::TooLarge => "document_too_large",
            Self::Empty => "empty_document",
        }
    }
    /// A sentence an operator can read verbatim. It says what was refused, never that some
    /// downstream answer would have changed.
    pub fn message(self) -> &'static str {
        match self {
            Self::Sensitive => {
                "Content matched a sensitive-value marker, so it was not indexed or exported."
            }
            Self::TooLarge => "Content exceeds the shared 32 KiB document bound.",
            Self::Empty => {
                "Content is empty after trimming, so there is nothing to index or export."
            }
        }
    }
}

/// Accept text for indexing or export, or say why not.
///
/// Redaction runs first so a document whose only problem was one secret-looking line survives as
/// its remaining lines; if the redacted result still matches a marker it is refused outright
/// rather than shipped in a partly cleaned state.
pub fn sanitize(text: &str) -> Result<String, Rejection> {
    if text.len() > MAX_DOCUMENT_BYTES {
        return Err(Rejection::TooLarge);
    }
    // Asked before redaction, so a document that redaction empties is reported as the secret it
    // was rather than as an incidental blank. The two are different operator problems.
    let started_sensitive = safety::sensitive(text);
    let redacted = safety::redact(text);
    let trimmed = redacted.trim();
    // A body that redaction reduced to nothing but markers carries no indexable content, and
    // shipping a row of placeholders would only advertise that a secret was once there.
    if trimmed.is_empty()
        || trimmed
            .lines()
            .all(|line| line.trim().is_empty() || line.trim() == safety::REDACTION_MARKER)
    {
        return Err(if started_sensitive {
            Rejection::Sensitive
        } else {
            Rejection::Empty
        });
    }
    if safety::sensitive(trimmed) {
        return Err(Rejection::Sensitive);
    }
    Ok(trimmed.to_string())
}

/// True when `text` is exactly what `sanitize` would emit. Used at read time as a second gate:
/// an index row written by an older sanitizer, or edited outside the writer, must not be served.
pub fn is_sanitized(text: &str) -> bool {
    sanitize(text).is_ok_and(|clean| clean == text)
}

/// SHA-256 of the exact bytes that were indexed or exported. The same helper is used for a
/// document body and for a serialized bundle, so a checksum always means "of exactly this".
pub fn checksum(text: &str) -> String {
    safety::fingerprint(text)
}

/// The traceable origin of one search hit or one exported item.
///
/// Every field is read from the stored row: nothing here is derived at read time, so a citation
/// cannot describe a version of the text that was never stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Citation {
    /// Stable identifier of the indexed document or memory.
    pub id: String,
    /// Which source table `source_id` points at: `turn`, `artifact` or `memory`.
    pub kind: String,
    /// Identifier of the underlying source row.
    pub source_id: String,
    /// The revision of the content this citation is about.
    pub revision: i64,
    pub scope: String,
    pub session_id: Option<String>,
    /// When the cited content was created in its source table.
    pub timestamp: String,
    pub content_sha256: String,
    pub sanitizer: String,
}

impl Citation {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "source_id": self.source_id,
            "revision": self.revision,
            "scope": self.scope,
            "session_id": self.session_id,
            "timestamp": self.timestamp,
            "content_sha256": self.content_sha256,
            "sanitizer": self.sanitizer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_refuses_secrets_and_accepts_ordinary_text() {
        assert_eq!(sanitize("we chose SQLite").unwrap(), "we chose SQLite");
        assert_eq!(sanitize("  \n"), Err(Rejection::Empty));
        assert_eq!(
            sanitize(&"a".repeat(MAX_DOCUMENT_BYTES + 1)),
            Err(Rejection::TooLarge)
        );
        // A single secret-looking line is replaced, the rest survives.
        let mixed = sanitize("keep this line\napi_key=abcdef123456").unwrap();
        assert!(mixed.contains("keep this line"));
        assert!(!mixed.contains("abcdef123456"));
        assert!(mixed.contains(crate::safety::REDACTION_MARKER));
        // Nothing but a secret is reported as the secret it was, not as an incidental blank.
        assert_eq!(sanitize("password: hunter2"), Err(Rejection::Sensitive));
        assert_eq!(sanitize("\n   \n"), Err(Rejection::Empty));
    }

    #[test]
    fn is_sanitized_rejects_text_the_writer_would_have_changed() {
        assert!(is_sanitized("plain"));
        assert!(!is_sanitized("  plain  "));
        assert!(!is_sanitized("bearer abc123def456"));
    }
}
