# Design: first real browser-to-Rust E2E test

Status: proposed implementation design.

## Goal

Prove one complete user journey through the real stack:

`Playwright Chromium → static UI → real axum server → real SQLite → real filesystem → separate HTTP provider → UI`

This is deliberately one narrow smoke test, not a second copy of every existing fixture. The current browser suites remain fast UI contract tests; this test proves the browser can use the compiled application without API route mocks.

## Scenario

Use a temporary project containing `notes.md` with:

```text
alpha
beta
```

Configure the real `global` scope with:

- `root_path`: temporary project;
- `permission_mode`: `ask`;
- `diagnostics_cmd`: `grep -q gamma notes.md`;
- a random loopback server port;
- a temporary SQLite database;
- a synthetic bearer token.

Run a separate local OpenAI-compatible provider process with this scripted exchange:

1. user prompt: `Change beta to gamma in notes.md, then verify it`;
2. assistant tool call: `read(notes.md)`;
3. assistant tool call: `edit(notes.md, beta → gamma, using the returned hash anchor)`;
4. assistant tool call: `bash(printf 'verified\\n')` or rely on the configured diagnostics command;
5. assistant final answer: `Changed beta to gamma and verified it.`

The provider is allowed to be deterministic, but it must be a real process speaking HTTP. The Rust provider adapter, agent loop, permission gate, database, tools, and browser must all be real.

## Test setup

Create `tests/browser_e2e.cjs` and `scripts/verify_e2e.sh`.

The script should:

1. require `target/debug/harness`, Node, Playwright, and the resolved Chromium binary;
2. create a temporary directory and write the fixture project;
3. create a temporary SQLite path;
4. start the provider fixture as a child process on a random port;
5. start the compiled Rust binary as a child process with `HARNESS_DB`, `HARNESS_ADDR`, `HARNESS_BASE_URL`, `HARNESS_AUTH_TOKEN`, and `HARNESS_MODEL` set;
6. wait for a real health/readiness response, failing with captured stderr if startup fails;
7. launch Chromium and navigate to the real server URL;
8. perform the journey below;
9. inspect the real filesystem, SQLite rows, HTTP responses, and rendered page;
10. terminate both child processes in a `finally` block and remove the temporary directory.

Never use `page.route`, `route.fulfill`, browser-local API fixtures, or a mocked SQLite layer in this test.

## Browser journey

### 1. Authenticate and configure

- Open the actual `/` page.
- Enter the synthetic bearer token and unlock.
- Open Project & models.
- Configure the temporary project root and `ask` permission mode.
- Assert the browser receives the saved scope from the real `GET /scopes` response.
- Assert the setup banner disappears and the project is shown as configured.

### 2. Submit a real turn

- Enter the prompt and click Send.
- Assert the composer clears after durable admission.
- Assert the UI shows a running turn, not a fake locally generated answer.
- Assert the provider process received the request with the expected tool definitions and current user message.
- Assert the first request is durable before the final answer appears by checking the real `/chat/requests/{id}` state while generation is in progress.

### 3. Approve the real edit

- Wait for the real permission card.
- Assert it names `edit notes.md` and contains the real diff preview.
- Click Approve.
- Assert the UI receives the real `permission_resolved`, `file_changed`, and `tool_finished` activity events.
- Assert the permission card disappears or becomes resolved.

### 4. Verify the real result

- Wait for the final answer from the real generation stream.
- Assert the answer text is rendered from the persisted generation event.
- Assert `notes.md` on disk is exactly `alpha\\ngamma\\n`.
- Assert the real diagnostics command succeeded and its exit code is recorded.
- Assert the real database contains, for this request:
  - one captured receipt and one completed receipt;
  - ordered model and tool steps;
  - one pending permission that ended `approved`;
  - one applied `file_changes` row for `notes.md`;
  - activity events for permission request/resolution, file change, tool completion, and answer saved;
  - no duplicate edit or bash step.

## Required assertions

The test passes only if all layers agree:

- **Browser:** final answer, resolved permission, visible step rail, no page errors.
- **HTTP:** authenticated requests hit the real server; SSE frames have increasing database sequence IDs and the expected request ID.
- **Provider:** exactly one scripted provider request per expected model turn; tool results are echoed with the correct `tool_call_id`.
- **SQLite:** durable rows match the visible event order and terminal state.
- **Filesystem:** only the intended file changed, with the expected content and no temporary harness file left behind.
- **Security:** unauthenticated API access returns 401; the browser never stores the bearer token in localStorage or sessionStorage; a hostile text fixture is rendered as text if included.
- **Cleanup:** child processes exit and the temporary database/project are removed.

## Failure tests to add after the smoke test

Keep these separate so the first E2E test stays readable:

1. Deny the permission and prove the file stays unchanged while the model receives a denial.
2. Submit an edit with a stale anchor and prove no disk mutation.
3. Kill the Rust process during `bash`, restart it, and prove the step is `interrupted` with no second execution.
4. Disconnect/reconnect the activity stream and prove cursor resume without duplicate events.
5. Reload the browser during generation and prove the answer resumes without a second provider call.

## Verification command

`scripts/verify_e2e.sh` should run:

```sh
cargo build --locked
node tests/browser_e2e.cjs
```

The browser test should print a compact evidence summary:

```text
E2E passed: browser → axum → SQLite → filesystem → provider → browser
request: <id>
steps: 8, permissions: 1 approved, changes: 1 applied, duplicate side effects: 0
```

Do not put this in the mocked `verify_browser.sh` lane. Add it to `verify_release.sh` only after it is stable and its runtime prerequisites are explicit. Initially run it as a required opt-in command in CI/local release verification.

## Exit criteria

This design is complete when a clean machine can run one documented command that builds the actual binary, starts the actual server and provider, drives a real browser, and proves the same edit through UI, HTTP, database, and filesystem evidence. A green mocked browser test alone does not count.

## Known limitation

The deterministic provider proves transport and harness behavior, not model quality. That is intentional for this first test. A separate live-provider lane can later test model variance, but it must use disposable projects, strict budgets, and explicit nondeterministic reporting.
