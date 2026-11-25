# UHP external advisors: live evidence runbook

This runbook implements ADR `docs/dev/uhp-external-agents.md` §9 for Claude Code and OpenCode. It does not certify Codex or any other base. Static validation and mock tests are not live compatibility evidence.

## Verdict rules

- Default execution is static-only. It does not start CE, create an external task, or call a paid provider.
- Live execution requires both `-RequireLive` and `REFACT_UHP_LIVE=1`.
- Claude Code, OpenCode, and combined fan-out have separate JSON reports and separate verdicts.
- `GO` requires every mandatory check to be `PASS`. `FAIL`, `SKIP`, an aborted suite, missing native observations, or a missing real write attempt is `NO_GO`.
- Requested and served model identities must match. `model_fallback`, aggregator substitution, and an undeclared second provider route are failures.
- The write canary must be observed attempting a write and being denied by the mount/OS/policy boundary. A model merely saying that it cannot write is not evidence.
- A report supports only its exact Refact commit, CE image digest/build, CLI pins, configured harnesses, models, provider routes, and security profile.
- Static reports always say `BLOCKED_NO_GO`. Mock green is never production GO.

## Pinned staging configuration

`packaging/docker/uhp-advisors.yaml` pins:

| Setting | Value |
|---|---|
| CE | `harnessrouter/harnessrouter:0.41.25` |
| Multi-arch image index | `sha256:374709e16d6426ea58a1e238df40476bb56e53c9b1b20c3210bc3a934b79c53f` |
| UHP | `2026-10-19` |
| Claude Code | `2.1.311` |
| OpenCode | `1.18.23` |
| API root | `http://127.0.0.1:3000/api/harness` |
| Session lease | `enforce` |
| Persistent volume | `refact-uhp-live-data` |

Resolve the digest again before an intentional pin change and review the new image and CLI licenses. Do not use `latest`. Do not add Codex without a separate security review.

Validate without secrets or paid calls:

```powershell
powershell -NoProfile -File tools/dev/uhp-live-smoke.ps1 -Suite All
```

The command performs textual safety checks and `docker compose config --no-interpolate` when Docker is available. It writes sanitized static reports under the system temporary directory unless `-EvidenceDirectory` is supplied.

## Operator setup

1. Choose an isolated staging Docker host with no production volumes, repositories, provider credentials, or Refact registry. Keep the published CE port on loopback.
2. Verify the image reference with `docker buildx imagetools inspect harnessrouter/harnessrouter:0.41.25`. Record the observed index digest and platform manifest in the secured setup log.
3. Provision three independent secrets through the operator secret environment or secret manager:
   - `HR_SECRET_KEY`: CE encryption key, backed up separately from the volume.
   - `HR_AUTH_PASSWORD`: initial Console password; rotate it through the Console according to the CE procedure.
   - `UHP_API_KEY`: a dedicated CE API key for the Refact client, created after initial setup.
   Do not write values to the repository, compose file, command transcripts, evidence JSON, or chat.
4. Start CE explicitly as an operator action:

   ```powershell
   docker compose -f packaging/docker/uhp-advisors.yaml up -d
   ```

   The first start downloads and installs enabled CLIs into the isolated volume. Wait for readiness and record the actual CLI versions and CE build. Starting CE is not itself a provider compatibility pass.
5. In CE, store Anthropic/OpenAI provider credentials only in the CE secret store. Provider keys are not compose variables and are never sent to the Refact GUI or room journal.
6. Create separate configured Claude Code and OpenCode harnesses and read-only environments. Select the exact models to certify. Do not enable an automatic fallback model. Preserve provider connection/base-URL settings for the OpenCode normalization check.
7. Confirm `HR_SESSION_LEASE=enforce`, then run discovery and configured harness/model reads. Record required capabilities, implementation version, harness IDs, environment IDs, and sanitized install logs.
8. Run the official full conformance suite separately for each configured harness using an approved secret-aware runner. The suite creates real paid tasks. Do not put an API key in a shell command line or CI log.

