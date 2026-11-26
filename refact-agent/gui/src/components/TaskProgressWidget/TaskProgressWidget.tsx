import React, { useCallback, useEffect, useMemo, useState } from "react";
import * as Collapsible from "@radix-ui/react-collapsible";
import classNames from "classnames";
import {
  Ban,
  ClipboardList,
  Pause,
  Play,
  Square,
  Target,
  Trash2,
  type LucideIcon,
} from "lucide-react";

import { useChatActions } from "../../hooks/useChatActions";
import { useAppDispatch } from "../../hooks/useAppDispatch";
import { useAppSelector } from "../../hooks/useAppSelector";
import {
  selectCurrentTasksById,
  selectGoalById,
  selectHasTasksById,
  selectPlanBannerState,
  selectTaskGoalExpandedById,
  selectTaskProgressById,
  selectTasksEverUsedById,
  selectTaskWidgetExpandedById,
  selectThreadModeById,
  selectThreadToolUseById,
  setTaskGoalExpanded,
  setTaskWidgetExpanded,
  useThreadId,
} from "../../features/Chat/Thread";
import type { TodoItem, TodoStatus } from "../../features/Chat/Thread/types";
import type {
  GoalAttempt,
  GoalEvent,
  GoalExecution,
  GoalResolution,
  GoalSnapshot,
  GoalStatus,
} from "../../services/refact/types";
import { getPlanMetadata } from "../../services/refact/types";
import type {
  GoalAutonomyCommand,
  GoalBudgetCommand,
  GoalControlAction,
} from "../../services/refact/chatCommands";
import { Box, Flex, Separator, Text } from "../LongTailPrimitives";
import { Badge, Button, Icon, IconButton } from "../ui";
import { Chevron } from "../Collapsible";
import { CircularProgress } from "../CircularProgress/CircularProgress";
import { Markdown } from "../Markdown";
import { PlanHistoryModal } from "../ChatContent/PlanBanner/PlanHistoryModal";
import { normalizePlanContent } from "../ChatContent/PlanBanner/planContent";
import { StatusDot, type StatusDotState } from "../StatusDot";
import {
  addBuddyCrashBreadcrumb,
  setBuddyCrashHotSlot,
} from "../../features/Buddy/reportBuddyFrontendError";
import { humanizeIdentifier } from "../../utils/displayNames";
import styles from "./TaskProgressWidget.module.css";

function getStatusDotState(status: TodoStatus): StatusDotState {
  switch (status) {
    case "in_progress":
      return "in_progress";
    case "completed":
      return "completed";
    case "failed":
      return "error";
    case "pending":
    default:
      return "idle";
  }
}

const STATUS_TOOLTIPS: Record<TodoStatus, string> = {
  completed: "Completed",
  in_progress: "In progress",
  pending: "Pending",
  failed: "Failed",
};

const GOAL_BUDGET_INPUT_MIN = 0;
const GOAL_BUDGET_INPUT_STEP = 1;

type GoalBudgetDraft = {
  maxTurns: string;
  maxMinutes: string;
  maxTokens: string;
};

function goalStatusTone(
  status: GoalStatus,
): React.ComponentProps<typeof Badge>["tone"] {
  switch (status) {
    case "active":
    case "verifying":
      return "accent";
    case "completed":
      return "success";
    case "paused":
      return "warning";
    case "stopped":
    case "budget_exhausted":
    case "no_progress":
      return "danger";
    case "transferred":
    default:
      return "muted";
  }
}

function hasModernGoalProjection(goal: GoalSnapshot): boolean {
  return (
    goal.resolution !== undefined &&
    goal.execution !== undefined &&
    goal.autonomy !== undefined
  );
}

function goalResolution(goal: GoalSnapshot): GoalResolution {
  if (goal.resolution) return goal.resolution;
  if (goal.status === "completed") return "achieved";
  if (goal.status === "stopped") return "cancelled";
  return "active";
}

function goalExecution(goal: GoalSnapshot): GoalExecution {
  if (goal.execution) return goal.execution;
  switch (goal.status) {
    case "verifying":
      return "verifying";
    case "paused":
    case "budget_exhausted":
    case "no_progress":
    case "transferred":
      return "paused";
    default:
      return "idle";
  }
}

function goalIsQuiescent(goal: GoalSnapshot): boolean {
  const latestEvent = goal.events.at(-1);
  return (
    latestEvent?.kind === "pursuit_quiescent" ||
    latestEvent?.kind === "quiescent"
  );
}

function goalExecutionLabel(goal: GoalSnapshot): string {
  const resolution = goalResolution(goal);
  if (resolution !== "active") return humanizeIdentifier(resolution);
  if (goal.snoozed_until_ms != null) return "Snoozed";
  if (goalIsQuiescent(goal)) return "Quiescent";
  const execution = goalExecution(goal);
  if (goal.status === "budget_exhausted") return "Paused · budget exhausted";
  if (goal.status === "no_progress") return "Paused · no progress";
  if (execution === "idle" && goal.autonomy === "off") {
    return "Idle · autonomy off";
  }
  return humanizeIdentifier(execution);
}

