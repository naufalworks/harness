# P22 · Full UI/UX Redesign & Session Experience

Status: planned. This phase redesigns the owner-facing Harness experience without weakening the
P21 security, provider, project-scope, memory, recovery, or activity guarantees.

## Product direction

Visual reference: the approved clean/light SaaS mockups discussed for Chat / Work, Session expired,
and Control Center. The redesign should feel modern, calm, informative, and fast rather than dense.
The UI should explain system state instead of forcing the owner to infer it from technical errors.

Core principles:

1. **Preserve trust boundaries.** Provider keys remain write-only. Master Harness access tokens are
   never persisted in localStorage/sessionStorage. Private chain-of-thought and `think` scratchpad
   remain hidden.
2. **Show useful work visibility.** Surface observable phases, plans, tool calls, permissions,
   verification, recovery, timing, usage, and provider/model routing. A Decision Trace may summarize
   recorded goals/actions/reasons/evidence, but must never fabricate or expose hidden reasoning.
3. **Re-auth without losing work.** Session expiry should return to a dedicated first screen that
   explains what happened, preserves safe tab-local restoration state, asks for the Harness access
   token again, and restores the same conversation without resending ambiguous work.
4. **One coherent design system.** Chat, Memory, History/Privacy, Imports/Jobs, Projects/Folders,
   Providers/Secrets, Model Roles, diagnostics and auth states must use the same spacing, type,
   components, status semantics, focus rules and responsive behavior.
5. **Reviewable and revertable.** P22 ships on a separate branch until explicitly accepted. P21
   remains the production baseline until the redesign passes release gates and is approved.

## Session / authentication UX

- Exchange the owner master token for a short-lived browser session as today.
- Prefer a refresh/sliding owner session while the user remains active, without persisting the
  master token.
- A terminal `401 unauthorized` from an expired browser session must transition to the
  re-authentication screen rather than leave the workspace in a broken state.
- Automatic re-auth is **not** manual Lock. It preserves safe UI restoration metadata: current
  conversation/session ID, scope, current view, pending request identity/cursor, and unsent draft
  where appropriate.
- Re-auth must never automatically resend a request. It checks the durable receipt and resumes or
  restores the recorded state.
- Manual Lock keeps the current stronger clearing behavior.
- Re-auth screen copy should explain: “Session expired”, “Your work is still saved”, and
  “Re-enter your Harness access token to continue.”
- The master token remains memory-only and is cleared immediately after session exchange.

## Workspace shell

Desktop target:
- modern left navigation with New chat, Chat / Work, Inbox / Memory, History & Privacy,
  Imports / Jobs, Projects / Folders, Providers / Secrets and Settings / Control Center;
- compact project/scope selector and connection health in the header;
- central work surface;
- optional right activity rail;
- recent chats available without overwhelming primary navigation.

Mobile/tablet:
- navigation and activity rail collapse into drawers;
- no horizontal overflow;
- composer and primary actions remain reachable with the on-screen keyboard.

## Chat / Work

- project overview banner when helpful;
- readable user/assistant message hierarchy;
- clear generated artifact/file cards;
- modern composer with attachments/project/model context;
- visible saved/failed/interrupted states that use truthful failure causes;
- no ambiguous “thinking” wording for private model state.

## Runtime Activity & Decision Trace

Right rail sections/tabs:
- **Activity** — current recorded phase, provider/model, elapsed time, usage, permissions,
  tool activity, recovery/interruption.
- **Decision Trace** — user-visible summary built only from explicit/recorded plan/action metadata:
  goal, plan step, chosen action, safe reason/provenance when recorded, evidence source,
  alternative/rejection only when explicitly recorded, next step, result.
- **Tools** — bounded safe previews for ordinary tools, duration/status, permission relationship.
- **Verification** — claims checked, verification status, file/result checks.

Never expose:
- hidden chain-of-thought,
- raw provider scratchpad,
- raw `think` tool content,
- inferred/fabricated “what the model was thinking”.

If a provider explicitly supplies a shareable reasoning-summary field, only show it through a
schema-validated/redacted adapter with provenance.

## Control Center

Reviewed surfaces:
- Projects & Folders
- Providers & Secrets
- Model Roles
- Diagnostics & Status
- General session/UI preferences when safe

Projects/Folders:
- typed server path plus approved-server-folder browser;
- breadcrumb and selected-path preview;
- explicit confirm/cancel;
- browsing never changes permissions or saves the scope implicitly.

Providers/Secrets:
- selected provider is obvious;
- add/edit/test/select/delete;
- secrets are password/write-only and never redisplayed;
- provider version/routing semantics remain unchanged.

Model Roles:
- Main / Extraction / Verification;
- selected provider association;
- discovered/manual labels;
- discovery status and capability status remain distinct.

## Other owner surfaces

Inbox/Memory, History/Privacy, Imports/Jobs and diagnostics must be redesigned using the same system,
not left as legacy screens.

## Error / empty / recovery states

Explicit designs and tests for:
- initial loading,
- no conversations,
- no memories,
- provider unavailable/unauthorized,
- model discovery empty/timeout,
- context setup failure,
- permission denied,
- interrupted/recovered turn,
- safe/unsafe retry,
- offline/reconnecting,
- session expired/re-auth,
- stale build,
- folder browse denied,
- invalid project scope.

## Accessibility / quality

- semantic headings/regions/labels;
- keyboard-complete navigation;
- deterministic focus on dialogs/drawers/re-auth;
- aria-live for changing status;
- no color-only information;
- dark mode parity;
- hostile text rendered only with textContent;
- long provider/model/path values wrap safely;
- responsive desktop/tablet/mobile;
- reduced-motion-friendly transitions.

## Release boundary

P22 is approved only when:
- mocked browser and real browser→Axum→SQLite→filesystem→provider E2E pass;
- re-auth expiry/restore does not resend ambiguous work;
- manual Lock still clears sensitive state;
- Decision Trace contains no private/raw reasoning;
- provider secret scans pass;
- Rust/Python/contracts/docs/supply-chain/release-evidence are green;
- verified recovery backup exists;
- exact CI-green commit is deployed and live verified;
- owner explicitly accepts the redesign.
