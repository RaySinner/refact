import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const tokensCss = readFileSync(
  join(process.cwd(), "src/styles/tokens.css"),
  "utf8",
);

const tokenValue = (name: string) => {
  const match = tokensCss.match(new RegExp(`${name}:\\s*(\\d+);`));
  return match ? Number(match[1]) : Number.NaN;
};

describe("design tokens", () => {
  it("elevates only modal-owned transient overlays above modal content", () => {
    expect(tokenValue("--rf-z-popover")).toBeLessThan(
      tokenValue("--rf-z-modal"),
    );
    expect(tokenValue("--rf-z-popover-modal")).toBeGreaterThan(
      tokenValue("--rf-z-modal"),
    );
    expect(tokenValue("--rf-z-tooltip-modal")).toBeGreaterThan(
      tokenValue("--rf-z-popover-modal"),
    );
  });

  it("uses a softer dark accent value and matching soft tint", () => {
    expect(tokensCss).toContain("--rf-color-accent: #7f93d8;");
    expect(tokensCss).toContain(
      "--rf-color-accent-soft: rgba(127, 147, 216, 0.12);",
    );
    expect(tokensCss).toContain("var(--rf-color-accent) 12%,");
    expect(tokensCss).toContain("--rf-focus-ring: rgba(127, 147, 216, 0.55);");
  });

  it("overrides Radix blue accent variables in dark appearance", () => {
    expect(tokensCss).toContain(
      '[data-appearance="dark"][data-accent-color="blue"]',
    );
    expect(tokensCss).toContain("--accent-9: var(--rf-color-accent);");
    expect(tokensCss).toContain("--blue-9: var(--rf-color-accent);");
  });

  it("keeps light theme accent bound to Radix", () => {
    expect(tokensCss).toContain("--rf-color-accent: var(--accent-9, #006adc);");
  });

  it("dresses the reader's own turn in the success colour", () => {
    // Slot 7 is the `user` role. It must derive from --rf-color-success rather
    // than a literal, so the reader's message follows the app's success green in
    // both appearances and never inherits a neutral slate.
    const slot7 = tokensCss.match(/--room-stripe-7:\s*([^;]+);/g);
    const label7 = tokensCss.match(/--room-label-7:\s*([^;]+);/g);
    expect(slot7).not.toBeNull();
    expect(label7).not.toBeNull();
    for (const declaration of [...(slot7 ?? []), ...(label7 ?? [])]) {
      expect(declaration).toContain("var(--rf-color-success)");
      expect(declaration).not.toMatch(/#[0-9a-f]{3,6}/i);
    }
  });

  it("does not re-bridge slot 7 to a host token", () => {
    // The VS Code adapter used to map slot 7 onto --vscode-descriptionForeground,
    // which is what rendered the reader's bubble as neutral grey.
    const vscodeAdapter = tokensCss.slice(
      tokensCss.indexOf('[data-host="vscode"]'),
    );
    expect(vscodeAdapter).not.toContain("--room-stripe-7:");
    expect(vscodeAdapter).not.toContain("--room-label-7:");
  });
});