function goalIsTransferred(goal: GoalSnapshot): boolean {
  return goal.status === "transferred" || Boolean(goal.transferred_to);
}

function hasGoalWork(goal: GoalSnapshot | null): boolean {
  return goal !== null && goal.content.trim().length > 0;
}

function isPositiveGoalLimit(
  value: number | null | undefined,
): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

function formatGoalUsageValue(value: number): string {
  return Number.isFinite(value) ? String(value) : "0";
}

function formatGoalBudgetPart(
  used: number,
  limit: number | null | undefined,
  label: string,
): string {
  const usage = formatGoalUsageValue(used);
  return isPositiveGoalLimit(limit)
    ? `${usage}/${limit} ${label}`
    : `${usage} ${label}`;
}

function GoalBudgetProgress({ goal }: { goal: GoalSnapshot }) {
  const { budget, progress } = goal;
  const parts = [
    {
      text: formatGoalBudgetPart(
        progress.turns_used,
        budget.max_turns,
        "turns",
      ),
      saturated:
        isPositiveGoalLimit(budget.max_turns) &&
        progress.turns_used >= budget.max_turns,
    },
    {
      text: formatGoalBudgetPart(
        progress.tokens_used,
        budget.max_tokens,
        "tokens",
      ),
      saturated:
        isPositiveGoalLimit(budget.max_tokens) &&
        progress.tokens_used >= budget.max_tokens,
    },
    {
      text: formatGoalBudgetPart(
        progress.no_progress_turns,
        budget.no_progress_turns,
        "no-progress",
      ),
      saturated:
        isPositiveGoalLimit(budget.no_progress_turns) &&
        progress.no_progress_turns >= budget.no_progress_turns,
    },
  ];
  const hasBudgetLimit = [
    budget.max_turns,
    budget.max_minutes,
    budget.max_tokens,
    budget.no_progress_turns,
  ].some(isPositiveGoalLimit);

  return (
    <>
      {parts.map((part, index) => (
        <React.Fragment key={part.text}>
          {index > 0 ? " · " : null}
          <span
            className={
              part.saturated ? styles.goalProgressSaturated : undefined
            }
          >
            {part.text}
          </span>
        </React.Fragment>
      ))}
      {!hasBudgetLimit ? " · No budget limits" : null}
    </>
  );
}

function goalLimitDraftValue(value: number | null | undefined): string {
  return isPositiveGoalLimit(value) ? String(value) : "";
}

function budgetDraftFromGoal(goal: GoalSnapshot | null): GoalBudgetDraft {
  return {
    maxTurns: goalLimitDraftValue(goal?.budget.max_turns),
    maxMinutes: goalLimitDraftValue(goal?.budget.max_minutes),
    maxTokens: goalLimitDraftValue(goal?.budget.max_tokens),
  };
}

function parseBudgetDraftValue(value: string): number | undefined {
  const trimmed = value.trim();
  if (trimmed.length === 0) return undefined;
  const parsed = Number(trimmed);
  return Number.isInteger(parsed) && parsed > 0 ? parsed : undefined;
}

function budgetCommandFromDraft(draft: GoalBudgetDraft): GoalBudgetCommand {
  const budget: GoalBudgetCommand = {};
  const maxTurns = parseBudgetDraftValue(draft.maxTurns);
  const maxMinutes = parseBudgetDraftValue(draft.maxMinutes);
  const maxTokens = parseBudgetDraftValue(draft.maxTokens);

  if (maxTurns !== undefined) budget.max_turns = maxTurns;
  if (maxMinutes !== undefined) budget.max_minutes = maxMinutes;
  if (maxTokens !== undefined) budget.max_tokens = maxTokens;

  return budget;
}

function hasBudgetCommandLimits(budget: GoalBudgetCommand): boolean {
  return Object.keys(budget).length > 0;
}

const GOAL_STOPPABLE_STATUSES: ReadonlySet<GoalStatus> = new Set<GoalStatus>([
  "active",
  "verifying",
  "paused",
  "budget_exhausted",
  "no_progress",
]);

const GOAL_RESUMABLE_STATUSES: ReadonlySet<GoalStatus> = new Set<GoalStatus>([
  "paused",
  "budget_exhausted",
  "no_progress",
]);

function goalControlAvailability(goal: GoalSnapshot): {
  canPause: boolean;
  canResume: boolean;
  canStop: boolean;
  canCancel: boolean;
  canClear: boolean;
  canSetAutonomy: boolean;
} {
  if (hasModernGoalProjection(goal)) {
    const owned = goal.active && !goalIsTransferred(goal);
    const activeResolution = goalResolution(goal) === "active";
    return {
      canPause: owned && activeResolution && goalExecution(goal) !== "paused",
      canResume: owned && activeResolution && goalExecution(goal) === "paused",
      canStop: owned && activeResolution,
      canCancel: owned && activeResolution,
      canClear: owned,
      canSetAutonomy: owned && activeResolution,
    };
  }
  return {
    canPause: goal.active && goal.status === "active",
    canResume: GOAL_RESUMABLE_STATUSES.has(goal.status),
    canStop: GOAL_STOPPABLE_STATUSES.has(goal.status),
    canCancel: false,
    canClear: false,
    canSetAutonomy: false,
  };
}

