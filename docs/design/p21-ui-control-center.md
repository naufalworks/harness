# P21 — UI control center and configuration coverage

Status: implementation in progress. P21-T01 through P21-T07 are implemented; later
tasks remain gated by the task ledger and final P21-T10 release verification.

## Goal

An owner can set up and operate Harness from its authenticated UI without editing
`.env`, guessing a model name, or manually discovering an absolute project path.
No UI control may imply a backend ability that does not exist; expose unsupported
capabilities honestly and give clear next actions. Keep P20 recovery and P19 external
history semantics unchanged.

## Verified baseline at P20 / deployed audit release

P21-T01 method-by-method classification is in
`docs/design/p21-ui-coverage.json`. The inventory is tested against both
`src/api/routes.rs` and `docs/api.yaml` and labels each operation as
`ui_supported`, `ui_partial`, or `api_only`; the last group includes
internal delivery endpoints that are not owner actions. This is a baseline,
not a claim that all routes already have UI.

### P21-T01 information architecture and interface states

Keep the existing **Chat**, **Inbox**, **History & privacy**, and
**Imports & jobs** views; retain the durable activity rail inside Chat.
Split the current **Project & models** view during P21 into **Projects &
folders**, **Providers & secrets**, and **Model roles**, with an owner-facing
feature-status/help entry that continues to name expert/API-only operations.
Do not expose recovery and exact-original archive actions as everyday UI
buttons until the owner/secret/recovery workflow is separately reviewed.

For each view and subview provide an accessible heading and current
scope/provider, keyboard-focusable controls with error/status announcements,
a non-misleading empty state, distinct loading/retry state, narrow-screen
overflow protection and a non-color-only status indicator. Forms must not
clear unsaved user inputs on fetch failures. Provider/folder selectors must
degrade to a labeled manual field only when the backend supports that
fallback. Preserve the current light/dark layout and inert rendering of
untrusted provider or filesystem text.

| Capability | Existing implementation | UI gap to close |
|---|---|---|
| Provider connection | One startup `HARNESS_BASE_URL` and `HARNESS_API_KEY` in `main.rs` / `MemoryAgents` | No add/edit/test/remove custom providers in UI; no live provider switch. |
| Model discovery | Owner-authenticated `GET /models` forwards to the configured provider's `/models` | "Show provider model names" prints text; no selectable models or per-provider cache/fallback. |
| Model roles | `GET/POST /config` saves main/extraction/verification names | Text-only inputs; no model validation or provider-aware selectors. |
| Project folders | `GET/POST /scopes/{scope}` validates and canonicalizes absolute root path | User must type an absolute server path; no server-side browse or safe suggested roots. |
| Activity | Durable model/tool/permission/verification steps and resumable activity; context receipts and incident graph | Present as partial rails/panels; unify status and action affordances; do **not** expose hidden model reasoning. |
| External history | P19 scoped session and event reads | Show missing transcript as unavailable unless client supplied it. |
| Other backend APIs | Routes and API schema include memory, history, archive, export/import, governance, diagnostics and provenance | Audit each operation against a usable UI entry or an explicitly documented expert/API-only route. |

The browser cannot select an arbitrary directory on the **remote server** via the
ordinary local `<input type=file>` picker; P21's picker must list only authorized
server-side directory names and retain a typed-path option.

## Provider configuration contract

**Implemented backend in P21-T02 and owner UI in P21-T04:** provider metadata/secret persistence,
selection, connection-test endpoint, environment fallback, provider-version
pinning on admitted turns, and the network/SSRF boundary below. The reviewed
Providers & secrets surface now lists profiles, identifies the selected provider,
and supports add/edit/test/select/delete without ever reading a stored key back
into the browser. P21-T03 owns the richer discovery/capability semantics; a
successful provider test proves only the selected provider's bounded model-list
discovery result and leaves generation/tools/streaming/usage explicitly untested.

Accept an owner-entered structured form **or** an optional bounded YAML/JSON paste
with identical validation. Example is deliberately a placeholder, not a real key:

```yaml
iamhc:
  baseUrl: https://api.iamhc.cn/v1
  apiKey: <enter-secret-in-UI>
  api: openai-completions
  discovery:
    type: proxy
```

