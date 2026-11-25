import {
  createApi,
  fetchBaseQuery,
  type FetchArgs,
  type FetchBaseQueryError,
} from "@reduxjs/toolkit/query/react";

import type { RootState } from "../../app/store";
import { buildApiUrlFromState } from "./apiUrl";

export type ExternalParticipantState =
  | "staged"
  | "unverified"
  | "verified"
  | "failed";

export interface ExternalCapabilities {
  streaming: boolean;
  sessions: boolean;
  cancellation: boolean;
  files_input: boolean;
  files_output: boolean;
  session_listing: boolean;
  harness_management: boolean;
  harness_retirement: boolean;
  input_history: boolean;
  input_requests: boolean;
  resume: boolean;
  session_sharing: boolean;
  session_branching: boolean;
  idempotency: boolean;
  harness_idempotency: boolean;
  plugins: boolean;
  environments: boolean;
  memories: boolean;
  [key: string]: boolean | undefined;
}

export interface ExternalParticipantConfig {
  id: string;
  name: string;
  base: string;
  harness_id: string | null;
  server_root: string;
  model: string;
  credential_profile_ref: string;
  environment_id: string | null;
  prompt: string;
  disabled_tools: string[];
  max_step: number | null;
  timeout_seconds: number | null;
  enabled: boolean;
  writes: false;
  cli_pin: string;
  server_image_digest: string;
}

export interface SanitizedExternalError {
  code: string;
  message: string;
}

export interface ExternalActionAvailability {
  enabled: boolean;
  reason?: string;
}

export type ExternalParticipantAction =
  | "discover"
  | "test"
  | "provision"
  | "update"
  | "cancel"
  | "retry"
  | "retire";

export type ExternalParticipantActions = Record<
  ExternalParticipantAction,
  ExternalActionAvailability
>;

export interface ExternalParticipantRead extends ExternalParticipantConfig {
  kind: "external";
  agent_chat_id: string | null;
  config_revision: number;
  state: ExternalParticipantState;
  capabilities: ExternalCapabilities;
  server_implementation: string | null;
  server_version: string | null;
  protocol_version: string | null;
  environment_version: number | null;
  retired: boolean;
  last_error: SanitizedExternalError | null;
  evidence_ids: string[];
  actions: ExternalParticipantActions;
}

export interface ExternalParticipantRegistryRead {
  mode: "internal_agents" | "external_participants";
  participants: ExternalParticipantRead[];
}

export interface ExternalSessionRead {
  participant_id: string;
  session_id: string;
  last_response_id: string | null;
  last_terminal_status: string | null;
  busy: boolean;
  queued: boolean;
  created_at: number;
  updated_at: number;
  replay_cursor: number | null;
}

export interface ExternalDeliveryRead {
  delivery_id: string;
  room_id: string;
  event_id: string;
  participant_id: string;
  status: "queued" | "busy" | "retrying" | "terminal";
  request_state:
    | "pending"
    | "in_progress"
    | "completed"
    | "failed"
    | "cancelled";
  logical_attempt: number;
  response_id: string | null;
  session_id: string | null;
  source_revision: string | null;
  source_digest: string | null;
  source_freshness: "unknown" | "fresh" | "stale";
  environment_version: number | null;
  error: SanitizedExternalError | null;
  created_at: number;
  updated_at: number;
}

export interface ExternalEvidenceRecord {
  smoke_suite_id: string;
  server_version: string;
  cli_version: string;
  model_version: string;
  checks: { name: string; status: "pass" | "fail" | "skip" }[];
  generated_at: number;
  sanitized_report_location: string;
}

export interface ExternalParticipantDetails {
  participant: ExternalParticipantRead;
  sessions: ExternalSessionRead[];
  deliveries: ExternalDeliveryRead[];
  evidence: ExternalEvidenceRecord[];
}

export interface ExternalParticipantWrite extends ExternalParticipantConfig {
  credential?: { bearer: string };
}

export interface ExternalDeleteResponse {
  participant_id: string;
  deleted: boolean;
  retired: boolean;
  retirement?: {
    participant_id: string;
    deleted: boolean;
    sessions: string;
    restorable_until: number | null;
  };
}

export interface CreateExternalResponseRequest {
  input: string;
  model?: string;
  metadata: { harness_id: string; environment?: string };
  stream: boolean;
  previous_response_id?: string;
  store: boolean;
  max_step?: number;
  timeout_seconds?: number;
}

export type ExternalAgentsState = { config: RootState["config"] };

