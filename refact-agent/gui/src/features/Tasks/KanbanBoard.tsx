import React, { useCallback } from "react";
import classNames from "classnames";
import { FileText, Link2, Network, User } from "lucide-react";
import { Badge, Card, Icon } from "../../components/ui";
import { useAppDispatch } from "../../hooks";
import { push } from "../Pages/pagesSlice";
import {
  useGetExternalParticipantQuery,
  useGetExternalParticipantsQuery,
  type ExternalDeliveryRead,
  type ExternalParticipantRead,
} from "../../services/refact/externalAgents";
import { getExternalEvidenceView } from "../Settings/externalAgentStatus";
import type {
  TaskBoard,
  BoardCard,
  BoardColumn,
  TeamMember,
} from "../../services/refact/tasks";
import {
  memberStatusText,
  roleAccent,
  roleInitials,
  roleLabel,
} from "../../utils/roomRoleAccent";
import { BranchIcon } from "../Worktrees/BranchIcon";
import styles from "./Tasks.module.css";

type BadgeTone = React.ComponentProps<typeof Badge>["tone"];

const priorityTone = (priority: string): BadgeTone => {
  if (priority === "P0") return "danger";
  if (priority === "P1") return "warning";
  return "muted";
};

function compactWorktreeLabel(label: string): string {
  const normalized = label.replace(/[\\/]+$/, "");
  const parts = normalized.split(/[\\/]/).filter(Boolean);
  if (parts.length <= 2) return normalized || label;
  return parts.slice(-2).join("/");
}

function cardWorktreeLabel(card: BoardCard): string | null {
  const label =
    card.agent_worktree_name ?? card.agent_branch ?? card.agent_worktree;
  return label ? compactWorktreeLabel(label) : null;
}

function columnToneClass(columnId: string): string {
  if (columnId === "doing") return styles.kanbanColumnDoing;
  if (columnId === "done") return styles.kanbanColumnDone;
  if (columnId === "failed") return styles.kanbanColumnFailed;
  return styles.kanbanColumnPlanned;
}

interface RoomMemberProps {
  member: TeamMember;
  card: BoardCard;
  onAgentClick?: (card: BoardCard) => void;
}

function latestExternalDelivery(
  deliveries: ExternalDeliveryRead[],
): ExternalDeliveryRead | undefined {
  return [...deliveries].sort(
    (left, right) => right.updated_at - left.updated_at,
  )[0];
}

const ExternalRoomMemberBadge: React.FC<{
  member: TeamMember;
  participant: ExternalParticipantRead;
}> = ({ member, participant }) => {
  const dispatch = useAppDispatch();
  const { data: details } = useGetExternalParticipantQuery(participant.id);
  const current = details?.participant ?? participant;
  const latestDelivery = latestExternalDelivery(details?.deliveries ?? []);
  const freshness =
    member.source_freshness ?? latestDelivery?.source_freshness ?? "unknown";
  const evidence = getExternalEvidenceView(current, details?.evidence ?? []);
  const source = member.source_digest ?? latestDelivery?.source_digest;
  const revision =
    member.source_revision ?? latestDelivery?.source_revision ?? undefined;
  const title = [
    `External advisor: ${current.name}`,
    `Base/model: ${current.base} / ${current.model}`,
    `Evidence: ${evidence.label}`,
    `Source: ${freshness}`,
    revision ? `Revision: ${revision}` : null,
    source ? `Digest: ${source}` : null,
    current.last_error?.message ?? member.error?.message,
    "Read-only; opens participant details and evidence",
  ]
    .filter(Boolean)
    .join("\n");

  return (
    <button
      type="button"
      className={`${styles.roomMember} ${styles.roomMemberClickable}`}
      data-testid={`room-member-${member.role}`}
      data-external-participant={current.id}
      title={title}
      aria-label={`Open external advisor ${current.name}`}
      onClick={(event) => {
        event.preventDefault();
        event.stopPropagation();
        dispatch(push({ name: "external agents settings" }));
      }}
    >
      <Network aria-hidden="true" size={12} />
      <span className={styles.roomMemberRole}>{current.name}</span>
      <span className={styles.roomMemberStatus}>read-only</span>
      <span className={styles.roomMemberStatus}>{evidence.label}</span>
      <span className={styles.roomMemberStatus} data-freshness={freshness}>
        {freshness}
      </span>
    </button>
  );
};

