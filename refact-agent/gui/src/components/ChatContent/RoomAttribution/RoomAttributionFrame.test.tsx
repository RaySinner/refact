import { describe, expect, it } from "vitest";
import { render, screen } from "../../../utils/test-utils";
import { roleAccentSlot } from "../../../utils/roomRoleAccent";
import { RoomAttributionFrame } from "./RoomAttributionFrame";
import type { RoomAttribution } from "./roomAttribution";

function attribution(
  overrides: Partial<RoomAttribution> = {},
): RoomAttribution {
  return {
    author: { chatId: "agent-1", role: "coder", displayName: "coder" },
    timeLabel: "13:42:05",
    divider: false,
    ...overrides,
  };
}

describe("RoomAttributionFrame", () => {
  it("shows an accent bar for the agent's message", () => {
    render(
      <RoomAttributionFrame attribution={attribution()}>
        <p>body</p>
      </RoomAttributionFrame>,
    );
    expect(screen.getByTestId("room-attribution-stripe")).toBeInTheDocument();
  });

  it("shows the agent's name", () => {
    render(
      <RoomAttributionFrame
        attribution={attribution({
          author: { chatId: "agent-1", role: "coder", displayName: "coder #1" },
        })}
      >
        <p>body</p>
      </RoomAttributionFrame>,
    );
    expect(screen.getByTestId("room-attribution-name")).toHaveTextContent(
      "coder #1",
    );
  });

  it("gives two agents different accent colours", () => {
    const { rerender } = render(
      <RoomAttributionFrame
        attribution={attribution({
          author: { chatId: "agent-1", role: "coder", displayName: "coder" },
        })}
      >
        <p>body</p>
      </RoomAttributionFrame>,
    );
    const coderStyle = screen
      .getByTestId("room-attribution")
      .getAttribute("style");
    expect(coderStyle).toContain(`--room-stripe-${roleAccentSlot("coder")}`);

    rerender(
      <RoomAttributionFrame
        attribution={attribution({
          author: {
            chatId: "agent-2",
            role: "reviewer",
            displayName: "reviewer",
          },
        })}
      >
        <p>body</p>
      </RoomAttributionFrame>,
    );
    const reviewerStyle = screen
      .getByTestId("room-attribution")
      .getAttribute("style");
    expect(reviewerStyle).toContain(
      `--room-stripe-${roleAccentSlot("reviewer")}`,
    );
    expect(reviewerStyle).not.toBe(coderStyle);
  });

  it("takes colours from tokens, never from a literal hex value", () => {
    render(
      <RoomAttributionFrame attribution={attribution()}>
        <p>body</p>
      </RoomAttributionFrame>,
    );
    const style =
      screen.getByTestId("room-attribution").getAttribute("style") ?? "";
    expect(style).toContain("var(--room-stripe-");
    expect(style).toContain("var(--room-label-");
    expect(style).not.toMatch(/#[0-9a-f]{3,6}/i);
  });

  it("shows a divider when the author changed", () => {
    render(
      <RoomAttributionFrame attribution={attribution({ divider: true })}>
        <p>body</p>
      </RoomAttributionFrame>,
    );
    expect(screen.getByTestId("room-attribution-divider")).toBeInTheDocument();
    expect(screen.getByTestId("room-attribution")).toHaveAttribute(
      "data-divider",
      "true",
    );
  });

  it("shows no divider between consecutive messages of one agent", () => {
    render(
      <RoomAttributionFrame attribution={attribution({ divider: false })}>
        <p>body</p>
      </RoomAttributionFrame>,
    );
    expect(
      screen.queryByTestId("room-attribution-divider"),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("room-attribution")).toHaveAttribute(
      "data-divider",
      "false",
    );
  });

  it("shows the exact time with seconds", () => {
    render(
      <RoomAttributionFrame attribution={attribution()}>
        <p>body</p>
      </RoomAttributionFrame>,
    );
    expect(screen.getByTestId("room-attribution-time")).toHaveTextContent(
      "13:42:05",
    );
  });

  it("omits the clock when no instant is known", () => {
    render(
      <RoomAttributionFrame attribution={attribution({ timeLabel: "" })}>
        <p>body</p>
      </RoomAttributionFrame>,
    );
    expect(
      screen.queryByTestId("room-attribution-time"),
    ).not.toBeInTheDocument();
    // The byline survives: a nameless stripe would be worse than no time.
    expect(screen.getByTestId("room-attribution-name")).toBeInTheDocument();
  });

  it("renders the time on every message it wraps", () => {
    render(
      <RoomAttributionFrame
        attribution={attribution({ timeLabel: "13:42:05" })}
      >
        <p>first</p>
      </RoomAttributionFrame>,
    );
    render(
      <RoomAttributionFrame
        attribution={attribution({ timeLabel: "13:43:06" })}
      >
        <p>second</p>
      </RoomAttributionFrame>,
    );
    const times = screen.getAllByTestId("room-attribution-time");
    expect(times).toHaveLength(2);
    expect(times[0]).toHaveTextContent("13:42:05");
    expect(times[1]).toHaveTextContent("13:43:06");
  });

  it("renders the message body unchanged", () => {
    render(
      <RoomAttributionFrame attribution={attribution()}>
        <p>the actual answer</p>
      </RoomAttributionFrame>,
    );
    expect(screen.getByText("the actual answer")).toBeInTheDocument();
  });
});
