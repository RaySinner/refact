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
    card.agent_worktree_name ??
    record?.meta.id ??
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
  return !worktree.legacy && !worktree.stale && worktree.id.trim().length > 0;
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
  const record = byName ?? byThread ?? byCard ?? byBranch;
  const meta = record?.meta ?? threadWorktree ?? null;
  const id = record?.meta.id ?? threadWorktree?.id ?? card.agent_worktree_name;
  if (!id) {
    if (card.agent_worktree ?? card.agent_branch) {
      return makeLegacyTarget(card, threadWorktree);
    }
    return null;
  }
  const label = worktreeLabel(card, record, meta);
  if (!label) return null;
  return {
    id,
    label: formatWorktreeTargetLabel(label),
    record,
    meta,
    legacy: false,
    stale:
      record?.status.path_exists === false ||
      record?.meta.lifecycle_state === "deleted" ||
      meta?.deleted === true ||
      meta?.stale === true,
    referenceCount: record?.reference_count ?? meta?.reference_count,
  };
}
