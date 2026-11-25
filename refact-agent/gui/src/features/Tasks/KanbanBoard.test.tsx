import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "../../utils/test-utils";
import { http, HttpResponse } from "msw";
import { server } from "../../utils/mockServer";
import {
  makeExternalDetails,
  makeExternalParticipant,
} from "../../__fixtures__/externalAgents";
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
  beforeEach(() => {
    server.use(
      http.get("*/v1/external-participants", () =>
        HttpResponse.json({ mode: "internal_agents", participants: [] }),
      ),
    );
  });

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

  it("room_member_badge_click_triggers_on_agent_click", async () => {
    const onAgentClick = vi.fn();
    const { user } = render(
      <KanbanBoard
        board={makeBoard(
          makeCard({
            agent_chat_id: "card-chat-id",
            team_members: [
              makeMember({ role: "architect", agent_chat_id: "chat-arch" }),
              makeMember({ role: "coder", agent_chat_id: "chat-coder" }),
            ],
          }),
        )}
        onAgentClick={onAgentClick}
      />,
    );

    const coderBadge = screen.getByTestId("room-member-coder");
    await user.click(coderBadge);

    expect(onAgentClick).toHaveBeenCalledWith(
      expect.objectContaining({
        agent_chat_id: "chat-coder",
      }),
    );
  });

  it("shows_one_writer_and_a_live_verified_external_advisor_without_chat_fallback", async () => {
    const participant = makeExternalParticipant({
      id: "external-reviewer",
      name: "External reviewer",
      state: "verified",
      server_version: "1.2.3",
    });
    const details = makeExternalDetails(participant, {
      deliveries: [
        {
          delivery_id: "delivery-fresh",
          room_id: "room-1",
          event_id: "event-1",
          participant_id: participant.id,
          status: "terminal",
          request_state: "completed",
          logical_attempt: 1,
          response_id: "response-1",
          session_id: "session-1",
          source_revision: "rev-fresh",
          source_digest: "digest-fresh",
          source_freshness: "fresh",
          environment_version: 1,
          error: null,
          created_at: 1,
          updated_at: 2,
        },
      ],
      evidence: [
        {
          smoke_suite_id: "smoke-1",
          server_version: "1.2.3",
          cli_version: participant.cli_pin,
          model_version: participant.model,
          checks: [{ name: "live", status: "pass" }],
          generated_at: 1,
          sanitized_report_location: "evidence/smoke-1",
        },
      ],
    });
    server.use(
      http.get("*/v1/external-participants", () =>
        HttpResponse.json({
          mode: "external_participants",
          participants: [participant],
        }),
      ),
      http.get(`*/v1/external-participants/${participant.id}`, () =>
        HttpResponse.json(details),
      ),
    );
    const onAgentClick = vi.fn();
    const { user, store } = render(
      <KanbanBoard
        board={makeBoard(
          makeCard({
            agent_chat_id: "writer-chat",
            team_members: [
              makeMember({
                role: "coder",
                agent_chat_id: "writer-chat",
                writes: true,
              }),
              makeMember({
                role: "reviewer",
                kind: "external",
                participant_id: participant.id,
                agent_id: participant.id,
                agent_chat_id: null,
                writes: false,
              }),
            ],
          }),
        )}
        onAgentClick={onAgentClick}
      />,
    );

    expect(screen.getByTestId("room-member-coder")).toHaveTextContent("writer");
    const external = await screen.findByLabelText(
      "Open external advisor External reviewer",
    );
    expect(external).toHaveTextContent("read-only");
    expect(external).toHaveTextContent("Live verified");
    expect(external).toHaveTextContent("fresh");
    expect(external).toHaveAttribute(
      "title",
      expect.stringContaining("rev-fresh"),
    );
    await user.click(external);
    expect(onAgentClick).not.toHaveBeenCalled();
    expect(store.getState().pages.at(-1)).toEqual({
      name: "external agents settings",
    });
  });

  it("distinguishes_staging_and_stale_external_evidence_from_live_verified", async () => {
    const participant = makeExternalParticipant({
      id: "staging-reviewer",
      name: "Staging reviewer",
      protocol_version: "2026-10-19",
      harness_id: "harness-1",
      environment_id: "environment-1",
      environment_version: 4,
    });
    const details = makeExternalDetails(participant, {
      deliveries: [
        {
          delivery_id: "delivery-stale",
          room_id: "room-1",
          event_id: "event-1",
          participant_id: participant.id,
          status: "terminal",
          request_state: "failed",
          logical_attempt: 2,
          response_id: null,
          session_id: "session-1",
          source_revision: "rev-stale",
          source_digest: "digest-stale",
          source_freshness: "stale",
          environment_version: 4,
          error: {
            code: "refact.room.stale_source",
            message: "Source changed",
          },
          created_at: 1,
          updated_at: 2,
        },
      ],
    });
    server.use(
      http.get("*/v1/external-participants", () =>
        HttpResponse.json({
          mode: "external_participants",
          participants: [participant],
        }),
      ),
      http.get(`*/v1/external-participants/${participant.id}`, () =>
        HttpResponse.json(details),
      ),
    );
    renderBoard(
      makeCard({
        team_members: [
          makeMember({
            role: "reviewer",
            participant_id: participant.id,
            agent_chat_id: null,
            writes: false,
          }),
          makeMember({
            role: "coder",
            agent_chat_id: "writer-chat",
            writes: true,
          }),
        ],
      }),
    );

    const external = await screen.findByLabelText(
      "Open external advisor Staging reviewer",
    );
    expect(external).toHaveTextContent("Staging ready — not live verified");
    expect(external).toHaveTextContent("stale");
    expect(external).not.toHaveTextContent("Live verified");
  });
});
