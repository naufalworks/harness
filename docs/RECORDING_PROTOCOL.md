# Recording protocol — checkpoint 1

## Admission and generation
`POST /chat/submit` accepts `{prompt, session_id?, request_id?, scope?, model?}` using the existing UUID/length/scope/auth/Origin validation. The browser always supplies stable IDs. A receipt (HTTP 202 for pending work, 200 for terminal replay) is returned only after sanitized message, receipt, outbox intent and capture event commit under SQLite WAL + synchronous=FULL. IDs are not authorization secrets; every endpoint requires authentication.

A background serial worker claims captured turns, builds a deterministic initial window with independent budgets for system rules, tools, future skills, a scoped ≤8 KiB repository map, active scoped hybrid recall, plan, prior compacted history, recent completed turns and the current message, then commits the exact prepared message/tool arrays plus the included/excluded byte ledger before calling the provider. Old current-turn tool bodies compact only in later provider windows; a 70%-token turn compaction is a separate durable step and cannot rewrite the initial receipt. A frontend disconnect has no ownership over this worker. One unresolved generation per session preserves pair ordering. Up to 100 outstanding generations globally; 1000 outstanding extraction jobs, with deferred recording outbox intents independent of that cap.

`POST /chat` remains a compatibility helper: wait about five seconds for a terminal result; otherwise return 202 + receipt. Clients MUST handle 202 and poll. This is a documented behavior change from the old blocking endpoint, not full legacy compatibility.

## Retrieval
- `GET /chat/requests/{request_id}`: durable state, timestamps, saved answer (if complete), redaction flag, memory-job status and recalled snapshots.
- `GET /chat/requests/{request_id}/context`: above plus event timeline, exact first-call message/tool arrays, included memory snapshots, and all nine category ledgers. Reference data only; not model-private reasoning or proof of provider delivery.
- `GET /sessions/{id}/messages?before_seq=N`: up to 100 chronological messages, `has_more`, `next_before_seq`.
- `GET /sessions?before_seq=N`: up to 50 most-recent sessions with saved title excerpt and message count. Keyset paging is not a frozen snapshot; concurrently updated sessions can move toward the newest page—refresh newest to see them.
- `GET /memory/candidates?scope=&request_id=|chat_only=true|imports_only=true`: pending review candidates, filtered so inline chat suggestions and the import Inbox do not duplicate each other.
- `POST /memory/candidates/{id}/edit`: revalidates and edits a still-pending value in the named scope; evidence/category/revision are unchanged. Save/Dismiss continue through `/memory/confirm`.
- `GET /chat/requests/{request_id}/retrieval`: the write-once retrieval receipt for the turn — strategy, embedding model, prompt fingerprint, recall budget, and every ranked candidate with its scores, revision, `included`/`excluded` decision and reason (`ranked_and_fit`, `rank_cutoff`, `payload_ceiling`, `category_budget`). It records what retrieval did, not what the model did with it; 404 when the turn recorded none. The prompt itself is never stored, only its fingerprint.
- `POST /memory/retrieval/preview`: re-runs the shipped ranking for a prompt, optionally with a still-pending candidate approved, inside a transaction that is always rolled back. Nothing is approved, no embedding is written and no recall counter moves. Reports `before`, `after`, `added`, `removed` and a note stating that a ranking change is not a claim about the answer.

## State machine
`captured → generating → complete | failed | interrupted`

- **captured:** sanitized message and extraction intent committed; provider not invoked by this turn yet. Resumes after process restart.
- **generating:** worker claimed it; context may be pending or saved. No successful answer is implied.
- **complete:** sanitized assistant answer and completion event committed. The outbox was already durable; extraction need not have a job slot yet.
- **failed:** generation/context/save failed; saved user message remains. An answer that could not be committed is not represented as recorded.
- **interrupted:** started work found on process restart. It is not automatically generated again; provider receipt/billing status may be unknown.

Same ID + same sanitized request signature returns current receipt, including terminal failures. Different signature or reused old-message ID → 409. Signature binds session, scope, sanitized prompt, requested model override and redaction flag. It intentionally does not hash raw secret-bearing input. Different raw inputs redacted to identical text can compare equal for the SAME caller-supplied ID. A changed default model does not invalidate an existing submission.

## Memory independence
The outbox is created at capture and dispatches only terminal turns. Even a failed provider turn can yield candidate suggestions based on its saved user message. Live jobs contain the exact user event plus separately labelled current-plan context; only an exact substring of the user event may be evidence. Corrections are deterministically high priority and decisions remain pending review. Outbox insert into jobs + linking receipt + event commit atomically. If the queue is full, the intent remains deferred; if dispatch fails, it retries later. No auto-activation was added.

Legacy v1 messages are preserved without invented context receipts. Inherited v1 pending messages are marked failed on restart. The old memory importer still creates v1 schema output, upgraded additively on a successful v2 startup.

## Boundaries
No raw artifact encryption, streaming chunks, tamper-proof log, power-loss proof beyond SQLite/OS/filesystem guarantees, multi-process coordination, archive search or automatic memory activation. Plaintext sanitized SQLite backups may still be sensitive. A local user/DB editor can alter records; “write once” context is an application/SQLite constraint, not forensic authenticity.

Frontend keeps only request/session/scope identifiers in sessionStorage. It does not persist raw unsent drafts or bearer tokens. Lost-response recovery polls instead of resending. Same-tab explicit Retry same message reuses the original ID and prompt. Leave for later starts a new session without cancelling/resending the old request; saved work remains discoverable in History. Unknown save status is never displayed as successful capture.


## External development history (P19-T01 contract only)

The versioned envelope and offline fixture contract are specified in
[External development history](design/external-development-history.md).
This is not a deployed ingestion API. P19-T02 and P19-T03 own producer policy
and durable ingestion respectively. External records must not enter `/chat/submit`
or trigger provider generation or development-action replay. Fixture conformance
is not evidence of authentication, redaction, durable commit or network delivery.
