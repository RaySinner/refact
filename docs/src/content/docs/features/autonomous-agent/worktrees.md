---
title: Worktrees
description: Use isolated git worktrees with Refact Agent chats and task agents.
---

Worktrees let Refact run a chat or task agent in a separate git checkout and branch. Changes stay outside your main workspace until you review, merge, or delete the worktree.

## What worktree scope changes

- Relative file, search, tree, patch, and shell paths resolve inside the active worktree.
- Absolute paths inside the source checkout are remapped to the matching path in the active worktree and the tool output shows a visible notice.
- Absolute paths already inside the active worktree are allowed and the tool output shows that an absolute path was used.
- Absolute paths outside both the worktree and source checkout are only allowed when the existing privacy policy permits them. Tool output shows a strong warning because the operation used content outside the active worktree.
- Privacy-blocked outside paths are rejected.

## Shell limitation

In a worktree-scoped chat, Refact enforces the shell tool's default cwd and explicit `workdir`. The command text itself is not OS-sandboxed. A command can still reference absolute paths, network resources, external tools, or processes if the operating system allows it, so review shell commands before approving them.

## Opening a worktree

Use Open in new window from the worktree menu when the IDE host supports opening folders. If the host cannot open folders directly, Refact copies the worktree path so you can open it manually.

## Shared worktrees

Multiple chats can attach to the same worktree. They share the same branch, files, status, and diff. Merging, deleting, discarding, or cleaning a shared worktree affects every chat that references it, and the UI shows the reference count before destructive actions.

## Stale or deleted worktrees

A worktree can become stale when its directory is removed outside Refact or no longer looks like a git worktree. Stale and deleted worktrees are shown as missing, stale, or deleted. Detach the chat from the stale worktree, recreate it, or clean it up intentionally.

## Task agents and subagents

Task agents use strict worktree isolation. If Refact cannot create the task-agent worktree, the spawn fails instead of editing the main workspace. Subagents inherit the active worktree scope from the parent chat.

## Merge and conflict behavior

Before merging, review the worktree diff. You can squash merge or regular merge into the target branch. If conflicts are detected, Refact reports the conflicting files, keeps the worktree, and does not delete it. Use the Ask Refact action to request conflict-resolution help.

## Checkpoints

Checkpoints created inside a worktree apply to that worktree root. Restoring a worktree checkpoint does not roll back the main workspace checkout.

## Buddy cleanup safety

Buddy can report worktree inventory, including clean, dirty, stale, conflicted, shared, and abandoned-clean counts. Buddy never deletes worktrees automatically. Cleanup requires explicit selected worktree IDs, and the default cleanup path only deletes clean, old, unshared candidates. Dirty, shared, conflicted, too-recent, and unsafe branch-deletion cases are skipped unless the user explicitly changes the cleanup request.
