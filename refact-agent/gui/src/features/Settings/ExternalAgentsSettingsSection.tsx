import React, { useId, useRef, useState } from "react";
import { Plus } from "lucide-react";

import { useAppDispatch, useAppSelector } from "../../hooks";
import {
  Badge,
  Button,
  EmptyState,
  Field,
  FieldSwitch,
  FieldText,
  FieldTextarea,
  Spinner,
} from "../../components/ui";
import {
  externalAgentsApi,
  getExternalErrorMessage,
  saveExternalParticipant,
  useCancelExternalParticipantMutation,
  useDeleteExternalParticipantMutation,
  useDiscoverExternalParticipantMutation,
  useGetExternalParticipantQuery,
  useGetExternalParticipantsQuery,
  useProvisionExternalParticipantMutation,
  useRetryExternalParticipantDeliveryMutation,
  useSetExternalParticipantEnabledMutation,
  useTestExternalParticipantMutation,
  useUpdateExternalParticipantRemoteMutation,
  type CreateExternalResponseRequest,
  type ExternalActionAvailability,
  type ExternalParticipantConfig,
  type ExternalParticipantDetails,
  type ExternalParticipantRead,
  type ExternalParticipantWrite,
} from "../../services/refact/externalAgents";
import { getExternalEvidenceView } from "./externalAgentStatus";
import { SettingsGroup, SettingsSection } from "./SettingsSection";
import styles from "./ExternalAgentsSettingsSection.module.css";

interface ParticipantDraft {
  id: string;
  name: string;
  base: string;
  harness_id: string;
  server_root: string;
  model: string;
  credential_profile_ref: string;
  environment_id: string;
  prompt: string;
  disabled_tools: string;
  max_step: string;
  timeout_seconds: string;
  enabled: boolean;
  cli_pin: string;
  server_image_digest: string;
}

const EMPTY_DRAFT: ParticipantDraft = {
  id: "",
  name: "",
  base: "",
  harness_id: "",
  server_root: "",
  model: "",
  credential_profile_ref: "",
  environment_id: "",
  prompt: "",
  disabled_tools: "",
  max_step: "",
  timeout_seconds: "",
  enabled: false,
  cli_pin: "",
  server_image_digest: "",
};

function draftFromParticipant(
  participant: ExternalParticipantRead,
): ParticipantDraft {
  return {
    id: participant.id,
    name: participant.name,
    base: participant.base,
    harness_id: participant.harness_id ?? "",
    server_root: participant.server_root,
    model: participant.model,
    credential_profile_ref: participant.credential_profile_ref,
    environment_id: participant.environment_id ?? "",
    prompt: participant.prompt,
    disabled_tools: participant.disabled_tools.join(", "),
    max_step: participant.max_step?.toString() ?? "",
    timeout_seconds: participant.timeout_seconds?.toString() ?? "",
    enabled: participant.enabled,
    cli_pin: participant.cli_pin,
    server_image_digest: participant.server_image_digest,
  };
}