type GoalControlDescriptor = {
  key: GoalControlAction;
  label: string;
  icon: LucideIcon;
  handler: () => void;
  disabled: boolean;
};

/**
 * Single source of truth for which goal controls exist, in which order, with
 * which label/icon/disabled state. Both the icon-only and the labelled control
 * rows consume this, keeping their own presentation markup.
 */
function goalControlActions(
  goal: GoalSnapshot,
  onControl: (action: GoalControlAction) => void,
): GoalControlDescriptor[] {
  const { canPause, canResume, canStop, canCancel, canClear } =
    goalControlAvailability(goal);
  if (!canPause && !canResume && !canStop && !canCancel && !canClear) {
    return [];
  }

  const actions: GoalControlDescriptor[] = [];
  if (canResume) {
    actions.push({
      key: "resume",
      label: "Resume",
      icon: Play,
      handler: () => onControl("resume"),
      disabled: false,
    });
  } else if (canPause || (!hasModernGoalProjection(goal) && canStop)) {
    actions.push({
      key: "pause",
      label: "Pause",
      icon: Pause,
      handler: () => onControl("pause"),
      disabled: !canPause,
    });
  }
  if (canStop) {
    actions.push({
      key: "stop",
      label: "Stop",
      icon: Square,
      handler: () => onControl("stop"),
      disabled: false,
    });
  }
  if (canCancel) {
    actions.push({
      key: "cancel",
      label: "Cancel goal",
      icon: Ban,
      handler: () => onControl("cancel"),
      disabled: false,
    });
  }
  if (canClear) {
    actions.push({
      key: "clear",
      label: "Clear goal",
      icon: Trash2,
      handler: () => onControl("clear"),
      disabled: false,
    });
  }
  return actions;
}

const GOAL_SUPPORTED_MODES = new Set(["agent", "task_agent", "task_planner"]);

function isGoalSupported(
  mode: string | undefined,
  toolUse: string | undefined,
): boolean {
  return (
    toolUse === "agent" ||
    (mode ? GOAL_SUPPORTED_MODES.has(mode.trim().toLowerCase()) : false)
  );
}

type StatusIconProps = {
  status: TodoStatus;
};

const StatusIcon: React.FC<StatusIconProps> = ({ status }) => {
  const dotState = getStatusDotState(status);
  return (
    <StatusDot
      state={dotState}
      size="small"
      tooltipText={STATUS_TOOLTIPS[status]}
    />
  );
};

type TaskRowProps = {
  task: TodoItem;
};

const TaskRow: React.FC<TaskRowProps> = ({ task }) => {
  const isActive = task.status === "in_progress";

  return (
    <Flex
      align="center"
      gap="2"
      className={classNames(styles.taskRow, { [styles.active]: isActive })}
    >
      <StatusIcon status={task.status} />
      <Text size="1" className={styles.taskText}>
        {task.content}
      </Text>
    </Flex>
  );
};

type GoalIndicatorProps = {
  goal: GoalSnapshot;
};

const GoalIndicator: React.FC<GoalIndicatorProps> = ({ goal }) => (
  <Flex align="center" gap="2" className={styles.goalIndicator}>
    <Icon icon={Target} size="sm" tone="accent" />
    <Text size="1" className={styles.goalIndicatorText}>
      Goal set
    </Text>
    <Badge tone={goalStatusTone(goal.status)} size="xs" variant="soft">
      {goalExecutionLabel(goal)}
    </Badge>
  </Flex>
);

const GoalStateDetails: React.FC<{ goal: GoalSnapshot }> = ({ goal }) => {
  if (!hasModernGoalProjection(goal)) {
    return (
      <Text size="1" color="gray">
        Legacy status: {humanizeIdentifier(goal.status)}
      </Text>
    );
  }
  return (
    <Flex direction="column" gap="1">
      <Text size="1" color="gray">
        Resolution: {humanizeIdentifier(goalResolution(goal))} · Execution:{" "}
        {goalExecutionLabel(goal)}
      </Text>
      <Text size="1" color="gray">
        Owner: {humanizeIdentifier(goal.owner ?? "user")} · Revision:{" "}
        {goal.revision ?? 1} · Autonomy:{" "}
        {humanizeIdentifier(goal.autonomy ?? "on")}
      </Text>
      {goal.stop_reason ? (
        <Text size="1" color="gray">
          Stop reason: {goal.stop_reason}
        </Text>
      ) : null}
      {goal.snoozed_until_ms != null ? (
        <Text size="1" color="gray">
          Snoozed until: {new Date(goal.snoozed_until_ms).toLocaleString()}
        </Text>
      ) : null}
      {goalIsQuiescent(goal) ? (
        <Text size="1" color="gray">
          Pursuit is quiescent and resumes on your next message.
        </Text>
      ) : null}
    </Flex>
  );
};

