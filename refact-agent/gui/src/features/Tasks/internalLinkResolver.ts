import type { TaskBoard, BoardCard } from "../../services/refact/tasks";
import type { PlannerInfo } from "./tasksSlice";

/**
 * Pure resolution of a `refact://chat/<chat_id>` link in the context of a
 * task workspace. Returns the action the workspace should take.
 *
 * Resolution priority (matches T-179 spec):
 *   1. Planner chat in this task → activate planner
 *   2. Board card whose `agent_chat_id` matches → activate agent
 *   3. Legacy `agent-<cardId>-<suffix>` pattern with a known card → activate agent
 *   4. Otherwise → unknown (workspace should surface a notification)
 */
export type ResolvedChatLink =
  | { kind: "planner"; chatId: string }
  | { kind: "agent"; cardId: string; chatId: string }
  | { kind: "unknown"; chatId: string };

export function resolveChatLink(
  chatId: string,
  plannerChats: PlannerInfo[],
  board: TaskBoard | null | undefined,
): ResolvedChatLink {
  if (plannerChats.some((planner) => planner.id === chatId)) {
    return { kind: "planner", chatId };
  }

  const card = board?.cards.find((c: BoardCard) => c.agent_chat_id === chatId);
  if (card) {
    return { kind: "agent", cardId: card.id, chatId };
  }

  if (chatId.startsWith("agent-")) {
    const withoutPrefix = chatId.slice("agent-".length);
    const lastDashIdx = withoutPrefix.lastIndexOf("-");
    if (lastDashIdx > 0) {
      const cardId = withoutPrefix.slice(0, lastDashIdx);
      const legacyCard = board?.cards.find((c: BoardCard) => c.id === cardId);
      if (legacyCard) {
        return { kind: "agent", cardId, chatId };
      }
    }
  }

  return { kind: "unknown", chatId };
}