const RoomMemberBadge: React.FC<RoomMemberProps> = ({
  member,
  card,
  onAgentClick,
}) => {
  const accent = roleAccent(member.role);
  const label = roleLabel(member.role);
  const status = memberStatusText(member);
  const targetChatId = member.agent_chat_id ?? card.agent_chat_id;
  const isClickable = Boolean(targetChatId);

  const details = [
    `Role: ${label}`,
    `Status: ${status}`,
    member.mandate ? `Mandate: ${member.mandate}` : null,
    member.tools && member.tools.length > 0
      ? `Tools: ${member.tools.join(", ")}`
      : null,
    isClickable ? "Click to open agent chat" : null,
  ]
    .filter(Boolean)
    .join("\n");

  const handleClick = (event: React.MouseEvent<HTMLButtonElement>) => {
    if (!targetChatId) return;
    event.preventDefault();
    event.stopPropagation();
    const cardWithChat =
      targetChatId === card.agent_chat_id
        ? card
        : { ...card, agent_chat_id: targetChatId };
    onAgentClick?.(cardWithChat);
  };

  return (
    <button
      type="button"
      className={classNames(
        styles.roomMember,
        isClickable && styles.roomMemberClickable,
      )}
      onClick={handleClick}
      title={details}
      data-testid={`room-member-${member.role}`}
      style={
        {
          "--room-member-stripe": accent.stripe,
          "--room-member-label": accent.label,
        } as React.CSSProperties
      }
    >
      <span className={styles.roomMemberInitials} aria-hidden="true">
        {roleInitials(member.role)}
      </span>
      <span className={styles.roomMemberRole}>{label}</span>
      <span className={styles.roomMemberStatus}>{status}</span>
      {member.writes === true ? (
        <span className={styles.roomMemberStatus}>writer</span>
      ) : member.writes === false ? (
        <span className={styles.roomMemberStatus}>advisor</span>
      ) : null}
    </button>
  );
};

interface RoomRosterProps {
  members: TeamMember[];
  card: BoardCard;
  onAgentClick?: (card: BoardCard) => void;
}

const RoomRoster: React.FC<RoomRosterProps> = ({
  members,
  card,
  onAgentClick,
}) => {
  const { data: registry, isLoading: registryLoading } =
    useGetExternalParticipantsQuery(undefined);
  return (
    <div className={styles.roomRoster} data-testid="room-roster">
      {members.map((member, index) => {
        const participantId = member.participant_id ?? member.agent_id;
        const participant = registry?.participants.find(
          (candidate) =>
            candidate.id === participantId &&
            (member.kind === "external" ||
              member.participant_id != null ||
              (member.agent_chat_id == null && member.writes === false)),
        );
        const explicitlyExternal =
          member.kind === "external" || member.participant_id != null;
        const possiblyExternal =
          explicitlyExternal ||
          (member.agent_chat_id == null && member.writes === false);
        const key =
          participant?.id ??
          member.agent_chat_id ??
          member.agent_id ??
          `${member.role}-${index}`;
        if (
          !participant &&
          possiblyExternal &&
          (explicitlyExternal || registryLoading)
        ) {
          return (
            <span
              key={key}
              className={styles.roomMember}
              data-testid={`room-member-${member.role}`}
              title="Loading external advisor details"
            >
              <Network aria-hidden="true" size={12} />
              <span className={styles.roomMemberRole}>
                {roleLabel(member.role)}
              </span>
              <span className={styles.roomMemberStatus}>external</span>
            </span>
          );
        }
        return participant ? (
          <ExternalRoomMemberBadge
            key={key}
            member={member}
            participant={participant}
          />
        ) : (
          <RoomMemberBadge
            key={key}
            member={member}
            card={card}
            onAgentClick={onAgentClick}
          />
        );
      })}
    </div>
  );
};

function roomMembersOf(card: BoardCard): TeamMember[] {
  return card.team_members ?? [];
}

interface KanbanCardProps {
  card: BoardCard;
  onClick?: (card: BoardCard) => void;
  onAgentClick?: (card: BoardCard) => void;
}