- `iamhc` is a user-chosen unique provider ID, not a built-in endorsement.
- Initially support `api: openai-completions` as the **OpenAI-compatible
  chat-completions adapter**, with the exact paths/response shape validated against
  a mock provider. Do not imply every provider exposes identical tools, streaming,
  usage accounting or reasoning-summary fields.
- `discovery.type: proxy` requests an authenticated, server-side `GET
  {baseUrl}/models`; if unsupported/empty/error, label discovery unavailable and
  permit explicitly typed model IDs. Do not quietly use models from another provider.
- Show `baseUrl`, API type, discovery state, capabilities, last test result, and
  **key present / replacement required**, never the stored key or its prefix.
  Secrets must not be returned by GET, written to logs/activity/memory/backup
  exports, embedded in HTML, URL query strings, or browser local/session storage.
- Keep keys in an owner-only 0600 secret store under a 0700 directory outside Git,
  or protect them using a separately managed encryption key. Store metadata
  separately. Crash-safe atomic writes and explicit permission/rollback tests;
  no raw API keys in ordinary SQLite snapshots or reviewed exports.
- Reject plaintext external HTTP, loopback exceptions only when explicitly opted
  in, URL userinfo/query/fragment, redirects, DNS rebinding, forbidden IP ranges,
  and SSRF to metadata/internal endpoints. Revalidate resolved address on
  connection; enforce response byte/time limits, allowed API paths, and no arbitrary
  user-supplied request headers. A successfully fetched model list is not proof that
  the provider supports tools or chat completions.
- Provider edits/replacements require authenticated owner action, origin/session
  controls, explicit confirmation for deletion/rotation, and a bounded connection
  test. Errors must be redacted and say whether settings were saved or not.
- Never mutate a provider object used by an in-flight turn. Each admitted turn
  pins a validated provider configuration/version; switching the default affects
  subsequent turns. Recovery receipts retain provider ID/model/version, **not key**.
  New settings must not reset P20 memory state or replay external actions.
- Startup `.env` provider remains a migration-compatible fallback until the owner
  explicitly selects a saved provider; no breaking migration or default endpoint
  change. Harness is a provider **client**, not a public OpenAI proxy service.

### P21-T04 bounded provider editor

The owner may use structured fields or paste one bounded JSON/YAML provider object.
Paste parsing is deliberately a small exact-contract parser rather than a general
YAML interpreter: it accepts only provider ID, `baseUrl`, `apiKey`,
`api: openai-completions`, and `discovery.type: proxy`, with byte/field limits
and unknown/duplicate-field refusal. Parsing fills the form and clears the paste
buffer; it never saves or selects automatically. Stored keys remain write-only:
editing with a blank key omits `apiKey` so the backend keeps the existing secret,
and successful save, cancel, lock, and form reset clear secret inputs from the DOM.
Browser storage is reserved for non-secret UI/session state and never receives a
provider key.

## Model selection

### P21-T03 model-discovery backend

The selected provider now owns discovery end-to-end: Harness issues only its
bounded authenticated `GET /models`, validates/deduplicates exact model IDs,
and returns provider ID/version with explicit `available`, `empty`, or
`unavailable` discovery state. Network failure, timeout, authorization failure,
unsupported discovery, redirects, malformed responses, excessive lists and
invalid IDs are named without exposing upstream bodies. A failing selected
provider never causes discovery against another configured provider.

Discovery deliberately reports generation, tools, streaming and usage as
`untested`; a model-list response is not a capability probe. The existing
runtime tools-unsupported fallback and spend/effect guards remain authoritative
when real turns execute. The response also declares manual model IDs allowed,
which P21-T05 exposes as the explicit UI fallback when discovery is empty
or unavailable.

### P21-T05 provider-scoped model roles

The Model roles panel now loads exact IDs from the currently selected provider
and offers them as browser-native searchable suggestions for the main,
extraction, and verification fields. The currently configured value is never
silently replaced: switching providers refreshes discovery only, and a value
that is not in the refreshed list is labeled as a manual exact ID. Empty,
unauthorized, timeout/network and other unavailable discovery states keep the
typed value intact and explicitly retain manual fallback. A successful model
list remains discovery evidence only and the UI states that generation, tools,
streaming, and usage are not thereby proven.

