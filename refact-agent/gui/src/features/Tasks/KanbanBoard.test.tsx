import { describe, expect, it } from "vitest";
import { render, screen } from "../../utils/test-utils";
import type {
  BoardCard,
  TaskBoard,
  TeamMember,
} from "../../services/refact/tasks";
import { roleAccentSlot } from "../../utils/roomRoleAccent";
import { KanbanBoard } from "./KanbanBoard";

const makeCard = (overrides: Partial<BoardCard> = {}): BoardCard => ({
  id: "T-1",
  title: "Card title",
  column: "doing",
  priority: "P1",
  depends_on: [],
  instructions: "",
  assignee: null,
  agent_chat_id: null,
  status_updates: [],
  final_report: null,
  created_at: "2026-06-15T00:00:00Z",
  started_at: null,
  completed_at: null,
  target_files: [],
  ...overrides,
});

const makeBoard = (card: BoardCard): TaskBoard => ({
  schema_version: 1,
  rev: 1,
  columns: [{ id: "doing", title: "Doing" }],
  cards: [card],
});

const makeMember = (overrides: Partial<TeamMember> = {}): TeamMember => ({
  role: "coder",
  ...overrides,
});

function renderBoard(card: BoardCard) {
  return render(<KanbanBoard board={makeBoard(card)} />);
}

describe("KanbanCard agent badge", () => {
  it("single_agent_card_renders_existing_badge", () => {
    renderBoard(
      makeCard({ assignee: "agent-abc123", agent_chat_id: "chat-1" }),
    );

    expect(screen.getByRole("button", { name: /agent/i })).toBeInTheDocument();
    expect(screen.queryByTestId("room-roster")).not.toBeInTheDocument();
  });

  it("room_card_with_no_team_members_falls_back_to_single_agent_badge", () => {
    renderBoard(
      makeCard({
        assignee: "agent-abc123",
        agent_chat_id: "chat-1",
        team_members: [],
      }),
    );

    expect(screen.getByRole("button", { name: /agent/i })).toBeInTheDocument();
    expect(screen.queryByTestId("room-roster")).not.toBeInTheDocument();
  });

  it("room_card_lists_every_member", () => {
    renderBoard(
      makeCard({
        assignee: "agent-architect",
        team_members: [
          makeMember({ role: "architect", agent_id: "agent-a" }),
          makeMember({ role: "coder", agent_id: "agent-b" }),
          makeMember({ role: "reviewer", agent_id: "agent-c" }),
        ],
      }),
    );

    expect(screen.getByTestId("room-roster")).toBeInTheDocument();
    expect(screen.getByTestId("room-member-architect")).toBeInTheDocument();
    expect(screen.getByTestId("room-member-coder")).toBeInTheDocument();
    expect(screen.getByTestId("room-member-reviewer")).toBeInTheDocument();
    // A room must not also claim a single agent.
    expect(
      screen.queryByRole("button", { name: /agent/i }),
    ).not.toBeInTheDocument();
  });

  it("room_card_shows_member_role_and_status", () => {
    renderBoard(
      makeCard({
        team_members: [
          makeMember({ role: "coder", member_status: "running" }),
          makeMember({ role: "reviewer", status: "completed" }),
          makeMember({ role: "researcher" }),
        ],
      }),
    );

    const coder = screen.getByTestId("room-member-coder");
    expect(coder).toHaveTextContent("coder");
    expect(coder).toHaveTextContent("running");

    // Legacy scalar `status` is parsed when `member_status` is absent.
    const reviewer = screen.getByTestId("room-member-reviewer");
    expect(reviewer).toHaveTextContent("done");

    // Nothing set at all reads as pending, matching the engine default.
    expect(screen.getByTestId("room-member-researcher")).toHaveTextContent(
      "pending",
    );
  });

  it("room_card_member_badges_use_role_accent", () => {
    renderBoard(
      makeCard({
        team_members: [
          makeMember({ role: "architect" }),
          makeMember({ role: "coder" }),
          makeMember({ role: "reviewer" }),
        ],
      }),
    );

    const architect = screen.getByTestId("room-member-architect");
    const coder = screen.getByTestId("room-member-coder");
    const reviewer = screen.getByTestId("room-member-reviewer");

    expect(architect.getAttribute("style")).toContain(
      `var(--room-stripe-${roleAccentSlot("architect")})`,
    );
    expect(coder.getAttribute("style")).toContain(
      `var(--room-stripe-${roleAccentSlot("coder")})`,
    );
    expect(reviewer.getAttribute("style")).toContain(
      `var(--room-stripe-${roleAccentSlot("reviewer")})`,
    );
    // Distinct roles must not share a stripe.
    expect(roleAccentSlot("architect")).not.toBe(roleAccentSlot("coder"));
    expect(architect.getAttribute("style")).not.toEqual(
      coder.getAttribute("style"),
    );
  });
});