type InnerBaseQuery = (
  arg: string | FetchArgs,
) => Promise<
  | { data: unknown; error?: undefined }
  | { error: FetchBaseQueryError; data?: undefined }
>;

function externalQueryFn<TArg, TResult>(
  buildRequest: (arg: TArg, state: ExternalAgentsState) => FetchArgs,
) {
  return async (
    arg: TArg,
    api: { getState: () => unknown },
    _options: object,
    baseQuery: InnerBaseQuery,
  ) => {
    const state = api.getState() as ExternalAgentsState;
    const result = await baseQuery(buildRequest(arg, state));
    if (result.error) return { error: result.error };
    return { data: result.data as TResult };
  };
}

function participantPath(participantId: string, suffix = ""): string {
  return `/v1/external-participants/${encodeURIComponent(
    participantId,
  )}${suffix}`;
}

function participantTags(participantId: string) {
  return [
    { type: "ExternalParticipant" as const, id: participantId },
    { type: "ExternalParticipant" as const, id: "LIST" },
  ];
}

export class ExternalParticipantRequestError extends Error {
  readonly code: string;

  constructor(error: SanitizedExternalError) {
    super(error.message);
    this.name = "ExternalParticipantRequestError";
    this.code = error.code;
  }
}

async function sanitizedResponseError(
  response: Response,
): Promise<ExternalParticipantRequestError> {
  let error: SanitizedExternalError = {
    code: "request_failed",
    message: "External agent request failed",
  };
  try {
    const data = (await response.json()) as unknown;
    if (data && typeof data === "object") {
      const record = data as Record<string, unknown>;
      if (
        typeof record.code === "string" &&
        typeof record.message === "string"
      ) {
        error = { code: record.code, message: record.message };
      }
    }
  } catch {
    return new ExternalParticipantRequestError(error);
  }
  return new ExternalParticipantRequestError(error);
}

export async function saveExternalParticipant(
  state: ExternalAgentsState,
  participantId: string | null,
  body: ExternalParticipantWrite,
): Promise<ExternalParticipantDetails> {
  const headers = new Headers({ "Content-Type": "application/json" });
  if (state.config.apiKey) {
    headers.set("Authorization", `Bearer ${state.config.apiKey}`);
  }
  const response = await fetch(
    buildApiUrlFromState(
      state,
      participantId === null
        ? "/v1/external-participants"
        : participantPath(participantId),
    ),
    {
      method: participantId === null ? "POST" : "PUT",
      headers,
      body: JSON.stringify(body),
    },
  );
  if (!response.ok) throw await sanitizedResponseError(response);
  return (await response.json()) as ExternalParticipantDetails;
}

export function getExternalErrorMessage(error: unknown): string {
  if (error instanceof ExternalParticipantRequestError) return error.message;
  if (error && typeof error === "object") {
    const record = error as Record<string, unknown>;
    if (record.data && typeof record.data === "object") {
      const data = record.data as Record<string, unknown>;
      if (typeof data.message === "string") return data.message;
    }
    if (typeof record.message === "string") return record.message;
  }
  return "External agent request failed";
}

