import { http, HttpResponse } from "msw";
import { describe, expect, test } from "vitest";

import { render, screen, waitFor, within } from "../../utils/test-utils";
import { server } from "../../utils/mockServer";
import type {
  Chat,
  ChatThreadRuntime,
  TodoItem,
  ToolUse,
} from "../../features/Chat/Thread/types";
import type {
  ChatMessages,
  EventMessage,
  GoalSnapshot,
  PlanMessage,
} from "../../services/refact/types";
import { TaskProgressWidget } from "./TaskProgressWidget";

const threadId = "goal-widget-chat";

function makeGoal(overrides: Partial<GoalSnapshot> = {}): GoalSnapshot {
  return {
    content: "Ship the frog pond",
    version: 1,
    active: true,
    status: "active",
    budget: {
      max_turns: 5,
      max_minutes: 10,
      max_tokens: 5000,
      cooldown_ms: 1500,
      no_progress_token_threshold: 50,
      no_progress_turns: 2,
    },
    progress: {
      turns_used: 2,
      tokens_used: 1200,
      started_at_ms: 1000,
      no_progress_turns: 1,
      last_nudge_at_ms: 2000,
    },
    attempts: [
      {
        at_ms: 3000,
        trigger: "manual",
        verdict: "needs_work",
        gaps: ["missing verifier coverage"],
        verifier_reply: "Add one more verifier test.",
      },
    ],
    events: [{ at_ms: 4000, kind: "nudge", text: "Asked for progress" }],
    transferred_from: null,
    transferred_to: null,
    ...overrides,
  };
}

function makeModernGoal(overrides: Partial<GoalSnapshot> = {}): GoalSnapshot {
  return makeGoal({
    resolution: "active",
    execution: "idle",
    autonomy: "on",
    owner: "user",
    revision: 4,
    can_pursue: true,
    prompt_enabled: true,
    ...overrides,
  });
}

function taskMessages(tasks: TodoItem[]): ChatMessages {
  return [
    {
      role: "assistant",
      content: "tasks",
      message_id: "assistant-tasks",
      tool_calls: [
        {
          id: "call-tasks",
          index: 0,
          type: "function",
          function: {
            name: "tasks_set",
            arguments: JSON.stringify({ tasks }),
          },
        },
      ],
    },
    {
      role: "tool",
      content: "ok",
      message_id: "tool-tasks",
      tool_call_id: "call-tasks",
      tool_failed: false,
    },
  ];
}

function planMessages(deltas: string[] = []): ChatMessages {
  const base: PlanMessage = {
    role: "plan",
    message_id: "plan-3",
    content: "## Execution route\n\n- Inspect existing plan state",
    extra: {
      plan: {
        mode: "task_planner",
        version: 3,
        created_at_ms: 1_700_000_000_000,
      },
    },
  };
  const events: EventMessage[] = deltas.map((content, index) => ({
    role: "event",
    message_id: `plan-delta-${index + 1}`,
    content,
    subkind: "plan_delta",
    source: "test",
  }));
  return [base, ...events];
}

function makeRuntime(options: {
  goal?: GoalSnapshot | null;
  messages?: ChatMessages;
  expanded?: boolean;
  goalExpanded?: boolean;
  mode?: string;
  toolUse?: ToolUse;
}): ChatThreadRuntime {
  return {
    thread: {
      id: threadId,
      messages: options.messages ?? [],
      title: "Goal Widget Chat",
      model: "gpt-4",
      mode: options.mode ?? "agent",
      tool_use: options.toolUse ?? "agent",
      new_chat_suggested: { wasSuggested: false },
      boost_reasoning: false,
      increase_max_tokens: false,
      include_project_info: true,
      auto_enrichment_enabled: false,
      goal: options.goal ?? null,
    },
    streaming: false,
    waiting_for_response: false,
    prevent_send: false,
    error: null,
    queued_items: [],
    send_immediately: false,
    attached_images: [],
    attached_text_files: [],
    background_agents: {},
    confirmation: {
      pause: false,
      pause_reasons: [],
      status: { wasInteracted: false, confirmationStatus: true },
    },
    snapshot_received: true,
    task_widget_expanded: options.expanded ?? false,
    task_goal_expanded: options.goalExpanded ?? false,
    memory_enrichment_user_touched: false,
    manual_preview_items: [],
    manual_preview_ran: false,
  };
}

