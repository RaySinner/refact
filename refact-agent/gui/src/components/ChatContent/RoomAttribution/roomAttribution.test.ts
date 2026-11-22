import { describe, expect, it } from "vitest";
import {
  displayItemExtra,
  displayItemTimestampFallbackMs,
  messageAuthor,
  messageTimestampMs,
  roomAttributionForItems,
} from "./roomAttribution";
import type {
  AssistantMessage,
  ChatMessages,
  EventMessage,
  ToolCall,
  UserMessage,
} from "../../../services/refact";
import { buildDisplayItems } from "../ChatContentDisplayItems";

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

function toolCall(startedAtMs?: number): ToolCall {
  return {
    id: "call-1",
    index: 0,
    type: "function",
    function: { name: "cat", arguments: "{}" },
    started_at_ms: startedAtMs,
  };
}

function timestampedAssistant(
  messageId: string,
  createdAtMs?: number,
  toolStartedAtMs?: number,
): AssistantMessage {
  return {
    role: "assistant",
    content: "",
    message_id: messageId,
    created_at_ms: createdAtMs,
    tool_calls: [toolCall(toolStartedAtMs)],
  };
}

function transcriptTimeLabels(
  messages: ChatMessages,
  firstSeenMs = 9_999,
): string[] {
  const items = buildDisplayItems(messages, false);
  return roomAttributionForItems(
    items,
    (item) => item.key,
    displayItemExtra,
    formatTime,
    (_extra, item) => displayItemTimestampFallbackMs(item, firstSeenMs),
    () => ({ chatId: "", role: "assistant", displayName: "Assistant" }),
  ).map((attributed) => attributed.attribution?.timeLabel ?? "");
}

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
  it("uses the delivery stamp when the message has no creation stamp", () => {
    expect(messageTimestampMs({ delivery: { at_ms: 1_700_000_005_000 } })).toBe(
      1_700_000_005_000,
    );
  });

  it("prefers the creation stamp over the delivery stamp", () => {
    expect(
      messageTimestampMs({
        created_at_ms: 1_700_000_001_000,
        delivery: { at_ms: 1_700_000_005_000 },
      }),
    ).toBe(1_700_000_001_000);
  });

  it("reads created_at_ms and falls back to at_ms", () => {
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

  it("exposes a flattened creation stamp for a message", () => {
    const flattenedItem = {
      key: "m1",
      message: {
        role: "assistant",
        content: "Hi",
        created_at_ms: 1_700_000_001_000,
      },
    };
    expect(displayItemExtra(flattenedItem as never)).toEqual({
      created_at_ms: 1_700_000_001_000,
    });
  });
});

describe("roomAttributionForItems", () => {
  it("uses canonical creation stamps for user, assistant, tool and event rows", () => {
    const user: UserMessage = {
      role: "user",
      content: "question",
      message_id: "user-1",
      created_at_ms: 1_000,
    };
    const assistant: AssistantMessage = {
      role: "assistant",
      content: "answer",
      message_id: "assistant-1",
      created_at_ms: 2_000,
    };
    const event: EventMessage = {
      role: "event",
      content: "notice",
      message_id: "event-1",
      subkind: "system_notice",
      source: "test",
      created_at_ms: 5_000,
      payload: { created_at_ms: 4_500 },
    };
    const messages: ChatMessages = [
      user,
      assistant,
      timestampedAssistant("tool-owner-1", 3_000, 2_500),
      {
        role: "tool",
        content: "first result",
        tool_call_id: "call-1",
        created_at_ms: 3_500,
      },
      timestampedAssistant("tool-owner-2", 4_000, 3_500),
      {
        role: "tool",
        content: "second result",
        tool_call_id: "call-1",
        created_at_ms: 4_500,
      },
      event,
    ];

    expect(transcriptTimeLabels(messages)).toEqual([
      "t1000",
      "t2000",
      "t3000",
      "t4000",
      "t5000",
    ]);
  });

  it("uses tool start only without an owning creation stamp and never uses tool result time", () => {
    const fallbackMessages: ChatMessages = [
      timestampedAssistant("tool-fallback", undefined, 6_000),
      {
        role: "tool",
        content: "result",
        tool_call_id: "call-1",
        created_at_ms: 7_000,
      },
    ];
    const canonicalMessages: ChatMessages = [
      timestampedAssistant("tool-canonical", 5_000, 6_000),
      fallbackMessages[1],
    ];

    expect(transcriptTimeLabels(fallbackMessages)).toEqual(["t6000"]);
    expect(transcriptTimeLabels(canonicalMessages)).toEqual(["t5000"]);
  });

  it("keeps live and restored tool instants identical across first-seen changes", () => {
    const messages: ChatMessages = [
      timestampedAssistant("tool-live", 5_000, 4_500),
      {
        role: "tool",
        content: "result",
        tool_call_id: "call-1",
        created_at_ms: 6_000,
      },
    ];

    expect(transcriptTimeLabels(messages, 1_111)).toEqual(["t5000"]);
    expect(transcriptTimeLabels(messages, 8_888)).toEqual(["t5000"]);
  });

  it("leaves legacy tool and event rows without first-seen mount labels", () => {
    const messages: ChatMessages = [
      timestampedAssistant("legacy-tool"),
      {
        role: "tool",
        content: "result",
        tool_call_id: "call-1",
        created_at_ms: 7_000,
      },
      {
        role: "event",
        content: "legacy event",
        message_id: "legacy-event",
        subkind: "system_notice",
        source: "test",
      },
    ];

    expect(transcriptTimeLabels(messages)).toEqual(["", ""]);
  });

  it("retains first-seen fallback for optimistic non-tool messages", () => {
    const messages: ChatMessages = [
      { role: "user", content: "live", message_id: "live-user" },
      { role: "assistant", content: "live", message_id: "live-assistant" },
    ];

    expect(transcriptTimeLabels(messages, 8_000)).toEqual(["t8000", "t8000"]);
  });

  it("leaves an unattributed message alone when no resolveAuthor is provided", () => {
    const [attributed] = roomAttributionForItems(
      [item("plain", undefined)],
      (i) => i.key,
      (i) => (i as { message: { extra?: unknown } }).message.extra,
      formatTime,
    );
    expect(attributed.attribution).toBeNull();
  });

  it("uses resolveAuthor fallback when extra has no provenance", () => {
    const [attributed] = roomAttributionForItems(
      [item("plain", { created_at_ms: 12345 })],
      (i) => i.key,
      (i) => (i as { message: { extra?: unknown } }).message.extra,
      formatTime,
      undefined,
      () => ({ chatId: "", role: "user", displayName: "User" }),
    );
    expect(attributed.attribution).toEqual({
      author: { chatId: "", role: "user", displayName: "User" },
      timeLabel: "t12345",
      divider: false,
    });
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

  it("uses the creation stamp instead of the delivery stamp", () => {
    const extra = {
      ...provenance("agent-a", "coder", "coder"),
      created_at_ms: 1111,
      delivery: { at_ms: 2222 },
    };
    const [attributed] = roomAttributionForItems(
      [item("a1", extra)],
      (i) => i.key,
      (i) => (i as { message: { extra?: unknown } }).message.extra,
      formatTime,
    );
    expect(attributed.attribution?.timeLabel).toBe("t1111");
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
