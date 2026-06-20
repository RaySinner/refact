import React from "react";
<<<<<<< HEAD
import { Badge } from "@radix-ui/themes";
=======
import { Badge, StatusDot, type StatusDotStatus } from "../../components/ui";
>>>>>>> upstream/main
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

<<<<<<< HEAD
=======
function statusBadge(
  label: string,
  tone: React.ComponentProps<typeof Badge>["tone"],
  status: StatusDotStatus,
  additions?: number | null,
  deletions?: number | null,
) {
  return (
    <Badge tone={tone} className={styles.statusBadge}>
      <StatusDot status={status} size="small" />
      {label} <DiffStats additions={additions} deletions={deletions} />
    </Badge>
  );
}

>>>>>>> upstream/main
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
<<<<<<< HEAD
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
=======
    return statusBadge("deleted", "danger", "error");
  }

  if (lifecycle === "missing" || status?.path_exists === false) {
    return statusBadge("missing", "danger", "error");
  }

  if (lifecycle === "conflicted" || status?.conflicted === true) {
    return statusBadge("conflicted", "warning", "warning");
>>>>>>> upstream/main
  }

  if (
    lifecycle === "stale" ||
    worktree?.stale === true ||
    status?.stale === true
  ) {
<<<<<<< HEAD
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
=======
    return statusBadge("stale", "warning", "warning");
  }

  if (status?.dirty === true) {
    return statusBadge("dirty", "warning", "warning", additions, deletions);
  }

  return statusBadge("worktree", "success", "success", additions, deletions);
>>>>>>> upstream/main
};

WorktreeStatusBadge.displayName = "WorktreeStatusBadge";
