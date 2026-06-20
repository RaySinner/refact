import React, { useCallback, useState, useEffect, useMemo } from "react";
<<<<<<< HEAD
import {
  Flex,
  Box,
  Text,
  Button,
  Badge,
  Dialog,
  Checkbox,
  Tooltip,
  Tabs,
} from "@radix-ui/themes";
import {
  PlusIcon,
  Cross2Icon,
  ChevronDownIcon,
  FileTextIcon,
} from "@radix-ui/react-icons";
=======
import { Flex, Box, Text } from "@radix-ui/themes";
import {
  Badge,
  Button,
  Dialog,
  Icon,
  IconButton,
  Popover,
  StatusDot,
  Tabs,
  Tooltip,
} from "../../components/ui";
import { Checkbox } from "../../components/Checkbox";
import { PlusIcon, ChevronDownIcon } from "@radix-ui/react-icons";
import { FileText, GitBranch, ListChecks, Target, X } from "lucide-react";
>>>>>>> upstream/main
import { AgentStatusDot } from "./AgentStatusDot";
import { ScrollArea } from "../../components/ScrollArea";
import { ChatLoading } from "../../components/ChatContent/ChatLoading";
import { useAppDispatch, useAppSelector } from "../../hooks";
import { pop, popBackTo, push } from "../Pages/pagesSlice";
import { KanbanBoard } from "./KanbanBoard";
import {
  useGetTaskQuery,
  useGetBoardQuery,
  useListTaskTrajectoriesQuery,
  useCreatePlannerChatMutation,
  useDeletePlannerChatMutation,
  BoardCard,
  tasksApi,
} from "../../services/refact/tasks";
import { Markdown } from "../../components/Markdown";
<<<<<<< HEAD
import { CollapsePanel } from "../../components/shared/CollapsePanel";
import { ResizeDivider } from "../Dashboard/components/ResizeDivider/ResizeDivider";
=======
import { ModeMenuItem } from "../../components/ChatForm/ModeSelect";
import modeSelectStyles from "../../components/ChatForm/ModeSelect.module.css";
>>>>>>> upstream/main
import styles from "./Tasks.module.css";
import { Chat } from "../Chat";
import { selectConfig } from "../Config/configSlice";
import {
  createChatWithId,
  setThreadWorktree,
  switchToThread,
} from "../Chat/Thread";
import {
  openTask,
  addPlannerChat,
  removePlannerChat,
  restorePlannerChat,
  selectOpenTasksFromRoot,
  setTaskActiveChat,
  selectTaskActiveChat,
  updatePlannerChat,
  PlannerInfo,
} from "./tasksSlice";
import {
  selectBackgroundAgentsByThread,
<<<<<<< HEAD
  selectRuntimeById,
  selectThreadById,
} from "../Chat/Thread";
=======
  selectCurrentThreadId,
  selectRuntimeById,
  selectThreadById,
} from "../Chat/Thread";
import { getStatusFromSessionState } from "../../utils/sessionStatus";
import { useGetChatModesQuery } from "../../services/refact/chatModes";
>>>>>>> upstream/main
import { InternalLinkProvider } from "../../contexts/InternalLinkContext";
import { parseRefactLink } from "../../contexts/internalLinkUtils";
import { resolveChatLink } from "./internalLinkResolver";
import {
  useDeleteWorktreeMutation,
  useListWorktreesQuery,
  useOpenWorktreeMutation,
  type MergeWorktreeResponse,
  type BackgroundAgentSummary,
} from "../../services/refact";
import {
  sendUserMessage,
  updateChatParams,
} from "../../services/refact/chatCommands";
import { useCopyToClipboard } from "../../hooks/useCopyToClipboard";
import { useEventsBusForIDE } from "../../hooks/useEventBusForIDE";
import {
  BranchIcon,
  WorktreeDiffPanel,
  MergeWorktreeModal,
  WorktreeStatusBadge,
  buildWorktreeConflictPrompt,
  worktreeErrorText,
} from "../Worktrees";
import {
<<<<<<< HEAD
  loadTaskWorkspaceLayout,
  saveTaskWorkspaceLayout,
=======
  loadTaskWorkspaceTab,
  saveTaskWorkspaceTab,
  type TaskWorkspaceTab,
>>>>>>> upstream/main
} from "../../utils/chatUiPersistence";
import { MemoryInboxPanel } from "./TaskMemories/MemoryInboxPanel";
import { DocumentsPanel } from "./TaskDocuments/DocumentsPanel";
import { CardCommentsSection } from "./CardComments";
import {
  isActionableWorktree,
  resolveCardWorktree,
  worktreeLabel,
  type CardWorktreeTarget,
} from "./TaskWorkspaceWorktree";

type ActiveChat =
  | { type: "planner"; chatId: string }
  | { type: "agent"; cardId: string; chatId: string }
  | null;

const LEGACY_WORKTREE_TOOLTIP =
  "This worktree was created before the registry; recreate it via `restart_agent(mode=fresh)` to enable actions.";
const EMPTY_BACKGROUND_AGENTS: Record<string, BackgroundAgentSummary> =
  Object.freeze({});
<<<<<<< HEAD
=======
const EMPTY_LINKED_CARDS: string[] = [];
>>>>>>> upstream/main

interface PlannerPanelProps {
  plannerChats: PlannerInfo[];
  activeChat: ActiveChat;
<<<<<<< HEAD
=======
  linkedCardsByPlanner: Map<string, string[]>;
>>>>>>> upstream/main
  onSelectPlanner: (chatId: string) => void;
  onRemovePlanner: (chatId: string) => void;
}

interface PlannerItemProps {
  planner: PlannerInfo;
  isSelected: boolean;
<<<<<<< HEAD
=======
  linkedCardIds?: string[];
>>>>>>> upstream/main
  onSelect: () => void;
  onRemove: () => void;
}

function formatPlannerDate(dateStr: string): string {
  if (!dateStr) return "";
  try {
    const date = new Date(dateStr);
    return date.toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  } catch {
    return "";
  }
}

function formatAgentChatTitle(
  cardId: string | undefined,
  cardTitle: string,
): string {
  return cardId ? `Agent: ${cardId} ${cardTitle}` : `Agent: ${cardTitle}`;
}

type AgentRef = { chat_id: string };

function parsePlannerDeleteError(err: unknown): string {
  if (typeof err === "object" && err && "status" in err) {
    const e = err as {
      status: number;
      data?: { error?: string; agent_refs?: AgentRef[] };
    };
    if (e.status === 409 && e.data?.agent_refs) {
      const ids = e.data.agent_refs
        .map((r) => r.chat_id)
        .slice(0, 3)
        .join(", ");
      const extra =
        e.data.agent_refs.length > 3
          ? ` (+${e.data.agent_refs.length - 3} more)`
          : "";
      return `${e.data.error ?? "Conflict"}: ${ids}${extra}`;
    }
    if (e.data?.error) return e.data.error;
  }
  return "Unknown error";
}

<<<<<<< HEAD
const DEFAULT_BOARD_HEIGHT_PX = 180;
const MIN_BOARD_HEIGHT_PX = 80;
const MAX_BOARD_HEIGHT_RATIO = 0.6;

function clampBoardHeight(value: number, containerHeight?: number): number {
  const maxHeight =
    containerHeight && Number.isFinite(containerHeight) && containerHeight > 0
      ? Math.max(MIN_BOARD_HEIGHT_PX, containerHeight * MAX_BOARD_HEIGHT_RATIO)
      : 480;
  return Math.max(MIN_BOARD_HEIGHT_PX, Math.min(maxHeight, value));
}

function defaultTaskWorkspaceLayout() {
  return {
    chatExpanded: false,
    panelsExpanded: false,
    boardHeightPx: DEFAULT_BOARD_HEIGHT_PX,
  };
}

=======
>>>>>>> upstream/main
function sameWaitingCards(a?: string[], b?: string[]): boolean {
  if (a === b) return true;
  const left = a ?? [];
  const right = b ?? [];
  if (left.length !== right.length) return false;
  for (let i = 0; i < left.length; i += 1) {
    if (left[i] !== right[i]) return false;
  }
  return true;
}

