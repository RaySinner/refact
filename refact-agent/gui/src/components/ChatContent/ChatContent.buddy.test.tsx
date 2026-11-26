import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import type { RootState } from "../../app/store";
import type { ChatMessages, EventMessage } from "../../services/refact";
import { createDefaultChatState, render, screen } from "../../utils/test-utils";
import { roleAccentSlot } from "../../utils/roomRoleAccent";
import { ChatContent } from "./ChatContent";
import { isBuddySystemNotice } from "./buddyNotice";

function eventMessage(overrides: Partial<EventMessage> = {}): EventMessage {
  const subkind = overrides.subkind ?? "system_notice";
  const source = overrides.source ?? "buddy.supervision";
  return {
    role: "event",
    content: "Please set **a goal** before continuing.",
    message_id: "buddy-1",
    subkind,
    source,
    extra: {
      event: {
        subkind,
        source,
        payload: {},
      },
    },
    ...overrides,
  };
}

function renderChat(messages: ChatMessages) {
  const chat = createDefaultChatState();
  const chatId = chat.current_thread_id;
  chat.threads[chatId].thread.messages = messages;
  const preloadedState: Partial<RootState> = { chat };
  return render(
    <ChatContent onRetry={() => undefined} onStopStreaming={() => undefined} />,
    { preloadedState },
  );
}

describe("Buddy transcript messages", () => {
  it.each([
    "buddy.stalled_turn",
    "buddy.supervision",
    "buddy.planner_backstop",
  ])("renders %s as a full inline third participant", (source) => {
    renderChat([
      { role: "user", content: "Keep going", message_id: "user-1" },
      {
        role: "assistant",
        content: "Working on it",
        message_id: "assistant-1",
      },
      eventMessage({ source }),
    ]);

    const frames = screen.getAllByTestId("room-attribution");
    expect(
      frames.map((frame) => frame.getAttribute("data-author-role")),
    ).toEqual(["user", "assistant", "buddy"]);

    const buddyFrame = frames[2];
    const body = screen.getByTestId("buddy-message");
    expect(body).toHaveAttribute("data-source", source);
    expect(body).toHaveTextContent("Please set a goal before continuing.");
    expect(body.querySelector("strong")).toHaveTextContent("a goal");
    expect(buddyFrame).toContainElement(body);
    expect(buddyFrame.getAttribute("style")).toContain(
      "var(--room-stripe-10)",
    );
    expect(buddyFrame.getAttribute("style")).toContain("var(--room-label-10)");
    expect(
      buddyFrame.querySelector("[data-testid='room-attribution-byline']"),
    ).toHaveTextContent("Buddy");
    expect(screen.queryByTestId("event-row")).not.toBeInTheDocument();
  });

  it("keeps generic SystemNotice events on the existing EventRow path", () => {
    renderChat([
      eventMessage({
        source: "chat.session",
        content: "Ordinary system notice",
        message_id: "event-ordinary",
      }),
    ]);

    expect(screen.getByTestId("event-row")).toHaveTextContent(
      "Ordinary system notice",
    );
    expect(screen.queryByTestId("buddy-message")).not.toBeInTheDocument();
    const frame = screen.getByTestId("room-attribution");
    expect(frame).toHaveAttribute("data-author-role", "event");
    expect(frame.getAttribute("style")).toContain("var(--room-stripe-9)");
  });

  it("classifies only nonempty buddy sources on SystemNotice events", () => {
    expect(isBuddySystemNotice(eventMessage())).toBe(true);
    expect(isBuddySystemNotice(eventMessage({ subkind: "tick" }))).toBe(false);
    expect(isBuddySystemNotice(eventMessage({ source: "buddy." }))).toBe(false);
    expect(
      isBuddySystemNotice(eventMessage({ source: "other.buddy.supervision" })),
    ).toBe(false);
  });

  it("assigns Buddy the fixed sage slot without changing existing roles", () => {
    expect(roleAccentSlot("buddy")).toBe(10);
    expect(roleAccentSlot("user")).toBe(7);
    expect(roleAccentSlot("assistant")).toBe(1);
    expect(roleAccentSlot("event")).toBe(9);
  });

  it("shares the existing sage tokens across appearances and IDE hosts", () => {
    const tokens = readFileSync(
      join(process.cwd(), "src/styles/tokens.css"),
      "utf8",
    );
    expect(tokens).toContain("--room-stripe-10: #a3c98f;");
    expect(tokens).toContain("--room-label-10: #b8d7a6;");
    expect(tokens).toContain("--room-stripe-10: #5a8a45;");
    expect(tokens).toContain("--room-label-10: #477036;");
    expect(tokens.match(/--room-stripe-10:/g)).toHaveLength(2);
    expect(tokens.match(/--room-label-10:/g)).toHaveLength(2);

    const jetbrainsAdapter = tokens.slice(
      tokens.indexOf('[data-host="jetbrains"]'),
      tokens.indexOf('[data-host="vscode"]'),
    );
    const vscodeAdapter = tokens.slice(tokens.indexOf('[data-host="vscode"]'));
    for (const adapter of [jetbrainsAdapter, vscodeAdapter]) {
      expect(adapter).not.toMatch(/--room-(?:stripe|label)-10\s*:/);
    }
  });
});