function optionalPositiveInteger(raw: string): number | null | undefined {
  if (!raw.trim()) return null;
  if (!/^\d+$/.test(raw.trim())) return undefined;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

function draftToRequest(
  draft: ParticipantDraft,
  bearer: string,
): ExternalParticipantWrite | string {
  const required = [
    draft.id,
    draft.name,
    draft.base,
    draft.server_root,
    draft.model,
    draft.credential_profile_ref,
  ];
  if (required.some((value) => !value.trim())) {
    return "Complete all required participant fields.";
  }
  let server: URL;
  try {
    server = new URL(draft.server_root.trim());
  } catch {
    return "Server root must be a valid HTTP(S) URL.";
  }
  if (
    !["http:", "https:"].includes(server.protocol) ||
    server.username ||
    server.password ||
    server.search ||
    server.hash
  ) {
    return "Server root must be HTTP(S) without credentials, query, or fragment.";
  }
  const maxStep = optionalPositiveInteger(draft.max_step);
  const timeoutSeconds = optionalPositiveInteger(draft.timeout_seconds);
  if (maxStep === undefined || timeoutSeconds === undefined) {
    return "Maximum steps and timeout must be positive whole numbers.";
  }
  const disabledTools = draft.disabled_tools
    .split(",")
    .map((tool) => tool.trim())
    .filter(Boolean);
  if (disabledTools.some((tool) => /[\p{Cc}]/u.test(tool))) {
    return "Disabled tool names must be printable.";
  }
  const config: ExternalParticipantConfig = {
    id: draft.id.trim(),
    name: draft.name.trim(),
    base: draft.base.trim(),
    harness_id: draft.harness_id.trim() || null,
    server_root: draft.server_root.trim(),
    model: draft.model.trim(),
    credential_profile_ref: draft.credential_profile_ref.trim(),
    environment_id: draft.environment_id.trim() || null,
    prompt: draft.prompt,
    disabled_tools: disabledTools,
    max_step: maxStep,
    timeout_seconds: timeoutSeconds,
    enabled: draft.enabled,
    writes: false,
    cli_pin: draft.cli_pin.trim(),
    server_image_digest: draft.server_image_digest.trim(),
  };
  return bearer ? { ...config, credential: { bearer } } : config;
}

function isCreateResponseRequest(
  value: unknown,
): value is CreateExternalResponseRequest {
  if (!value || typeof value !== "object") return false;
  const request = value as Record<string, unknown>;
  if (
    typeof request.input !== "string" ||
    typeof request.stream !== "boolean" ||
    typeof request.store !== "boolean" ||
    !request.metadata ||
    typeof request.metadata !== "object"
  ) {
    return false;
  }
  const metadata = request.metadata as Record<string, unknown>;
  return typeof metadata.harness_id === "string";
}

interface ParticipantFormProps {
  participant?: ExternalParticipantRead;
  onCancel: () => void;
  onSaved: (details: ExternalParticipantDetails) => void;
}

function ParticipantForm({
  participant,
  onCancel,
  onSaved,
}: ParticipantFormProps) {
  const config = useAppSelector((state) => state.config);
  const dispatch = useAppDispatch();
  const idPrefix = useId();
  const credentialRef = useRef<HTMLInputElement>(null);
  const [draft, setDraft] = useState<ParticipantDraft>(() =>
    participant ? draftFromParticipant(participant) : EMPTY_DRAFT,
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const update = <Key extends keyof ParticipantDraft>(
    key: Key,
    value: ParticipantDraft[Key],
  ) => setDraft((current) => ({ ...current, [key]: value }));

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    const bearer = credentialRef.current?.value ?? "";
    if (credentialRef.current) credentialRef.current.value = "";
    const body = draftToRequest(draft, bearer);
    if (typeof body === "string") {
      setError(body);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const details = await saveExternalParticipant(
        { config },
        participant?.id ?? null,
        body,
      );
      dispatch(
        externalAgentsApi.util.invalidateTags([
          { type: "ExternalParticipant", id: "LIST" },
          { type: "ExternalParticipant", id: details.participant.id },
        ]),
      );
      onSaved(details);
    } catch (requestError) {
      setError(getExternalErrorMessage(requestError));
    } finally {
      setSaving(false);
    }
  };

  const fieldId = (name: string) => `${idPrefix}-${name}`;

  return (
    <SettingsGroup
      title={participant ? `Edit ${participant.name}` : "Add external advisor"}
      description="External participants are advisors only. Refact remains the sole writer."
    >
      <form
        className={styles.form}
        noValidate
        onSubmit={(event) => void submit(event)}
      >
        <div className={styles.formGrid}>
          <Field label="Name" htmlFor={fieldId("name")} required>
            <FieldText
              id={fieldId("name")}
              value={draft.name}
              onChange={(value) => update("name", value)}
              required
            />
          </Field>
          <Field label="Participant ID" htmlFor={fieldId("id")} required>
            <FieldText
              id={fieldId("id")}
              value={draft.id}
              onChange={(value) => update("id", value)}
              disabled={Boolean(participant)}
              required
            />
          </Field>
          <Field
            label="UHP server root"
            htmlFor={fieldId("server_root")}
            helper="HTTP(S) URL without embedded credentials, query, or fragment."
            required
          >
            <FieldText
              id={fieldId("server_root")}
              type="url"
              value={draft.server_root}
              onChange={(value) => update("server_root", value)}
              placeholder="https://uhp.example.com"
              required
            />
          </Field>
          <Field
            label="Base"
            htmlFor={fieldId("base")}
            helper="A server-defined base identifier; no vendor-specific presets are applied."
            required
          >
            <FieldText
              id={fieldId("base")}
              value={draft.base}
              onChange={(value) => update("base", value)}
              required
            />
          </Field>
          <Field label="Harness ID" htmlFor={fieldId("harness_id")}>
            <FieldText
              id={fieldId("harness_id")}
              value={draft.harness_id}
              onChange={(value) => update("harness_id", value)}
              placeholder="Provision one when supported"
            />
          </Field>
          <Field
            label="Model"
            htmlFor={fieldId("model")}
            helper="Use a model supported by the discovered server, then run Test model."
            required
          >
            <FieldText
              id={fieldId("model")}
              value={draft.model}
              onChange={(value) => update("model", value)}
              required
            />
          </Field>
          <Field
            label="Credential profile reference"
            htmlFor={fieldId("credential_profile_ref")}
            helper="An opaque reference, never the credential itself."
            required
          >
            <FieldText
              id={fieldId("credential_profile_ref")}
              value={draft.credential_profile_ref}
              onChange={(value) => update("credential_profile_ref", value)}
              placeholder="credential/external-1"
              required
            />
          </Field>
          <Field
            label={
              participant ? "Replace bearer credential" : "Bearer credential"
            }
            htmlFor={fieldId("bearer")}
            helper={
              participant
                ? "Write-only. Leave blank to keep the stored credential; it is never displayed."
                : "Write-only. It is cleared before the request completes and never enters Redux."
            }
          >
            <input
              ref={credentialRef}
              id={fieldId("bearer")}
              className={styles.input}
              type="password"
              autoComplete="new-password"
              data-testid="external-agent-bearer"
            />
          </Field>
          <Field label="Environment ID" htmlFor={fieldId("environment_id")}>
            <FieldText
              id={fieldId("environment_id")}
              value={draft.environment_id}
              onChange={(value) => update("environment_id", value)}
            />
          </Field>
          <Field
            label="CLI pin"
            htmlFor={fieldId("cli_pin")}
            helper="Recorded for exact live-evidence matching."
          >
            <FieldText
              id={fieldId("cli_pin")}
              value={draft.cli_pin}
              onChange={(value) => update("cli_pin", value)}
            />
          </Field>
          <Field
            label="Server image digest"
            htmlFor={fieldId("server_image_digest")}
          >
            <FieldText
              id={fieldId("server_image_digest")}
              value={draft.server_image_digest}
              onChange={(value) => update("server_image_digest", value)}
              placeholder="sha256:…"
            />
          </Field>
          <Field label="Maximum steps" htmlFor={fieldId("max_step")}>
            <FieldText
              id={fieldId("max_step")}
              type="number"
              min={1}
              step={1}
              value={draft.max_step}
              onChange={(value) => update("max_step", value)}
            />
          </Field>
          <Field label="Timeout seconds" htmlFor={fieldId("timeout_seconds")}>
            <FieldText
              id={fieldId("timeout_seconds")}
              type="number"
              min={1}
              step={1}
              value={draft.timeout_seconds}
              onChange={(value) => update("timeout_seconds", value)}
            />
          </Field>
          <Field
            className={styles.wide}
            label="Disabled tools"
            htmlFor={fieldId("disabled_tools")}
            helper="Comma-separated server tool names. Read-only enforcement does not rely on this list alone."
          >
            <FieldText
              id={fieldId("disabled_tools")}
              value={draft.disabled_tools}
              onChange={(value) => update("disabled_tools", value)}
              placeholder="shell, apply_patch"
            />
          </Field>
          <Field
            className={styles.wide}
            label="System prompt"
            htmlFor={fieldId("prompt")}
          >
            <FieldTextarea
              id={fieldId("prompt")}
              rows={4}
              value={draft.prompt}
              onChange={(value) => update("prompt", value)}
            />
          </Field>
          <Field label="Enabled" htmlFor={fieldId("enabled")}>
            <FieldSwitch
              id={fieldId("enabled")}
              aria-label="Enable external advisor"
              checked={draft.enabled}
              onChange={(value) => update("enabled", value)}
            />
          </Field>
          <Field label="Access">
            <Badge tone="accent">Read-only</Badge>
          </Field>
        </div>
        {error ? (
          <p className={styles.error} role="alert">
            {error}
          </p>
        ) : null}
        <div className={styles.formActions}>
          <Button type="button" onClick={onCancel} disabled={saving}>
            Cancel
          </Button>
          <Button type="submit" variant="primary" loading={saving}>
            {participant ? "Save changes" : "Create advisor"}
          </Button>
        </div>
      </form>
    </SettingsGroup>
  );
}

interface ActionButtonProps {
  availability: ExternalActionAvailability;
  children: React.ReactNode;
  loading?: boolean;
  onClick: () => void;
  variant?: "ghost" | "soft" | "primary" | "danger";
}

function ActionButton({
  availability,
  children,
  loading,
  onClick,
  variant = "soft",
}: ActionButtonProps) {
  return (
    <span className={styles.action}>
      <Button
        size="sm"
        variant={variant}
        disabled={!availability.enabled}
        loading={loading}
        title={!availability.enabled ? availability.reason : undefined}
        onClick={onClick}
      >
        {children}
      </Button>
      {!availability.enabled && availability.reason ? (
        <span className={styles.actionReason}>{availability.reason}</span>
      ) : null}
    </span>
  );
}

interface ParticipantCardProps {
  participant: ExternalParticipantRead;
  onEdit: (participant: ExternalParticipantRead) => void;
}

function ParticipantCard({ participant, onEdit }: ParticipantCardProps) {
  const { data: details } = useGetExternalParticipantQuery(participant.id);
  const [setEnabled] = useSetExternalParticipantEnabledMutation();
  const [discover] = useDiscoverExternalParticipantMutation();
  const [testModel] = useTestExternalParticipantMutation();
  const [provision] = useProvisionExternalParticipantMutation();
  const [remoteUpdate] = useUpdateExternalParticipantRemoteMutation();
  const [deleteParticipant] = useDeleteExternalParticipantMutation();
  const [cancel] = useCancelExternalParticipantMutation();
  const [retry] = useRetryExternalParticipantDeliveryMutation();
  const provisionKey = useRef({ revision: -1, value: "" });
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [retryDeliveryId, setRetryDeliveryId] = useState<string | null>(null);
  const [retryRequest, setRetryRequest] = useState("");
  const current = details?.participant ?? participant;
  const evidence = details?.evidence ?? [];
  const evidenceView = getExternalEvidenceView(
    current,
    evidence,
    pending !== null,
  );

  const runAction = async (key: string, operation: () => Promise<unknown>) => {
    setPending(key);
    setError(null);
    try {
      await operation();
      return true;
    } catch (requestError) {
      setError(getExternalErrorMessage(requestError));
      return false;
    } finally {
      setPending(null);
    }
  };

  const provisionParticipant = () => {
    if (provisionKey.current.revision !== current.config_revision) {
      provisionKey.current = {
        revision: current.config_revision,
        value: crypto.randomUUID(),
      };
    }
    return runAction("provision", () =>
      provision({
        participantId: current.id,
        idempotencyKey: provisionKey.current.value,
      }).unwrap(),
    );
  };

  const submitRetry = (deliveryId: string) => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(retryRequest) as unknown;
    } catch {
      setError("Original CreateResponse payload must be valid JSON.");
      return;
    }
    if (!isCreateResponseRequest(parsed)) {
      setError("Original CreateResponse payload is missing required fields.");
      return;
    }
    void runAction(`retry-${deliveryId}`, () =>
      retry({
        participantId: current.id,
        deliveryId,
        request: parsed,
      }).unwrap(),
    ).then((succeeded) => {
      if (succeeded) {
        setRetryDeliveryId(null);
        setRetryRequest("");
      }
    });
  };

  const deleteAvailability = current.harness_id
    ? current.actions.retire
    : { enabled: true, reason: undefined };

  return (
    <article
      className={styles.card}
      data-testid={`external-agent-${current.id}`}
    >
      <div className={styles.cardHeader}>
        <div className={styles.cardTitle}>
          <div className={styles.badges}>
            <h3>{current.name}</h3>
            <Badge tone={evidenceView.tone}>{evidenceView.label}</Badge>
            <Badge tone="accent">Read-only</Badge>
            {current.retired ? <Badge tone="muted">Retired</Badge> : null}
          </div>
          <p>
            {current.base} · {current.model}
            {current.harness_id ? ` · ${current.harness_id}` : ""}
          </p>
          <p className={styles.muted}>{evidenceView.description}</p>
        </div>
        <FieldSwitch
          aria-label={`Enable ${current.name}`}
          checked={current.enabled}
          disabled={pending !== null}
          onChange={(enabled) =>
            void runAction("enabled", () =>
              setEnabled({ participantId: current.id, enabled }).unwrap(),
            )
          }
        />
      </div>
      <dl className={styles.metadata}>
        <div>
          <dt>Server</dt>
          <dd>{current.server_implementation ?? "Not discovered"}</dd>
        </div>
        <div>
          <dt>Protocol</dt>
          <dd>{current.protocol_version ?? "Unknown"}</dd>
        </div>
        <div>
          <dt>Environment</dt>
          <dd>
            {current.environment_id ?? "Not configured"}
            {current.environment_version !== null
              ? ` · v${current.environment_version}`
              : ""}
          </dd>
        </div>
      </dl>
      <div
        className={styles.actions}
        aria-label={`Actions for ${current.name}`}
      >
        <ActionButton
          availability={current.actions.discover}
          loading={pending === "discover"}
          onClick={() =>
            void runAction("discover", () =>
              discover({ participantId: current.id }).unwrap(),
            )
          }
        >
          Discover
        </ActionButton>
        <ActionButton
          availability={current.actions.test}
          loading={pending === "test"}
          onClick={() =>
            void runAction("test", () =>
              testModel({ participantId: current.id }).unwrap(),
            )
          }
        >
          Test model
        </ActionButton>
        <ActionButton
          availability={current.actions.provision}
          loading={pending === "provision"}
          onClick={() => void provisionParticipant()}
        >
          Provision
        </ActionButton>
        <ActionButton
          availability={current.actions.update}
          loading={pending === "update"}
          onClick={() =>
            void runAction("update", () =>
              remoteUpdate({ participantId: current.id }).unwrap(),
            )
          }
        >
          Update remote
        </ActionButton>
        <Button size="sm" onClick={() => onEdit(current)}>
          Configure
        </Button>
        <ActionButton
          availability={deleteAvailability}
          variant="danger"
          loading={pending === "delete"}
          onClick={() => setConfirmDelete(true)}
        >
          {current.harness_id ? "Retire" : "Delete"}
        </ActionButton>
      </div>
      {confirmDelete ? (
        <div
          className={styles.confirm}
          role="group"
          aria-label="Confirm removal"
        >
          <span>
            {current.harness_id
              ? "Retire this remote harness? Operational evidence is preserved."
              : "Delete this staged participant and its credential reference?"}
          </span>
          <Button size="sm" onClick={() => setConfirmDelete(false)}>
            Keep
          </Button>
          <Button
            size="sm"
            variant="danger"
            onClick={() =>
              void runAction("delete", () =>
                deleteParticipant({ participantId: current.id }).unwrap(),
              ).then((succeeded) => {
                if (succeeded) setConfirmDelete(false);
              })
            }
          >
            Confirm {current.harness_id ? "retire" : "delete"}
          </Button>
        </div>
      ) : null}
      {error ? (
        <p className={styles.error} role="alert">
          {error}
        </p>
      ) : null}
      <details className={styles.details}>
        <summary>Sessions, deliveries, and evidence</summary>
        <div className={styles.detailsBody}>
          <section>
            <h4>Sessions</h4>
            {details?.sessions.length ? (
              details.sessions.map((session) => (
                <div className={styles.operationRow} key={session.session_id}>
                  <span>
                    {session.session_id} · {session.busy ? "busy" : "idle"}
                    {session.queued ? " · queued" : ""}
                  </span>
                  <div className={styles.inlineActions}>
                    {session.last_response_id ? (
                      <ActionButton
                        availability={current.actions.cancel}
                        onClick={() =>
                          void runAction(`cancel-${session.session_id}`, () =>
                            cancel({
                              participantId: current.id,
                              target: {
                                kind: "response",
                                id: session.last_response_id ?? "",
                              },
                            }).unwrap(),
                          )
                        }
                      >
                        Cancel response
                      </ActionButton>
                    ) : null}
                    <ActionButton
                      availability={current.actions.cancel}
                      onClick={() =>
                        void runAction(`cancel-${session.session_id}`, () =>
                          cancel({
                            participantId: current.id,
                            target: {
                              kind: "session",
                              id: session.session_id,
                            },
                          }).unwrap(),
                        )
                      }
                    >
                      Cancel session
                    </ActionButton>
                  </div>
                </div>
              ))
            ) : (
              <p className={styles.muted}>No external sessions recorded.</p>
            )}
          </section>
          <section>
            <h4>Deliveries</h4>
            {details?.deliveries.length ? (
              details.deliveries.map((delivery) => (
                <div key={delivery.delivery_id}>
                  <div className={styles.operationRow}>
                    <span>
                      {delivery.delivery_id} · {delivery.status} · attempt{" "}
                      {delivery.logical_attempt} · source{" "}
                      {delivery.source_freshness}
                    </span>
                    <ActionButton
                      availability={current.actions.retry}
                      onClick={() => {
                        setRetryDeliveryId(delivery.delivery_id);
                        setRetryRequest("");
                      }}
                    >
                      Retry delivery
                    </ActionButton>
                  </div>
                  {retryDeliveryId === delivery.delivery_id ? (
                    <div className={styles.retryForm}>
                      <Field
                        label={`Original CreateResponse JSON for ${delivery.delivery_id}`}
                        htmlFor={`retry-${current.id}-${delivery.delivery_id}`}
                        helper="Paste the exact original public payload so the server can verify its persisted fingerprint."
                      >
                        <FieldTextarea
                          id={`retry-${current.id}-${delivery.delivery_id}`}
                          aria-label={`Original CreateResponse JSON for ${delivery.delivery_id}`}
                          rows={5}
                          value={retryRequest}
                          onChange={setRetryRequest}
                        />
                      </Field>
                      <Button
                        size="sm"
                        variant="primary"
                        loading={pending === `retry-${delivery.delivery_id}`}
                        onClick={() => submitRetry(delivery.delivery_id)}
                      >
                        Verify and retry
                      </Button>
                    </div>
                  ) : null}
                </div>
              ))
            ) : (
              <p className={styles.muted}>No external deliveries recorded.</p>
            )}
          </section>
          <section>
            <h4>Evidence</h4>
            {evidence.length ? (
              evidence.map((record) => (
                <div className={styles.evidence} key={record.smoke_suite_id}>
                  <strong>{record.smoke_suite_id}</strong>
                  <span>
                    {record.server_version} · {record.cli_version} ·{" "}
                    {record.model_version}
                  </span>
                  <ul>
                    {record.checks.map((check) => (
                      <li key={check.name} data-status={check.status}>
                        {check.name}: {check.status}
                      </li>
                    ))}
                  </ul>
                </div>
              ))
            ) : (
              <p className={styles.muted}>
                No live evidence. Discovery and model tests are not production
                GO.
              </p>
            )}
          </section>
        </div>
      </details>
    </article>
  );
}