function makeChat(runtime: ChatThreadRuntime): Chat {
  return {
    current_thread_id: threadId,
    open_thread_ids: [threadId],
    threads: { [threadId]: runtime },
    system_prompt: {},
    tool_use: "agent",
    sse_refresh_requested: null,
    stream_version: 0,
  };
}

function renderWidget(runtime: ChatThreadRuntime) {
  return render(<TaskProgressWidget />, {
    preloadedState: { chat: makeChat(runtime) },
  });
}

function captureCommands() {
  const commands: Record<string, unknown>[] = [];
  server.use(
    http.post("*/v1/chats/:id/commands", async ({ request }) => {
      commands.push((await request.json()) as Record<string, unknown>);
      return HttpResponse.json({ status: "queued" });
    }),
  );
  return commands;
}

describe("TaskProgressWidget goal projection", () => {
  test("renders in a goal-supporting fresh chat and exposes the goal editor", async () => {
    const { user } = renderWidget(
      makeRuntime({ goal: null, mode: "task_agent", toolUse: "explore" }),
    );

    expect(screen.getByText("Set a goal")).toBeInTheDocument();
    expect(screen.queryByText("Tasks cleared")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Set a goal/i }));
    await user.click(screen.getByRole("button", { name: /Goal Not set/i }));

    expect(screen.getByLabelText("Goal text")).toBeInTheDocument();
  });

  test("empty collapsed header distinguishes fresh goal creation from cleared tasks", () => {
    const freshView = renderWidget(
      makeRuntime({ goal: null, mode: "agent", toolUse: "explore" }),
    );

    expect(screen.getByText("Set a goal")).toBeInTheDocument();
    expect(screen.queryByText("Tasks cleared")).not.toBeInTheDocument();
    freshView.unmount();

    renderWidget(
      makeRuntime({
        goal: null,
        messages: taskMessages([]),
        mode: "agent",
        toolUse: "explore",
      }),
    );

    expect(screen.getByText("Tasks cleared")).toBeInTheDocument();
    expect(screen.queryByText("Set a goal")).not.toBeInTheDocument();
  });

  test("does not render in a non-goal mode without tasks or a goal", () => {
    renderWidget(
      makeRuntime({ goal: null, mode: "explore", toolUse: "explore" }),
    );

    expect(screen.queryByText("Set a goal")).not.toBeInTheDocument();
    expect(screen.queryByText("Tasks cleared")).not.toBeInTheDocument();
    expect(screen.queryByText("Task Progress")).not.toBeInTheDocument();
  });

  test("collapsed widget shows the goal-set indicator and status", () => {
    renderWidget(makeRuntime({ goal: makeGoal() }));

    expect(screen.getByText("Goal set")).toBeInTheDocument();
    expect(screen.getByText("Idle")).toBeInTheDocument();
  });

  test("expanded widget starts with the goal row", () => {
    renderWidget(makeRuntime({ goal: makeGoal(), expanded: true }));

    expect(screen.getByText("Task Progress")).toBeInTheDocument();
    expect(screen.getByText("Goal")).toBeInTheDocument();
    expect(screen.getByText("Idle")).toBeInTheDocument();
  });

  test.each<[Partial<GoalSnapshot>, string]>([
    [{ execution: "running" }, "Running"],
    [{ execution: "verifying", status: "verifying" }, "Verifying"],
    [{ execution: "paused", status: "paused" }, "Paused"],
    [
      { execution: "paused", status: "budget_exhausted" },
      "Paused · budget exhausted",
    ],
    [{ snoozed_until_ms: 4102444800000 }, "Snoozed"],
    [
      {
        events: [
          { at_ms: 5000, kind: "pursuit_quiescent", text: "No progress" },
        ],
      },
      "Quiescent",
    ],
    [{ autonomy: "off", can_pursue: false }, "Idle · autonomy off"],
  ])("renders effective goal state %s", (override, label) => {
    renderWidget(
      makeRuntime({
        goal: makeModernGoal(override),
        expanded: true,
        goalExpanded: true,
      }),
    );

    expect(screen.getAllByText(label).length).toBeGreaterThan(0);
  });

  test("renders owner, revision, stop reason, and latest verdict from the projection", () => {
    renderWidget(
      makeRuntime({
        goal: makeModernGoal({
          owner: "delegate",
          revision: 7,
          execution: "paused",
          status: "paused",
          stop_reason: "waiting for product decision",
        }),
        expanded: true,
        goalExpanded: true,
      }),
    );

    expect(
      screen.getByText(/Owner: Delegate · Revision: 7/),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Stop reason: waiting for product decision"),
    ).toBeInTheDocument();
    expect(screen.getByText("Needs work")).toBeInTheDocument();
  });

  test("legacy snapshots keep legacy rendering and hide modern-only commands", () => {
    renderWidget(
      makeRuntime({ goal: makeGoal(), expanded: true, goalExpanded: true }),
    );

    expect(screen.getByText("Legacy status: Active")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Replace" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Clear goal" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Turn autonomy off" }),
    ).not.toBeInTheDocument();
  });

  test("expanded goal block renders verifier attempts, events, and finite budget ratios", () => {
    renderWidget(
      makeRuntime({ goal: makeGoal(), expanded: true, goalExpanded: true }),
    );

    expect(screen.getByText("2/5 turns")).toBeInTheDocument();
    expect(screen.getByText("1200/5000 tokens")).toBeInTheDocument();
    expect(screen.queryByText("No budget limits")).not.toBeInTheDocument();
    expect(screen.getByText("Verifier attempts")).toBeInTheDocument();
    expect(screen.getByText("Needs work")).toBeInTheDocument();
    expect(screen.getByText("Manual")).toBeInTheDocument();
    expect(screen.getByText("missing verifier coverage")).toBeInTheDocument();
    expect(screen.getByText("Add one more verifier test.")).toBeInTheDocument();
    expect(screen.getByText("Goal events")).toBeInTheDocument();
    expect(screen.getByText("Nudge")).toBeInTheDocument();
    expect(screen.getByText("Asked for progress")).toBeInTheDocument();
  });

  test("emphasizes only saturated budget segments", () => {
    renderWidget(
      makeRuntime({
        goal: makeGoal({
          progress: {
            turns_used: 5,
            tokens_used: 1200,
            started_at_ms: 1000,
            no_progress_turns: 1,
            last_nudge_at_ms: 2000,
          },
        }),
        expanded: true,
        goalExpanded: true,
      }),
    );

    expect(screen.getByText("5/5 turns").className).toContain(
      "goalProgressSaturated",
    );
    expect(screen.getByText("1200/5000 tokens").className).not.toContain(
      "goalProgressSaturated",
    );
  });

  test("expanded goal block renders omitted budget limits as unlimited", () => {
    const { container } = renderWidget(
      makeRuntime({
        goal: makeGoal({
          budget: {
            cooldown_ms: 1500,
            no_progress_token_threshold: 50,
          },
        }),
        expanded: true,
        goalExpanded: true,
      }),
    );

    expect(screen.getByText("2 turns")).toBeInTheDocument();
    expect(screen.getByText("1200 tokens")).toBeInTheDocument();
    expect(container.textContent).toContain("No budget limits");
    expect(container.textContent).not.toContain("/0");
    expect(container.textContent).not.toContain("/undefined");
    expect(container.textContent).not.toContain("/null");
    expect(container.textContent).not.toContain("NaN");
    expect(container.textContent).not.toContain("undefined");
  });

  test("expanded goal block treats zero budget limits as unlimited", () => {
    const { container } = renderWidget(
      makeRuntime({
        goal: makeGoal({
          budget: {
            max_turns: 0,
            max_minutes: 0,
            max_tokens: 0,
            cooldown_ms: 1500,
            no_progress_token_threshold: 50,
            no_progress_turns: 0,
          },
        }),
        expanded: true,
        goalExpanded: true,
      }),
    );

    expect(screen.getByText("2 turns")).toBeInTheDocument();
    expect(screen.getByText("1200 tokens")).toBeInTheDocument();
    expect(container.textContent).toContain("No budget limits");
    expect(container.textContent).not.toContain("0/0");
    expect(container.textContent).not.toContain("/0");
  });

  test("editing an existing goal dispatches update_goal", async () => {
    const commands = captureCommands();
    const { user } = renderWidget(
      makeRuntime({ goal: makeGoal(), expanded: true, goalExpanded: true }),
    );

    await user.click(screen.getByRole("button", { name: "Edit" }));
    const input = screen.getByLabelText("Goal text");
    await user.clear(input);
    await user.type(input, "Ship the shiny frog pond");
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(commands).toHaveLength(1));
    expect(commands[0]).toMatchObject({
      type: "update_goal",
      note: "Ship the shiny frog pond",
    });
  });

  test("editing an empty goal block dispatches set_goal", async () => {
    const commands = captureCommands();
    const { user } = renderWidget(
      makeRuntime({
        goal: null,
        expanded: true,
        goalExpanded: true,
        messages: taskMessages([]),
      }),
    );

    await user.type(screen.getByLabelText("Goal text"), "Start a fresh goal");
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(commands).toHaveLength(1));
    expect(commands[0]).toMatchObject({
      type: "set_goal",
      content: "Start a fresh goal",
    });
    expect(commands[0]).not.toHaveProperty("budget");
  });

  test("fresh goal dispatches set_goal with a positive max turns budget", async () => {
    const commands = captureCommands();
    const { user } = renderWidget(
      makeRuntime({
        goal: null,
        expanded: true,
        goalExpanded: true,
        messages: taskMessages([]),
      }),
    );

    await user.type(
      screen.getByLabelText("Goal text"),
      "Start a budgeted goal",
    );
    await user.type(screen.getByLabelText("Max turns"), "5");
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(commands).toHaveLength(1));
    expect(commands[0]).toMatchObject({
      type: "set_goal",
      content: "Start a budgeted goal",
    });
    expect(commands[0].budget).toEqual({ max_turns: 5 });
  });

  test("existing goal dispatches set_goal_budget from budget inputs", async () => {
    const commands = captureCommands();
    const { user } = renderWidget(
      makeRuntime({ goal: makeGoal(), expanded: true, goalExpanded: true }),
    );

    await user.clear(screen.getByLabelText("Max turns"));
    await user.type(screen.getByLabelText("Max turns"), "7");
    await user.clear(screen.getByLabelText("Max minutes"));
    await user.clear(screen.getByLabelText("Max tokens"));
    await user.click(screen.getByRole("button", { name: "Apply budget" }));

    await waitFor(() => expect(commands).toHaveLength(1));
    expect(commands[0]).toMatchObject({ type: "set_goal_budget" });
    expect(commands[0].budget).toEqual({ max_turns: 7 });
  });

  test("clearing existing goal budget inputs dispatches unlimited budget", async () => {
    const commands = captureCommands();
    const { user } = renderWidget(
      makeRuntime({ goal: makeGoal(), expanded: true, goalExpanded: true }),
    );

    await user.clear(screen.getByLabelText("Max turns"));
    await user.clear(screen.getByLabelText("Max minutes"));
    await user.clear(screen.getByLabelText("Max tokens"));
    await user.click(screen.getByRole("button", { name: "Apply budget" }));

    await waitFor(() => expect(commands).toHaveLength(1));
    expect(commands[0]).toMatchObject({ type: "set_goal_budget" });
    expect(commands[0].budget).toEqual({});
  });

  test("goal controls dispatch pause, resume, and stop", async () => {
    const commands = captureCommands();
    const activeView = renderWidget(
      makeRuntime({ goal: makeGoal(), expanded: true, goalExpanded: true }),
    );

    await activeView.user.click(screen.getByRole("button", { name: "Pause" }));
    await activeView.user.click(screen.getByRole("button", { name: "Stop" }));

    await waitFor(() => expect(commands).toHaveLength(2));
    activeView.unmount();

    const pausedView = renderWidget(
      makeRuntime({
        goal: makeGoal({ active: false, status: "paused" }),
        expanded: true,
        goalExpanded: true,
      }),
    );
    await pausedView.user.click(screen.getByRole("button", { name: "Resume" }));

    await waitFor(() => expect(commands).toHaveLength(3));
    expect(commands.map((command) => command.action)).toEqual([
      "pause",
      "stop",
      "resume",
    ]);
    expect(commands.every((command) => command.type === "goal_control")).toBe(
      true,
    );
  });

  test("collapsed widget keeps goal controls clickable without expanding", async () => {
    const commands = captureCommands();
    const { user } = renderWidget(makeRuntime({ goal: makeGoal() }));

    expect(screen.getByText("Goal set")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Pause goal" }));

    await waitFor(() => expect(commands).toHaveLength(1));
    expect(commands[0]).toMatchObject({
      type: "goal_control",
      action: "pause",
    });
    expect(screen.queryByText("Task Progress")).not.toBeInTheDocument();
  });

  test("collapsed goal section keeps controls clickable without expanding", async () => {
    const commands = captureCommands();
    const { user } = renderWidget(
      makeRuntime({
        goal: makeGoal({ active: false, status: "paused" }),
        expanded: true,
        goalExpanded: false,
      }),
    );

    await user.click(screen.getByRole("button", { name: "Resume goal" }));

    await waitFor(() => expect(commands).toHaveLength(1));
    expect(commands[0]).toMatchObject({
      type: "goal_control",
      action: "resume",
    });
    expect(screen.queryByLabelText("Goal text")).not.toBeInTheDocument();
  });

  test("collapsed goal controls reflect goal status availability", () => {
    renderWidget(
      makeRuntime({ goal: makeGoal({ active: false, status: "paused" }) }),
    );

    expect(
      screen.queryByRole("button", { name: "Pause goal" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Resume goal" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Stop goal" })).toBeEnabled();
  });

  test("terminal goal statuses hide the collapsed controls", () => {
    renderWidget(
      makeRuntime({ goal: makeGoal({ active: false, status: "stopped" }) }),
    );

    expect(
      screen.queryByRole("button", { name: "Pause goal" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Resume goal" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Stop goal" }),
    ).not.toBeInTheDocument();
  });

  test("transferred goals hide all controls and show the transfer owner", () => {
    renderWidget(
      makeRuntime({
        goal: makeGoal({
          active: false,
          status: "transferred",
          transferred_to: "other-chat",
        }),
        expanded: true,
        goalExpanded: true,
      }),
    );

    expect(screen.getByText("Transferred to other-chat")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Pause" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Resume" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Stop" }),
    ).not.toBeInTheDocument();
  });

  test.each(["no_progress", "budget_exhausted"] as const)(
    "%s goals allow both resume and stop",
    (status) => {
      renderWidget(
        makeRuntime({
          goal: makeGoal({ active: false, status }),
          expanded: true,
          goalExpanded: true,
        }),
      );

      expect(screen.getByRole("button", { name: "Resume" })).toBeEnabled();
      expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();
      expect(
        screen.queryByRole("button", { name: "Pause" }),
      ).not.toBeInTheDocument();
    },
  );

  test.each(["no_progress", "budget_exhausted"] as const)(
    "%s goals can be stopped from the collapsed header",
    async (status) => {
      const commands = captureCommands();
      const { user } = renderWidget(
        makeRuntime({ goal: makeGoal({ active: false, status }) }),
      );

      expect(
        screen.queryByRole("button", { name: "Pause goal" }),
      ).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Resume goal" })).toBeEnabled();

      await user.click(screen.getByRole("button", { name: "Stop goal" }));

      await waitFor(() => expect(commands).toHaveLength(1));
      expect(commands[0]).toMatchObject({
        type: "goal_control",
        action: "stop",
      });
    },
  );

  test("verifying goals allow stop but not pause or resume", () => {
    renderWidget(
      makeRuntime({
        goal: makeGoal({ active: true, status: "verifying" }),
        expanded: true,
        goalExpanded: true,
      }),
    );

    expect(screen.getByRole("button", { name: "Pause" })).toBeDisabled();
    expect(
      screen.queryByRole("button", { name: "Resume" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();
  });

  test.each(["completed", "stopped", "transferred"] as const)(
    "%s goals expose no controls at all",
    (status) => {
      renderWidget(
        makeRuntime({
          goal: makeGoal({ active: false, status }),
          expanded: true,
          goalExpanded: true,
        }),
      );

      expect(
        screen.queryByRole("button", { name: "Pause" }),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByRole("button", { name: "Resume" }),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByRole("button", { name: "Stop" }),
      ).not.toBeInTheDocument();
    },
  );

  test("active goals allow pause and stop but not resume", () => {
    renderWidget(
      makeRuntime({ goal: makeGoal(), expanded: true, goalExpanded: true }),
    );

    expect(screen.getByRole("button", { name: "Pause" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();
    expect(
      screen.queryByRole("button", { name: "Resume" }),
    ).not.toBeInTheDocument();
  });

  test("modern goal dispatches replace, clear, cancel, and autonomy commands", async () => {
    const commands = captureCommands();
    const { user } = renderWidget(
      makeRuntime({
        goal: makeModernGoal(),
        expanded: true,
        goalExpanded: true,
      }),
    );

    await user.click(screen.getByRole("button", { name: "Turn autonomy off" }));
    await user.click(screen.getByRole("button", { name: "Cancel goal" }));
    await user.click(screen.getByRole("button", { name: "Clear goal" }));
    await user.click(screen.getByRole("button", { name: "Replace" }));
    const input = screen.getByLabelText("Goal text");
    await user.clear(input);
    await user.type(input, "Ship the replacement");
    await user.click(screen.getByRole("button", { name: "Replace goal" }));

    await waitFor(() => expect(commands).toHaveLength(4));
    expect(commands[0]).toMatchObject({
      type: "set_goal_autonomy",
      autonomy: "off",
    });
    expect(commands[1]).toMatchObject({
      type: "goal_control",
      action: "cancel",
    });
    expect(commands[2]).toMatchObject({
      type: "goal_control",
      action: "clear",
    });
    expect(commands[3]).toMatchObject({
      type: "set_goal",
      content: "Ship the replacement",
      replace: true,
    });
  });

  test("terminal goals expose replace and clear but cannot be resumed", () => {
    renderWidget(
      makeRuntime({
        goal: makeModernGoal({
          resolution: "achieved",
          execution: "idle",
          status: "completed",
          can_pursue: false,
          prompt_enabled: false,
        }),
        expanded: true,
        goalExpanded: true,
      }),
    );

    expect(screen.getAllByText("Achieved").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Replace" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Clear goal" })).toBeEnabled();
    for (const name of ["Resume", "Pause", "Stop", "Cancel goal"]) {
      expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
    }
  });

  test("server transition errors are visible without changing the projected goal", async () => {
    server.use(
      http.post("*/v1/chats/:id/commands", () =>
        HttpResponse.json(
          { detail: "goal pause requires an active goal" },
          { status: 409 },
        ),
      ),
    );
    const { user } = renderWidget(
      makeRuntime({
        goal: makeModernGoal(),
        expanded: true,
        goalExpanded: true,
      }),
    );

    await user.click(screen.getByRole("button", { name: "Pause" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      /goal pause requires an active goal/,
    );
    expect(screen.getByText("Ship the frog pond")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Pause" })).toBeEnabled();
  });
});

describe("TaskProgressWidget plan projection", () => {
  test("omits the Plan section when no base plan exists", () => {
    renderWidget(
      makeRuntime({
        goal: makeGoal(),
        expanded: true,
        messages: taskMessages([
          {
            id: "task-1",
            content: "Keep the existing layout",
            status: "pending",
          },
        ]),
      }),
    );

    expect(screen.queryByTestId("task-plan-section")).not.toBeInTheDocument();
    expect(screen.getByText("Goal")).toBeInTheDocument();
    expect(screen.getByText("Keep the existing layout")).toBeInTheDocument();
  });

  test("opens a base-only plan with its version and current body", async () => {
    const { user } = renderWidget(
      makeRuntime({ expanded: true, messages: planMessages() }),
    );

    const trigger = screen.getByRole("button", {
      name: "Plan v3 · 0 updates",
    });
    expect(screen.queryByTestId("task-plan-body")).not.toBeInTheDocument();

    await user.click(trigger);

    expect(screen.getByTestId("task-plan-body")).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Execution route" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Inspect existing plan state")).toBeInTheDocument();
  });

  test("synthesizes the base plan and two deltas in order", async () => {
    const { user } = renderWidget(
      makeRuntime({
        expanded: true,
        messages: planMessages([
          "First route adjustment",
          "Second route adjustment",
        ]),
      }),
    );

    await user.click(
      screen.getByRole("button", { name: "Plan v3 · 2 updates" }),
    );

    const body = screen.getByTestId("task-plan-body");
    expect(
      within(body).getByRole("heading", { name: "Plan updates" }),
    ).toBeInTheDocument();
    expect(body).toHaveTextContent("First route adjustment");
    expect(body).toHaveTextContent("Second route adjustment");
    expect(body.textContent?.indexOf("First route adjustment")).toBeLessThan(
      body.textContent?.indexOf("Second route adjustment") ?? -1,
    );
  });

  test("opens existing history with the base plan and deltas", async () => {
    const { user } = renderWidget(
      makeRuntime({
        expanded: true,
        messages: planMessages([
          "First route adjustment",
          "Second route adjustment",
        ]),
      }),
    );

    await user.click(
      within(screen.getByTestId("task-plan-section")).getByRole("button", {
        name: "History",
      }),
    );

    expect(
      await screen.findByRole("heading", { name: "Plan history" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("📋 Base plan — task_planner · v3"),
    ).toBeInTheDocument();
    expect(screen.getByText("📋 Plan update 1")).toBeInTheDocument();
    expect(screen.getByText("📋 Plan update 2")).toBeInTheDocument();
    expect(screen.getByText("First route adjustment")).toBeInTheDocument();
    expect(screen.getByText("Second route adjustment")).toBeInTheDocument();
  });

  test("collapses the plan body without hiding history", async () => {
    const { user } = renderWidget(
      makeRuntime({ expanded: true, messages: planMessages() }),
    );
    const trigger = screen.getByRole("button", {
      name: "Plan v3 · 0 updates",
    });

    await user.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByTestId("task-plan-body")).toBeInTheDocument();

    await user.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByTestId("task-plan-body")).not.toBeInTheDocument();
    expect(
      within(screen.getByTestId("task-plan-section")).getByRole("button", {
        name: "History",
      }),
    ).toBeInTheDocument();
  });

  test("keeps Goal, Plan, and Tasks semantically distinct", async () => {
    const { user } = renderWidget(
      makeRuntime({
        goal: makeGoal({ content: "Ship a verified outcome" }),
        expanded: true,
        messages: [
          ...planMessages(),
          ...taskMessages([
            {
              id: "task-1",
              content: "Implement the Plan section",
              status: "in_progress",
            },
          ]),
        ],
      }),
    );

    await user.click(
      screen.getByRole("button", { name: "Plan v3 · 0 updates" }),
    );
    await user.click(screen.getByRole("button", { name: "Goal Idle" }));

    expect(screen.getByText("Ship a verified outcome")).toBeInTheDocument();
    expect(
      screen.getByText("Execution route and updates, not the goal."),
    ).toBeInTheDocument();
    expect(screen.getByText("Inspect existing plan state")).toBeInTheDocument();
    expect(screen.getByText("Implement the Plan section")).toBeInTheDocument();
  });
});