type GoalControlIconsProps = {
  goal: GoalSnapshot;
  onControl: (action: GoalControlAction) => void;
  className?: string;
};

const GoalControlIcons: React.FC<GoalControlIconsProps> = ({
  goal,
  onControl,
  className,
}) => {
  const actions = goalControlActions(goal, onControl);
  const visibleActions = actions.filter((action) =>
    ["pause", "resume", "stop"].includes(action.key),
  );

  if (visibleActions.length === 0) return null;

  return (
    <Flex
      align="center"
      gap="1"
      className={classNames(styles.goalControlIcons, className)}
    >
      {visibleActions.map((action) => (
        <IconButton
          key={action.key}
          aria-label={`${action.label} goal`}
          icon={action.icon}
          size="sm"
          variant={action.key === "stop" ? "danger" : "soft"}
          disabled={action.disabled}
          onClick={action.handler}
        />
      ))}
    </Flex>
  );
};

type GoalSectionProps = {
  goal: GoalSnapshot | null;
  expanded: boolean;
  onExpandedChange: (open: boolean) => void;
  onCreate: (content: string, budget?: GoalBudgetCommand) => void;
  onReplace: (content: string, budget?: GoalBudgetCommand) => void;
  onUpdateText: (content: string) => void;
  onSetAutonomy: (autonomy: GoalAutonomyCommand) => void;
  onApplyBudget: (budget: GoalBudgetCommand) => void;
  onControl: (action: GoalControlAction) => void;
};

