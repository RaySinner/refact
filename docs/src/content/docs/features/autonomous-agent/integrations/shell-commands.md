---
title: Shell Tool
description: Run one-off shell commands with confirmations and output filtering.
---

The built-in shell tool lets agent modes run local commands such as tests, builds, linters, formatters, and diagnostics. It is intended for one-off commands that finish and return stdout and stderr.

## Behavior

- Commands run in a shell on the user's machine.
- The default working directory is the workspace unless the tool call specifies another allowed directory.
- Commands have a timeout.
- Output can be limited, filtered by regex, and focused on the beginning or end of large output.
- Results are shown in chat for the agent and user to inspect.

## Confirmation rules

Shell commands can be controlled with allow, ask, and deny rules. Keep confirmation enabled for destructive operations such as deleting files, changing remotes, installing packages globally, or touching credentials.

## When to use shell vs integrations

Use shell for project-local commands that are not worth turning into a reusable tool. Use command-line tool integrations for repeated commands with structured parameters. Use command-line service integrations for long-running processes.

## Advanced Configuration Options

### Output Filter

Controls how the output of executed commands is processed and displayed:

#### Basic Limits

- **Limit Lines**: Restricts the output to a specified number of lines (e.g., 100).
- **Limit Characters**: Restricts the output to a maximum number of characters (e.g., 10,000).

#### Output Processing

- **Valuable Top or Bottom**: Determines whether the tool prioritizes the start (top) or end (bottom) of the output for relevance.
- **Grep**: Uses a regular expression (e.g., `(?i)error`) to filter and highlight specific content in the output.
- **Grep Context Lines**: Defines the number of surrounding lines to include with matches from the grep filter.
- **Remove from Output**: Allows removing unwanted patterns or content from the displayed output.

These settings help manage large or verbose outputs, focusing only on the most critical information.

## Worktree-scoped chats

When a chat is attached to a worktree, the shell tool runs from the active worktree by default. If a command sets `workdir`, Refact resolves it through the same worktree path policy used by file tools:

- source-checkout absolute paths are remapped into the active worktree and shown in tool output;
- privacy-permitted outside absolute paths are allowed with a strong warning;
- privacy-blocked outside paths are rejected.

This is cwd/workdir enforcement, not an operating-system sandbox. The shell command text can still reference absolute paths, external tools, network resources, or processes if the OS allows it.