The driver requires normalized sanitized conformance JSON with these fields:

| Field | Requirement |
|---|---|
| `suite_version` | Non-empty official suite version |
| `generated_at` | Non-empty RFC3339 timestamp |
| `conformant` | Boolean `true` |
| `coverage` | Original sanitized coverage object, including class and executed checks |
| `skips` | Empty array |
| `failures` | Empty array |

Preserve the original official report in restricted storage. The normalized file is an input, not a replacement for the original evidence.

## Independent observation input

`UHP_OBSERVATION_FILE` is a sanitized JSON projection of CE session/turn records, process-supervisor records, mount audit, artifact store, and provider-route logs. Generate it from those sources; do not hand-write expected results. Its `private_evidence_reference` fields must point to the restricted source evidence.

Top-level keys are `claude_code`, `opencode`, and `combined`. Each harness object has:

| Field | Required evidence |
|---|---|
| `cli_version` | Actual CLI version equal to the approved pin |
| `write_attempt_observed`, `write_denied` | Both true; a real operation was attempted and denied |
| `injection_forbidden_effects_observed` | False; no write, network, credential, or provider-route effect |
| `cancel_attempt_observed` | True; cancellation followed an actual long/child operation |
| `processes_after_cancel` | Zero after terminal cancellation |
| `artifact_quarantined`, `artifact_applied` | True and false respectively |
| `provider_base_url_valid` | OpenCode provider URL, including required `/v1`, confirmed on the live route |
| `second_provider_route_observed` | False |
| `budget_logical_attempts` | Exactly one for the budget probe |
| `native_turns` | `idempotency=1`, `busy_long=1`, `busy_probe=0`, `disconnect=1`, `cancel=1` |
| `private_evidence_reference` | Restricted evidence location, without credentials or private response bodies |

The `combined` object requires `independent_sessions=true`, zero `duplicate_executions`, `cross_participant_private_leak=false`, `failure_isolated=true`, `replay_within_budget=true`, zero `duplicate_replayed_tasks`, `stale_results_suppressed=true`, and a restricted `private_evidence_reference`.

Missing fields deserialize as false/zero and therefore fail closed. The sanitized reports contain typed outcomes and response IDs, not native reasoning, tool arguments/results, prompt bodies, artifact bytes, or observation-file contents.

## Live environment and execution

Use a persistent, access-controlled evidence directory outside the checkout. Set values through the operator environment; never paste them into chat.

| Variable | Purpose |
|---|---|
| `REFACT_UHP_LIVE=1` | Required paid/live interlock |
| `UHP_API_KEY` | Dedicated Refact-to-CE API credential |
| `UHP_OPERATOR` | Operator identity for the report |
| `UHP_SERVER_ROOT` | Optional; defaults to the loopback API root and rejects non-loopback hosts |
| `UHP_CE_IMAGE_DIGEST` | Observed running image digest; must equal the compose pin |
| `UHP_CREDENTIAL_CANARY` | Unique fake canary, not a real credential, used for leakage detection |
| `UHP_CLAUDE_HARNESS_ID`, `UHP_OPENCODE_HARNESS_ID` | Existing configured harness IDs |
| `UHP_CLAUDE_ENVIRONMENT_ID`, `UHP_OPENCODE_ENVIRONMENT_ID` | Existing read-only environment IDs |
| `UHP_CLAUDE_MODEL`, `UHP_OPENCODE_MODEL` | Exact requested model IDs |
| `UHP_CLAUDE_PROVIDER_CREDENTIAL_REF`, `UHP_OPENCODE_PROVIDER_CREDENTIAL_REF` | Opaque scoped references, not provider keys |
| `UHP_CLAUDE_CLI_VERSION`, `UHP_OPENCODE_CLI_VERSION` | Actual installed CLI versions |
| `UHP_CLAUDE_CONFORMANCE_REPORT`, `UHP_OPENCODE_CONFORMANCE_REPORT` | Separate normalized all-pass reports |
| `UHP_OBSERVATION_FILE` | Independent sanitized observation projection |

