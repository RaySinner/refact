import { describe, it, expect, vi } from "vitest";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { http, HttpResponse } from "msw";
import {
  render,
  screen,
  within,
  createDefaultChatState,
} from "../../utils/test-utils";
import { setUpStore } from "../../app/store";
import { server, goodCaps, goodChatModes } from "../../utils/mockServer";
import { TrajectoryButton } from "./TrajectoryButton";

vi.mock("../Portal/Portal", () => ({
  Portal: ({ children }: { children: JSX.Element }) => children,
}));

const CONFIG_STATE = {
  config: {
    apiKey: "test",
    lspPort: 8001,
    themeProps: {},
    host: "web" as const,
  },
};

const transformStats = {
  before_message_count: 10,
  after_message_count: 5,
  before_approx_tokens: 5000,
  after_approx_tokens: 2000,
  context_messages_modified: 3,
  tool_messages_modified: 2,
};

describe("TrajectoryButton", () => {
  it("renders the trajectory button", () => {
    render(<TrajectoryButton />);
    const button = screen.getByTestId("trajectory-button");
    expect(button).toBeInTheDocument();
  });

  it("has correct aria-label", () => {
    render(<TrajectoryButton />);
    const button = screen.getByLabelText("Compress or rebuild context");
    expect(button).toBeInTheDocument();
  });

  it("uses the standard tab strip geometry for the handoff popover", async () => {
    const css = await readFile(
      path.resolve(__dirname, "TrajectoryPopover.module.css"),
      "utf8",
    );
    const tabStrip = css.match(/\.tabStrip \{[^}]+\}/)?.[0] ?? "";

    expect(tabStrip).toContain("max-width: 100%;");
    expect(tabStrip).not.toContain("width: max-content;");
    expect(tabStrip).not.toContain("grid-auto-columns: max-content;");
  });

  it("retains static and LLM compression tabs without Handoff with preview-gated actions", async () => {
    const { user } = render(<TrajectoryButton />);

    await user.click(screen.getByTestId("trajectory-button"));

    const popover = screen.getByRole("dialog");
    expect(
      within(popover).getByRole("tab", { name: "Compress in-place" }),
    ).toBeInTheDocument();
    expect(
      within(popover).getByRole("tab", { name: "LLM compression" }),
    ).toBeInTheDocument();

    expect(
      within(popover).queryByRole("tab", { name: "Handoff" }),
    ).not.toBeInTheDocument();
    expect(
      within(popover).getByRole("checkbox", { name: "Drop all context files" }),
    ).toBeInTheDocument();
    expect(
      within(popover).getByRole("checkbox", {
        name: "Remove usage and metering details",
      }),
    ).toBeInTheDocument();
    expect(
      within(popover).getByRole("button", { name: "Preview" }),
    ).toBeInTheDocument();
    expect(
      within(popover).getByRole("button", { name: "Apply" }),
    ).toBeInTheDocument();

    await user.click(
      within(popover).getByRole("tab", { name: "LLM compression" }),
    );
    expect(
      within(popover).getByText(/Rebuilds the active conversation context/i),
    ).toBeInTheDocument();
    expect(
      within(popover).getByRole("button", { name: "Rebuild context" }),
    ).toBeDisabled();
  });

  it("displays transform error alert in the compress tab when apply fails", async () => {
    server.use(
      goodCaps,
      goodChatModes,
      http.post(
        "*/v1/chats/*/trajectory/transform/preview",
        () =>
          HttpResponse.json({
            stats: transformStats,
            actions: ["Dropped 3 context files"],
          }),
      ),
      http.post(
        "*/v1/chats/*/trajectory/transform/apply",
        () =>
          HttpResponse.json(
            { error: "Session is busy. Try again later." },
            { status: 409 },
          ),
      ),
    );

    const store = setUpStore({
      config: CONFIG_STATE.config,
      chat: createDefaultChatState(),
    });
    const { user } = render(<TrajectoryButton />, { store });

    await user.click(screen.getByTestId("trajectory-button"));
    const popover = screen.getByRole("dialog");

    await user.click(
      within(popover).getByRole("button", { name: "Preview" }),
    );
    const applyButton = within(popover).getByRole("button", { name: "Apply" });
    await screen.findByText((content) => content.includes("reduction"));
    expect(applyButton).toBeEnabled();
    await user.click(applyButton);

    const alert = await within(popover).findByRole("alert");
    expect(alert).toHaveTextContent("Unable to apply compression.");
  });

  it("does not display a transform error alert when no error is set", async () => {
    const { user } = render(<TrajectoryButton />);

    await user.click(screen.getByTestId("trajectory-button"));

    const popover = screen.getByRole("dialog");
    expect(
      within(popover).queryByRole("alert"),
    ).not.toBeInTheDocument();
  });
});