const GoalSection: React.FC<GoalSectionProps> = ({
  goal,
  expanded,
  onExpandedChange,
  onCreate,
  onReplace,
  onUpdateText,
  onSetAutonomy,
  onApplyBudget,
  onControl,
}) => {
  const [draft, setDraft] = useState(goal?.content ?? "");
  const [budgetDraft, setBudgetDraft] = useState<GoalBudgetDraft>(() =>
    budgetDraftFromGoal(goal),
  );

  useEffect(() => {
    setDraft(goal?.content ?? "");
  }, [goal?.content]);

  const goalMaxTurns = goal?.budget.max_turns;
  const goalMaxMinutes = goal?.budget.max_minutes;
  const goalMaxTokens = goal?.budget.max_tokens;

  useEffect(() => {
    setBudgetDraft({
      maxTurns: goalLimitDraftValue(goalMaxTurns),
      maxMinutes: goalLimitDraftValue(goalMaxMinutes),
      maxTokens: goalLimitDraftValue(goalMaxTokens),
    });
  }, [goalMaxMinutes, goalMaxTokens, goalMaxTurns]);

  const [editingGoal, setEditingGoal] = useState(false);
  const [replacingGoal, setReplacingGoal] = useState(false);
  const trimmedDraft = draft.trim();
  const hasGoal = goal !== null;
  const canSave =
    trimmedDraft.length > 0 && trimmedDraft !== (goal?.content ?? "");
  const budgetCommand = useMemo(
    () => budgetCommandFromDraft(budgetDraft),
    [budgetDraft],
  );

  const handleSave = useCallback(() => {
    if (!canSave) return;
    if (hasGoal && replacingGoal) {
      onReplace(
        trimmedDraft,
        hasBudgetCommandLimits(budgetCommand) ? budgetCommand : undefined,
      );
      setEditingGoal(false);
      setReplacingGoal(false);
      return;
    }
    if (hasGoal) {
      onUpdateText(trimmedDraft);
      setEditingGoal(false);
      return;
    }
    onCreate(
      trimmedDraft,
      hasBudgetCommandLimits(budgetCommand) ? budgetCommand : undefined,
    );
  }, [
    budgetCommand,
    canSave,
    hasGoal,
    onCreate,
    onReplace,
    onUpdateText,
    replacingGoal,
    trimmedDraft,
  ]);

  const modernProjection = goal ? hasModernGoalProjection(goal) : false;
  const activeResolution = goal ? goalResolution(goal) === "active" : false;
  const canReplace = Boolean(
    goal && modernProjection && goal.active && !goalIsTransferred(goal),
  );
  const canEdit = Boolean(
    goal &&
      (!modernProjection || (activeResolution && goal.active)) &&
      !goalIsTransferred(goal),
  );

  const handleBudgetChange = useCallback(
    (field: keyof GoalBudgetDraft, value: string) => {
      setBudgetDraft((current) => ({ ...current, [field]: value }));
    },
    [],
  );

  const handleApplyBudget = useCallback(() => {
    onApplyBudget(budgetCommand);
  }, [budgetCommand, onApplyBudget]);

  return (
    <Collapsible.Root open={expanded} onOpenChange={onExpandedChange}>
      <Flex direction="column" gap="2" className={styles.goalBlock}>
        <Flex align="center" className={styles.goalHeaderRow}>
          <Collapsible.Trigger asChild>
            <button className={styles.goalHeader} type="button">
              <Flex align="center" gap="2" className={styles.goalHeaderContent}>
                <Icon icon={Target} size="sm" tone="accent" />
                <Text size="1" weight="medium" className={styles.goalTitle}>
                  Goal
                </Text>
                {goal ? (
                  <Badge
                    tone={goalStatusTone(goal.status)}
                    size="xs"
                    variant="soft"
                  >
                    {goalExecutionLabel(goal)}
                  </Badge>
                ) : (
                  <Badge tone="muted" size="xs" variant="soft">
                    Not set
                  </Badge>
                )}
              </Flex>
              <Chevron open={expanded} />
            </button>
          </Collapsible.Trigger>
          {goal && !expanded ? (
            <GoalControlIcons
              goal={goal}
              onControl={onControl}
              className={styles.goalHeaderRowControls}
            />
          ) : null}
        </Flex>

        <Collapsible.Content>
          <Flex direction="column" gap="3" className={styles.goalBody}>
            <Flex direction="column" gap="2">
              {goal && !editingGoal ? (
                <>
                  <Markdown>{goal.content}</Markdown>
                  <GoalStateDetails goal={goal} />
                  {goal.status === "transferred" && goal.transferred_to ? (
                    <Text size="1" color="gray">
                      Transferred to {goal.transferred_to}
                    </Text>
                  ) : null}
                </>
              ) : (
                <>
                  <label className={styles.goalLabel} htmlFor="task-goal-input">
                    Goal text
                  </label>
                  <textarea
                    className={styles.goalInput}
                    id="task-goal-input"
                    value={draft}
                    onChange={(event) => setDraft(event.currentTarget.value)}
                    placeholder="Set a goal for this thread"
                  />
                </>
              )}
              <GoalBudgetEditor
                draft={budgetDraft}
                showApply={hasGoal && (!modernProjection || activeResolution)}
                onApply={handleApplyBudget}
                onChange={handleBudgetChange}
              />
              <Flex align="center" justify="end" gap="3" wrap="wrap">
                <Text size="1" color="gray" className={styles.goalProgress}>
                  {goal ? (
                    <GoalBudgetProgress goal={goal} />
                  ) : (
                    "Save to start tracking a goal"
                  )}
                </Text>
                {goal && !editingGoal ? (
                  <>
                    {canReplace ? (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => {
                          setReplacingGoal(true);
                          setEditingGoal(true);
                        }}
                      >
                        Replace
                      </Button>
                    ) : null}
                    {canEdit ? (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => setEditingGoal(true)}
                      >
                        Edit
                      </Button>
                    ) : null}
                  </>
                ) : (
                  <>
                    {goal ? (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => {
                          setDraft(goal.content);
                          setEditingGoal(false);
                          setReplacingGoal(false);
                        }}
                      >
                        Cancel
                      </Button>
                    ) : null}
                    <Button
                      size="sm"
                      variant="primary"
                      disabled={!canSave}
                      onClick={handleSave}
                    >
                      {replacingGoal ? "Replace goal" : "Save"}
                    </Button>
                  </>
                )}
              </Flex>
            </Flex>

            {goal ? (
              <>
                <GoalControls
                  goal={goal}
                  onControl={onControl}
                  onSetAutonomy={onSetAutonomy}
                />
                <GoalAttempts attempts={goal.attempts} />
                <GoalEvents events={goal.events} />
              </>
            ) : null}
          </Flex>
        </Collapsible.Content>
      </Flex>
    </Collapsible.Root>
  );
};

type PlanSectionProps = {
  planState: ReturnType<typeof selectPlanBannerState>;
};