<<<<<<< HEAD
export const PlannerItem: React.FC<PlannerItemProps> = ({
  planner,
  isSelected,
=======
const cardStatusTone = (
  column: string,
): React.ComponentProps<typeof Badge>["tone"] => {
  if (column === "done") return "success";
  if (column === "failed") return "danger";
  if (column === "doing") return "accent";
  return "muted";
};

const workspaceTabIndex = (tab: TaskWorkspaceTab): number => {
  if (tab === "chat") return 1;
  if (tab === "memories") return 2;
  if (tab === "documents") return 3;
  return 0;
};

const isTaskWorkspaceTab = (value: string): value is TaskWorkspaceTab =>
  value === "board" ||
  value === "chat" ||
  value === "memories" ||
  value === "documents";

export const PlannerItem: React.FC<PlannerItemProps> = ({
  planner,
  isSelected,
  linkedCardIds = EMPTY_LINKED_CARDS,
>>>>>>> upstream/main
  onSelect,
  onRemove,
}) => {
  const thread = useAppSelector((state) => selectThreadById(state, planner.id));
  const runtime = useAppSelector((state) =>
    selectRuntimeById(state, planner.id),
  );
  const title = thread?.title ?? planner.title;
  const hasGeneratedTitle =
    title && title !== "New Chat" && title.trim() !== "";
  const displayTitle = hasGeneratedTitle
    ? title
    : formatPlannerDate(planner.createdAt);

  const sessionState = runtime?.session_state ?? planner.sessionState;
<<<<<<< HEAD
=======
  const statusDot = getStatusFromSessionState(sessionState);
  const mode = planner.mode ?? thread?.mode;
  const showModeBadge = Boolean(mode) && mode !== "task_planner";
>>>>>>> upstream/main
  const isWaiting = sessionState === "waiting_user_input";
  const waitingCards = planner.waitingForCardIds ?? [];
  const showWaitingChips = isWaiting && waitingCards.length > 0;
  const visibleCards = waitingCards.slice(0, 5);
  const hiddenCount = Math.max(0, waitingCards.length - 5);
<<<<<<< HEAD

  return (
    <Box
      className={`${styles.panelItem} ${
=======
  const visibleLinkedCards = linkedCardIds.slice(0, 4);
  const hiddenLinkedCount = Math.max(0, linkedCardIds.length - 4);

  return (
    <Box
      className={`${styles.panelItem} rf-pressable ${
>>>>>>> upstream/main
        isSelected ? styles.panelItemSelected : ""
      }`}
      role="button"
      tabIndex={0}
<<<<<<< HEAD
      aria-label={`Open planner chat ${displayTitle}`}
=======
      aria-label={`Open chat ${displayTitle}`}
>>>>>>> upstream/main
      onClick={onSelect}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect();
        }
      }}
    >
<<<<<<< HEAD
      <Flex align="center" gap="1" className={styles.panelItemLead}>
        <Badge size="1" color="violet">
          <FileTextIcon />
        </Badge>
      </Flex>
      <Box className={styles.panelItemContent}>
        <Text size="1" className={styles.panelItemTitle}>
          {displayTitle}
        </Text>
      </Box>
      {showWaitingChips && (
        <Flex
          gap="1"
          wrap="nowrap"
          align="center"
          className={styles.plannerWaitingChips}
          data-testid={`planner-waiting-chips-${planner.id}`}
        >
          {visibleCards.map((cardId) => (
            <Badge
              key={cardId}
              size="1"
              color="amber"
              variant="soft"
              title={`Waiting for ${cardId}`}
            >
              {cardId}
            </Badge>
          ))}
          {hiddenCount > 0 && (
            <Text size="1" color="gray" className={styles.plannerWaitingMore}>
              … and {hiddenCount} more
            </Text>
          )}
        </Flex>
      )}
      <Tooltip content="Delete planner chat">
        <Button
          size="1"
          variant="ghost"
          color="gray"
          aria-label="Delete planner chat"
          onClick={(e) => {
            e.stopPropagation();
            onRemove();
          }}
        >
          <Cross2Icon />
        </Button>
=======
      <div className={styles.panelItemLead}>
        <StatusDot
          status={statusDot}
          size="medium"
          pulse={statusDot === "in_progress"}
        />
      </div>
      <Box className={styles.panelItemContent}>
        <div className={styles.panelItemTitleRow}>
          {showModeBadge && (
            <Badge tone="muted" className={styles.agentItemBadge}>
              {mode}
            </Badge>
          )}
          <Text size="1" className={styles.panelItemTitle}>
            {displayTitle}
          </Text>
        </div>
        {linkedCardIds.length > 0 && (
          <Flex
            gap="1"
            wrap="wrap"
            align="center"
            className={styles.plannerLinkedCards}
            data-testid={`planner-linked-cards-${planner.id}`}
          >
            {visibleLinkedCards.map((cardId) => (
              <Badge
                key={cardId}
                tone="muted"
                className={styles.agentItemBadge}
                title={`Spawned agent for ${cardId}`}
              >
                {cardId}
              </Badge>
            ))}
            {hiddenLinkedCount > 0 && (
              <Text size="1" color="gray" className={styles.plannerWaitingMore}>
                +{hiddenLinkedCount}
              </Text>
            )}
          </Flex>
        )}
        {showWaitingChips && (
          <Flex
            gap="1"
            wrap="nowrap"
            align="center"
            className={styles.plannerWaitingChips}
            data-testid={`planner-waiting-chips-${planner.id}`}
          >
            {visibleCards.map((cardId) => (
              <Badge
                key={cardId}
                tone="warning"
                title={`Waiting for ${cardId}`}
              >
                {cardId}
              </Badge>
            ))}
            {hiddenCount > 0 && (
              <Text size="1" color="gray" className={styles.plannerWaitingMore}>
                … and {hiddenCount} more
              </Text>
            )}
          </Flex>
        )}
      </Box>
      <Tooltip content="Delete chat">
        <span>
          <IconButton
            size="sm"
            variant="ghost"
            aria-label="Delete chat"
            icon={X}
            onClick={(e) => {
              e.stopPropagation();
              onRemove();
            }}
          />
        </span>
>>>>>>> upstream/main
      </Tooltip>
    </Box>
  );
};

const PlannerPanel: React.FC<PlannerPanelProps> = ({
  plannerChats,
  activeChat,
<<<<<<< HEAD
=======
  linkedCardsByPlanner,
>>>>>>> upstream/main
  onSelectPlanner,
  onRemovePlanner,
}) => {
  return (
    <Box className={styles.panelList}>
      <Box className={styles.panelContent}>
        {plannerChats.length === 0 ? (
<<<<<<< HEAD
          <Flex align="center" justify="center" style={{ flex: 1 }}>
            <Text size="1" color="gray">
              No planner chats yet
            </Text>
          </Flex>
        ) : (
          <ScrollArea scrollbars="vertical">
            <Flex direction="column" gap="1">
=======
          <Flex align="center" justify="center" className={styles.emptyState}>
            <Text size="1" color="gray">
              No chats yet
            </Text>
          </Flex>
        ) : (
          <ScrollArea
            className={styles.panelScrollArea}
            data-testid="planner-panel-scroll-owner"
            scrollbars="vertical"
          >
            <Flex direction="column" gap="1" className="rf-stagger">
>>>>>>> upstream/main
              {plannerChats.map((planner) => (
                <PlannerItem
                  key={planner.id}
                  planner={planner}
                  isSelected={
                    activeChat?.type === "planner" &&
                    activeChat.chatId === planner.id
                  }
<<<<<<< HEAD
=======
                  linkedCardIds={
                    linkedCardsByPlanner.get(planner.id) ?? EMPTY_LINKED_CARDS
                  }
>>>>>>> upstream/main
                  onSelect={() => onSelectPlanner(planner.id)}
                  onRemove={() => onRemovePlanner(planner.id)}
                />
              ))}
            </Flex>
          </ScrollArea>
        )}
      </Box>
    </Box>
  );
};

<<<<<<< HEAD
=======
type AgentChatStatus = "doing" | "done" | "failed";

interface AgentChatEntry {
  card: BoardCard;
  status: AgentChatStatus;
}

function agentChatEntries(cards: BoardCard[]): AgentChatEntry[] {
  const byColumn = (column: AgentChatStatus): AgentChatEntry[] =>
    cards
      .filter((card) => card.column === column && card.agent_chat_id)
      .map((card) => ({ card, status: column }));
  return [...byColumn("doing"), ...byColumn("done"), ...byColumn("failed")];
}

interface AgentItemProps {
  card: BoardCard;
  status: AgentChatStatus;
  isSelected: boolean;
  onSelect: () => void;
}

const AgentItem: React.FC<AgentItemProps> = ({
  card,
  status,
  isSelected,
  onSelect,
}) => {
  return (
    <Box
      className={`${styles.panelItem} rf-pressable ${
        isSelected ? styles.panelItemSelected : ""
      }`}
      role="button"
      tabIndex={0}
      aria-label={`Open agent chat ${card.id} ${card.title}`}
      onClick={onSelect}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect();
        }
      }}
    >
      <div className={styles.panelItemLead}>
        <AgentStatusDot status={status} size="medium" />
      </div>
      <Flex align="center" gap="1" className={styles.panelItemContent}>
        <Badge tone="muted" className={styles.agentItemBadge}>
          {card.id}
        </Badge>
        <Text size="1" className={styles.panelItemTitle}>
          {card.title}
        </Text>
      </Flex>
    </Box>
  );
};

>>>>>>> upstream/main
interface AgentsPanelProps {
  cards: BoardCard[];
  activeChat: ActiveChat;
  onSelectAgent: (cardId: string, chatId: string) => void;
}

const AgentsPanel: React.FC<AgentsPanelProps> = ({
  cards,
  activeChat,
  onSelectAgent,
}) => {
<<<<<<< HEAD
  const activeAgents = cards.filter(
    (c) => c.column === "doing" && c.agent_chat_id,
  );
  const completedAgents = cards.filter(
    (c) => c.column === "done" && c.agent_chat_id,
  );
  const failedAgents = cards.filter(
    (c) => c.column === "failed" && c.agent_chat_id,
  );

  const renderAgentItem = (
    card: BoardCard,
    status: "doing" | "done" | "failed",
  ) => {
    const isActive =
      activeChat?.type === "agent" && activeChat.cardId === card.id;
    return (
      <Box
        key={card.id}
        className={`${styles.panelItem} ${
          isActive ? styles.panelItemSelected : ""
        }`}
        onClick={() =>
          card.agent_chat_id && onSelectAgent(card.id, card.agent_chat_id)
        }
      >
        <div className={styles.panelItemLead}>
          <AgentStatusDot status={status} size="medium" />
        </div>
        <Flex align="center" gap="1" className={styles.panelItemContent}>
          <Badge size="1" color="gray" variant="soft">
            {card.id}
          </Badge>
          <Text size="1" className={styles.panelItemTitle}>
            {card.title}
          </Text>
        </Flex>
      </Box>
    );
  };
=======
  const agents = agentChatEntries(cards);
>>>>>>> upstream/main

  return (
    <Box className={styles.panelList}>
      <Box className={styles.panelContent}>
<<<<<<< HEAD
        {activeAgents.length === 0 &&
        completedAgents.length === 0 &&
        failedAgents.length === 0 ? (
          <Flex align="center" justify="center" style={{ flex: 1 }}>
            <Text size="1" color="gray">
              No agents yet
            </Text>
          </Flex>
        ) : (
          <ScrollArea scrollbars="vertical">
            <Flex direction="column" gap="1">
              {activeAgents.map((card) => renderAgentItem(card, "doing"))}
              {completedAgents.map((card) => renderAgentItem(card, "done"))}
              {failedAgents.map((card) => renderAgentItem(card, "failed"))}
=======
        {agents.length === 0 ? (
          <Flex align="center" justify="center" className={styles.emptyState}>
            <Text size="1" color="gray">
              No task agents yet
            </Text>
          </Flex>
        ) : (
          <ScrollArea
            className={styles.panelScrollArea}
            data-testid="agents-panel-scroll-owner"
            scrollbars="vertical"
          >
            <Flex direction="column" gap="1" className="rf-stagger">
              {agents.map(({ card, status }) => (
                <AgentItem
                  key={card.id}
                  card={card}
                  status={status}
                  isSelected={
                    activeChat?.type === "agent" &&
                    activeChat.cardId === card.id
                  }
                  onSelect={() =>
                    card.agent_chat_id &&
                    onSelectAgent(card.id, card.agent_chat_id)
                  }
                />
              ))}
>>>>>>> upstream/main
            </Flex>
          </ScrollArea>
        )}
      </Box>
    </Box>
  );
};

<<<<<<< HEAD
=======
interface BoardRailProps {
  plannerChats: PlannerInfo[];
  cards: BoardCard[];
  activeChat: ActiveChat;
  linkedCardsByPlanner: Map<string, string[]>;
  onSelectPlanner: (chatId: string) => void;
  onRemovePlanner: (chatId: string) => void;
  onSelectAgent: (cardId: string, chatId: string) => void;
}

const BoardRail: React.FC<BoardRailProps> = ({
  plannerChats,
  cards,
  activeChat,
  linkedCardsByPlanner,
  onSelectPlanner,
  onRemovePlanner,
  onSelectAgent,
}) => {
  const agentChats = cards.filter((card) => card.agent_chat_id);
  const doneAgentChats = agentChats.filter((card) => card.column === "done");

  return (
    <aside className={styles.boardRail} aria-label="Chats and task agents">
      <div className={styles.railGroupHeader}>
        <Text
          size="1"
          weight="bold"
          color="gray"
          className={styles.sectionHeaderLabel}
        >
          Chats
        </Text>
        <Flex align="center" gap="2" className={styles.sectionHeaderMeta}>
          <Badge tone="muted">{plannerChats.length}</Badge>
        </Flex>
      </div>
      <PlannerPanel
        plannerChats={plannerChats}
        activeChat={activeChat}
        linkedCardsByPlanner={linkedCardsByPlanner}
        onSelectPlanner={onSelectPlanner}
        onRemovePlanner={onRemovePlanner}
      />
      <div className={styles.railGroupHeader}>
        <Text
          size="1"
          weight="bold"
          color="gray"
          className={styles.sectionHeaderLabel}
        >
          Task Agents
        </Text>
        <Flex align="center" gap="2" className={styles.sectionHeaderMeta}>
          <Badge tone="muted">
            {doneAgentChats.length}/{agentChats.length}
          </Badge>
        </Flex>
      </div>
      <AgentsPanel
        cards={cards}
        activeChat={activeChat}
        onSelectAgent={onSelectAgent}
      />
    </aside>
  );
};

interface ChatSwitcherProps {
  label: string;
  plannerChats: PlannerInfo[];
  cards: BoardCard[];
  activeChat: ActiveChat;
  linkedCardsByPlanner: Map<string, string[]>;
  onSelectPlanner: (chatId: string) => void;
  onRemovePlanner: (chatId: string) => void;
  onSelectAgent: (cardId: string, chatId: string) => void;
}

const ChatSwitcher: React.FC<ChatSwitcherProps> = ({
  label,
  plannerChats,
  cards,
  activeChat,
  linkedCardsByPlanner,
  onSelectPlanner,
  onRemovePlanner,
  onSelectAgent,
}) => {
  const [open, setOpen] = useState(false);
  const agents = agentChatEntries(cards);
  const activeAgent =
    activeChat?.type === "agent"
      ? agents.find(({ card }) => card.id === activeChat.cardId)
      : undefined;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <Popover.Trigger asChild>
        <button
          type="button"
          className={styles.chatSwitcherTrigger}
          aria-label="Switch chat"
          title="Switch chat"
        >
          {activeAgent && (
            <AgentStatusDot status={activeAgent.status} size="small" />
          )}
          <Text size="1" className={styles.chatSwitcherLabel}>
            {label}
          </Text>
          <ChevronDownIcon className={styles.chatSwitcherChevron} />
        </button>
      </Popover.Trigger>
      <Popover.Content
        align="end"
        sideOffset={6}
        maxWidth="340px"
        className={styles.chatSwitcherContent}
      >
        <Text
          size="1"
          weight="bold"
          color="gray"
          className={styles.sectionHeaderLabel}
        >
          Chats
        </Text>
        {plannerChats.length === 0 ? (
          <Text size="1" color="gray">
            No chats yet
          </Text>
        ) : (
          <Flex direction="column" gap="1">
            {plannerChats.map((planner) => (
              <PlannerItem
                key={planner.id}
                planner={planner}
                isSelected={
                  activeChat?.type === "planner" &&
                  activeChat.chatId === planner.id
                }
                linkedCardIds={
                  linkedCardsByPlanner.get(planner.id) ?? EMPTY_LINKED_CARDS
                }
                onSelect={() => {
                  setOpen(false);
                  onSelectPlanner(planner.id);
                }}
                onRemove={() => onRemovePlanner(planner.id)}
              />
            ))}
          </Flex>
        )}
        {agents.length > 0 && (
          <>
            <Text
              size="1"
              weight="bold"
              color="gray"
              className={styles.sectionHeaderLabel}
            >
              Task Agents
            </Text>
            <Flex direction="column" gap="1">
              {agents.map(({ card, status }) => (
                <AgentItem
                  key={card.id}
                  card={card}
                  status={status}
                  isSelected={
                    activeChat?.type === "agent" &&
                    activeChat.cardId === card.id
                  }
                  onSelect={() => {
                    if (!card.agent_chat_id) return;
                    setOpen(false);
                    onSelectAgent(card.id, card.agent_chat_id);
                  }}
                />
              ))}
            </Flex>
          </>
        )}
      </Popover.Content>
    </Popover>
  );
};

interface NewChatModeButtonProps {
  disabled?: boolean;
  onCreate: (mode: string) => void;
}

const EXCLUDED_NEW_CHAT_MODES = new Set(["task_planner", "task_agent"]);

const NewChatModeButton: React.FC<NewChatModeButtonProps> = ({
  disabled,
  onCreate,
}) => {
  const [open, setOpen] = useState(false);
  const { data } = useGetChatModesQuery(undefined);
  const modes = useMemo(
    () =>
      (data?.modes ?? []).filter(
        (mode) => !EXCLUDED_NEW_CHAT_MODES.has(mode.id),
      ),
    [data],
  );

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <Popover.Trigger asChild>
        <button
          type="button"
          className={styles.headerActionButton}
          disabled={disabled}
          aria-label="New chat"
          title="New chat"
        >
          <PlusIcon />
          <Text size="1">Chat</Text>
          <ChevronDownIcon className={styles.chatSwitcherChevron} />
        </button>
      </Popover.Trigger>
      <Popover.Content
        align="end"
        sideOffset={6}
        maxWidth="320px"
        className={styles.chatSwitcherContent}
      >
        <Text
          size="1"
          weight="bold"
          color="gray"
          className={styles.sectionHeaderLabel}
        >
          New chat
        </Text>
        {modes.length === 0 ? (
          <Text size="1" color="gray">
            No modes available
          </Text>
        ) : (
          <div className={modeSelectStyles.modeList}>
            {modes.map((mode, index) => (
              <React.Fragment key={mode.id}>
                {index > 0 && <div className={modeSelectStyles.separator} />}
                <ModeMenuItem
                  mode={mode}
                  isSelected={false}
                  onSelect={() => {
                    setOpen(false);
                    onCreate(mode.id);
                  }}
                />
              </React.Fragment>
            ))}
          </div>
        )}
      </Popover.Content>
    </Popover>
  );
};

>>>>>>> upstream/main
interface CardDetailProps {
  taskId: string;
  card: BoardCard;
  worktree: CardWorktreeTarget | null;
  worktreeLabel: string | null;
  isWorktreeLoading: boolean;
  onClose: () => void;
  onInternalLink?: (url: string) => boolean;
  onViewDiff: (worktree: CardWorktreeTarget) => void;
  onMerge: (worktree: CardWorktreeTarget) => void;
  onOpenWorktree: (worktree: CardWorktreeTarget) => void;
  onDeleteWorktree: (worktree: CardWorktreeTarget) => void;
}

const CardDetail: React.FC<CardDetailProps> = ({
  taskId,
  card,
  worktree,
  worktreeLabel,
  isWorktreeLoading,
  onClose,
  onInternalLink,
  onViewDiff,
  onMerge,
  onOpenWorktree,
  onDeleteWorktree,
}) => {
  const worktreeActionsDisabled = !worktree || !isActionableWorktree(worktree);
  const worktreeActionsTooltip = worktree?.legacy
    ? LEGACY_WORKTREE_TOOLTIP
    : worktree?.stale
      ? "This worktree appears stale, missing, or deleted."
      : undefined;
  const invokeWorktreeAction = (
    action: (target: CardWorktreeTarget) => void,
  ) => {
    if (!worktree || worktreeActionsDisabled) return;
    action(worktree);
  };
  const wrapWorktreeAction = (button: React.ReactNode) =>
    worktreeActionsTooltip ? (
      <Tooltip content={worktreeActionsTooltip}>
        <span>{button}</span>
      </Tooltip>
    ) : (
      button
    );

  return (
<<<<<<< HEAD
    <Dialog.Content maxWidth="600px">
      <Flex direction="column" gap="3">
        <Flex justify="between" align="center">
          <Dialog.Title size="3" className={styles.cardDetailTitle}>
            <Badge size="1" color="gray" variant="soft" mr="2">
              {card.id}
            </Badge>
            {card.title}
          </Dialog.Title>
          <Badge
            color={
              card.column === "done"
                ? "green"
                : card.column === "failed"
                  ? "red"
                  : "blue"
            }
          >
            {card.column}
          </Badge>
        </Flex>

        {card.depends_on.length > 0 && (
          <Box>
            <Text size="2" weight="medium" color="gray">
              Dependencies
            </Text>
            <Flex gap="1" mt="1">
              {card.depends_on.map((dep) => (
                <Badge key={dep} size="1" variant="soft">
                  {dep}
                </Badge>
              ))}
            </Flex>
          </Box>
        )}

        {worktreeLabel && (
          <Box>
            <Text size="2" weight="medium" color="gray">
              Worktree
            </Text>
            <Flex direction="column" gap="2" mt="1">
              <Flex gap="2" align="center" wrap="wrap">
                <Badge size="1" color="green" variant="soft">
=======
    <Dialog.Content
      className={styles.cardDetailDialog}
      maxHeight="min(760px, calc(100dvh - var(--rf-space-5)))"
      maxWidth="720px"
    >
      <div className={styles.cardDetailRoot}>
        <div className={styles.cardDetailHeader}>
          <div className={styles.cardDetailTitleGroup}>
            <Badge tone="muted">{card.id}</Badge>
            <Dialog.Title className={styles.cardDetailTitle}>
              {card.title}
            </Dialog.Title>
          </div>
          <Badge tone={cardStatusTone(card.column)}>
            {card.column === "doing" ||
            card.column === "done" ||
            card.column === "failed" ? (
              <AgentStatusDot status={card.column} size="small" />
            ) : null}
            {card.column}
          </Badge>
        </div>

        <section className={styles.cardDetailMetaGrid}>
          <div className={styles.cardDetailMetaItem}>
            <span className={styles.cardDetailMetaLabel}>Priority</span>
            <Badge
              tone={
                card.priority === "P0"
                  ? "danger"
                  : card.priority === "P1"
                    ? "warning"
                    : "muted"
              }
            >
              {card.priority}
            </Badge>
          </div>
          {card.depends_on.length > 0 && (
            <div className={styles.cardDetailMetaItem}>
              <span className={styles.cardDetailMetaLabel}>Dependencies</span>
              <div className={styles.cardDetailChipRow}>
                {card.depends_on.map((dep) => (
                  <Badge key={dep} tone="muted">
                    {dep}
                  </Badge>
                ))}
              </div>
            </div>
          )}
          {worktreeLabel && (
            <div className={styles.cardDetailMetaItem}>
              <span className={styles.cardDetailMetaLabel}>Worktree</span>
              <div className={styles.cardDetailChipRow}>
                <Badge tone="success" title={`Worktree: ${worktreeLabel}`}>
>>>>>>> upstream/main
                  <BranchIcon /> {worktreeLabel}
                </Badge>
                {worktree?.record ?? worktree?.meta ? (
                  <WorktreeStatusBadge
                    worktree={worktree.meta ?? worktree.record?.meta}
                    record={worktree.record}
                  />
                ) : null}
                {worktree?.referenceCount && worktree.referenceCount > 1 ? (
<<<<<<< HEAD
                  <Badge size="1" color="amber" variant="soft">
                    shared by {worktree.referenceCount}
                  </Badge>
                ) : null}
              </Flex>
=======
                  <Badge tone="warning">
                    shared by {worktree.referenceCount}
                  </Badge>
                ) : null}
              </div>
            </div>
          )}
        </section>

        {worktreeLabel && (
          <section className={styles.cardDetailSectionBlock}>
            <div className={styles.cardDetailSectionHeader}>
              <Icon icon={GitBranch} size="sm" tone="muted" />
              <Text size="2" weight="medium">
                Worktree actions
              </Text>
            </div>
            <div className={styles.cardDetailWorktreeBody}>
>>>>>>> upstream/main
              {isWorktreeLoading && (
                <Text size="1" color="gray">
                  Loading worktree metadata...
                </Text>
              )}
              {!isWorktreeLoading && !worktree && (
                <Text size="1" color="gray">
                  Worktree metadata is unavailable or stale.
                </Text>
              )}
              {worktree?.stale && (
                <Text size="1" color="amber">
                  This worktree appears stale, missing, or deleted.
                </Text>
              )}
              {worktree?.legacy && (
                <Text size="1" color="amber">
                  Legacy / unregistered worktree
                </Text>
              )}
<<<<<<< HEAD
              <Flex gap="2" wrap="wrap">
                {wrapWorktreeAction(
                  <Button
                    type="button"
                    size="1"
=======
              <div className={styles.cardDetailActions}>
                {wrapWorktreeAction(
                  <Button
                    type="button"
                    size="sm"
>>>>>>> upstream/main
                    variant="soft"
                    disabled={worktreeActionsDisabled}
                    title={worktreeActionsTooltip}
                    onClick={() => invokeWorktreeAction(onViewDiff)}
                  >
                    View Diff
                  </Button>,
                )}
                {wrapWorktreeAction(
                  <Button
                    type="button"
<<<<<<< HEAD
                    size="1"
=======
                    size="sm"
>>>>>>> upstream/main
                    variant="soft"
                    disabled={worktreeActionsDisabled}
                    title={worktreeActionsTooltip}
                    onClick={() => invokeWorktreeAction(onMerge)}
                  >
                    Merge
                  </Button>,
                )}
                {wrapWorktreeAction(
                  <Button
                    type="button"
<<<<<<< HEAD
                    size="1"
                    variant="soft"
                    color="gray"
=======
                    size="sm"
                    variant="soft"
>>>>>>> upstream/main
                    disabled={worktreeActionsDisabled}
                    title={worktreeActionsTooltip}
                    onClick={() => invokeWorktreeAction(onOpenWorktree)}
                  >
                    Open
                  </Button>,
                )}
                {wrapWorktreeAction(
                  <Button
                    type="button"
<<<<<<< HEAD
                    size="1"
                    variant="soft"
                    color="red"
=======
                    size="sm"
                    variant="danger"
>>>>>>> upstream/main
                    disabled={worktreeActionsDisabled}
                    title={worktreeActionsTooltip}
                    onClick={() => invokeWorktreeAction(onDeleteWorktree)}
                  >
                    Discard/Delete
                  </Button>,
                )}
<<<<<<< HEAD
              </Flex>
            </Flex>
          </Box>
        )}

        {card.instructions && (
          <Box>
            <Text size="2" weight="medium" color="gray">
              Instructions
            </Text>
=======
              </div>
            </div>
          </section>
        )}

        <section className={styles.cardDetailSectionBlock}>
          <div className={styles.cardDetailSectionHeader}>
            <Icon icon={Target} size="sm" tone="muted" />
            <Text size="2" weight="medium">
              Goal
            </Text>
          </div>
          <Box className={styles.cardDetailSection}>{card.title}</Box>
        </section>

        {card.target_files.length > 0 && (
          <section className={styles.cardDetailSectionBlock}>
            <div className={styles.cardDetailSectionHeader}>
              <Icon icon={FileText} size="sm" tone="muted" />
              <Text size="2" weight="medium">
                Files
              </Text>
            </div>
            <Box className={styles.cardDetailSection}>
              <div className={styles.cardDetailFileList}>
                {card.target_files.map((file) => (
                  <Badge key={file} tone="muted">
                    {file}
                  </Badge>
                ))}
              </div>
            </Box>
          </section>
        )}

        {card.instructions && (
          <section className={styles.cardDetailSectionBlock}>
            <div className={styles.cardDetailSectionHeader}>
              <Icon icon={ListChecks} size="sm" tone="muted" />
              <Text size="2" weight="medium">
                Instructions
              </Text>
            </div>
>>>>>>> upstream/main
            <Box className={styles.cardDetailSection}>
              {onInternalLink ? (
                <InternalLinkProvider
                  onInternalLink={(url) => {
                    onClose();
                    return onInternalLink(url);
                  }}
                >
                  <Markdown canHaveInteractiveElements={false}>
                    {card.instructions}
                  </Markdown>
                </InternalLinkProvider>
              ) : (
                <Markdown canHaveInteractiveElements={false}>
                  {card.instructions}
                </Markdown>
              )}
            </Box>
<<<<<<< HEAD
          </Box>
        )}

        {card.final_report && (
          <Box>
            <Text size="2" weight="medium" color="gray">
              Final Report
            </Text>
            <Box
              className={styles.cardDetailSection}
              style={{ background: "var(--green-2)" }}
=======
          </section>
        )}

        {card.final_report && (
          <section className={styles.cardDetailSectionBlock}>
            <div className={styles.cardDetailSectionHeader}>
              <Icon icon={FileText} size="sm" tone="success" />
              <Text size="2" weight="medium">
                Final Report
              </Text>
            </div>
            <Box
              className={`${styles.cardDetailSection} ${styles.finalReportSection}`}
>>>>>>> upstream/main
            >
              <Markdown canHaveInteractiveElements={false}>
                {card.final_report}
              </Markdown>
            </Box>
<<<<<<< HEAD
          </Box>
        )}

        {card.status_updates.length > 0 && (
          <Box>
            <Text size="2" weight="medium" color="gray">
              Updates
            </Text>
            <Flex direction="column" gap="1" mt="1">
              {card.status_updates.map((update, i) => (
                <Text key={i} size="1" color="gray">
                  {new Date(update.timestamp).toLocaleString()}:{" "}
                  {update.message}
                </Text>
              ))}
            </Flex>
          </Box>
=======
          </section>
        )}

        {card.status_updates.length > 0 && (
          <section className={styles.cardDetailSectionBlock}>
            <div className={styles.cardDetailSectionHeader}>
              <Icon icon={ListChecks} size="sm" tone="muted" />
              <Text size="2" weight="medium">
                Updates
              </Text>
            </div>
            <div className={`${styles.cardDetailUpdates} rf-stagger`}>
              {card.status_updates.map((update, i) => (
                <div
                  key={i}
                  className={`${styles.cardDetailUpdate} rf-enter-rise`}
                >
                  <Text size="1" color="gray">
                    {new Date(update.timestamp).toLocaleString()}
                  </Text>
                  <Text size="2">{update.message}</Text>
                </div>
              ))}
            </div>
          </section>
>>>>>>> upstream/main
        )}

        <CardCommentsSection
          taskId={taskId}
          cardId={card.id}
          comments={card.comments ?? []}
        />

<<<<<<< HEAD
        <Flex justify="end">
          <Dialog.Close>
            <Button variant="soft">Close</Button>
          </Dialog.Close>
        </Flex>
      </Flex>
=======
        <div className={styles.cardDetailFooter}>
          <Dialog.Close asChild>
            <Button variant="soft">Close</Button>
          </Dialog.Close>
        </div>
      </div>
>>>>>>> upstream/main
    </Dialog.Content>
  );
};

interface TaskWorkspaceProps {
  taskId: string;
}

export const TaskWorkspace: React.FC<TaskWorkspaceProps> = ({ taskId }) => {
  const dispatch = useAppDispatch();
<<<<<<< HEAD
  const taskWorkspaceRef = React.useRef<HTMLDivElement>(null);
  const config = useAppSelector(selectConfig);
  const { data: task, isLoading: taskLoading } = useGetTaskQuery(taskId, {
    pollingInterval: 0,
  });
  const { data: board, isLoading: boardLoading } = useGetBoardQuery(taskId, {
=======
  const config = useAppSelector(selectConfig);
  const {
    data: task,
    isLoading: taskLoading,
    isError: taskError,
  } = useGetTaskQuery(taskId, {
    pollingInterval: 0,
  });
  const {
    data: board,
    isLoading: boardLoading,
    isError: boardError,
  } = useGetBoardQuery(taskId, {
>>>>>>> upstream/main
    pollingInterval: 0,
  });
  const { data: worktreesData, isLoading: worktreesLoading } =
    useListWorktreesQuery(undefined);
  const [openWorktree] = useOpenWorktreeMutation();
  const [deleteWorktree, deleteWorktreeState] = useDeleteWorktreeMutation();
  const copyToClipboard = useCopyToClipboard();
  const { openFolderInNewWindow } = useEventsBusForIDE();
<<<<<<< HEAD
  const { data: savedPlanners } = useListTaskTrajectoriesQuery({
    taskId,
    role: "planner",
=======
  const { data: savedPlanners, isLoading: savedPlannersLoading } =
    useListTaskTrajectoriesQuery({
      taskId,
      role: "planner",
    });
  const { data: savedAgents } = useListTaskTrajectoriesQuery({
    taskId,
    role: "agents",
>>>>>>> upstream/main
  });
  const [createPlannerChat, { isLoading: isCreatingPlanner }] =
    useCreatePlannerChatMutation();
  const [deletePlannerChat] = useDeletePlannerChatMutation();
  const openTasks = useAppSelector(selectOpenTasksFromRoot);
  const currentTaskUI = openTasks.find((t) => t.id === taskId);
  const plannerChats = useMemo(
    () =>
      [...(currentTaskUI?.plannerChats ?? [])].sort((a, b) =>
        b.updatedAt.localeCompare(a.updatedAt),
      ),
    [currentTaskUI?.plannerChats],
  );
<<<<<<< HEAD
  const activeChat = useAppSelector((state) =>
    selectTaskActiveChat(state, taskId),
  );
=======
  const linkedCardsByPlanner = useMemo(() => {
    const agentToPlanner = new Map<string, string>();
    for (const traj of savedAgents ?? []) {
      if (traj.parent_id) agentToPlanner.set(traj.id, traj.parent_id);
    }
    const result = new Map<string, string[]>();
    for (const card of board?.cards ?? []) {
      if (!card.agent_chat_id) continue;
      const plannerId = agentToPlanner.get(card.agent_chat_id);
      if (!plannerId) continue;
      const existing = result.get(plannerId);
      if (existing) {
        existing.push(card.id);
      } else {
        result.set(plannerId, [card.id]);
      }
    }
    return result;
  }, [savedAgents, board?.cards]);
  const activeChat = useAppSelector((state) =>
    selectTaskActiveChat(state, taskId),
  );
  const hasActiveChatRuntime = useAppSelector((state) =>
    activeChat ? Boolean(selectRuntimeById(state, activeChat.chatId)) : false,
  );
  const currentThreadId = useAppSelector(selectCurrentThreadId);
>>>>>>> upstream/main
  const activeChatBackgroundAgents = useAppSelector((state) =>
    activeChat
      ? selectBackgroundAgentsByThread(state, activeChat.chatId)
      : EMPTY_BACKGROUND_AGENTS,
  );
  const [selectedCardId, setSelectedCardId] = useState<string | null>(null);
  const selectedCard = useMemo(
    () =>
      selectedCardId
        ? board?.cards.find((c) => c.id === selectedCardId) ?? null
        : null,
    [board, selectedCardId],
  );
  const [diffTarget, setDiffTarget] = useState<CardWorktreeTarget | null>(null);
  const [mergeTargetId, setMergeTargetId] = useState<string | null>(null);
  const [mergeTargetWorktree, setMergeTargetWorktree] =
    useState<CardWorktreeTarget | null>(null);
  const mergeTarget = useMemo(() => {
    if (!mergeTargetId || !mergeTargetWorktree) return null;
    const card = board?.cards.find((c) => c.id === mergeTargetId) ?? null;
    if (!card) return null;
    return { card, worktree: mergeTargetWorktree };
  }, [board, mergeTargetId, mergeTargetWorktree]);
  const [deleteTargetId, setDeleteTargetId] = useState<string | null>(null);
  const [deleteTargetWorktree, setDeleteTargetWorktree] =
    useState<CardWorktreeTarget | null>(null);
  const deleteTarget = useMemo(() => {
    if (!deleteTargetId || !deleteTargetWorktree) return null;
    const card = board?.cards.find((c) => c.id === deleteTargetId) ?? null;
    if (!card) return null;
    return { card, worktree: deleteTargetWorktree };
  }, [board, deleteTargetId, deleteTargetWorktree]);
  const [deleteBranch, setDeleteBranch] = useState(false);
  const [notification, setNotification] = useState<string | null>(null);
  const notificationTimerRef = React.useRef<ReturnType<
    typeof setTimeout
  > | null>(null);
  useEffect(() => {
    return () => {
      if (notificationTimerRef.current)
        clearTimeout(notificationTimerRef.current);
    };
  }, []);
<<<<<<< HEAD
  const [layout, setLayout] = useState(() =>
    loadTaskWorkspaceLayout(taskId, defaultTaskWorkspaceLayout()),
  );
  const [workspaceTab, setWorkspaceTab] = useState("chat");
  const prevTaskStatusRef = React.useRef<string | undefined>(undefined);
  const chatExpanded = layout.chatExpanded;
  const panelsExpanded = layout.panelsExpanded;
  const boardHeightPx = layout.boardHeightPx;
=======
  const [explicitTab, setExplicitTab] = useState<TaskWorkspaceTab | null>(() =>
    loadTaskWorkspaceTab(taskId),
  );
  const smartDefaultTab: TaskWorkspaceTab =
    (savedPlanners?.length ?? 0) > 0 ? "chat" : "board";
  const workspaceTab: TaskWorkspaceTab = explicitTab ?? smartDefaultTab;
  const prevTaskStatusRef = React.useRef<string | undefined>(undefined);
  // Just-created chats are protected from reconciliation until the saved
  // trajectory list refetch includes them (prevents bouncing to old planner).
  const pendingCreatedPlannerIdsRef = React.useRef<Set<string>>(new Set());
>>>>>>> upstream/main
  const worktreeRecords = useMemo(
    () => worktreesData?.worktrees ?? [],
    [worktreesData?.worktrees],
  );
  const selectedCardThread = useAppSelector((state) =>
    selectedCard?.agent_chat_id
      ? selectThreadById(state, selectedCard.agent_chat_id)
      : null,
  );
  const selectedCardWorktree = useMemo(
    () =>
      selectedCard
        ? resolveCardWorktree(
            taskId,
            selectedCard,
            worktreeRecords,
            selectedCardThread?.worktree,
          )
        : null,
    [selectedCard, selectedCardThread?.worktree, taskId, worktreeRecords],
  );
  const selectedCardWorktreeLabel = selectedCard
    ? selectedCardWorktree?.label ??
      worktreeLabel(selectedCard, undefined, selectedCardThread?.worktree)
    : null;

  useEffect(() => {
    if (task) {
      dispatch(openTask({ id: taskId, name: task.name }));
    }
  }, [dispatch, taskId, task]);

  // Restore saved planner trajectories into Redux once the OpenTask entry
  // exists. This effect is intentionally idempotent: the per-planner dedup
  // check below guards against duplicate dispatches, so it can safely re-run
  // when `savedPlanners` or `currentTaskUI` updates. We must wait for
  // `currentTaskUI` (created by `openTask` after `task` loads) — without it,
  // `addPlannerChat`/`setTaskActiveChat` reducers silently no-op and the
  // restore is permanently lost (race condition: savedPlanners can arrive
  // before task).
  useEffect(() => {
    if (!savedPlanners || !currentTaskUI) return;

<<<<<<< HEAD
=======
    const savedPlannerIds = new Set(savedPlanners.map((planner) => planner.id));

    const pendingCreatedIds = pendingCreatedPlannerIdsRef.current;
    for (const id of Array.from(pendingCreatedIds)) {
      if (savedPlannerIds.has(id)) pendingCreatedIds.delete(id);
    }

    for (const planner of currentTaskUI.plannerChats) {
      if (
        !savedPlannerIds.has(planner.id) &&
        !pendingCreatedIds.has(planner.id)
      ) {
        dispatch(removePlannerChat({ taskId, chatId: planner.id }));
      }
    }

>>>>>>> upstream/main
    for (const traj of savedPlanners) {
      dispatch(
        createChatWithId({
          id: traj.id,
          title: traj.title,
          isTaskChat: true,
<<<<<<< HEAD
          mode: "TASK_PLANNER",
=======
          openTab: false,
          mode: traj.mode ?? "TASK_PLANNER",
>>>>>>> upstream/main
          taskMeta: {
            task_id: taskId,
            role: "planner",
            planner_chat_id: traj.id,
          },
        }),
      );

      const existing = currentTaskUI.plannerChats.find((p) => p.id === traj.id);
      if (existing) {
        if (
          existing.title !== traj.title ||
          existing.updatedAt !== traj.updated_at ||
          existing.sessionState !== traj.session_state ||
<<<<<<< HEAD
=======
          existing.mode !== traj.mode ||
>>>>>>> upstream/main
          !sameWaitingCards(
            existing.waitingForCardIds,
            traj.waiting_for_card_ids,
          )
        ) {
          dispatch(
            updatePlannerChat({
              taskId,
              planner: {
                id: traj.id,
                title: traj.title,
                updatedAt: traj.updated_at,
                sessionState: traj.session_state,
<<<<<<< HEAD
=======
                mode: traj.mode,
>>>>>>> upstream/main
                waitingForCardIds: traj.waiting_for_card_ids,
              },
            }),
          );
        }
        continue;
      }

      dispatch(
        addPlannerChat({
          taskId,
          planner: {
            id: traj.id,
            title: traj.title,
            createdAt: traj.created_at,
            updatedAt: traj.updated_at,
            sessionState: traj.session_state,
<<<<<<< HEAD
=======
            mode: traj.mode,
>>>>>>> upstream/main
            waitingForCardIds: traj.waiting_for_card_ids,
          },
        }),
      );
    }

<<<<<<< HEAD
    if (savedPlanners.length > 0 && !activeChat) {
      const mostRecent = savedPlanners.reduce((latest, p) =>
        p.updated_at > latest.updated_at ? p : latest,
      );
      dispatch(
        setTaskActiveChat({
          taskId,
          activeChat: { type: "planner", chatId: mostRecent.id },
        }),
      );
=======
    const mostRecentPlanner =
      savedPlanners.length > 0
        ? savedPlanners.reduce((latest, planner) =>
            planner.updated_at > latest.updated_at ? planner : latest,
          )
        : null;
    const fallbackActiveChat = mostRecentPlanner
      ? { type: "planner" as const, chatId: mostRecentPlanner.id }
      : null;

    if (!activeChat) {
      if (fallbackActiveChat) {
        dispatch(setTaskActiveChat({ taskId, activeChat: fallbackActiveChat }));
      }
      return;
    }

    if (
      activeChat.type === "planner" &&
      !savedPlannerIds.has(activeChat.chatId) &&
      !pendingCreatedIds.has(activeChat.chatId)
    ) {
      dispatch(setTaskActiveChat({ taskId, activeChat: fallbackActiveChat }));
>>>>>>> upstream/main
    }
  }, [dispatch, taskId, savedPlanners, currentTaskUI, activeChat]);

  useEffect(() => {
    const fallbackPlannerId = plannerChats[0]?.id;
    if (!activeChat && fallbackPlannerId) {
      dispatch(
        setTaskActiveChat({
          taskId,
          activeChat: { type: "planner", chatId: fallbackPlannerId },
        }),
      );
      return;
    }

    if (
      activeChat?.type === "planner" &&
      !plannerChats.some((p) => p.id === activeChat.chatId)
    ) {
      dispatch(
        setTaskActiveChat({
          taskId,
          activeChat: fallbackPlannerId
            ? { type: "planner", chatId: fallbackPlannerId }
            : null,
        }),
      );
    }
  }, [activeChat, plannerChats, dispatch, taskId]);

  useEffect(() => {
    if (activeChat?.type === "agent" && board) {
      const card = board.cards.find((c) => c.id === activeChat.cardId);
      if (!card || card.agent_chat_id !== activeChat.chatId) {
        const fallbackPlannerId = plannerChats[0]?.id;
        dispatch(
          setTaskActiveChat({
            taskId,
            activeChat: fallbackPlannerId
              ? { type: "planner", chatId: fallbackPlannerId }
              : null,
          }),
        );
      }
    }
  }, [activeChat, board, dispatch, taskId, plannerChats]);

  useEffect(() => {
<<<<<<< HEAD
=======
    if (activeChat?.type !== "agent" || !board || hasActiveChatRuntime) return;
    const card = board.cards.find(
      (candidate) =>
        candidate.id === activeChat.cardId &&
        candidate.agent_chat_id === activeChat.chatId,
    );
    if (!card) return;

    dispatch(
      createChatWithId({
        id: activeChat.chatId,
        title: formatAgentChatTitle(card.id, card.title),
        isTaskChat: true,
        openTab: false,
        mode: "TASK_AGENT",
        taskMeta: {
          task_id: taskId,
          role: "agents",
          card_id: card.id,
        },
      }),
    );
  }, [activeChat, board, dispatch, hasActiveChatRuntime, taskId]);

  useEffect(() => {
>>>>>>> upstream/main
    if (!task) return;

    const prevStatus = prevTaskStatusRef.current;
    const currentStatus = task.status;

    prevTaskStatusRef.current = currentStatus;

    if (prevStatus === "planning" && currentStatus === "active") {
      setNotification("Planning complete! You can now spawn agents.");
      if (notificationTimerRef.current)
        clearTimeout(notificationTimerRef.current);
      notificationTimerRef.current = setTimeout(
        () => setNotification(null),
        3000,
      );
    }
  }, [task]);

<<<<<<< HEAD
  // Switch chat when activeChat changes
  useEffect(() => {
    if (!activeChat) return;
    const chatId = activeChat.chatId;
    dispatch(switchToThread({ id: chatId, openTab: false }));
  }, [dispatch, activeChat]);
=======
  useEffect(() => {
    if (!activeChat || !hasActiveChatRuntime) return;
    if (currentThreadId === activeChat.chatId) return;
    dispatch(switchToThread({ id: activeChat.chatId, openTab: false }));
  }, [dispatch, activeChat, hasActiveChatRuntime, currentThreadId]);
>>>>>>> upstream/main

  const handleBack = useCallback(() => {
    dispatch(pop());
  }, [dispatch]);

  const handleCardClick = useCallback((card: BoardCard) => {
    setSelectedCardId(card.id);
  }, []);

  const showNotification = useCallback((message: string) => {
    setNotification(message);
    if (notificationTimerRef.current)
      clearTimeout(notificationTimerRef.current);
    notificationTimerRef.current = setTimeout(
      () => setNotification(null),
      3000,
    );
  }, []);

<<<<<<< HEAD
  const handleNewPlanner = useCallback(() => {
    if (isCreatingPlanner) return;
    createPlannerChat(taskId)
      .unwrap()
      .then((result) => {
        const newChatId = result.chat_id;
        const now = new Date().toISOString();
        dispatch(
          createChatWithId({
            id: newChatId,
            title: "",
            isTaskChat: true,
            mode: "TASK_PLANNER",
            taskMeta: {
              task_id: taskId,
              role: "planner",
              planner_chat_id: newChatId,
            },
          }),
        );
        dispatch(
          addPlannerChat({
            taskId,
            planner: {
              id: newChatId,
              title: "",
              createdAt: now,
              updatedAt: now,
            },
          }),
        );
        dispatch(
          setTaskActiveChat({
            taskId,
            activeChat: { type: "planner", chatId: newChatId },
          }),
        );
      })
      .catch((err: unknown) => {
        showNotification(`Create failed: ${parsePlannerDeleteError(err)}`);
      });
  }, [
    dispatch,
    taskId,
    createPlannerChat,
    isCreatingPlanner,
    showNotification,
  ]);
=======
  const handleWorkspaceTabChange = useCallback(
    (value: string) => {
      if (!isTaskWorkspaceTab(value)) return;
      setExplicitTab(value);
      saveTaskWorkspaceTab(taskId, value);
    },
    [taskId],
  );

  const openChatTab = useCallback(() => {
    handleWorkspaceTabChange("chat");
  }, [handleWorkspaceTabChange]);

  const createTaskChat = useCallback(
    (mode: string) => {
      if (isCreatingPlanner) return;
      createPlannerChat({ taskId, mode })
        .unwrap()
        .then((result) => {
          const newChatId = result.chat_id;
          const resolvedMode = result.mode ?? mode;
          const now = new Date().toISOString();
          pendingCreatedPlannerIdsRef.current.add(newChatId);
          dispatch(
            createChatWithId({
              id: newChatId,
              title: "",
              isTaskChat: true,
              openTab: false,
              mode: resolvedMode,
              taskMeta: {
                task_id: taskId,
                role: "planner",
                planner_chat_id: newChatId,
              },
            }),
          );
          dispatch(
            addPlannerChat({
              taskId,
              planner: {
                id: newChatId,
                title: "",
                createdAt: now,
                updatedAt: now,
                mode: resolvedMode,
              },
            }),
          );
          dispatch(
            setTaskActiveChat({
              taskId,
              activeChat: { type: "planner", chatId: newChatId },
            }),
          );
          openChatTab();
        })
        .catch((err: unknown) => {
          showNotification(`Create failed: ${parsePlannerDeleteError(err)}`);
        });
    },
    [
      dispatch,
      taskId,
      createPlannerChat,
      isCreatingPlanner,
      openChatTab,
      showNotification,
    ],
  );

  const handleNewPlanner = useCallback(() => {
    createTaskChat("task_planner");
  }, [createTaskChat]);
>>>>>>> upstream/main

  const handleRemovePlanner = useCallback(
    (chatId: string) => {
      const previous = plannerChats.find((p) => p.id === chatId);
<<<<<<< HEAD
=======
      pendingCreatedPlannerIdsRef.current.delete(chatId);
>>>>>>> upstream/main
      dispatch(removePlannerChat({ taskId, chatId }));
      if (activeChat?.type === "planner" && activeChat.chatId === chatId) {
        const remaining = plannerChats.filter((p) => p.id !== chatId);
        dispatch(
          setTaskActiveChat({
            taskId,
            activeChat: remaining[0]
              ? { type: "planner", chatId: remaining[0].id }
              : null,
          }),
        );
      }
      void deletePlannerChat({ taskId, chatId })
        .unwrap()
        .then(() => {
          showNotification("Planner chat deleted.");
        })
        .catch((err: unknown) => {
          if (previous)
            dispatch(restorePlannerChat({ taskId, planner: previous }));
          showNotification(`Delete failed: ${parsePlannerDeleteError(err)}`);
        });
    },
    [
      dispatch,
      taskId,
      activeChat,
      plannerChats,
      deletePlannerChat,
      showNotification,
    ],
  );

  const handleSelectPlanner = useCallback(
    (chatId: string) => {
      dispatch(
        setTaskActiveChat({ taskId, activeChat: { type: "planner", chatId } }),
      );
<<<<<<< HEAD
    },
    [dispatch, taskId],
=======
      openChatTab();
    },
    [dispatch, taskId, openChatTab],
>>>>>>> upstream/main
  );

  const handleSelectAgent = useCallback(
    (cardId: string, chatId: string) => {
      const card = board?.cards.find((c) => c.id === cardId);
      const cardTitle = card?.title ?? `Card ${cardId}`;

      dispatch(
        createChatWithId({
          id: chatId,
          title: formatAgentChatTitle(cardId, cardTitle),
          isTaskChat: true,
<<<<<<< HEAD
=======
          openTab: false,
>>>>>>> upstream/main
          mode: "TASK_AGENT",
          taskMeta: {
            task_id: taskId,
            role: "agents",
            card_id: cardId,
          },
        }),
      );

      dispatch(
        setTaskActiveChat({
          taskId,
          activeChat: { type: "agent", cardId, chatId },
        }),
      );
<<<<<<< HEAD
    },
    [board, taskId, dispatch],
=======
      openChatTab();
    },
    [board, taskId, dispatch, openChatTab],
>>>>>>> upstream/main
  );

  const handleCardAgentClick = useCallback(
    (card: BoardCard) => {
      if (!card.agent_chat_id) return;
      handleSelectAgent(card.id, card.agent_chat_id);
      setSelectedCardId(null);
    },
    [handleSelectAgent],
  );

  const handleInternalLink = useCallback(
    (url: string): boolean => {
      const parsed = parseRefactLink(url);
      if (!parsed) return false;

      if (parsed.type !== "chat" || !parsed.id) return false;

      const action = resolveChatLink(parsed.id, plannerChats, board);
      switch (action.kind) {
        case "planner":
<<<<<<< HEAD
          dispatch(
            setTaskActiveChat({
              taskId,
              activeChat: { type: "planner", chatId: action.chatId },
            }),
          );
=======
          handleSelectPlanner(action.chatId);
>>>>>>> upstream/main
          return true;
        case "agent":
          handleSelectAgent(action.cardId, action.chatId);
          return true;
        case "unknown": {
          const agent = Object.values(activeChatBackgroundAgents).find(
            (candidate) => candidate.child_chat_id === action.chatId,
          );
          if (!agent) showNotification(`Chat not found: ${action.chatId}`);
          dispatch(
            createChatWithId({
              id: action.chatId,
              parentId: activeChat?.chatId,
              linkType:
                agent?.kind ??
                (activeChat?.type === "agent" ? "delegate" : "subagent"),
            }),
          );
          dispatch(switchToThread({ id: action.chatId }));
          dispatch(popBackTo({ name: "history" }));
          dispatch(push({ name: "chat" }));
          return true;
        }
      }
    },
    [
      activeChat,
      activeChatBackgroundAgents,
      board,
<<<<<<< HEAD
      taskId,
      dispatch,
      plannerChats,
=======
      dispatch,
      plannerChats,
      handleSelectPlanner,
>>>>>>> upstream/main
      handleSelectAgent,
      showNotification,
    ],
  );

<<<<<<< HEAD
  const handleToggleChatExpanded = useCallback(() => {
    setLayout((prev) => {
      const next = { ...prev, chatExpanded: !prev.chatExpanded };
      saveTaskWorkspaceLayout(taskId, next);
      return next;
    });
  }, [taskId]);

  const handleTogglePanelsExpanded = useCallback(() => {
    setLayout((prev) => {
      const next = { ...prev, panelsExpanded: !prev.panelsExpanded };
      saveTaskWorkspaceLayout(taskId, next);
      return next;
    });
  }, [taskId]);

  const handleBoardResizeDrag = useCallback(
    (clientY: number) => {
      const container = taskWorkspaceRef.current;
      const rect = container?.getBoundingClientRect();
      const nextHeight = clampBoardHeight(
        rect ? clientY - rect.top : clientY,
        rect?.height,
      );
      setLayout((prev) => {
        const next = { ...prev, boardHeightPx: nextHeight };
        saveTaskWorkspaceLayout(taskId, next);
        return next;
      });
    },
    [taskId],
  );

  const handleBoardResizeReset = useCallback(() => {
    setLayout((prev) => {
      const next = { ...prev, boardHeightPx: DEFAULT_BOARD_HEIGHT_PX };
      saveTaskWorkspaceLayout(taskId, next);
      return next;
    });
  }, [taskId]);

=======
>>>>>>> upstream/main
  useEffect(() => {
    if (!board || !selectedCardId) return;
    if (!board.cards.some((c) => c.id === selectedCardId)) {
      setSelectedCardId(null);
      showNotification("Card was deleted by another planner.");
    }
  }, [board, selectedCardId, showNotification]);

  useEffect(() => {
    const onVisible = () => {
      if (document.visibilityState === "visible") {
<<<<<<< HEAD
        dispatch(tasksApi.util.invalidateTags([{ type: "Board", id: taskId }]));
=======
        dispatch(
          tasksApi.util.invalidateTags([
            { type: "Board", id: taskId },
            { type: "TaskTrajectories", id: `${taskId}/agents` },
          ]),
        );
>>>>>>> upstream/main
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [dispatch, taskId]);

  const invalidateTaskQueries = useCallback(() => {
    dispatch(
      tasksApi.util.invalidateTags([
        { type: "Tasks", id: taskId },
        { type: "Board", id: taskId },
        "Tasks",
      ]),
    );
  }, [dispatch, taskId]);

  const handleViewCardDiff = useCallback((worktree: CardWorktreeTarget) => {
    if (!isActionableWorktree(worktree)) return;
    setDiffTarget(worktree);
  }, []);

  const handleMergeCardWorktree = useCallback(
    (worktree: CardWorktreeTarget) => {
      if (!selectedCard || !isActionableWorktree(worktree)) return;
      setMergeTargetId(selectedCard.id);
      setMergeTargetWorktree(worktree);
    },
    [selectedCard],
  );

  const handleOpenCardWorktree = useCallback(
    async (worktree: CardWorktreeTarget) => {
      if (!isActionableWorktree(worktree)) return;
      try {
        const response = await openWorktree({
          id: worktree.id,
          source_workspace_root:
            worktree.record?.meta.source_workspace_root ??
            worktree.meta?.source_workspace_root,
        }).unwrap();
        const hostCanOpenFolder =
          config.host === "vscode" ||
          config.host === "jetbrains" ||
          config.host === "ide";
        if (response.can_open_folder && hostCanOpenFolder) {
          openFolderInNewWindow(response.path);
          showNotification("Opening worktree in a new window.");
        } else {
          copyToClipboard(response.path);
          showNotification("Worktree path copied to clipboard.");
        }
      } catch (error) {
        showNotification(`Open failed: ${worktreeErrorText(error)}`);
      }
    },
    [
      config.host,
      copyToClipboard,
      openFolderInNewWindow,
      openWorktree,
      showNotification,
    ],
  );

  const handleDeleteCardWorktree = useCallback(
    (worktree: CardWorktreeTarget) => {
      if (!selectedCard || !isActionableWorktree(worktree)) return;
      setDeleteBranch(false);
      setDeleteTargetId(selectedCard.id);
      setDeleteTargetWorktree(worktree);
    },
    [selectedCard],
  );

  const handleConfirmDeleteCardWorktree = useCallback(async () => {
    if (!deleteTarget || !isActionableWorktree(deleteTarget.worktree)) return;
    try {
      await deleteWorktree({
        id: deleteTarget.worktree.id,
        source_workspace_root:
          deleteTarget.worktree.record?.meta.source_workspace_root ??
          deleteTarget.worktree.meta?.source_workspace_root,
        delete_branch: deleteBranch,
        force_referenced: true,
      }).unwrap();
      if (deleteTarget.card.agent_chat_id) {
        dispatch(
          setThreadWorktree({
            chatId: deleteTarget.card.agent_chat_id,
            worktree: null,
          }),
        );
      }
      setDeleteTargetId(null);
      setDeleteTargetWorktree(null);
      invalidateTaskQueries();
      showNotification("Worktree deleted.");
    } catch (error) {
      showNotification(`Delete failed: ${worktreeErrorText(error)}`);
    }
  }, [
    deleteBranch,
    deleteTarget,
    deleteWorktree,
    dispatch,
    invalidateTaskQueries,
    showNotification,
  ]);

  const handleCardMergeCompleted = useCallback(
    (response: MergeWorktreeResponse) => {
      if (
        response.cleanup?.worktree_deleted &&
        mergeTarget?.card.agent_chat_id
      ) {
        dispatch(
          setThreadWorktree({
            chatId: mergeTarget.card.agent_chat_id,
            worktree: null,
          }),
        );
      }
      invalidateTaskQueries();
      showNotification("Worktree merge completed.");
    },
    [dispatch, invalidateTaskQueries, mergeTarget, showNotification],
  );

  const handleAskRefactForMerge = useCallback(
    async (files: string[], response: MergeWorktreeResponse) => {
      if (!mergeTarget) throw new Error("No task worktree is selected.");
      const fallbackPlannerId =
        activeChat?.type === "planner"
          ? activeChat.chatId
          : plannerChats[0]?.id;
      const chatId = mergeTarget.card.agent_chat_id ?? fallbackPlannerId;
      if (!chatId) throw new Error("No agent or planner chat is available.");
      const apiKey = config.apiKey ?? undefined;
      const prompt = buildWorktreeConflictPrompt({
        worktree: mergeTarget.worktree.meta,
        record: mergeTarget.worktree.record,
        response,
        files,
        taskId,
        cardId: mergeTarget.card.id,
      });
      if (mergeTarget.card.agent_chat_id) {
        dispatch(
          createChatWithId({
            id: chatId,
            title: formatAgentChatTitle(
              mergeTarget.card.id,
              mergeTarget.card.title,
            ),
            isTaskChat: true,
<<<<<<< HEAD
=======
            openTab: false,
>>>>>>> upstream/main
            mode: "TASK_AGENT",
            taskMeta: {
              task_id: taskId,
              role: "agents",
              card_id: mergeTarget.card.id,
            },
            worktree: mergeTarget.worktree.meta ?? null,
          }),
        );
        dispatch(
          setTaskActiveChat({
            taskId,
            activeChat: {
              type: "agent",
              cardId: mergeTarget.card.id,
              chatId,
            },
          }),
        );
      } else {
        dispatch(
          setTaskActiveChat({
            taskId,
            activeChat: { type: "planner", chatId },
          }),
        );
      }
<<<<<<< HEAD
=======
      openChatTab();
>>>>>>> upstream/main
      dispatch(switchToThread({ id: chatId, openTab: false }));
      if (mergeTarget.worktree.meta) {
        dispatch(
          setThreadWorktree({ chatId, worktree: mergeTarget.worktree.meta }),
        );
      }
      await updateChatParams(
        chatId,
        { worktree_id: mergeTarget.worktree.id },
        config,
        apiKey,
      );
      await sendUserMessage(chatId, prompt, config, apiKey, true);
      showNotification("Conflict resolution request sent to Refact.");
    },
    [
      activeChat,
      plannerChats,
      config,
      dispatch,
      mergeTarget,
<<<<<<< HEAD
=======
      openChatTab,
>>>>>>> upstream/main
      showNotification,
      taskId,
    ],
  );

<<<<<<< HEAD
  if (taskLoading || boardLoading || !task || !board) {
=======
  if (taskError || boardError) {
    return (
      <Flex
        align="center"
        justify="center"
        className={styles.fullHeightEmptyState}
      >
        <Text color="gray">Task is no longer available.</Text>
      </Flex>
    );
  }

  if (taskLoading || boardLoading || savedPlannersLoading || !task || !board) {
>>>>>>> upstream/main
    return <ChatLoading />;
  }

  const chatLabel = !activeChat
    ? "No chat selected"
    : activeChat.type === "planner"
<<<<<<< HEAD
      ? `Planner`
=======
      ? "Planner"
>>>>>>> upstream/main
      : formatAgentChatTitle(
          activeChat.cardId,
          board.cards.find((c) => c.id === activeChat.cardId)?.title ?? "",
        );
<<<<<<< HEAD
  const agentChats = board.cards.filter((card) => card.agent_chat_id);
  const doneAgentChats = agentChats.filter((card) => card.column === "done");
  const chatToggleLabel = chatExpanded ? "Collapse chat" : "Expand chat";
  const panelsToggleLabel = panelsExpanded
    ? "Collapse planners and agents"
    : "Expand planners and agents";
  const boardSectionStyle: React.CSSProperties = {
    flex: `0 0 ${boardHeightPx}px`,
  };

  return (
    <Box ref={taskWorkspaceRef} className={styles.taskWorkspace}>
      <CollapsePanel
        collapsed={chatExpanded}
        className={styles.workspaceChromeCollapse}
      >
        <Box className={styles.boardSection} style={boardSectionStyle}>
          <KanbanBoard
            board={board}
            onCardClick={handleCardClick}
            onAgentClick={handleCardAgentClick}
          />
        </Box>

        <ResizeDivider
          onDrag={handleBoardResizeDrag}
          onReset={handleBoardResizeReset}
        />

        <Box className={styles.panelsWrapper}>
          <div className={styles.panelsHeader}>
            <button
              type="button"
              onClick={handleTogglePanelsExpanded}
              aria-expanded={panelsExpanded}
              aria-label={panelsToggleLabel}
              title={panelsToggleLabel}
              className={styles.sectionHeaderToggle}
            >
              <ChevronDownIcon
                className={`${styles.chevron} ${
                  panelsExpanded ? styles.chevronExpanded : ""
                }`}
              />
              <Text
                size="1"
                weight="bold"
                color="gray"
                className={styles.sectionHeaderLabel}
              >
                Planners / Agents
              </Text>
            </button>
            <Flex align="center" gap="2" className={styles.sectionHeaderMeta}>
              <Badge size="1" color="gray" variant="soft">
                {plannerChats.length} planner
                {plannerChats.length === 1 ? "" : "s"}
              </Badge>
              {agentChats.length > 0 && (
                <Badge size="1" color="gray" variant="soft">
                  {doneAgentChats.length}/{agentChats.length} agents
                </Badge>
              )}
              <button
                type="button"
                className={styles.sectionHeaderActionButton}
                onClick={handleNewPlanner}
                aria-label="New planner"
                title="New planner"
              >
                <PlusIcon />
              </button>
            </Flex>
          </div>

          <CollapsePanel
            collapsed={!panelsExpanded}
            className={styles.panelsCollapse}
          >
            <Flex className={styles.panelsSection}>
              <PlannerPanel
                plannerChats={plannerChats}
                activeChat={activeChat}
                onSelectPlanner={handleSelectPlanner}
                onRemovePlanner={handleRemovePlanner}
              />
              <AgentsPanel
                cards={board.cards}
                activeChat={activeChat}
                onSelectAgent={handleSelectAgent}
              />
            </Flex>
          </CollapsePanel>
        </Box>
      </CollapsePanel>

      <Box className={styles.chatSection}>
        <Tabs.Root
          value={workspaceTab}
          onValueChange={setWorkspaceTab}
          className={styles.workspaceTabs}
        >
          <div className={styles.chatHeader}>
            <button
              type="button"
              onClick={handleToggleChatExpanded}
              aria-expanded={chatExpanded}
              aria-label={chatToggleLabel}
              title={chatToggleLabel}
              className={`${styles.sectionHeaderToggle} ${styles.chatHeaderToggle}`}
            >
              <ChevronDownIcon
                className={`${styles.chevron} ${
                  chatExpanded ? styles.chevronExpanded : ""
                }`}
              />
              <Text
                size="1"
                weight="bold"
                color="gray"
                className={styles.sectionHeaderLabel}
              >
                Task
              </Text>
              {workspaceTab === "chat" && (
                <Text size="1" color="gray" className={styles.chatHeaderLabel}>
                  {chatLabel}
                </Text>
              )}
            </button>
            <Tabs.List size="1">
              <Tabs.Trigger value="chat">Chat</Tabs.Trigger>
              <Tabs.Trigger value="memories">Memories</Tabs.Trigger>
              <Tabs.Trigger value="documents">Documents</Tabs.Trigger>
            </Tabs.List>
          </div>
          <Box className={styles.chatContent}>
            {workspaceTab === "chat" ? (
              <Box className={styles.workspaceTabContent}>
                {activeChat ? (
=======
  const runningAgentCount = board.cards.filter(
    (card) => card.column === "doing" && card.agent_chat_id,
  ).length;
  const waitingPlannerCount = plannerChats.filter(
    (planner) => planner.sessionState === "waiting_user_input",
  ).length;

  return (
    <Box className={styles.taskWorkspace}>
      <Tabs
        value={workspaceTab}
        onValueChange={handleWorkspaceTabChange}
        className={styles.workspaceTabs}
      >
        <div className={styles.workspaceHeader}>
          <Tabs.List
            activeIndex={workspaceTabIndex(workspaceTab)}
            className={styles.workspaceTabList}
            itemCount={4}
          >
            <Tabs.Trigger value="board">
              <span className={styles.tabTriggerContent}>
                Board
                {runningAgentCount > 0 && (
                  <Badge
                    tone="accent"
                    title={`${runningAgentCount} running agent${
                      runningAgentCount === 1 ? "" : "s"
                    }`}
                  >
                    {runningAgentCount}
                  </Badge>
                )}
                {waitingPlannerCount > 0 && (
                  <Badge
                    tone="warning"
                    title={`${waitingPlannerCount} planner${
                      waitingPlannerCount === 1 ? "" : "s"
                    } waiting for input`}
                  >
                    {waitingPlannerCount}
                  </Badge>
                )}
              </span>
            </Tabs.Trigger>
            <Tabs.Trigger value="chat">Chat</Tabs.Trigger>
            <Tabs.Trigger value="memories">Memories</Tabs.Trigger>
            <Tabs.Trigger value="documents">Documents</Tabs.Trigger>
          </Tabs.List>
          <div className={styles.headerActionsPanel}>
            <button
              type="button"
              className={styles.headerActionButton}
              onClick={handleNewPlanner}
              disabled={isCreatingPlanner}
              aria-label="New task planner"
              title="New task planner"
            >
              <PlusIcon />
              <Text size="1">Planner</Text>
            </button>
            <NewChatModeButton
              disabled={isCreatingPlanner}
              onCreate={createTaskChat}
            />
            <ChatSwitcher
              label={chatLabel}
              plannerChats={plannerChats}
              cards={board.cards}
              activeChat={activeChat}
              linkedCardsByPlanner={linkedCardsByPlanner}
              onSelectPlanner={handleSelectPlanner}
              onRemovePlanner={handleRemovePlanner}
              onSelectAgent={handleSelectAgent}
            />
          </div>
        </div>
        <Box className={styles.chatContent}>
          {workspaceTab === "board" ? (
            <Box className={styles.workspaceTabContent}>
              <div className={styles.boardTabLayout}>
                <BoardRail
                  plannerChats={plannerChats}
                  cards={board.cards}
                  activeChat={activeChat}
                  linkedCardsByPlanner={linkedCardsByPlanner}
                  onSelectPlanner={handleSelectPlanner}
                  onRemovePlanner={handleRemovePlanner}
                  onSelectAgent={handleSelectAgent}
                />
                <Box className={styles.boardArea}>
                  <KanbanBoard
                    board={board}
                    onCardClick={handleCardClick}
                    onAgentClick={handleCardAgentClick}
                  />
                </Box>
              </div>
            </Box>
          ) : workspaceTab === "chat" ? (
            <Box className={styles.workspaceTabContent}>
              {activeChat ? (
                hasActiveChatRuntime ? (
>>>>>>> upstream/main
                  <InternalLinkProvider onInternalLink={handleInternalLink}>
                    <Chat
                      host={config.host}
                      tabbed={false}
                      backFromChat={handleBack}
<<<<<<< HEAD
=======
                      chatId={activeChat.chatId}
>>>>>>> upstream/main
                    />
                  </InternalLinkProvider>
                ) : (
                  <Flex
                    align="center"
                    justify="center"
<<<<<<< HEAD
                    style={{ height: "100%" }}
                  >
                    <Text color="gray">
                      Create a planner chat to get started
                    </Text>
                  </Flex>
                )}
              </Box>
            ) : workspaceTab === "memories" ? (
              <Box className={styles.workspaceTabContent}>
                <MemoryInboxPanel taskId={taskId} />
              </Box>
            ) : (
              <Box className={styles.workspaceTabContent}>
                <DocumentsPanel taskId={taskId} />
              </Box>
            )}
          </Box>
        </Tabs.Root>
      </Box>

      <Dialog.Root
=======
                    className={styles.fullHeightEmptyState}
                  >
                    <Text color="gray">Loading chat…</Text>
                  </Flex>
                )
              ) : (
                <Flex
                  align="center"
                  justify="center"
                  className={styles.fullHeightEmptyState}
                >
                  <Text color="gray">Create a planner chat to get started</Text>
                </Flex>
              )}
            </Box>
          ) : workspaceTab === "memories" ? (
            <Box className={styles.workspaceTabContent}>
              <MemoryInboxPanel taskId={taskId} />
            </Box>
          ) : (
            <Box className={styles.workspaceTabContent}>
              <DocumentsPanel taskId={taskId} />
            </Box>
          )}
        </Box>
      </Tabs>

      <Dialog
>>>>>>> upstream/main
        open={Boolean(selectedCard)}
        onOpenChange={(open) => {
          if (!open) setSelectedCardId(null);
        }}
      >
        {selectedCard && (
          <CardDetail
            taskId={taskId}
            card={selectedCard}
            worktree={selectedCardWorktree}
            worktreeLabel={selectedCardWorktreeLabel}
            isWorktreeLoading={worktreesLoading}
            onClose={() => setSelectedCardId(null)}
            onInternalLink={handleInternalLink}
            onViewDiff={handleViewCardDiff}
            onMerge={handleMergeCardWorktree}
            onOpenWorktree={(worktree) => void handleOpenCardWorktree(worktree)}
            onDeleteWorktree={handleDeleteCardWorktree}
          />
        )}
<<<<<<< HEAD
      </Dialog.Root>
=======
      </Dialog>
>>>>>>> upstream/main

      <WorktreeDiffPanel
        open={Boolean(diffTarget)}
        worktreeId={diffTarget?.id}
        worktree={diffTarget?.meta}
        record={diffTarget?.record}
        onOpenChange={(open) => {
          if (!open) setDiffTarget(null);
        }}
      />

      <MergeWorktreeModal
        open={Boolean(mergeTarget)}
        worktreeId={mergeTarget?.worktree.id}
        worktree={mergeTarget?.worktree.meta}
        record={mergeTarget?.worktree.record}
        taskId={taskId}
        defaultTargetBranch={task.base_branch}
        onOpenChange={(open) => {
          if (!open) {
            setMergeTargetId(null);
            setMergeTargetWorktree(null);
          }
        }}
        onMerged={handleCardMergeCompleted}
        onAskRefact={handleAskRefactForMerge}
        onOpenWorktree={() =>
          mergeTarget ? handleOpenCardWorktree(mergeTarget.worktree) : undefined
        }
      />

<<<<<<< HEAD
      <Dialog.Root
=======
      <Dialog
>>>>>>> upstream/main
        open={Boolean(deleteTarget)}
        onOpenChange={(open) => {
          if (!open) {
            setDeleteTargetId(null);
            setDeleteTargetWorktree(null);
          }
        }}
      >
<<<<<<< HEAD
        <Dialog.Content maxWidth="420px">
          <Dialog.Title>Delete worktree</Dialog.Title>
          <Dialog.Description size="2" color="gray">
            Delete or discard this task agent worktree from disk.
          </Dialog.Description>
          <Flex direction="column" gap="3" mt="3">
            <Text size="2" weight="medium">
              {deleteTarget?.worktree.label ?? "Worktree"}
            </Text>
            {deleteTarget?.worktree.referenceCount !== undefined &&
              deleteTarget.worktree.referenceCount > 1 && (
                <Text size="2" color="amber">
                  This worktree is shared by{" "}
                  {deleteTarget.worktree.referenceCount} references.
                </Text>
              )}
            <Text as="label" size="2">
              <Flex align="center" gap="2">
                <Checkbox
                  checked={deleteBranch}
                  onCheckedChange={(checked) =>
                    setDeleteBranch(checked === true)
                  }
                  disabled={deleteWorktreeState.isLoading}
                />
                Delete git branch too
              </Flex>
            </Text>
          </Flex>
          <Flex justify="end" gap="2" mt="4">
            <Dialog.Close>
              <Button
                type="button"
                variant="soft"
                color="gray"
                disabled={deleteWorktreeState.isLoading}
              >
                Cancel
              </Button>
            </Dialog.Close>
            <Button
              type="button"
              color="red"
              disabled={!deleteTarget || deleteWorktreeState.isLoading}
              onClick={() => void handleConfirmDeleteCardWorktree()}
            >
              {deleteWorktreeState.isLoading
                ? "Deleting..."
                : "Delete worktree"}
            </Button>
          </Flex>
        </Dialog.Content>
      </Dialog.Root>
=======
        <Dialog.Content
          className={styles.deleteWorktreeDialog}
          maxWidth="420px"
        >
          <div className={styles.deleteWorktreeRoot}>
            <Dialog.Title>Delete worktree</Dialog.Title>
            <Dialog.Description>
              Delete or discard this task agent worktree from disk.
            </Dialog.Description>
            <div className={styles.deleteWorktreeBody}>
              <Text size="2" weight="medium">
                {deleteTarget?.worktree.label ?? "Worktree"}
              </Text>
              {deleteTarget?.worktree.referenceCount !== undefined &&
                deleteTarget.worktree.referenceCount > 1 && (
                  <Text size="2" color="amber">
                    This worktree is shared by{" "}
                    {deleteTarget.worktree.referenceCount} references.
                  </Text>
                )}
              <Checkbox
                checked={deleteBranch}
                onCheckedChange={(checked) => setDeleteBranch(checked === true)}
                disabled={deleteWorktreeState.isLoading}
              >
                Delete git branch too
              </Checkbox>
            </div>
            <div className={styles.deleteWorktreeActions}>
              <Dialog.Close asChild>
                <Button
                  type="button"
                  variant="soft"
                  disabled={deleteWorktreeState.isLoading}
                >
                  Cancel
                </Button>
              </Dialog.Close>
              <Button
                type="button"
                variant="danger"
                disabled={!deleteTarget || deleteWorktreeState.isLoading}
                loading={deleteWorktreeState.isLoading}
                onClick={() => void handleConfirmDeleteCardWorktree()}
              >
                {deleteWorktreeState.isLoading
                  ? "Deleting..."
                  : "Delete worktree"}
              </Button>
            </div>
          </div>
        </Dialog.Content>
      </Dialog>
>>>>>>> upstream/main

      {notification && (
        <Box
          role="status"
          aria-live="polite"
<<<<<<< HEAD
          style={{
            position: "fixed",
            bottom: "var(--space-4)",
            left: "50%",
            transform: "translateX(-50%)",
            background: "var(--accent-9)",
            color: "white",
            padding: "var(--space-3) var(--space-4)",
            borderRadius: "var(--radius-3)",
            zIndex: 50,
            boxShadow: "0 4px 12px rgba(0, 0, 0, 0.15)",
          }}
=======
          className={styles.notificationToast}
>>>>>>> upstream/main
        >
          <Text size="2">{notification}</Text>
        </Box>
      )}
    </Box>
  );
};
