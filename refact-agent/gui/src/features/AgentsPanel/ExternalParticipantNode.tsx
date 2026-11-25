import { ChevronRight, Network } from "lucide-react";
import { useState } from "react";

import { Badge, Button } from "../../components/ui";
import { useAppDispatch } from "../../hooks";
import {
  getExternalErrorMessage,
  useCancelExternalParticipantMutation,
  useDeleteExternalParticipantMutation,
  useGetExternalParticipantQuery,
  type ExternalActionAvailability,
  type ExternalParticipantRead,
  type ExternalSessionRead,
} from "../../services/refact/externalAgents";
import { push } from "../Pages/pagesSlice";
import { getExternalEvidenceView } from "../Settings/externalAgentStatus";
import styles from "./AgentsPanel.module.css";

type Confirmation =
  | { kind: "retire" }
  | { kind: "session"; session: ExternalSessionRead }
  | null;

function availabilityTitle(
  action: ExternalActionAvailability,
  actionLabel: string,
): string {
  return action.enabled
    ? actionLabel
    : action.reason ?? `${actionLabel} is not supported by this participant`;
}

export function ExternalParticipantNode({
  participant,
}: {
  participant: ExternalParticipantRead;
}) {
  const dispatch = useAppDispatch();
  const { data: details } = useGetExternalParticipantQuery(participant.id, {
    pollingInterval: 30_000,
    refetchOnFocus: true,
  });
  const [cancel, cancelState] = useCancelExternalParticipantMutation();
  const [retire, retireState] = useDeleteExternalParticipantMutation();
  const [expanded, setExpanded] = useState(false);
  const [confirmation, setConfirmation] = useState<Confirmation>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const current = details?.participant ?? participant;
  const sessions = details?.sessions ?? [];
  const deliveries = [...(details?.deliveries ?? [])].sort(
    (left, right) => right.updated_at - left.updated_at,
  );
  const evidence = details?.evidence ?? [];
  const evidenceView = getExternalEvidenceView(current, evidence);
  const pending = cancelState.isLoading || retireState.isLoading;
  const connection = current.retired
    ? { label: "Retired", tone: "muted" as const }
    : !current.enabled
      ? { label: "Disabled", tone: "muted" as const }
      : current.state === "failed"
        ? { label: "Connection failed", tone: "danger" as const }
        : current.protocol_version
          ? {
              label: `UHP ${current.protocol_version}`,
              tone: "accent" as const,
            }
          : { label: "Not connected", tone: "warning" as const };
  const checkCounts = evidence.reduce(
    (counts, record) => {
      for (const check of record.checks) {
        if (check.status === "pass") counts.passed += 1;
        counts.total += 1;
      }
      return counts;
    },
    { passed: 0, total: 0 },
  );

  const openDetails = () => {
    dispatch(push({ name: "external agents settings" }));
  };

  const confirmAction = async () => {
    if (!confirmation) return;
    setActionError(null);
    try {
      if (confirmation.kind === "retire") {
        await retire({ participantId: current.id }).unwrap();
      } else {
        await cancel({
          participantId: current.id,
          target: {
            kind: "session",
            id: confirmation.session.session_id,
          },
        }).unwrap();
      }
      setConfirmation(null);
    } catch (error) {
      setActionError(getExternalErrorMessage(error));
    }
  };

  return (
    <li
      className={styles.node}
      data-testid={`external-participant-${current.id}`}
      data-agent-chat-id="none"
    >
      <div className={styles.nodeRow}>
        <div className={styles.titleLine}>
          <button
            type="button"
            className={styles.expandChip}
            aria-label={`${expanded ? "Collapse" : "Expand"} ${current.name}`}
            aria-expanded={expanded}
            onClick={() => setExpanded((value) => !value)}
          >
            <ChevronRight
              className={styles.expandChevron}
              data-expanded={expanded}
              size={14}
            />
          </button>
          <Network aria-label="External advisor" size={15} />
          <button
            type="button"
            className={styles.nodeTitle}
            title={`${current.name} — external read-only advisor`}
            onClick={() => setExpanded((value) => !value)}
          >
            {current.name}
          </button>
          <Badge tone="accent">Read-only</Badge>
        </div>
        <div className={styles.metaLine}>
          <Badge tone={connection.tone}>{connection.label}</Badge>
          <Badge tone={evidenceView.tone}>{evidenceView.label}</Badge>
          <span title={`${current.base} / ${current.model}`}>
            {current.base} · {current.model}
          </span>
        </div>
        {expanded ? (
          <div className={styles.externalDetails}>
            <p className={styles.externalDescription}>
              {evidenceView.description}
            </p>
            {current.last_error ? (
              <p role="alert">{current.last_error.message}</p>
            ) : null}
            <Button size="sm" variant="soft" onClick={openDetails}>
              Participant details & evidence
            </Button>
            <section aria-label={`Sessions for ${current.name}`}>
              <h4>Sessions</h4>
              {sessions.length ? (
                sessions.map((session) => (
                  <div
                    className={styles.externalRecord}
                    key={session.session_id}
                  >
                    <span>{session.session_id}</span>
                    <Badge tone={session.busy ? "warning" : "muted"}>
                      {session.busy ? "busy" : "idle"}
                      {session.queued ? " · queued" : ""}
                      {session.last_terminal_status
                        ? ` · ${session.last_terminal_status}`
                        : ""}
                    </Badge>
                    <Button
                      size="sm"
                      variant="soft"
                      disabled={!current.actions.cancel.enabled || pending}
                      title={availabilityTitle(
                        current.actions.cancel,
                        "Cancel external session",
                      )}
                      onClick={() =>
                        setConfirmation({ kind: "session", session })
                      }
                    >
                      Cancel session
                    </Button>
                  </div>
                ))
              ) : (
                <p className={styles.externalDescription}>
                  No sessions recorded.
                </p>
              )}
            </section>
            <section aria-label={`Deliveries for ${current.name}`}>
              <h4>Deliveries</h4>
              {deliveries.length ? (
                deliveries.map((delivery) => (
                  <div
                    className={styles.externalRecord}
                    key={delivery.delivery_id}
                  >
                    <span>
                      {delivery.delivery_id} · attempt{" "}
                      {delivery.logical_attempt}
                    </span>
                    <Badge
                      tone={
                        delivery.source_freshness === "fresh"
                          ? "success"
                          : delivery.source_freshness === "stale"
                            ? "danger"
                            : "muted"
                      }
                    >
                      {delivery.request_state} · {delivery.source_freshness}
                    </Badge>
                    <Button
                      size="sm"
                      variant="soft"
                      disabled={!current.actions.retry.enabled}
                      title={availabilityTitle(
                        current.actions.retry,
                        "Retry with the original public payload in participant details",
                      )}
                      onClick={openDetails}
                    >
                      Retry delivery
                    </Button>
                  </div>
                ))
              ) : (
                <p className={styles.externalDescription}>
                  No deliveries recorded.
                </p>
              )}
            </section>
            <section aria-label={`Evidence for ${current.name}`}>
              <h4>Evidence</h4>
              <p className={styles.externalDescription}>
                {evidence.length} records · {checkCounts.passed}/
                {checkCounts.total} checks passed
              </p>
            </section>
            <div className={styles.externalActions}>
              <Button
                size="sm"
                variant="danger"
                disabled={!current.actions.retire.enabled || pending}
                title={availabilityTitle(
                  current.actions.retire,
                  current.harness_id
                    ? "Retire external harness"
                    : "Delete staged participant",
                )}
                onClick={() => setConfirmation({ kind: "retire" })}
              >
                {current.harness_id ? "Retire" : "Delete"}
              </Button>
            </div>
            {confirmation ? (
              <div
                role="group"
                aria-label="Confirm external participant action"
              >
                <p>
                  {confirmation.kind === "retire"
                    ? current.harness_id
                      ? "Retire this remote harness? Operational evidence is preserved."
                      : "Delete this staged participant?"
                    : `Cancel external session ${confirmation.session.session_id}?`}
                </p>
                <Button size="sm" onClick={() => setConfirmation(null)}>
                  Keep
                </Button>
                <Button
                  size="sm"
                  variant="danger"
                  loading={pending}
                  onClick={() => void confirmAction()}
                >
                  Confirm
                </Button>
              </div>
            ) : null}
            {actionError ? <p role="alert">{actionError}</p> : null}
          </div>
        ) : null}
      </div>
    </li>
  );
}
