# Design — safe incremental publication (P7-T05)

This document is the contract for publishing generation text **before** `[DONE]` without
weakening `safety::redact`. It governs `src/safety.rs`, the streaming path in
`src/memory_agents.rs`, the generation-event writes in `src/recording.rs`, and the answer
rendering in `static/app.js`. It changes *when* text becomes visible, never *what counts
as sensitive*, and nothing in `docs/RECORDING_PROTOCOL.md` (admission, receipts, outbox)
is affected.

## Today (P7-T02a .. P7-T03)

- `consume_stream_response` accumulates every decoded delta, and only after `[DONE]` calls
  `sink.delta(&safety::redact(&text))` followed by `sink.complete(&usage)`.
- Failure paths (`provider_http_error`, `incomplete_stream`, oversized body, invalid UTF-8)
  call `sink.fail(..)` and publish no text at all.
- Storage writes the `chunk` row and the `completed` row together; readers reassemble
  `state='chunk' ORDER BY seq`; the UI paints the whole answer atomically.
- Cost: the user sees "generating" with no text for the whole turn.

## Why chunk-wise redaction is unsafe (the real constraint)

`safety::redact` is **line granular with cross-line state**:

1. the decision unit is a line (`text.split('\n')`);
2. a matched line is dropped *whole* and replaced by `[REDACTED SENSITIVE CONTENT]`;
3. `-----BEGIN … PRIVATE KEY-----` opens a state that drops every line until `-----END`.

Two consequences follow, and they are the reason this task exists:

- A pattern can be completed by bytes that arrive later **in the same line** (`pass` +
  `word=hidden`), so a partial line is never safe to publish.
- Because a match erases the whole line, releasing part of a line **cannot be undone**:
  generation events are append-only, durable, and already rendered by clients.

Therefore the safe publication unit is a **completed line** — not a provider chunk, and not
a token.

## Invariants

| # | Invariant |
|---|---|
| I1 | **Equivalence.** For every possible chunking, the concatenation of published text equals `redact(full_answer)`. |
| I2 | **Monotone prefix.** At any moment the published text is a prefix of the final redacted answer. Nothing is ever retracted or rewritten. |
| I3 | **Holdback.** No line is published before its terminating `\n` is observed, or before the stream reaches `[DONE]`. |
| I4 | **Terminal discard.** On failure or interruption the withheld tail is discarded, never flushed. Lines already published stay; the terminal state stays explicit. |
| I5 | **Unchanged sensitivity.** `sensitive()` and its marker vocabulary are untouched; only publication timing changes. |

Byte-boundary safety is already handled upstream: `parse_sse_event` validates UTF-8 and
holds incomplete frames, so this layer only ever sees complete `&str` deltas.

## Design — `safety::StreamRedactor`

A small state machine next to `redact`, sharing its branch ladder:

```rust
pub struct StreamRedactor { pending: String, in_key: bool, emitted: bool }

impl StreamRedactor {
    pub fn push(&mut self, text: &str) -> String; // publishable text, often empty
    pub fn finish(&mut self) -> String;           // flush the final unterminated line
    pub fn pending_len(&self) -> usize;           // MAX_PROVIDER_TEXT accounting
}
```

- `push` appends to `pending` and drains only complete lines; each line goes through the
  same BEGIN / `in_key` / `sensitive` / keep ladder as `redact`.
- Emission joins with `\n` guarded by `emitted`, so dropped key-body lines leave no blank
  line. This is exactly what makes I1 hold.
- `finish` runs the residual `pending` as the last, unterminated line.
- The failure path is "never call `finish`" — dropping the value discards the tail (I4).
- Test-enforced identity: `redact(t)` equals `push(t) + finish()` for every `t`.

Worst case (an answer with no newline) publishes nothing before `[DONE]`, which is exactly
today's behavior — never worse.

## Rejected: intra-line / token-level masking

Releasing a prefix of an unfinished line would require changing `redact` from "drop the
line" to "mask the span", so that an already-released prefix stays valid. Rejected here:
it weakens a deliberately conservative module (`//! Best-effort redaction, not a secret
vault`), invalidates the existing whole-line expectations, and any mistake is
unrecoverable because publication is durable. If it is ever wanted, it needs its own task
that owns the redaction-semantics change and its own review.

## Wire and storage

- One `chunk` row per publication, each committed **before** delivery; ordering is the
  `seq` sequence, not the transport.
- Event vocabulary is unchanged (`chunk`, `completed`, `failed`, `interrupted`) and no
  migration is needed; only the number of `chunk` rows per request changes.
- `completed` still commits atomically with the receipt transition. `chunk` rows never
  carry a terminal state, so a crash mid-answer still yields exactly one `interrupted`
  row from `recording::recover`.
- Reassembly (`SELECT content FROM generation_events WHERE request_id=?1 AND state='chunk'
  ORDER BY seq`) already concatenates in order; a multi-chunk answer must equal
  `redact(full_answer)` byte for byte.
- Cursor/resume semantics are unchanged: a reconnect after chunk *N* replays only later
  rows, and I2 makes append-only replay correct.

## UI

- Append `chunk` content to the answer node with `textContent` in `seq` order. No
  `innerHTML`, no typewriter simulation — pacing belongs to the server.
- Keep the cursor-advance guard; a reload replays chunks and must produce identical text.
- Terminal cards (`completed`, `failed`, `interrupted`) are unchanged. A turn that fails
  mid-answer keeps its published lines *and* shows the explicit failure card, per the
  honest-state copy rules in `docs/design/ui.md`.

## Testing

- `tests/test_incremental_publication.py` (landed with this design) is the executable
  reference spec. It derives the marker vocabulary, token thresholds and the redaction
  marker from `src/safety.rs` so it cannot drift, then proves I1–I4 over every single-cut
  split, seeded multi-cut splits and one-character chunks, using split-secret,
  private-key, CRLF, Unicode, token-shape and no-newline fixtures. It is a specification,
  not a substitute for cargo.
- Rust unit: `StreamRedactor` equals `redact` on the same corpus; an abandoned redactor
  publishes nothing.
- Rust streaming: a multi-chunk answer writes increasing `seq` rows whose concatenation
  equals `redact(full)`; `incomplete_stream` and provider HTTP failures add no content row;
  restart mid-answer keeps prior chunks and writes exactly one `interrupted` row.
- HTTP fixture: SSE subscriber and poller observe the same ordered chunks, and resuming
  from a mid-answer cursor returns only later rows with no duplicates.
- Browser fixture (`tests/recording_ui.cjs`): incremental append plus mid-answer reload —
  blocked on the runtime gap tracked by `P7-T06`.
- Gate: `cargo test --locked streaming && cargo test --locked redact && bash scripts/verify_release.sh`.
