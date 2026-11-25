import type { BadgeTone } from "../../components/ui";
import type {
  ExternalEvidenceRecord,
  ExternalParticipantRead,
} from "../../services/refact/externalAgents";

export type ExternalEvidenceStatus =
  | "disabled"
  | "checking"
  | "staging-ready"
  | "live-verified"
  | "failed"
  | "unverified";

export interface ExternalEvidenceView {
  status: ExternalEvidenceStatus;
  label: string;
  description: string;
  tone: BadgeTone;
}

export function hasMatchingLiveEvidence(
  participant: ExternalParticipantRead,
  evidence: ExternalEvidenceRecord[],
): boolean {
  return evidence.some(
    (record) =>
      record.server_version === participant.server_version &&
      record.cli_version === participant.cli_pin &&
      record.model_version === participant.model &&
      record.checks.length > 0 &&
      record.checks.every((check) => check.status === "pass"),
  );
}

export function getExternalEvidenceView(
  participant: ExternalParticipantRead,
  evidence: ExternalEvidenceRecord[],
  checking = false,
): ExternalEvidenceView {
  if (!participant.enabled) {
    return {
      status: "disabled",
      label: "Disabled",
      description: "This external advisor is disabled.",
      tone: "muted",
    };
  }
  if (checking) {
    return {
      status: "checking",
      label: "Checking",
      description: "A server operation is in progress; evidence is unchanged.",
      tone: "accent",
    };
  }
  if (participant.state === "failed") {
    return {
      status: "failed",
      label: "Failed",
      description:
        participant.last_error?.message ??
        "The last external agent operation failed.",
      tone: "danger",
    };
  }
  if (
    participant.state === "verified" &&
    hasMatchingLiveEvidence(participant, evidence)
  ) {
    return {
      status: "live-verified",
      label: "Live verified",
      description: "Matching all-pass live evidence supports production use.",
      tone: "success",
    };
  }
  if (
    participant.protocol_version &&
    participant.harness_id &&
    participant.environment_id &&
    participant.environment_version !== null
  ) {
    return {
      status: "staging-ready",
      label: "Staging ready — not live verified",
      description:
        "Discovery and provisioning do not prove production readiness. Matching all-pass live evidence is still required.",
      tone: "warning",
    };
  }
  return {
    status: "unverified",
    label: "Unverified",
    description:
      "Discovery, model tests, and mock checks never grant production GO.",
    tone: "warning",
  };
}
