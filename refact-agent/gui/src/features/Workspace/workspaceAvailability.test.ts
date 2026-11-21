import { describe, expect, it } from "vitest";
import { resolveCapabilities } from "../../utils/capabilities";
import { resolveWorkspaceDockAvailability } from "./workspaceAvailability";

describe("resolveWorkspaceDockAvailability", () => {
  it("enables only the terminal panel for VS Code defaults", () => {
    expect(
      resolveWorkspaceDockAvailability(
        "vscode",
        resolveCapabilities("vscode"),
        false,
      ),
    ).toEqual({
      dock: false,
      files: false,
      git: false,
      tasks: false,
      terminal: true,
    });
  });

  it("honors an explicit VS Code terminal capability override", () => {
    expect(
      resolveWorkspaceDockAvailability(
        "vscode",
        resolveCapabilities("vscode", { terminalPanel: false }),
        false,
      ),
    ).toMatchObject({
      files: false,
      git: false,
      terminal: false,
    });
  });

  it("keeps web availability unchanged", () => {
    expect(
      resolveWorkspaceDockAvailability(
        "web",
        resolveCapabilities("web"),
        false,
      ),
    ).toEqual({
      dock: true,
      files: true,
      git: true,
      tasks: true,
      terminal: true,
    });
  });

  it("keeps the terminal disabled for JetBrains and generic IDE hosts", () => {
    for (const host of ["jetbrains", "ide"] as const) {
      expect(
        resolveWorkspaceDockAvailability(
          host,
          resolveCapabilities(host),
          false,
        ),
      ).toMatchObject({
        files: false,
        git: false,
        terminal: false,
      });
    }
  });

  it("preserves panelsForced as the highest-priority availability", () => {
    expect(
      resolveWorkspaceDockAvailability(
        "vscode",
        resolveCapabilities("vscode", { terminalPanel: false }),
        true,
      ),
    ).toEqual({
      dock: true,
      files: true,
      git: true,
      tasks: true,
      terminal: true,
    });
  });
});
