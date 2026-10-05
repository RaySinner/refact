import { describe, expect, it } from "vitest";
import {
  displayItemExtra,
  messageAuthor,
  messageTimestampMs,
  roomAttributionForItems,
} from "./roomAttribution";

function provenance(
  chatId: string,
  role: string,
  displayName: string,
): Record<string, unknown> {
  return {
    provenance: { chat_id: chatId, role, display_name: displayName },
  };
}

function item(key: string, extra: unknown) {
  return { key, message: { role: "assistant", content: key, extra } };
}

const formatTime = (epochMs: number): string => `t${epochMs}`;

describe("messageAuthor", () => {
  it("reads the engine's stamp", () => {
    expect(messageAuthor(provenance("agent-1", "coder", "coder #1"))).toEqual({
      chatId: "agent-1",
      role: "coder",
      displayName: "coder #1",
    });
  });

  it("returns null for a plain conversation message", () => {
    expect(messageAuthor({})).toBeNull();
    expect(messageAuthor(undefined)).toBeNull();
    expect(messageAuthor({ provenance: "nonsense" })).toBeNull();
  });

  it("keeps an author that has only a chat id", () => {
    expect(messageAuthor({ provenance: { chat_id: "planner-1" } })).toEqual({
      chatId: "planner-1",
      role: "",
      displayName: "",
    });
  });

  it("falls back to the role when the engine sent no display name", () => {
    expect(
      messageAuthor({ provenance: { chat_id: "a", role: "reviewer" } })
        ?.displayName,
    ).toBe("reviewer");
  });
});

describe("messageTimestampMs", () => {
  it("prefers the engine's delivery stamp", () => {
    expect(messageTimestampMs({ delivery: { at_ms: 1_700_000_005_000 } })).toBe(
      1_700_000_005_000,
    );
  });

  it("falls back to created_at_ms and at_ms", () => {
    expect(messageTimestampMs({ created_at_ms: 111 })).toBe(111);
    expect(messageTimestampMs({ at_ms: 222 })).toBe(222);
  });

  it("returns null instead of guessing", () => {
    expect(messageTimestampMs({})).toBeNull();
    expect(messageTimestampMs(undefined)).toBeNull();
    expect(messageTimestampMs({ delivery: { at_ms: 0 } })).toBeNull();
    expect(messageTimestampMs({ created_at_ms: "soon" })).toBeNull();
  });
});

describe("displayItemExtra", () => {
  it("reads the extra of the message an item wraps", () => {
    const extra = provenance("agent-1", "coder", "coder");
    expect(displayItemExtra(item("k1", extra) as never)).toBe(extra);
  });

  it("reads the extra of an event row", () => {
    const extra = provenance("agent-2", "reviewer", "reviewer");
    const eventItem = {
      key: "e1",
      type: "event",
      messageIndex: 0,
      event: { role: "event", content: "", subkind: "x", source: "y", extra },
      run: "single",
    };
    expect(displayItemExtra(eventItem as never)).toBe(extra);
  });
});

describe("roomAttributionForItems", () => {
  it("leaves an unattributed message alone", () => {
    const [attributed] = roomAttributionForItems(
      [item("plain", undefined)],
      (i) => i.key,
      (i) => (i as { message: { extra?: unknown } }).message.extra,
      formatTime,
    );
    expect(attributed.attribution).toBeNull();
  });

  it("puts a divider on the second agent's message only", () => {
    const extraOf = (i: { message: { extra?: unknown } }) => i.message.extra;
    const items = [
      item("a1", provenance("agent-a", "coder", "coder")),
      item("a2", provenance("agent-a", "coder", "coder")),
      item("b1", provenance("agent-b", "reviewer", "reviewer")),
    ];
    const result = roomAttributionForItems(
      items,
      (i) => i.key,
      extraOf,
      formatTime,
    );

    expect(result[0].attribution?.divider).toBe(false);
    expect(result[1].attribution?.divider).toBe(false);
    expect(result[2].attribution?.divider).toBe(true);
  });

  it("ignores unattributed items between two messages of one agent", () => {
    const extraOf = (i: {
      message?: { extra?: unknown };
      rawExtra?: unknown;
    }) => i.message?.extra ?? i.rawExtra;
    const items = [
      {
        key: "a1",
        message: { extra: provenance("agent-a", "coder", "coder") },
      },
      {
        key: "ctx",
        message: { role: "assistant", extra: undefined },
      },
      {
        key: "a2",
        message: { extra: provenance("agent-a", "coder", "coder") },
      },
    ];
    const result = roomAttributionForItems(
      items,
      (i) => i.key,
      extraOf,
      formatTime,
    );

    expect(result[1].attribution).toBeNull();
    expect(result[2].attribution?.divider).toBe(false);
  });

  it("uses the stamped time when the engine provided one", () => {
    const extra = {
      ...provenance("agent-a", "coder", "coder"),
      delivery: { at_ms: 1234 },
    };
    const [attributed] = roomAttributionForItems(
      [item("a1", extra)],
      (i) => i.key,
      (i) => (i as { message: { extra?: unknown } }).message.extra,
      formatTime,
    );
    expect(attributed.attribution?.timeLabel).toBe("t1234");
  });

  it("falls back to the caller-supplied time only when none was stamped", () => {
    const withoutStamp = item("a1", provenance("agent-a", "coder", "coder"));
    const result = roomAttributionForItems(
      [withoutStamp],
      (i) => i.key,
      (i) => (i as { message: { extra?: unknown } }).message.extra,
      formatTime,
      () => 9999,
    );
    expect(result[0].attribution?.timeLabel).toBe("t9999");
  });

  it("shows no time when no instant is known at all", () => {
    const withoutStamp = item("a1", provenance("agent-a", "coder", "coder"));
    const result = roomAttributionForItems(
      [withoutStamp],
      (i) => i.key,
      (i) => (i as { message: { extra?: unknown } }).message.extra,
      formatTime,
      () => null,
    );
    expect(result[0].attribution?.timeLabel).toBe("");
  });

  it("carries the item key up so list identity survives", () => {
    const result = roomAttributionForItems(
      [item("a1", undefined)],
      (i) => i.key,
      () => undefined,
      formatTime,
    );
    expect(result[0].key).toBe("a1");
  });
});
