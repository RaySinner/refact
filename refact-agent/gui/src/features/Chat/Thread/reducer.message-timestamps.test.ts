import { describe, expect, test } from "vitest";
import { chatReducer } from "./reducer";
import { newChatAction, applyChatEvent, restoreChat } from "./actions";
import type {
  ChatEventEnvelope,
} from "../../../services/refact/chatSubscription";
import type { ChatMessage } from "../../../services/refact/types";
import type { ChatHistoryItem } from "../../History/historySlice";
import type { Chat } from "./types";
import {
  displayItemExtra,
  messageTimestampMs,
} from "../../../components/ChatContent/RoomAttribution/roomAttribution";

const CREATED_AT_MS = 1_700_000_001_000;

const baseState = (): Chat => {
  const emptyState = chatReducer(undefined, { type: "@@INIT" });
  return chatReducer(emptyState, newChatAction(undefined));
};

const messagesOf = (state: Chat, chatId: string): ChatMessage[] => {
  const runtime = state.threads[chatId];
  if (!runtime) throw new Error(`Runtime not found for chat ${chatId}`);
  return runtime.thread.messages;
};

const snapshotEvent = (
  chatId: string,
  messages: ChatMessage[],
): ChatEventEnvelope => ({
  chat_id: chatId,
  seq: "1",
  type: "snapshot",
  thread: {
    id: chatId,
    title: "Test",
    model: "gpt-4",
    mode: "AGENT",
    tool_use: "agent",
    boost_reasoning: false,
    context_tokens_cap: 8192,
    include_project_info: true,
    checkpoints_enabled: true,
    is_title_generated: false,
  },
  runtime: {
    state: "idle",
    paused: false,
    error: null,
    queue_size: 0,
    pause_reasons: [],
    queued_items: [],
  },
  background_agents: [],
  messages,
});

const historyItem = (
  chatId: string,
  messages: ChatHistoryItem["messages"],
): ChatHistoryItem => ({
  id: chatId,
  messages,
  model: "gpt-4",
  title: "Restored",
  tool_use: "agent",
  mode: "agent",
  createdAt: "2026-05-31T00:00:00.000Z",
  updatedAt: "2026-05-31T00:01:00.000Z",
  boost_reasoning: false,
  include_project_info: true,
  increase_max_tokens: false,
  context_tokens_cap: undefined,
  last_user_message_id: "",
});

const timestampSeenByRoom = (message: ChatMessage): number | null => {
  const extra = displayItemExtra({ key: "message", message } as never);
  return messageTimestampMs(extra as Record<string, unknown> | undefined);
};

describe("message creation timestamps", () => {
  test("snapshot, restore and live share one creation timestamp", () => {
    const wireMessage: ChatMessage = {
      role: "assistant",
      content: "Hi",
      message_id: "m-1",
      created_at_ms: CREATED_AT_MS,
    };

    const initial = baseState();
    const chatId = initial.current_thread_id;

    const snapshotState = chatReducer(
      initial,
      applyChatEvent(snapshotEvent(chatId, [wireMessage])),
    );
    const restoredState = chatReducer(
      initial,
      restoreChat(historyItem(chatId, [wireMessage])),
    );
    const liveState = chatReducer(
      chatReducer(
        initial,
        applyChatEvent(snapshotEvent(chatId, [])),
      ),
      applyChatEvent({
        chat_id: chatId,
        seq: "2",
        type: "message_added",
        message: wireMessage,
        index: 0,
      }),
    );

    const snapshotMessage = messagesOf(snapshotState, chatId)[0];
    const restoredMessage = messagesOf(restoredState, chatId)[0];
    const liveMessage = messagesOf(liveState, chatId)[0];

    expect(timestampSeenByRoom(snapshotMessage)).toBe(CREATED_AT_MS);
    expect(timestampSeenByRoom(restoredMessage)).toBe(CREATED_AT_MS);
    expect(timestampSeenByRoom(liveMessage)).toBe(CREATED_AT_MS);
    expect(snapshotMessage.created_at_ms).toBe(CREATED_AT_MS);
    expect(restoredMessage.created_at_ms).toBe(CREATED_AT_MS);
    expect(liveMessage.created_at_ms).toBe(CREATED_AT_MS);
  });

  test("keeps two different creation stamps distinct", () => {
    const initial = baseState();
    const chatId = initial.current_thread_id;
    const state = chatReducer(
      initial,
      applyChatEvent(
        snapshotEvent(chatId, [
          {
            role: "user",
            content: "first",
            message_id: "m-1",
            created_at_ms: 1_700_000_001_000,
          },
          {
            role: "assistant",
            content: "second",
            message_id: "m-2",
            created_at_ms: 1_700_000_009_000,
          },
        ]),
      ),
    );

    const messages = messagesOf(state, chatId);
    expect(timestampSeenByRoom(messages[0])).toBe(1_700_000_001_000);
    expect(timestampSeenByRoom(messages[1])).toBe(1_700_000_009_000);
  });

  test("prefers the flattened stamp over a legacy nested stamp", () => {
    const initial = baseState();
    const chatId = initial.current_thread_id;
    const state = chatReducer(
      initial,
      applyChatEvent(
        snapshotEvent(chatId, [
          {
            role: "assistant",
            content: "Hi",
            message_id: "m-1",
            created_at_ms: 1_700_000_001_000,
            extra: { created_at_ms: 1_600_000_000_000 },
          },
        ]),
      ),
    );

    expect(timestampSeenByRoom(messagesOf(state, chatId)[0])).toBe(
      1_700_000_001_000,
    );
  });

  test("keeps a legacy nested stamp", () => {
    const initial = baseState();
    const chatId = initial.current_thread_id;
    const state = chatReducer(
      initial,
      applyChatEvent(
        snapshotEvent(chatId, [
          {
            role: "assistant",
            content: "Hi",
            message_id: "m-1",
            extra: { created_at_ms: 1_600_000_000_000 },
          },
        ]),
      ),
    );

    const message = messagesOf(state, chatId)[0];
    expect(timestampSeenByRoom(message)).toBe(1_600_000_000_000);
    expect(message.created_at_ms).toBe(1_600_000_000_000);
  });

  test("does not fabricate a stamp for legacy or malformed messages", () => {
    const initial = baseState();
    const chatId = initial.current_thread_id;
    const malformed: ChatMessage = {
      role: "assistant",
      content: "Hi",
      message_id: "m-2",
      created_at_ms: "soon" as unknown as number,
    };
    const state = chatReducer(
      initial,
      applyChatEvent(
        snapshotEvent(chatId, [
          { role: "user", content: "no stamp", message_id: "m-1" },
          malformed,
        ]),
      ),
    );

    const messages = messagesOf(state, chatId);
    expect(timestampSeenByRoom(messages[0])).toBeNull();
    expect(timestampSeenByRoom(messages[1])).toBeNull();
  });
});
