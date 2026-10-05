import type { DisplayItem } from "../ChatContentDisplayItems";

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
 * The delivery stamp (`extra.delivery.at_ms`) is the engine's own record of when the
 * message entered a chat, which is the moment a reader means by "when this was said".
 * The bare `created_at_ms` / `at_ms` keys are accepted as fallbacks because plan, goal
 * and event metadata already use them.
 *
 * Returns `null` rather than guessing: a wrong clock is worse than no clock.
 */
export function messageTimestampMs(
  extra: Record<string, unknown> | undefined,
): number | null {
  if (!extra) return null;

  const delivery = extra.delivery;
  if (isRecord(delivery)) {
    const atMs = delivery.at_ms;
    if (typeof atMs === "number" && Number.isFinite(atMs) && atMs > 0) {
      return atMs;
    }
  }

  for (const key of ["created_at_ms", "at_ms", "timestamp_ms"]) {
    const value = extra[key];
    if (typeof value === "number" && Number.isFinite(value) && value > 0) {
      return value;
    }
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
    if (isRecord(value) && "extra" in value) return value.extra;
  }
  if ("rawExtra" in record) return record.rawExtra;
  return null;
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
export function roomAttributionForItems<T>(
  items: readonly T[],
  keyOf: (item: T) => string,
  extraOf: (item: T) => unknown,
  formatTime: (epochMs: number) => string,
  resolveTimestamp?: (extra: unknown, item: T) => number | null,
): AttributedItem<T>[] {
  const attributed: AttributedItem<T>[] = [];
  let previousChatId: string | null = null;

  for (const item of items) {
    const key = keyOf(item);
    const extra = extraOf(item);
    const extraRecord = isRecord(extra) ? extra : null;
    const author = messageAuthor(extraRecord ?? undefined);
    if (author === null) {
      attributed.push({ key, item, attribution: null });
      continue;
    }

    const stamped = messageTimestampMs(extraRecord ?? undefined);
    const epochMs = stamped ?? resolveTimestamp?.(extra, item) ?? null;
    const divider = previousChatId !== null && previousChatId !== author.chatId;
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
