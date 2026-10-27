import { readFileSync } from "node:fs";
import { describe, expect, test } from "vitest";

const stylesheet = readFileSync(
  "src/components/Markdown/DiagramBlock.module.css",
  "utf8",
);

function readCssRule(source: string, selectorText: string): string {
  const group = selectorText
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) =>
      part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/\s+/g, "\\s+"),
    )
    .join("\\s*,\\s*");
  const match = new RegExp(`(?:^|\\r?\\n)\\s*${group}\\s*{`).exec(source);
  if (match?.index === undefined) {
    throw new Error(`Missing CSS block for ${selectorText}`);
  }
  const open = source.indexOf("{", match.index);
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === "{") depth += 1;
    if (source[i] === "}") depth -= 1;
    if (depth === 0) return source.slice(open + 1, i);
  }
  throw new Error(`Unclosed CSS block for ${selectorText}`);
}

describe("DiagramBlock sizing", () => {
  test("uses intrinsic dimensions and scrolls oversized diagrams", () => {
    const container = readCssRule(stylesheet, ".diagram_container");
    expect(container).toContain("width: fit-content");
    expect(container).toContain("var(--rf-control-h-icon-sm)");
    expect(container).toContain("var(--rf-control-h-lg)");

    const canvas = readCssRule(stylesheet, ".diagram_canvas");
    expect(canvas).toContain("width: fit-content");
    expect(canvas).toContain("max-height: clamp(240px, 70vh, 720px)");
    expect(canvas).toContain("overflow-y: auto");
    expect(canvas).not.toMatch(/^\s+height:/mu);

    const toolbar = readCssRule(stylesheet, ".diagram_toolbar");
    expect(toolbar).toContain("position: absolute");

    expect(readCssRule(stylesheet, ".diagram_canvas:focus-visible")).toContain(
      "var(--rf-focus-ring-w)",
    );
  });

  test("allows foreignObject text to overflow node boundaries", () => {
    const foreignObject = readCssRule(
      stylesheet,
      ".diagram_render :global(foreignObject)",
    );
    expect(foreignObject).toContain("overflow: visible");
  });
});

const FOREIGN_OBJECT_LABEL_SELECTORS = [
  ".diagram_render :global(foreignObject div)",
  ".diagram_render :global(foreignObject span)",
  ".diagram_render :global(foreignObject p)",
  ".diagram_fallback :global(foreignObject div)",
  ".diagram_fallback :global(foreignObject span)",
  ".diagram_fallback :global(foreignObject p)",
].join(", ");

describe("DiagramBlock label typography", () => {
  test("never collapses label line boxes in the diagram container", () => {
    expect(stylesheet).not.toMatch(/line-height:\s*0\s*;/u);
    expect(readCssRule(stylesheet, ".diagram_render svg")).toContain(
      "display: block",
    );
  });

  test("declares typography for HTML labels inside foreignObject", () => {
    const labels = readCssRule(stylesheet, FOREIGN_OBJECT_LABEL_SELECTORS);
    const lineHeight = Number(/line-height:\s*([\d.]+)/u.exec(labels)?.[1]);

    expect(lineHeight).toBeGreaterThanOrEqual(1.2);
    expect(lineHeight).toBeLessThanOrEqual(1.4);
    expect(labels).toContain("white-space: normal");
    expect(labels).toContain("overflow-wrap: anywhere");
  });
});
