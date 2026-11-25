# Rooms, goals, and Buddy: implemented architecture

Status: implemented with mock/fake acceptance coverage. External Claude Code/OpenCode compatibility is not live-verified by these tests and is not production GO evidence.

## Boundaries

- `refact-room` owns the durable public journal, roster, exactly-one-writer rule, public projection, recipient fan-out, delivery identity, replay, and retry decisions.
- `refact-uhp` owns the versioned UHP client, protocol DTOs, response/session state, idempotency, recovery primitives, cancellation, and extraction of completed public assistant text.
- `refact-runtime-api` maps engine state, worktrees, snapshots, and external participant configuration to those libraries. It does not implement another UHP HTTP/SSE client or base-specific Claude Code/OpenCode branches.
- `refact-chat-api` owns goal projections, ledger replay, transition authorization, and pursuit arbitration. Provider adapters decide which goal messages reach the wire.
- `refact-buddy-core` owns bounded redacted supervision events, detectors, structured advice validation, policy, and the durable journal. Runtime delivery uses the shared pursuit gate.

The UHP protocol remains an application-to-harness boundary, not a room broadcast protocol. Refact does not implement a full UHP server.

## Rooms and privacy

A multi-member room has exactly one writer. The writer owns the canonical mergeable worktree and starts before advisors. Internal non-writing members receive a read-only `room_advisor` execution scope rooted at that same worktree, so they can inspect uncommitted changes without owning cleanup or a merge branch. Read-only is enforced through catalog and execution tool policy, not an OS sandbox.

External members have no synthetic local chat ID and cannot write. Each delivery exports a fresh immutable writer snapshot, including permitted uncommitted changes, and binds it to a source revision/digest and environment version. A response computed against an older snapshot is rejected as `refact.room.stale_source` instead of being published as current advice.

| Data | Writer/provider history | Internal advisor | External advisor | Public room journal |
|---|---|---|---|---|
| Completed assistant body | Yes | Yes, after the delivery boundary | Yes, in the UHP input | Yes, with the actual author |
| Reasoning/thinking | Yes | No | No | No |
| Tool calls and arguments | Yes | No from room peers | No | No |
| Tool results/output | Yes | No from room peers | No | No |
| Permitted current source | Local worktree | Live read-only view | Immutable read-only snapshot | Revision/digest metadata only |
| Credentials/private artifacts | According to local policy | No new access | Never serialized by the registry/read APIs | No |

Only completed non-empty assistant `output_text` is extracted from an external response. Advisor responses are directed to the original author to avoid advisor-to-advisor publication loops. Event IDs, delivery IDs, turn IDs, and response IDs make retries deduplicate at the journal boundary.

## External delivery and recovery

Each external participant has an independent configured harness and participant-scoped UHP session chain, even if a server reuses the same textual session ID for another participant. Requests contain only the public event and fresh snapshot metadata. The registry durably stores the request fingerprint, idempotency key, previous response ID, session/response state, and logical attempt.

- Same-key/same-body recovery retries the original request; a changed body is a conflict.
- A known stored response ID uses GET recovery. A non-terminal response remains busy/pending and does not publish a partial body.
- `session_busy` does not create a duplicate delivery or lose the pending request.
- `session_expired` permits one linked attempt with a new chain; it is not an unlimited retry loop.
- Response/session cancellation updates the matching participant deliveries and terminal session state.
- A failure for one advisor does not block another advisor's independent delivery.

External side effects are not exactly-once. Recovery is explicit and evidence remains sanitized. Discovery, provisioning, and mock tests never mark a participant live-verified; delivery requires matching non-empty all-pass live evidence.

## Goal state machine

A goal is optional. Its effective behavior is the product of three dimensions:

| Dimension | Values and effect |
|---|---|
| Resolution | `active`, `achieved`, `failed`, `cancelled`; only `active` can be pursued or remain an active prompt constraint |
| Execution | `idle`, `running`, `verifying`, `waiting_input`, `waiting_approval`, `paused`, `retry_scheduled`; only idle/retry-scheduled states permit scheduling |
| Autonomy | `on` permits authorized automatic turns; `off` suppresses them and makes Buddy visible-only |

Ownership and transfer are separate gates. An inactive or transferred goal cannot be pursued by its old chat. An explicit user message still starts a turn when autonomy is off and resets no-progress accounting. Autonomy off does not hide the active goal from the provider prompt.

User-only transitions include replace, clear, requirement changes, autonomy/budget changes, stop, and resume where legal. Model progress updates do not change requirements. Achieved/failed goals cannot be resumed. A cleared goal is inactive and cannot be resumed; a legacy manual stopped goal retains its compatibility resume path. Replacement is the explicit way to start different requirements after a terminal resolution.

Provider adapters use the latest goal generation. Completed, failed/cleared, paused, or transferred goals and their delta messages are omitted from subsequent provider-visible requests. Pursuit messages from the monitor, verifier rearm, silent-tail recovery, and Buddy share evidence deduplication and autonomy gates. Unlimited goals can become quiescent after repeated no-progress nudges without being marked failed.

The automatic verifier is transcript-based and tool-free. It can detect contradictions in supplied evidence, but it cannot independently execute commands or prove effects that are absent from the transcript. Infrastructure failure is not an UNMET verdict.

## Buddy supervision authority

Modes are `off`, `shadow`, and `advise`. Shadow detection records candidates without model arbitration or visible delivery. Advise validates a strict structured model reply against candidate-bound evidence IDs, confidence, and a concrete next step before creating an attributed `systemNotice`.

Detectors cover repeated tool loops, stale stagnation, possible off-goal work, and terminal provider disconnects. Fresh process heartbeats, active stream activity, waiting-for-input/approval states, and Buddy-origin events suppress false positives. Projections are bounded and redacted; raw reasoning, tool arguments/results, credentials, and full tool output are not arbiter inputs.

Policy keys combine chat, goal revision, trigger, and evidence hash. Journal reservation/finalization deduplicates retries and restarts, while cooldown and hourly budgets survive replay. Off-goal advice requires confirmation on new evidence. Buddy cannot impersonate a user, edit files, change goals/plans, invoke tools, or start its own retry loop. Autonomy-off delivery is visible-only; terminal provider failures never wake the provider.

## Compatibility and acceptance limits

Legacy room events/snapshots, goal status JSON, and Buddy settings/state use additive defaults. Missing goal dimensions derive from legacy status; missing Buddy supervision settings retain advise/80 defaults. Loading an old Buddy `state.json` does not create or overwrite the supervision journal.

`refact-agent/engine/tests/room_goal_buddy_e2e.rs` composes the real spawn-to-provider room regression with mock UHP delivery/recovery/cancellation, pursuit arbitration, OpenAI-wire goal visibility, Buddy shadow/advice/journal, and legacy-load regressions. These tests use no paid provider and no running external `refact-lsp`.

Remaining limits:

- Only external advisors use UHP snapshots; there is no external writer or automatic patch application.
- Internal advisor read-only policy is not a hostile-process OS isolation boundary.
- Mock UHP evidence does not certify a real HarnessRouter/CLI/model combination. Separate live smoke evidence is required for production GO.
- Public room history contains assistant bodies, not private reasoning or tool traces; this is intentional and limits what recipients can infer.
- The goal verifier cannot validate facts missing from its transcript and must not be treated as an independent test runner.