const KanbanCard: React.FC<KanbanCardProps> = ({
  card,
  onClick,
  onAgentClick,
}) => {
  const handleClick = useCallback(() => {
    onClick?.(card);
  }, [card, onClick]);

  const handleAgentClick = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>) => {
      if (!card.agent_chat_id) return;
      event.preventDefault();
      event.stopPropagation();
      onAgentClick?.(card);
    },
    [card, onAgentClick],
  );

  const hasAgent = card.assignee !== null;
  const hasDeps = card.depends_on.length > 0;
  const worktree = cardWorktreeLabel(card);
  const roomMembers = roomMembersOf(card);
  const isRoom = roomMembers.length > 1;

  return (
    <Card
      animated="rise"
      className={classNames(
        styles.kanbanCard,
        onClick && styles.kanbanCardClickable,
        onClick && "rf-pressable",
      )}
      interactive={Boolean(onClick)}
      onClick={handleClick}
    >
      <div className={styles.kanbanCardFrame}>
        <div className={styles.kanbanCardTopRow}>
          <Badge tone="muted">{card.id}</Badge>
          <Badge tone={priorityTone(card.priority)}>{card.priority}</Badge>
        </div>

        <span className={styles.kanbanCardTitle}>{card.title}</span>

        {isRoom && (
          <RoomRoster
            members={roomMembers}
            card={card}
            onAgentClick={onAgentClick}
          />
        )}

        <div className={styles.kanbanCardBadges}>
          {!isRoom &&
            hasAgent &&
            (card.agent_chat_id ? (
              <button
                type="button"
                className={styles.agentBadgeAction}
                title={`Agent: ${card.assignee}`}
                onClick={handleAgentClick}
              >
                <Icon icon={User} size="sm" tone="accent" /> Agent
              </button>
            ) : (
              <Badge tone="accent" title={`Agent: ${card.assignee}`}>
                <Icon icon={User} size="sm" tone="accent" /> Agent
              </Badge>
            ))}
          {worktree && (
            <Badge tone="success" title={`Worktree: ${worktree}`}>
              <BranchIcon /> {worktree}
            </Badge>
          )}
          {hasDeps && (
            <Badge
              tone="muted"
              title={`Depends on: ${card.depends_on.join(", ")}`}
            >
              <Icon icon={Link2} size="sm" tone="muted" />
              {card.depends_on.length}
            </Badge>
          )}
          {card.status_updates.length > 0 && (
            <Badge tone="muted">
              <Icon icon={FileText} size="sm" tone="muted" />
              {card.status_updates.length}
            </Badge>
          )}
        </div>
      </div>
    </Card>
  );
};

interface KanbanColumnProps {
  column: BoardColumn;
  cards: BoardCard[];
  onCardClick?: (card: BoardCard) => void;
  onAgentClick?: (card: BoardCard) => void;
}

const KanbanColumn: React.FC<KanbanColumnProps> = ({
  column,
  cards,
  onCardClick,
  onAgentClick,
}) => {
  return (
    <section
      className={classNames(
        styles.kanbanColumn,
        columnToneClass(column.id),
        "rf-enter-rise",
      )}
    >
      <header className={styles.kanbanColumnHeader}>
        <h3 className={styles.kanbanColumnTitle}>{column.title}</h3>
        <Badge tone="muted">{cards.length}</Badge>
      </header>
      <div className={classNames(styles.kanbanColumnContent, "rf-stagger")}>
        {cards.map((card) => (
          <KanbanCard
            key={card.id}
            card={card}
            onClick={onCardClick}
            onAgentClick={onAgentClick}
          />
        ))}
      </div>
    </section>
  );
};

interface KanbanBoardProps {
  board: TaskBoard;
  onCardClick?: (card: BoardCard) => void;
  onAgentClick?: (card: BoardCard) => void;
}

export const KanbanBoard: React.FC<KanbanBoardProps> = ({
  board,
  onCardClick,
  onAgentClick,
}) => {
  const getCardsForColumn = useCallback(
    (columnId: string): BoardCard[] => {
      return board.cards.filter((card) => card.column === columnId);
    },
    [board.cards],
  );

  return (
    <div className={classNames(styles.kanbanBoard, "rf-enter")}>
      {board.columns.map((column) => (
        <KanbanColumn
          key={column.id}
          column={column}
          cards={getCardsForColumn(column.id)}
          onCardClick={onCardClick}
          onAgentClick={onAgentClick}
        />
      ))}
    </div>
  );
};
