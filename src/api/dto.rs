//! Typed views over the untyped JSON the storage layer hands to the HTTP layer.
//!
//! P12-T03. Handlers used to branch on `receipt["state"] == "generating"` and
//! `receipt["request_id"].as_str()`. Indexing an untyped `Value` with a string
//! literal compiles whatever the literal says: a typo silently yields `Null`,
//! matches no arm, and changes a status code without any test or compiler
//! noticing. The parse happens once here so handlers match on variants.
//!
//! This is a view, not a schema for the whole receipt. The receipt carries many
//! more fields, and they are still forwarded to the client verbatim; this type
//! only names the parts the server makes decisions with.
use crate::recording::RequestState;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ReceiptView {
    #[serde(default)]
    pub(crate) request_id: Option<String>,
    #[serde(default)]
    pub(crate) scope: Option<String>,
    /// Deliberately the raw string rather than `Option<RequestState>`. A state
    /// this build does not recognise would fail deserialization for the whole
    /// struct, which would also blank `request_id` and turn an unknown state
    /// into a missing identifier. Parsing is separate, in `state()`.
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
}

impl ReceiptView {
    pub(crate) fn of(receipt: &Value) -> Self {
        serde_json::from_value(receipt.clone()).unwrap_or_default()
    }

    /// `None` when the receipt carries no state, or carries one this build does
    /// not know. Callers must handle that explicitly; it is never quietly
    /// treated as terminal, because reporting a made-up terminal state is worse
    /// than continuing to poll.
    pub(crate) fn state(&self) -> Option<RequestState> {
        self.state.as_deref().and_then(RequestState::parse)
    }

    /// Distinguishes a deliberate cancellation from any other interruption.
    pub(crate) fn was_cancelled(&self) -> bool {
        self.error_code.as_deref() == Some("cancelled")
    }
}

/// Shown on a change card when the row itself could not be read. Refusing is
/// the only safe direction: a revert writes to the user's files, so an
/// unreadable row must never be treated as revertable.
pub(crate) const UNREADABLE_CHANGE: &str = "This change could not be read; revert unavailable";

/// One recorded file change, as stored by `file_changes` and returned by both
/// `turn_changes` (a list) and `file_change` (a single row). The two producers
/// emit the same field names, so one view serves both; fields only one of them
/// sends are simply absent here, because the revert logic does not read them.
///
/// Every field is optional except `applied`, which is written as a real JSON
/// boolean by the storage layer (`r.get::<_, i64>(..)? == 1`). Keeping it a
/// strict `bool` means a row carrying anything else fails to parse, and a row
/// that fails to parse is refused rather than reverted. That matches the
/// previous `change["applied"] != json!(true)` test, which also demanded a
/// literal `true`.
#[derive(Debug, Deserialize)]
pub(crate) struct ChangeRow {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) path: Option<String>,
    #[serde(default)]
    pub(crate) action: Option<String>,
    #[serde(default)]
    pub(crate) applied: bool,
    #[serde(default)]
    pub(crate) reverted_at: Option<String>,
    #[serde(default)]
    pub(crate) before_hash: Option<String>,
    #[serde(default)]
    pub(crate) after_hash: Option<String>,
    #[serde(default)]
    pub(crate) diff: Option<String>,
}

impl ChangeRow {
    /// `None` when the row cannot be read. Callers must refuse the revert in
    /// that case; there is no safe default for "which bytes should I restore".
    pub(crate) fn of(change: &Value) -> Option<Self> {
        serde_json::from_value(change.clone()).ok()
    }

