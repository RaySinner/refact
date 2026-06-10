import React from "react";
import { Badge } from "@radix-ui/themes";
import type { WorktreeMeta, WorktreeRecordView } from "../../services/refact";
import styles from "./Worktrees.module.css";

type WorktreeStatusBadgeProps = {
  worktree?: WorktreeMeta | null;
  record?: WorktreeRecordView | null;
  additions?: number | null;
  deletions?: number | null;
};

function hasDiffStats(
  additions?: number | null,
  deletions?: number | null,
): boolean {
  return (additions ?? 0) > 0 || (deletions ?? 0) > 0;
}

function DiffStats({
  additions,
  deletions,
}: {
  additions?: number | null;
  deletions?: number | null;
}) {
  if (!hasDiffStats(additions, deletions)) return null;
  const added = additions ?? 0;
  const removed = deletions ?? 0;
  return (
    <span className={styles.diffStatsBadge}>
      <span>(</span>
      <span className={styles.diffStatsAdd}>+{added}</span>
      <span className={styles.diffStatsRemove}>-{removed}</span>
      <span>)</span>
    </span>
  );
}

export const WorktreeStatusBadge: React.FC<WorktreeStatusBadgeProps> = ({
  worktree,
  record,
  additions,
  deletions,
}) => {
  const status = record?.status ?? worktree?.status ?? null;
  const lifecycle = record?.meta.lifecycle_state ?? worktree?.lifecycle_state;

  if (
    lifecycle === "deleted" ||
    worktree?.deleted === true ||
    status?.deleted === true
  ) {
    return (
      <Badge size="1" color="red" variant="soft">
        deleted
      </Badge>
    );
  }

  if (lifecycle === "missing" || status?.path_exists === false) {
    return (
      <Badge size="1" color="red" variant="soft">
        missing
      </Badge>
    );
  }

  if (lifecycle === "conflicted" || status?.conflicted === true) {
    return (
      <Badge size="1" color="amber" variant="soft">
        conflicted
      </Badge>
    );
  }

  if (
    lifecycle === "stale" ||
    worktree?.stale === true ||
    status?.stale === true
  ) {
    return (
      <Badge size="1" color="amber" variant="soft">
        stale
      </Badge>
    );
  }

  if (status?.dirty === true) {
    return (
      <Badge size="1" color="amber" variant="soft">
        dirty <DiffStats additions={additions} deletions={deletions} />
      </Badge>
    );
  }

  return (
    <Badge size="1" color="green" variant="soft">
      worktree <DiffStats additions={additions} deletions={deletions} />
    </Badge>
  );
};

WorktreeStatusBadge.displayName = "WorktreeStatusBadge";