export const externalAgentsApi = createApi({
  reducerPath: "externalAgentsApi",
  baseQuery: fetchBaseQuery({
    prepareHeaders: (headers, { getState }) => {
      const token = (getState() as RootState).config.apiKey;
      if (token) headers.set("Authorization", `Bearer ${token}`);
      return headers;
    },
  }),
  tagTypes: ["ExternalParticipant"],
  endpoints: (builder) => ({
    getExternalParticipants: builder.query<
      ExternalParticipantRegistryRead,
      undefined
    >({
      queryFn: externalQueryFn<undefined, ExternalParticipantRegistryRead>(
        () => ({ url: "/v1/external-participants" }),
      ),
      providesTags: (result) => [
        { type: "ExternalParticipant", id: "LIST" },
        ...(result?.participants.map((participant) => ({
          type: "ExternalParticipant" as const,
          id: participant.id,
        })) ?? []),
      ],
    }),
    getExternalParticipant: builder.query<ExternalParticipantDetails, string>({
      queryFn: externalQueryFn<string, ExternalParticipantDetails>(
        (participantId) => ({ url: participantPath(participantId) }),
      ),
      providesTags: (_result, _error, participantId) => [
        { type: "ExternalParticipant", id: participantId },
      ],
    }),
    setExternalParticipantEnabled: builder.mutation<
      ExternalParticipantDetails,
      { participantId: string; enabled: boolean }
    >({
      queryFn: externalQueryFn<
        { participantId: string; enabled: boolean },
        ExternalParticipantDetails
      >(({ participantId, enabled }) => ({
        url: participantPath(participantId, "/enabled"),
        method: "POST",
        body: { enabled },
      })),
      invalidatesTags: (_result, _error, { participantId }) =>
        participantTags(participantId),
    }),
    discoverExternalParticipant: builder.mutation<
      ExternalParticipantDetails,
      { participantId: string }
    >({
      queryFn: externalQueryFn<
        { participantId: string },
        ExternalParticipantDetails
      >(({ participantId }) => ({
        url: participantPath(participantId, "/discovery"),
        method: "POST",
      })),
      invalidatesTags: (_result, _error, { participantId }) =>
        participantTags(participantId),
    }),
    testExternalParticipant: builder.mutation<
      ExternalParticipantDetails,
      { participantId: string }
    >({
      queryFn: externalQueryFn<
        { participantId: string },
        ExternalParticipantDetails
      >(({ participantId }) => ({
        url: participantPath(participantId, "/test"),
        method: "POST",
      })),
      invalidatesTags: (_result, _error, { participantId }) =>
        participantTags(participantId),
    }),
    provisionExternalParticipant: builder.mutation<
      ExternalParticipantDetails,
      { participantId: string; idempotencyKey: string }
    >({
      queryFn: externalQueryFn<
        { participantId: string; idempotencyKey: string },
        ExternalParticipantDetails
      >(({ participantId, idempotencyKey }) => ({
        url: participantPath(participantId, "/provision"),
        method: "POST",
        body: { idempotency_key: idempotencyKey },
      })),
      invalidatesTags: (_result, _error, { participantId }) =>
        participantTags(participantId),
    }),
    updateExternalParticipantRemote: builder.mutation<
      ExternalParticipantDetails,
      { participantId: string }
    >({
      queryFn: externalQueryFn<
        { participantId: string },
        ExternalParticipantDetails
      >(({ participantId }) => ({
        url: participantPath(participantId, "/update"),
        method: "POST",
      })),
      invalidatesTags: (_result, _error, { participantId }) =>
        participantTags(participantId),
    }),
    deleteExternalParticipant: builder.mutation<
      ExternalDeleteResponse,
      { participantId: string }
    >({
      queryFn: externalQueryFn<
        { participantId: string },
        ExternalDeleteResponse
      >(({ participantId }) => ({
        url: participantPath(participantId),
        method: "DELETE",
      })),
      invalidatesTags: (_result, _error, { participantId }) =>
        participantTags(participantId),
    }),
    cancelExternalParticipant: builder.mutation<
      ExternalParticipantDetails,
      {
        participantId: string;
        target: { kind: "response" | "session"; id: string };
      }
    >({
      queryFn: externalQueryFn<
        {
          participantId: string;
          target: { kind: "response" | "session"; id: string };
        },
        ExternalParticipantDetails
      >(({ participantId, target }) => ({
        url: participantPath(participantId, "/cancel"),
        method: "POST",
        body: target,
      })),
      invalidatesTags: (_result, _error, { participantId }) =>
        participantTags(participantId),
    }),
    retryExternalParticipantDelivery: builder.mutation<
      ExternalParticipantDetails,
      {
        participantId: string;
        deliveryId: string;
        request: CreateExternalResponseRequest;
      }
    >({
      queryFn: externalQueryFn<
        {
          participantId: string;
          deliveryId: string;
          request: CreateExternalResponseRequest;
        },
        ExternalParticipantDetails
      >(({ participantId, deliveryId, request }) => ({
        url: participantPath(
          participantId,
          `/deliveries/${encodeURIComponent(deliveryId)}/retry`,
        ),
        method: "POST",
        body: request,
      })),
      invalidatesTags: (_result, _error, { participantId }) =>
        participantTags(participantId),
    }),
  }),
  refetchOnMountOrArgChange: true,
});

export const {
  useGetExternalParticipantsQuery,
  useGetExternalParticipantQuery,
  useSetExternalParticipantEnabledMutation,
  useDiscoverExternalParticipantMutation,
  useTestExternalParticipantMutation,
  useProvisionExternalParticipantMutation,
  useUpdateExternalParticipantRemoteMutation,
  useDeleteExternalParticipantMutation,
  useCancelExternalParticipantMutation,
  useRetryExternalParticipantDeliveryMutation,
} = externalAgentsApi;
