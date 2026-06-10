import { describe, expect, test } from "vitest";
import { render, screen } from "@testing-library/react";
import { Theme } from "@radix-ui/themes";

import { ProcessStatusBadge } from "./ProcessStatusBadge";

describe("ProcessStatusBadge", () => {
  test.each([
    ["running", "running"],
    ["running_in_background", "background"],
  ] as const)("%s status renders distinctly", (status, label) => {
    render(
      <Theme>
        <ProcessStatusBadge status={status} />
      </Theme>,
    );

    const badge = screen.getByTestId(`exec-status-${status}`);
    expect(badge).toHaveTextContent(label);
    expect(badge.className).toContain(
      status === "running" ? "statusRunning" : "statusRunningInBackground",
    );
  });

  test("unknown process status renders a neutral fallback", () => {
    render(
      <Theme>
        <ProcessStatusBadge status="paused" />
      </Theme>,
    );

    expect(screen.getByTestId("exec-status-paused")).toHaveTextContent(
      "unknown",
    );
  });
});