- Load `/models` through the selected provider only. Show searchable selection
  for **main**, **extraction**, and **verification** roles; display exact model IDs,
  no invented capabilities, and a manually entered ID when discovery is unavailable.
- Distinguish configured, discovered, selectable, unavailable, and last-known
  models. Warn before deleting a provider used by active/default roles.
- Verify model permissions/tool and streaming compatibility with safe, bounded
  test fixtures; preserve the existing tools-unsupported fallback and spend guard.
- A provider returning a valid `data: [{id: ...}]` is sufficient to populate a
  dropdown, **not** sufficient to assert successful generation or tool support.

## Server folder chooser

P21-T06 implements the authenticated read-only `GET /project-directories`
backend. `HARNESS_PROJECT_BROWSE_ROOTS` is a comma-separated operator allowlist
and defaults to deny-all when empty. The endpoint exposes configured roots first,
then bounded directory-only pages with breadcrumbs, parent and cursor metadata.
Every requested path must be its own canonical spelling inside an allowlisted
root; `..`, aliases/symlinks, filesystem-root traversal, hidden/sensitive
directories and Harness data paths are refused. P21-T07 adds the reviewed UI:
the owner can browse approved server roots with breadcrumbs or type an absolute
path, cancellation restores the prior draft/permission mode, and selecting a
folder only copies it into the form until Save project settings explicitly runs
the existing canonical scope validator.

- Add an authenticated, read-only, bounded directory-list endpoint using a
  configurable list of allowed workspace roots; default to **deny** until an owner
  explicitly configures an allowed root. No traversal from `/` by default.
- Show directories only, bounded pagination, parent/breadcrumbs, clear symlink
  and permission-denied states. Never list hidden/secret paths, archive/key/data
  folders, or return arbitrary file content.
- Canonicalize and resolve symlinks at the backend, refuse escapes, `..`,
  denied roots, and racey path swaps at save/use time. Reuse existing scope
  path gate for the final selected or typed root.
- UI offers **Choose folder** and **Type absolute path** side by side. Preview
  resolved path, target scope, diagnostics and tool-permission mode before save.
  Cancel never mutates scope; scope changes never grant tools automatically.

## Work visibility: activity, not private thought

Display the known step phase, elapsed time, provider/model label, durable
tool-call status, permission requests, tool output previews subject to current
redaction, plan changes, verification and budget/cost estimates. Distinguish
waiting, generating, model responding, tool executing, permission required,
completed, cancelled, interrupted and failed. Reuse durable cursors for reload
and reconnect and never fabricate missing history.

No hidden chain-of-thought/raw internal reasoning. If a provider **explicitly**
supplies an approved, shareable reasoning-summary field, support it only through
an opt-in, schema-validated, redacted adapter and clear provenance; default UI
shows event-based progress. Keep context receipts clearly labeled as model
inputs rather than evidence of what the model thought.

## Feature coverage inventory and UI acceptance

Maintain a test-backed coverage matrix for every owner-facing operation in
`docs/api.yaml`: **UI supported**, **expert/API-only with rationale**, or
**unsupported/stub**. Organize setup, projects, providers/models, work/approvals,
memory/history, imports/exports, diagnostics and recovery in discoverable
navigation. Destructive actions need confirm/permissions; missing external
evidence must never be converted into a chat transcript. Preserve keyboard,
screen-reader, small-screen, dark-mode and inert rendering of untrusted text.
Browser E2E must test secret masking, reload, failed API requests, provider
switch mid-turn, empty `/models`, denied folder escapes, lost ACK, and restart.

## Release and acceptance

Each task has its own tests and review. On integration run locked Rust suite,
strict Clippy, formatting, Python/UI contract tests, mocked browser and real
browser-to-Axum-to-SQLite E2E, dependency/security scans, and the fail-closed
release gate. Create and verify a production recovery snapshot before
promotion; check schema, memory baseline, ready workers and current/previous
provider credentials after restart. P21 is complete **only when the ledger's
tasks are implemented, verified, pushed and deployed**; this document and a
passing prior release do not complete P21.
