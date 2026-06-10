import { describe, it, expect } from "vitest";
import { pickDesiredChatSubscriptions } from "../hooks/useAllChatsSubscription";

describe("pickDesiredChatSubscriptions", () => {
  it("keeps active chat first and limits to default size", () => {
    const result = pickDesiredChatSubscriptions({
      openThreadIds: ["chat-1", "chat-2", "chat-3", "chat-4", "chat-5"],
      activeChatId: "chat-5",
      subscribedThreadIds: [],
    });

    expect(result).toEqual(["chat-5", "chat-4", "chat-3", "chat-2"]);
  });

  it("prefers currently subscribed chats after active to reduce churn", () => {
    const result = pickDesiredChatSubscriptions({
      openThreadIds: ["chat-1", "chat-2", "chat-3", "chat-4", "chat-5"],
      activeChatId: "chat-3",
      subscribedThreadIds: ["chat-1", "chat-2"],
      maxSubscriptions: 4,
    });

    expect(result).toEqual(["chat-3", "chat-1", "chat-2", "chat-5"]);
  });

  it("includes active chat even when it is not in open tabs", () => {
    const result = pickDesiredChatSubscriptions({
      openThreadIds: ["chat-1", "chat-2", "chat-3", "chat-4"],
      activeChatId: "chat-external",
      subscribedThreadIds: [],
      maxSubscriptions: 4,
    });

    expect(result).toEqual(["chat-external", "chat-4", "chat-3", "chat-2"]);
  });

  it("returns full ordered list when maxSubscriptions is non-positive", () => {
    const result = pickDesiredChatSubscriptions({
      openThreadIds: ["chat-1", "chat-2", "chat-3"],
      activeChatId: "chat-2",
      subscribedThreadIds: ["chat-1"],
      maxSubscriptions: 0,
    });

    expect(result).toEqual(["chat-2", "chat-1", "chat-3"]);
  });
});
