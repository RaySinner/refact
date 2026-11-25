import { describe, expect, it } from "vitest";

import { makeExternalParticipant } from "../../__fixtures__/externalAgents";
import type { ExternalEvidenceRecord } from "../../services/refact/externalAgents";
import { getExternalEvidenceView } from "./externalAgentStatus";

function liveEvidence(): ExternalEvidenceRecord {
  return {
    smoke_suite_id: "live-smoke",
    server_version: "0.41.25",
    cli_version: "1.2.3",
    model_version: "model-1",
    checks: [{ name: "live_read_only_smoke", status: "pass" }],
    generated_at: 1,
    sanitized_report_location: "evidence/live-smoke.json",
  };
}

describe("external agent evidence states", () => {
  it("distinguishes disabled, checking, failed, and staging readiness", () => {
    expect(
      getExternalEvidenceView(makeExternalParticipant({ enabled: false }), [])
        .status,
    ).toBe("disabled");
    expect(
      getExternalEvidenceView(makeExternalParticipant(), [], true).status,
    ).toBe("checking");
    expect(
      getExternalEvidenceView(
        makeExternalParticipant({
          state: "failed",
          last_error: { code: "transport", message: "Operation failed" },
        }),
        [],
      ),
    ).toMatchObject({ status: "failed", description: "Operation failed" });
    expect(
      getExternalEvidenceView(
        makeExternalParticipant({
          harness_id: "harness-1",
          environment_id: "environment-1",
          environment_version: 1,
          protocol_version: "2026-10-19",
        }),
        [],
      ).status,
    ).toBe("staging-ready");
  });

  it("grants live-verified only for matching all-pass evidence", () => {
    const participant = makeExternalParticipant({
      state: "verified",
      server_version: "0.41.25",
    });
    expect(
      getExternalEvidenceView(participant, [liveEvidence()]),
    ).toMatchObject({ status: "live-verified", tone: "success" });
    expect(getExternalEvidenceView(participant, []).status).toBe("unverified");
    expect(
      getExternalEvidenceView(participant, [
        { ...liveEvidence(), model_version: "other-model" },
      ]).status,
    ).toBe("unverified");
    expect(
      getExternalEvidenceView(participant, [
        {
          ...liveEvidence(),
          checks: [{ name: "live_read_only_smoke", status: "skip" }],
        },
      ]).status,
    ).toBe("unverified");
  });
});