const PlanSection: React.FC<PlanSectionProps> = ({ planState }) => {
  const [expanded, setExpanded] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  const { base, synthesizedText, history } = planState;
  const metadata = useMemo(
    () => (base ? getPlanMetadata(base) : undefined),
    [base],
  );

  if (!base) return null;

  const version =
    metadata?.version !== undefined ? `v${metadata.version}` : "v?";
  const deltaCount = Math.max(0, history.length - 1);

  return (
    <>
      <Collapsible.Root open={expanded} onOpenChange={setExpanded}>
        <Flex
          direction="column"
          gap="2"
          className={styles.goalBlock}
          data-testid="task-plan-section"
        >
          <Flex align="center" className={styles.goalHeaderRow}>
            <Collapsible.Trigger asChild>
              <button className={styles.goalHeader} type="button">
                <Flex
                  align="center"
                  gap="2"
                  className={styles.goalHeaderContent}
                >
                  <Icon icon={ClipboardList} size="sm" tone="accent" />
                  <Text size="1" weight="medium" className={styles.goalTitle}>
                    Plan
                  </Text>
                  <Badge tone="muted" size="xs" variant="soft">
                    {version} · {deltaCount}{" "}
                    {deltaCount === 1 ? "update" : "updates"}
                  </Badge>
                </Flex>
                <Chevron open={expanded} />
              </button>
            </Collapsible.Trigger>
            <Button
              size="sm"
              variant="ghost"
              className={styles.planHistoryButton}
              onClick={() => setHistoryOpen(true)}
            >
              History
            </Button>
          </Flex>

          <Collapsible.Content>
            <Flex
              direction="column"
              gap="2"
              className={styles.goalBody}
              data-testid="task-plan-body"
            >
              <Text size="1" color="gray">
                Execution route and updates, not the goal.
              </Text>
              <Box className={styles.planContent}>
                <Markdown>
                  {normalizePlanContent(synthesizedText ?? base.content)}
                </Markdown>
              </Box>
            </Flex>
          </Collapsible.Content>
        </Flex>
      </Collapsible.Root>
      <PlanHistoryModal
        open={historyOpen}
        onOpenChange={setHistoryOpen}
        items={history}
      />
    </>
  );
};

type GoalBudgetEditorProps = {
  draft: GoalBudgetDraft;
  showApply: boolean;
  onApply: () => void;
  onChange: (field: keyof GoalBudgetDraft, value: string) => void;
};

const GoalBudgetEditor: React.FC<GoalBudgetEditorProps> = ({
  draft,
  showApply,
  onApply,
  onChange,
}) => (
  <fieldset className={styles.goalBudgetGroup}>
    <legend className={styles.goalBudgetLegend}>
      Budget limits (optional — leave blank for unlimited)
    </legend>
    <div className={styles.goalBudgetGrid}>
      <label className={styles.goalBudgetField}>
        <span className={styles.goalBudgetLabel}>Max turns</span>
        <input
          className={styles.goalBudgetInput}
          inputMode="numeric"
          min={GOAL_BUDGET_INPUT_MIN}
          step={GOAL_BUDGET_INPUT_STEP}
          type="number"
          value={draft.maxTurns}
          onChange={(event) => onChange("maxTurns", event.currentTarget.value)}
        />
      </label>
      <label className={styles.goalBudgetField}>
        <span className={styles.goalBudgetLabel}>Max minutes</span>
        <input
          className={styles.goalBudgetInput}
          inputMode="numeric"
          min={GOAL_BUDGET_INPUT_MIN}
          step={GOAL_BUDGET_INPUT_STEP}
          type="number"
          value={draft.maxMinutes}
          onChange={(event) =>
            onChange("maxMinutes", event.currentTarget.value)
          }
        />
      </label>
      <label className={styles.goalBudgetField}>
        <span className={styles.goalBudgetLabel}>Max tokens</span>
        <input
          className={styles.goalBudgetInput}
          inputMode="numeric"
          min={GOAL_BUDGET_INPUT_MIN}
          step={GOAL_BUDGET_INPUT_STEP}
          type="number"
          value={draft.maxTokens}
          onChange={(event) => onChange("maxTokens", event.currentTarget.value)}
        />
      </label>
    </div>
    {showApply ? (
      <Flex justify="end" className={styles.goalBudgetActions}>
        <Button size="sm" variant="soft" onClick={onApply}>
          Apply budget
        </Button>
      </Flex>
    ) : null}
  </fieldset>
);

type GoalControlsProps = {
  goal: GoalSnapshot;
  onControl: (action: GoalControlAction) => void;
  onSetAutonomy: (autonomy: GoalAutonomyCommand) => void;
};

const GoalControls: React.FC<GoalControlsProps> = ({
  goal,
  onControl,
  onSetAutonomy,
}) => {
  const actions = goalControlActions(goal, onControl);
  const { canSetAutonomy } = goalControlAvailability(goal);

  if (actions.length === 0 && !canSetAutonomy) return null;

  return (
    <Flex align="center" gap="2" wrap="wrap" className={styles.goalControls}>
      {actions.map((action) => (
        <Button
          key={action.key}
          size="sm"
          variant={
            ["stop", "cancel", "clear"].includes(action.key) ? "danger" : "soft"
          }
          leftIcon={action.icon}
          disabled={action.disabled}
          onClick={action.handler}
        >
          {action.label}
        </Button>
      ))}
      {canSetAutonomy ? (
        <Button
          size="sm"
          variant="soft"
          onClick={() => onSetAutonomy(goal.autonomy === "on" ? "off" : "on")}
        >
          Turn autonomy {goal.autonomy === "on" ? "off" : "on"}
        </Button>
      ) : null}
    </Flex>
  );
};

type GoalAttemptsProps = {
  attempts: GoalAttempt[];
};