export const ExternalAgentsSettingsSection: React.FC = () => {
  const registry = useGetExternalParticipantsQuery(undefined);
  const [formParticipant, setFormParticipant] = useState<
    ExternalParticipantRead | null | undefined
  >(undefined);
  const formOpen = formParticipant !== undefined;

  return (
    <SettingsSection
      title="External agents"
      description="Explicitly configure read-only UHP advisors. Internal agents remain the default."
      width="wide"
      actions={
        <Button
          variant="primary"
          leftIcon={Plus}
          disabled={formOpen}
          onClick={() => setFormParticipant(null)}
        >
          Add external advisor
        </Button>
      }
    >
      {registry.isLoading ? <Spinner label="Loading external agents" /> : null}
      {registry.error ? (
        <p className={styles.error} role="alert">
          {getExternalErrorMessage(registry.error)}
        </p>
      ) : null}
      {formOpen ? (
        <ParticipantForm
          key={formParticipant?.id ?? "create"}
          participant={formParticipant ?? undefined}
          onCancel={() => setFormParticipant(undefined)}
          onSaved={() => setFormParticipant(undefined)}
        />
      ) : null}
      {registry.data?.mode === "internal_agents" ? (
        <EmptyState
          title="Internal agents are active by default"
          description="No external participants are configured. Adding one is an explicit opt-in and never enables an external writer."
        />
      ) : null}
      {registry.data?.participants.length ? (
        <div className={styles.list}>
          {registry.data.participants.map((participant) => (
            <ParticipantCard
              key={participant.id}
              participant={participant}
              onEdit={(selected) => setFormParticipant(selected)}
            />
          ))}
        </div>
      ) : null}
    </SettingsSection>
  );
};
