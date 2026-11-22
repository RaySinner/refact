import type { DisplayItem } from "../ChatContentDisplayItems";
import {
  decodeMessageCreatedAtMs,
  epochMsOrNull,
} from "../../../services/refact/messageTimestamps";
import { eventTimestampMs } from "../EventRow/eventTimestamp";

/**
 * Who wrote a transcript item, as the engine recorded it.
 *
 * The stamp arrives in `ChatMessage.extra.provenance` (see the engine's
 * `message_provenance.rs`). Only the engine knows the truth: a room peer is a plain
 * chat session whose role lives in the card's `team_members`, and nothing else on the
 * wire carries that. So the GUI reads the stamp instead of guessing an author from a
 * scalar `assignee`, which cannot tell two neighbours apart.
 */
export type RoomAuthor = {
  chatId: string;
  role: string;
  displayName: string;
};

export type RoomAttribution = {
  author: RoomAuthor;
  /** Exact `HH:MM:SS`, or `""` when no trustworthy instant is known. */
  timeLabel: string;
  /** A hairline above this item, because the author changed since the previous one. */
  divider: boolean;
};

export type AttributedItem<T> = {
  /** Lifted so the virtualized list keeps its identity when attribution changes. */
  key: string;
  item: T;
  attribution: RoomAttribution | null;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nonEmptyString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

/**
 * The stamped author of a message, or `null` when there is none.
 *
 * `null` is the common, healthy case: a plain conversation and a lone agent card carry
 * no provenance, and a transcript must not invent a byline for them. An author with
 * nothing but a chat id is still an author — that is exactly the planner or a peer the
 * engine could not name a role for — so it is kept rather than dropped.
 */
export function messageAuthor(
  extra: Record<string, unknown> | undefined,
): RoomAuthor | null {
  if (!extra) return null;
  const raw = extra.provenance;
  if (!isRecord(raw)) return null;

  const chatId = nonEmptyString(raw.chat_id);
  const role = nonEmptyString(raw.role) ?? "";
  const displayName = nonEmptyString(raw.display_name) ?? role;
  if (chatId === null && role === "" && displayName === "") return null;

  return { chatId: chatId ?? "", role, displayName };
}

/**
 * The instant a message was stamped, in epoch milliseconds.
 *
 * `extra.created_at_ms` is the engine's record of when the message was created, so it
 * wins over the delivery stamp (`extra.delivery.at_ms`), which only records when the
 * message entered a chat. The bare `at_ms` / `timestamp_ms` keys are accepted as
 * fallbacks because plan, goal and event metadata already use them.
 *
 * Returns `null` rather than guessing: a wrong clock is worse than no clock.
 */
export function messageTimestampMs(
  extra: Record<string, unknown> | undefined,
): number | null {
  if (!extra) return null;

  if ("created_at_ms" in extra) {
    return epochMsOrNull(extra.created_at_ms);
  }

  const delivery = extra.delivery;
  if (isRecord(delivery)) {
    const atMs = epochMsOrNull(delivery.at_ms);
    if (atMs !== null) return atMs;
  }

  for (const key of ["at_ms", "timestamp_ms"]) {
    const value = epochMsOrNull(extra[key]);
    if (value !== null) return value;
  }

  return null;
}

/**
 * The `extra` map of whichever message an item wraps, or `null` for items that render
 * no message of their own (a plain-text or error card synthesised from one).
 */
export function displayItemExtra(item: DisplayItem): unknown {
  const record = item as unknown as Record<string, unknown>;
  for (const key of ["message", "event"]) {
    const value = record[key];
    if (!isRecord(value)) continue;
    const extra = value.extra;
    const extraRecord = isRecord(extra) ? extra : undefined;
    const createdAtMs = decodeMessageCreatedAtMs({
      created_at_ms: value.created_at_ms,
      extra: extraRecord,
    });
    if (createdAtMs === null) {
      if ("extra" in value) return extra;
      continue;
    }
    const nested = epochMsOrNull(extraRecord?.created_at_ms);
    if (extraRecord && nested === createdAtMs) return extra;
    return { ...(extraRecord ?? {}), created_at_ms: createdAtMs };
  }
  if ("rawExtra" in record) return record.rawExtra;
  return null;
}

export function displayItemTimestampFallbackMs(
  item: DisplayItem,
  firstSeenMs: number | null,
): number | null {
  if (item.type === "event") return eventTimestampMs(item.event);
  if (item.type !== "assistant") return firstSeenMs;

  const toolCalls = [
    ...(item.message.tool_calls ?? []),
    ...(item.message.server_executed_tools ?? []),
  ];
  if (toolCalls.length === 0) return firstSeenMs;

  let startedAtMs: number | null = null;
  for (const toolCall of toolCalls) {
    const candidate = epochMsOrNull(toolCall.started_at_ms);
    if (candidate === null) continue;
    if (startedAtMs === null || candidate < startedAtMs) {
      startedAtMs = candidate;
    }
  }
  return startedAtMs;
}

/**
 * Pair every rendered item with the attribution it deserves.
 *
 * The divider is decided against the previous *attributed* item, not the previous item
 * of any kind: tool cards and event rows sit between two of an agent's messages, and a
 * line at every one of those would shred the very grouping the byline exists to create.
 *
 * `formatTime` is injected so the pure logic here carries no locale and the component
 * decides how a clock is rendered.
 */
/**
 * Pair every rendered item with the attribution it deserves.
 *
 * `resolveAuthor` is the escape hatch for transcripts without a stamped
 * `extra.provenance` — a plain user/assistant conversation. When the engine has
 * not named an author, the caller supplies a role-derived byline so the message
 * still gets a name and a clock. `null` keeps an item frame-less.
 */
export function roomAttributionForItems<T>(
  items: readonly T[],
  keyOf: (item: T) => string,
  extraOf: (item: T) => unknown,
  formatTime: (epochMs: number) => string,
  resolveTimestamp?: (extra: unknown, item: T) => number | null,
  resolveAuthor?: (item: T, extra: unknown) => RoomAuthor | null,
): AttributedItem<T>[] {
  const attributed: AttributedItem<T>[] = [];
  let previousChatId: string | null = null;

  for (const item of items) {
    const key = keyOf(item);
    const extra = extraOf(item);
    const extraRecord = isRecord(extra) ? extra : null;
    const author =
      messageAuthor(extraRecord ?? undefined) ??
      resolveAuthor?.(item, extra) ??
      null;
    if (author === null) {
      attributed.push({ key, item, attribution: null });
      continue;
    }

    const stamped = messageTimestampMs(extraRecord ?? undefined);
    const epochMs = stamped ?? resolveTimestamp?.(extra, item) ?? null;
    // Only a real room author (non-empty chat id) can open a new turn. A
    // role-derived byline for a plain conversation never draws a divider, so a
    // one-to-one chat stays a quiet column of cards.
    const divider =
      author.chatId !== "" &&
      previousChatId !== null &&
      previousChatId !== author.chatId;
    if (author.chatId !== "") previousChatId = author.chatId;

    attributed.push({
      key,
      item,
      attribution: {
        author,
        timeLabel: epochMs === null ? "" : formatTime(epochMs),
        divider,
      },
    });
  }

  return attributed;
}