const GoalAttempts: React.FC<GoalAttemptsProps> = ({ attempts }) => {
  if (attempts.length === 0) {
    return (
      <Text size="1" color="gray">
        No verifier attempts yet
      </Text>
    );
  }

  return (
    <Flex direction="column" gap="2">
      <Text size="1" weight="medium">
        Verifier attempts
      </Text>
      <Flex direction="column" gap="2" className={styles.attemptList}>
        {attempts.map((attempt) => (
          <div
            className={styles.attemptCard}
            key={`${attempt.at_ms}-${attempt.trigger}`}
          >
            <Flex align="center" gap="2" wrap="wrap">
              <Badge tone="accent" size="xs" variant="soft">
                {humanizeIdentifier(attempt.verdict)}
              </Badge>
              <Text size="1" color="gray">
                {humanizeIdentifier(attempt.trigger)}
              </Text>
            </Flex>
            {attempt.gaps.length > 0 ? (
              <ul className={styles.gapList}>
                {attempt.gaps.map((gap) => (
                  <li key={gap}>{gap}</li>
                ))}
              </ul>
            ) : null}
            <Text as="div" size="1" className={styles.verifierReply}>
              {attempt.verifier_reply}
            </Text>
          </div>
        ))}
      </Flex>
    </Flex>
  );
};

type GoalEventsProps = {
  events: GoalEvent[];
};

const GoalEvents: React.FC<GoalEventsProps> = ({ events }) => {
  if (events.length === 0) {
    return (
      <Text size="1" color="gray">
        No goal events yet
      </Text>
    );
  }

  return (
    <Flex direction="column" gap="2">
      <Text size="1" weight="medium">
        Goal events
      </Text>
      <div className={styles.eventList}>
        {events.map((event) => (
          <div
            className={styles.eventRow}
            key={`${event.at_ms}-${event.kind}-${event.text}`}
          >
            <Badge tone="muted" size="xs" variant="soft">
              {humanizeIdentifier(event.kind)}
            </Badge>
            <Text size="1" className={styles.eventText}>
              {event.text}
            </Text>
          </div>
        ))}
      </div>
    </Flex>
  );
};

