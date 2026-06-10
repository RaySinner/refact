import { describe, expect, it, vi } from "vitest";
import {
  tasksSlice,
  addPlannerChat,
  removePlannerChat,
  restorePlannerChat,
} from "./tasksSlice";
import type { PlannerInfo, TasksUIState } from "./tasksSlice";

vi.mock("../../utils/chatUiPersistence", () => ({
  loadPersistedTasksUIState: () => ({ openTasks: [] }),
  savePersistedTasksUIState: vi.fn(),
}));

vi.mock("../Chat/Thread/actions", () => ({
  hydratePersistedChatTabs: { type: "chatThread/hydratePersistedChatTabs" },
}));

const TASK_ID = "task-1";

function makePlanner(id: string): PlannerInfo {
  return {
    id,
    title: `Planner ${id}`,
    createdAt: "2026-01-01T00:00:00Z",
    updatedAt: "2026-01-01T00:00:00Z",
  };
}

function makeState(...plannerIds: string[]): TasksUIState {
  return {
    openTasks: [
      {
        id: TASK_ID,
        name: "Test Task",
        plannerChats: plannerIds.map(makePlanner),
        activeChat: null,
      },
    ],
  };
}

describe("tasksSlice planner reducers", () => {
  it("removePlannerChat_filters_entry", () => {
    const state = makeState("p-1", "p-2");
    const next = tasksSlice.reducer(
      state,
      removePlannerChat({ taskId: TASK_ID, chatId: "p-1" }),
    );
    expect(next.openTasks[0].plannerChats).toEqual([makePlanner("p-2")]);
  });

  it("restorePlannerChat_restores_at_original_position", () => {
    const planner = makePlanner("p-1");
    const state = makeState("p-2");
    const afterRemove = tasksSlice.reducer(
      state,
      removePlannerChat({ taskId: TASK_ID, chatId: "p-1" }),
    );
    const next = tasksSlice.reducer(
      afterRemove,
      restorePlannerChat({ taskId: TASK_ID, planner }),
    );
    const found = next.openTasks[0].plannerChats.find((p) => p.id === "p-1");
    expect(found).toEqual(planner);
  });

  it("restorePlannerChat_does_not_duplicate_if_already_present", () => {
    const planner = makePlanner("p-1");
    const state = makeState("p-1", "p-2");
    const next = tasksSlice.reducer(
      state,
      restorePlannerChat({ taskId: TASK_ID, planner }),
    );
    const matches = next.openTasks[0].plannerChats.filter(
      (p) => p.id === "p-1",
    );
    expect(matches).toHaveLength(1);
  });

  it("addPlannerChat_does_not_set_removed_flag", () => {
    const planner = makePlanner("p-new");
    const state = makeState();
    const next = tasksSlice.reducer(
      state,
      addPlannerChat({ taskId: TASK_ID, planner }),
    );
    const added = next.openTasks[0].plannerChats.find((p) => p.id === "p-new");
    expect(added).toEqual(planner);
    expect(added).not.toHaveProperty("removed");
  });
});