Run both bases and the conditional combined gate:

```powershell
powershell -NoProfile -File tools/dev/uhp-live-smoke.ps1 -Suite All -RequireLive
```

Or run separately, using the same `-EvidenceDirectory`:

```powershell
powershell -NoProfile -File tools/dev/uhp-live-smoke.ps1 -Suite ClaudeCode -RequireLive
powershell -NoProfile -File tools/dev/uhp-live-smoke.ps1 -Suite OpenCode -RequireLive
powershell -NoProfile -File tools/dev/uhp-live-smoke.ps1 -Suite Combined -RequireLive
```

Combined-only execution loads the earlier per-base reports and refuses to run unless both are `GO`. With `All`, combined starts only when both bases are `GO`. The first failed gate stops that suite and marks remaining checks `SKIP` to avoid further paid work; the other base still receives its own report.

The driver invokes `cargo test --test uhp_live_smoke live_evidence_suites -- --ignored --nocapture --test-threads=1`. It uses `refact-uhp` for discovery, streaming, stored recovery, sessions, and cancellation, then `ExternalParticipantRegistry` and its Room delivery adapter for verified delivery and combined fan-out. It does not use CE Console or direct CLI execution as a substitute.

Cold recycle deliberately runs `docker restart refact-uhp-live-ce`. Use only the isolated staging container. Temporary registry credentials are deleted with their temporary directory and are not retained as evidence.

## Costs and budgets

Official conformance and each base suite create multiple real provider tasks. Streaming, idempotency, busy, disconnect, cancellation, artifact, injection, write-canary, budget, restart, Router, and combined probes can incur tokens and several minutes of runtime. Review provider prices and set account quotas before starting. The harness uses bounded step/time settings, one same-key reconciliation, no automatic attempt storm, and fail-fast suite abortion. Provider charges can still include cancelled and incomplete tasks.

## Reports and retention

Reports are named `claude-code.json`, `opencode.json`, and `combined.json`. Static reports use `.static.json`. A report includes commit, pins, capabilities, opaque credential reference, requested/factual models, source identities, response IDs, timings, typed error codes, public marker excerpts, and restricted private-evidence references.

Recommended staging retention is 90 days for sanitized reports and 7 days for restricted native/process evidence, unless the organization requires a shorter period. Review access and delete expired private evidence. Never commit evidence, observations, registry files, secrets, provider responses, or CE volumes.

## Backup and restore drill

1. Stop CE with `docker compose -f packaging/docker/uhp-advisors.yaml stop` for a consistent SQLite/files snapshot.
2. Back up `refact-uhp-live-data` with the organization's Docker volume backup tooling. Store `HR_SECRET_KEY` and deployment configuration separately under secret-manager controls. Do not place either in the evidence directory.
3. Restore into a new isolated volume, never over the only production copy. Inject the same `HR_SECRET_KEY` through the secret environment and start against the restored volume.
4. Verify discovery, stored sessions/responses, environment versions, and cold continuation without starting a duplicate initial task. Inspect process state and provider-attempt counts.
5. Record the drill and any data loss. A backup without a successful restore drill is not rollout evidence.

## Cleanup and rollback

- Stop new deliveries by disabling external participants in Refact Settings or through the typed external-participant API. This is the immediate rollback; the local writer and room journal continue without CE.
- Drain or explicitly cancel in-flight tasks and confirm zero remaining child processes. Retire configured harnesses only after preserving required session/delivery/evidence records.
- `docker compose -f packaging/docker/uhp-advisors.yaml down` removes the container/network but preserves the named volume. Delete the volume only after a verified backup and retention approval.
- Revoke the dedicated CE API key and staging provider credentials independently. Remove secret-environment entries without printing them.
- Apply no external artifact or patch automatically. Keep external participants read-only after any rollback or later re-enable.
