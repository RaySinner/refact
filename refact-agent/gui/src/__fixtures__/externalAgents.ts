import type {
  ExternalCapabilities,
  ExternalParticipantActions,
  ExternalParticipantDetails,
  ExternalParticipantRead,
  ExternalParticipantWrite,
} from "../services/refact/externalAgents";

export function makeExternalCapabilities(
  overrides: Partial<ExternalCapabilities> = {},
): ExternalCapabilities {
  return {
    streaming: true,
    sessions: true,
    cancellation: true,
    files_input: true,
    files_output: true,
    session_listing: true,
    harness_management: true,
    harness_retirement: true,
    input_history: true,
    input_requests: true,
    resume: true,
    session_sharing: false,
    session_branching: false,
    idempotency: true,
    harness_idempotency: true,
    plugins: false,
    environments: true,
    memories: false,
    ...overrides,
  };
}

export function makeExternalActions(
  overrides: Partial<ExternalParticipantActions> = {},
): ExternalParticipantActions {
  return {
    discover: { enabled: true },
    test: { enabled: true },
    provision: { enabled: true },
    update: { enabled: true },
    cancel: { enabled: true },
    retry: { enabled: true },
    retire: { enabled: true },
    ...overrides,
  };
}

export function makeExternalParticipant(
  overrides: Partial<ExternalParticipantRead> = {},
): ExternalParticipantRead {
  return {
    id: "participant-1",
    name: "Reviewer",
    base: "server-base",
    harness_id: null,
    server_root: "https://uhp.example.test",
    model: "model-1",
    credential_profile_ref: "credential/external-1",
    environment_id: null,
    prompt: "Review only",
    disabled_tools: ["shell"],
    max_step: 20,
    timeout_seconds: 300,
    enabled: true,
    writes: false,
    cli_pin: "1.2.3",
    server_image_digest: "sha256:abc",
    kind: "external",
    agent_chat_id: null,
    config_revision: 0,
    state: "unverified",
    capabilities: makeExternalCapabilities(),
    server_implementation: null,
    server_version: null,
    protocol_version: null,
    environment_version: null,
    retired: false,
    last_error: null,
    evidence_ids: [],
    actions: makeExternalActions(),
    ...overrides,
  };
}

export function makeExternalDetails(
  participant = makeExternalParticipant(),
  overrides: Partial<Omit<ExternalParticipantDetails, "participant">> = {},
): ExternalParticipantDetails {
  return {
    participant,
    sessions: [],
    deliveries: [],
    evidence: [],
    ...overrides,
  };
}

export function makeExternalWrite(
  overrides: Partial<ExternalParticipantWrite> = {},
): ExternalParticipantWrite {
  return {
    id: "participant-1",
    name: "Reviewer",
    base: "server-base",
    harness_id: null,
    server_root: "https://uhp.example.test",
    model: "model-1",
    credential_profile_ref: "credential/external-1",
    environment_id: null,
    prompt: "Review only",
    disabled_tools: ["shell"],
    max_step: 20,
    timeout_seconds: 300,
    enabled: true,
    writes: false,
    cli_pin: "1.2.3",
    server_image_digest: "sha256:abc",
    ...overrides,
  };
}