export const TaskProgressWidget: React.FC = () => {
  const dispatch = useAppDispatch();
  const chatId = useThreadId();
  const hasTasks = useAppSelector((state) => selectHasTasksById(state, chatId));
  const everUsed = useAppSelector((state) =>
    selectTasksEverUsedById(state, chatId),
  );
  const tasks = useAppSelector((state) =>
    selectCurrentTasksById(state, chatId),
  );
  const goal = useAppSelector((state) => selectGoalById(state, chatId));
  const planState = useAppSelector((state) =>
    selectPlanBannerState(state, chatId),
  );
  const isExpanded = useAppSelector((state) =>
    selectTaskWidgetExpandedById(state, chatId),
  );
  const goalExpanded = useAppSelector((state) =>
    selectTaskGoalExpandedById(state, chatId),
  );
  const threadMode = useAppSelector((state) =>
    selectThreadModeById(state, chatId),
  );
  const threadToolUse = useAppSelector((state) =>
    selectThreadToolUseById(state, chatId),
  );
  const { done, total, activeTitle } = useAppSelector((state) =>
    selectTaskProgressById(state, chatId),
  );
  const { setGoal, setGoalBudget, updateGoal, setGoalAutonomy, controlGoal } =
    useChatActions(chatId);
  const [goalActionError, setGoalActionError] = useState<string | null>(null);
  const hasGoal = hasGoalWork(goal);
  const goalSupported = isGoalSupported(threadMode, threadToolUse);
  const isFreshGoalOpportunity =
    !everUsed && !hasTasks && !hasGoal && goalSupported;
  const isTasksCleared = everUsed && !hasTasks && !hasGoal;
  const shouldRender = everUsed || hasGoal || goalSupported;

  const crashSummary = useMemo(() => {
    const taskSummary =
      total > 0 ? `${done}/${total} active=${activeTitle ?? "none"}` : null;
    const goalSummary = goal ? `goal=${goal.status}` : null;
    return [taskSummary, goalSummary].filter(Boolean).join(" ") || null;
  }, [activeTitle, done, goal, total]);

  useEffect(() => {
    setBuddyCrashHotSlot("tasks", crashSummary);
    if (crashSummary) {
      addBuddyCrashBreadcrumb("tasks", crashSummary);
    }
  }, [crashSummary]);

  const handleOpenChange = useCallback(
    (open: boolean) => {
      if (chatId) {
        dispatch(setTaskWidgetExpanded({ id: chatId, expanded: open }));
      }
    },
    [dispatch, chatId],
  );

  const handleGoalOpenChange = useCallback(
    (open: boolean) => {
      if (chatId) {
        dispatch(setTaskGoalExpanded({ id: chatId, expanded: open }));
      }
    },
    [dispatch, chatId],
  );

  const handleGoalCreate = useCallback(
    (content: string, budget?: GoalBudgetCommand) => {
      setGoalActionError(null);
      void setGoal(content, budget).catch((error: unknown) => {
        setGoalActionError(
          error instanceof Error ? error.message : String(error),
        );
      });
    },
    [setGoal],
  );

  const handleGoalReplace = useCallback(
    (content: string, budget?: GoalBudgetCommand) => {
      setGoalActionError(null);
      void setGoal(content, budget, true).catch((error: unknown) => {
        setGoalActionError(
          error instanceof Error ? error.message : String(error),
        );
      });
    },
    [setGoal],
  );

  const handleGoalTextUpdate = useCallback(
    (content: string) => {
      setGoalActionError(null);
      void updateGoal(content).catch((error: unknown) => {
        setGoalActionError(
          error instanceof Error ? error.message : String(error),
        );
      });
    },
    [updateGoal],
  );

  const handleGoalBudgetApply = useCallback(
    (budget: GoalBudgetCommand) => {
      setGoalActionError(null);
      void setGoalBudget(budget).catch((error: unknown) => {
        setGoalActionError(
          error instanceof Error ? error.message : String(error),
        );
      });
    },
    [setGoalBudget],
  );

  const handleGoalControl = useCallback(
    (action: GoalControlAction) => {
      setGoalActionError(null);
      void controlGoal(action).catch((error: unknown) => {
        setGoalActionError(
          error instanceof Error ? error.message : String(error),
        );
      });
    },
    [controlGoal],
  );

  const handleGoalAutonomy = useCallback(
    (autonomy: GoalAutonomyCommand) => {
      setGoalActionError(null);
      void setGoalAutonomy(autonomy).catch((error: unknown) => {
        setGoalActionError(
          error instanceof Error ? error.message : String(error),
        );
      });
    },
    [setGoalAutonomy],
  );

  if (!shouldRender) return null;

  return (
    <Box className={styles.widget}>
      <Collapsible.Root open={isExpanded} onOpenChange={handleOpenChange}>
        <Flex align="center" className={styles.headerRow}>
          <Collapsible.Trigger asChild>
            <button className={styles.header} type="button">
              <Flex align="center" gap="3" className={styles.headerInner}>
                <Flex align="center" gap="2" className={styles.headerMain}>
                  {!isExpanded && hasGoal && goal ? (
                    <GoalIndicator goal={goal} />
                  ) : null}

                  {!isExpanded && hasTasks ? (
                    <>
                      <Flex gap="1" align="center">
                        {tasks.map((task) => (
                          <StatusIcon key={task.id} status={task.status} />
                        ))}
                      </Flex>

                      <CircularProgress done={done} total={total} />

                      {activeTitle ? (
                        <Text
                          size="1"
                          color="gray"
                          className={styles.activeHint}
                        >
                          {activeTitle}
                        </Text>
                      ) : null}
                    </>
                  ) : null}

                  {!isExpanded && isFreshGoalOpportunity ? (
                    <Flex
                      align="center"
                      gap="2"
                      className={styles.goalAffordance}
                    >
                      <Icon icon={Target} size="sm" tone="accent" />
                      <Text
                        size="1"
                        weight="medium"
                        className={styles.goalAffordanceText}
                      >
                        Set a goal
                      </Text>
                    </Flex>
                  ) : null}

                  {!isExpanded && isTasksCleared ? (
                    <Text size="1" color="gray">
                      Tasks cleared
                    </Text>
                  ) : null}

                  {isExpanded ? (
                    <Text size="1" weight="medium">
                      Task Progress
                    </Text>
                  ) : null}
                </Flex>

                <Chevron open={isExpanded} />
              </Flex>
            </button>
          </Collapsible.Trigger>
          {!isExpanded && hasGoal && goal ? (
            <GoalControlIcons
              goal={goal}
              onControl={handleGoalControl}
              className={styles.headerRowControls}
            />
          ) : null}
        </Flex>

        <Collapsible.Content>
          <Flex direction="column" gap="3" className={styles.content}>
            {goalActionError ? (
              <Text size="1" color="red" role="alert">
                Goal command failed: {goalActionError}
              </Text>
            ) : null}
            <GoalSection
              goal={goal}
              expanded={goalExpanded}
              onExpandedChange={handleGoalOpenChange}
              onCreate={handleGoalCreate}
              onReplace={handleGoalReplace}
              onUpdateText={handleGoalTextUpdate}
              onSetAutonomy={handleGoalAutonomy}
              onApplyBudget={handleGoalBudgetApply}
              onControl={handleGoalControl}
            />

            <PlanSection key={chatId} planState={planState} />

            {hasTasks ? (
              <>
                <div className={styles.taskList}>
                  {tasks.map((task) => (
                    <div key={task.id} className={styles.taskRowEnter}>
                      <TaskRow task={task} />
                    </div>
                  ))}
                </div>
                <Separator size="4" />
                <Text size="1" color="gray">
                  {done}/{total} completed
                </Text>
              </>
            ) : (
              <Text size="1" color="gray">
                No active tasks
              </Text>
            )}
          </Flex>
        </Collapsible.Content>
      </Collapsible.Root>
    </Box>
  );
};

export default TaskProgressWidget;