    pub(crate) fn is_action(&self, action: &str) -> bool {
        self.action.as_deref() == Some(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL: [RequestState; 5] = [
        RequestState::Captured,
        RequestState::Generating,
        RequestState::Complete,
        RequestState::Failed,
        RequestState::Interrupted,
    ];

    fn wire(state: RequestState) -> String {
        serde_json::to_value(state)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn api_receipt_view_reads_the_fields_handlers_branch_on() {
        let view = ReceiptView::of(&json!({
            "request_id": "r1",
            "state": "interrupted",
            "error_code": "cancelled",
            "response": "ignored",
            "memory_status": "deferred"
        }));
        assert_eq!(view.request_id.as_deref(), Some("r1"));
        assert_eq!(view.state(), Some(RequestState::Interrupted));
        assert!(view.was_cancelled());
    }

    #[test]
    fn api_an_unknown_state_does_not_parse_and_does_not_lose_the_request_id() {
        // The whole point of keeping `state` as a string: a state written by a
        // newer build must not make the identifier unreadable.
        let view = ReceiptView::of(&json!({"request_id": "r2", "state": "teleported"}));
        assert_eq!(view.request_id.as_deref(), Some("r2"));
        assert_eq!(view.state(), None);
        assert!(!view.was_cancelled());
    }

    #[test]
    fn api_a_receipt_without_the_branching_fields_is_not_an_error() {
        let view = ReceiptView::of(&json!({"unrelated": true}));
        assert_eq!(view.request_id, None);
        assert_eq!(view.state(), None);
    }

    #[test]
    fn api_an_interruption_without_a_cancel_code_is_not_a_cancellation() {
        let view =
            ReceiptView::of(&json!({"state": "interrupted", "error_code": "provider_error"}));
        assert_eq!(view.state(), Some(RequestState::Interrupted));
        assert!(!view.was_cancelled());
    }

    #[test]
    fn api_pending_states_are_exactly_captured_and_generating() {
        // These two are what the admission queries filter on
        // (`state IN ('captured','generating')`). If this set ever changes,
        // those SQL predicates must change with it.
        let pending: Vec<String> = ALL
            .into_iter()
            .filter(|state| state.is_pending())
            .map(wire)
            .collect();
        assert_eq!(pending, vec!["captured", "generating"]);
    }

    #[test]
    fn api_request_states_round_trip_through_their_durable_strings() {
        for state in ALL {
            let text = wire(state);
            // The wire string, the parser, and the deserializer must agree;
            // these strings are stored in the database and sent to clients.
            assert_eq!(RequestState::parse(&text), Some(state));
            assert_eq!(
                serde_json::from_value::<RequestState>(json!(text)).unwrap(),
                state
            );
        }
    }

    #[test]
    fn api_only_failed_and_interrupted_requests_are_retryable() {
        // Retryable is narrower than terminal: `complete` is terminal too, and
        // re-running a finished answer would pay for it a second time.
        let retryable: Vec<String> = ALL
            .into_iter()
            .filter(|state| matches!(state, RequestState::Failed | RequestState::Interrupted))
            .map(wire)
            .collect();
        assert_eq!(retryable, vec!["failed", "interrupted"]);
        assert!(!RequestState::Complete.is_pending());
    }

    /// The exact shape `turn_changes` emits, field for field.
    fn recorded_change() -> Value {
        json!({
            "id": "c1",
            "step_id": "s1",
            "path": "src/main.rs",
            "action": "edit",
            "before_hash": "aaa",
            "after_hash": "bbb",
            "diff": "@@\n-a\n+b\n",
            "applied": true,
            "reverted_at": Value::Null,
            "created_at": "2026-01-01T00:00:00Z"
        })
    }

    #[test]
    fn api_change_row_reads_a_recorded_change() {
        let change = ChangeRow::of(&recorded_change()).expect("the listed row shape must parse");
        assert_eq!(change.id.as_deref(), Some("c1"));
        assert_eq!(change.path.as_deref(), Some("src/main.rs"));
        assert_eq!(change.before_hash.as_deref(), Some("aaa"));
        assert_eq!(change.after_hash.as_deref(), Some("bbb"));
        assert!(change.applied);
        assert_eq!(change.reverted_at, None);
        assert!(change.is_action("edit"));
        assert!(!change.is_action("delete"));
    }

    #[test]
    fn api_change_row_reads_the_single_row_shape_too() {
        // `file_change` adds scope/session_id/request_id and omits created_at.
        // The revert path reads neither, but the row must still parse.
        let mut row = recorded_change();
        let object = row.as_object_mut().unwrap();
        object.remove("created_at");
        object.insert("scope".into(), json!("global"));
        object.insert("session_id".into(), json!("sess"));
        object.insert("request_id".into(), json!("req"));
        let change = ChangeRow::of(&row).expect("the single-row shape must parse");
        assert_eq!(change.path.as_deref(), Some("src/main.rs"));
        assert!(change.applied);
    }

    #[test]
    fn api_a_change_row_that_cannot_be_read_is_refused_rather_than_reverted() {
        // `applied` is written as a real boolean. Anything else means this row
        // did not come from a build that agrees with us about the format, and a
        // revert writes to the user's files, so the row is refused outright.
        let mut row = recorded_change();
        row.as_object_mut()
            .unwrap()
            .insert("applied".into(), json!(1));
        assert!(ChangeRow::of(&row).is_none());
        assert!(ChangeRow::of(&json!("not even an object")).is_none());
    }

    #[test]
    fn api_a_change_row_without_an_applied_flag_is_not_applied() {
        // Absent is not the same as unreadable: the row parses, and the missing
        // flag means "never applied", which is not revertable either.
        let mut row = recorded_change();
        row.as_object_mut().unwrap().remove("applied");
        let change = ChangeRow::of(&row).expect("a missing flag still parses");
        assert!(!change.applied);
    }

    #[test]
    fn api_a_change_row_may_omit_its_hashes_and_diff() {
        // A created file has no previous content; the revert path relies on
        // these being absent rather than empty.
        let change = ChangeRow::of(&json!({
            "id": "c2",
            "path": "new.txt",
            "action": "create",
            "applied": true,
            "before_hash": Value::Null,
            "after_hash": "ccc",
            "diff": Value::Null,
            "reverted_at": Value::Null
        }))
        .expect("a create row must parse");
        assert_eq!(change.before_hash, None);
        assert_eq!(change.diff, None);
        assert!(change.is_action("create"));
    }
}
