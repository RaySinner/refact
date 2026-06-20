import type { BoardCard } from "../../services/refact/tasks";
import type { WorktreeMeta, WorktreeRecordView } from "../../services/refact";

export type CardWorktreeTarget = {
  id: string;
  label: string;
  record?: WorktreeRecordView;
  meta?: WorktreeMeta | null;
  legacy: boolean;
  stale: boolean;
  referenceCount?: number;
};

function compactPath(path: string): string {
  const normalized = path.replace(/[\\/]+$/, "");
  const parts = normalized.split(/[\\/]/).filter(Boolean);
  if (parts.length <= 2) return normalized || path;
  return parts.slice(-2).join("/");
}

export function worktreeLabel(
  card: BoardCard,
  record?: WorktreeRecordView,
  meta?: WorktreeMeta | null,
): string | null {
  return (
<<<<<<< HEAD
    card.agent_worktree_name ??
    record?.meta.id ??
=======
    record?.meta.id ??
    meta?.id ??
    card.agent_worktree_name ??
>>>>>>> upstream/main
    card.agent_branch ??
    record?.meta.branch ??
    meta?.branch ??
    record?.meta.root ??
    meta?.root ??
    card.agent_worktree ??
    null
  );
}

function formatWorktreeTargetLabel(label: string): string {
  return label.includes("/") || label.includes("\\")
    ? compactPath(label)
    : label;
}

function makeLegacyTarget(
  card: BoardCard,
  threadWorktree?: WorktreeMeta | null,
): CardWorktreeTarget | null {
  const label = worktreeLabel(card, undefined, threadWorktree);
  if (!label) return null;
  return {
    id: "",
    label: formatWorktreeTargetLabel(label),
    meta: threadWorktree ?? null,
    legacy: true,
    stale: threadWorktree?.deleted === true || threadWorktree?.stale === true,
    referenceCount: threadWorktree?.reference_count,
  };
}

export function isActionableWorktree(worktree: CardWorktreeTarget): boolean {
<<<<<<< HEAD
  return !worktree.legacy && !worktree.stale && worktree.id.trim().length > 0;
=======
  return (
    Boolean(worktree.record) &&
    !worktree.legacy &&
    !worktree.stale &&
    worktree.id.trim().length > 0
  );
}

function referenceMatchesCard(
  taskId: string,
  card: BoardCard,
  record: WorktreeRecordView,
): boolean {
  return record.references.some((reference) => {
    if (reference.task_id === taskId && reference.card_id === card.id) {
      return true;
    }
    if (card.agent_chat_id && reference.chat_id === card.agent_chat_id) {
      return true;
    }
    return Boolean(
      card.assignee &&
        reference.task_id === taskId &&
        reference.agent_id === card.assignee,
    );
  });
}

function recordIsStale(record: WorktreeRecordView): boolean {
  return (
    !record.status.path_exists ||
    !record.status.is_git_worktree ||
    record.status.lifecycle_state === "deleted" ||
    record.status.lifecycle_state === "missing" ||
    record.status.lifecycle_state === "stale" ||
    record.status.deleted === true ||
    record.status.stale === true ||
    record.meta.lifecycle_state === "deleted" ||
    record.meta.lifecycle_state === "missing" ||
    record.meta.lifecycle_state === "stale" ||
    record.meta.deleted === true ||
    record.meta.stale === true
  );
>>>>>>> upstream/main
}

export function resolveCardWorktree(
  taskId: string,
  card: BoardCard,
  records: WorktreeRecordView[],
  threadWorktree?: WorktreeMeta | null,
): CardWorktreeTarget | null {
  const byName = card.agent_worktree_name
    ? records.find((record) => record.meta.id === card.agent_worktree_name)
    : undefined;
  const byThread = threadWorktree
    ? records.find((record) => record.meta.id === threadWorktree.id)
    : undefined;
<<<<<<< HEAD
=======
  const byReference = records.find((record) =>
    referenceMatchesCard(taskId, card, record),
  );
>>>>>>> upstream/main
  const byCard = records.find(
    (record) =>
      record.meta.task_id === taskId && record.meta.card_id === card.id,
  );
  const byBranch = card.agent_branch
    ? records.find(
        (record) =>
          record.meta.branch === card.agent_branch &&
          (!record.meta.task_id || record.meta.task_id === taskId),
      )
    : undefined;
<<<<<<< HEAD
  const record = byName ?? byThread ?? byCard ?? byBranch;
  const meta = record?.meta ?? threadWorktree ?? null;
  const id = record?.meta.id ?? threadWorktree?.id ?? card.agent_worktree_name;
=======
  const record = byName ?? byThread ?? byReference ?? byCard ?? byBranch;
  const meta = record?.meta ?? threadWorktree ?? null;
  const id = record?.meta.id ?? threadWorktree?.id ?? card.agent_worktree_name;
  const label = worktreeLabel(card, record, meta);
  if (!label) return null;
  if (!record && id) {
    return {
      id,
      label: formatWorktreeTargetLabel(label),
      meta,
      legacy: false,
      stale: true,
      referenceCount: meta?.reference_count,
    };
  }
>>>>>>> upstream/main
  if (!id) {
    if (card.agent_worktree ?? card.agent_branch) {
      return makeLegacyTarget(card, threadWorktree);
    }
    return null;
  }
<<<<<<< HEAD
  const label = worktreeLabel(card, record, meta);
  if (!label) return null;
=======
>>>>>>> upstream/main
  return {
    id,
    label: formatWorktreeTargetLabel(label),
    record,
    meta,
    legacy: false,
<<<<<<< HEAD
    stale:
      record?.status.path_exists === false ||
      record?.meta.lifecycle_state === "deleted" ||
      meta?.deleted === true ||
      meta?.stale === true,
=======
    stale: record ? recordIsStale(record) : true,
>>>>>>> upstream/main
    referenceCount: record?.reference_count ?? meta?.reference_count,
  };
}
