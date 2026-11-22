import { act, fireEvent, render, screen } from "../../utils/test-utils";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

const mermaidMock = vi.hoisted(() => ({
  render: vi.fn(),
  initialize: vi.fn(),
  registerLayoutLoaders: vi.fn(),
  detectType: vi.fn(),
}));

vi.mock("mermaid", () => ({ default: mermaidMock }));
vi.mock("../../hooks/useAppearance", () => ({
  useAppearance: () => ({ appearance: "light" }),
}));
vi.mock("../../features/Buddy/reportBuddyFrontendError", () => ({
  reportBuddyFrontendError: vi.fn(),
}));

import { MermaidBlock } from "./MermaidBlock";

type RenderedMermaid = { svg: string };

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function diagram(label: string) {
  return `<svg viewBox="0 0 100 50" width="100" height="50"><text>${label}</text></svg>`;
}

async function advanceTimers(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function flushPromises() {
  await act(async () => {
    for (let index = 0; index < 6; index += 1) {
      await Promise.resolve();
    }
  });
}

function renderedBox(canvas: HTMLElement) {
  return canvas.firstElementChild as HTMLElement;
}

describe("MermaidBlock render failures", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    mermaidMock.render.mockReset();
    mermaidMock.initialize.mockClear();
    mermaidMock.registerLayoutLoaders.mockClear();
    mermaidMock.detectType.mockReset();
    mermaidMock.detectType.mockReturnValue("sequence");
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  test("keeps the last SVG, geometry, zoom and scroll after a later timeout", async () => {
    const pending = deferred<RenderedMermaid>();
    mermaidMock.render
      .mockResolvedValueOnce({ svg: diagram("last good") })
      .mockReturnValueOnce(pending.promise);
    const { rerender } = render(<MermaidBlock code="first source" />);

    await advanceTimers(100);
    await flushPromises();
    const canvas = screen.getByTestId("mermaid-canvas");
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    await advanceTimers(16);
    canvas.scrollLeft = 37;
    canvas.scrollTop = 19;

    expect(renderedBox(canvas)).toHaveStyle({
      width: "140px",
      height: "70px",
    });
    rerender(<MermaidBlock code="changed source" />);
    await advanceTimers(100);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
    await advanceTimers(15_000);
    await flushPromises();

    expect(screen.getByRole("alert")).toHaveTextContent(
      "Mermaid render failed: Mermaid render timed out after 15000ms",
    );
    expect(screen.getByTestId("mermaid-canvas")).toBe(canvas);
    expect(canvas.querySelector("svg")?.textContent).toBe("last good");
    expect(renderedBox(canvas)).toHaveStyle({
      width: "140px",
      height: "70px",
    });
    expect(canvas.scrollLeft).toBe(37);
    expect(canvas.scrollTop).toBe(19);
    expect(screen.getByText("140%")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Show source" }),
    ).toBeInTheDocument();

    rerender(<MermaidBlock code="changed source" />);
    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(screen.getByTestId("mermaid-canvas")).toBe(canvas);
  });

  test("shows persistent raw source and a readable error when no SVG succeeded", async () => {
    mermaidMock.render.mockRejectedValueOnce(
      new Error("initial syntax error"),
    );
    const { rerender } = render(<MermaidBlock code="invalid source" />);

    await advanceTimers(100);
    await flushPromises();

    expect(screen.queryByTestId("mermaid-canvas")).toBeNull();
    expect(screen.getByText("invalid source")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Mermaid render failed: initial syntax error",
    );
    rerender(<MermaidBlock code="invalid source" />);
    expect(screen.getByText("invalid source")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toBeInTheDocument();
  });

  test("ignores a stale result and applies a real source change atomically", async () => {
    const stale = deferred<RenderedMermaid>();
    const current = deferred<RenderedMermaid>();
    mermaidMock.render
      .mockResolvedValueOnce({ svg: diagram("initial") })
      .mockReturnValueOnce(stale.promise)
      .mockReturnValueOnce(current.promise);
    const { rerender } = render(<MermaidBlock code="initial source" />);

    await advanceTimers(100);
    await flushPromises();
    const canvas = screen.getByTestId("mermaid-canvas");
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    await advanceTimers(16);
    canvas.scrollLeft = 42;
    canvas.scrollTop = 24;

    rerender(<MermaidBlock code="stale source" />);
    await advanceTimers(100);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
    rerender(<MermaidBlock code="current source" />);
    await advanceTimers(100);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
    await act(async () => {
      stale.resolve({ svg: diagram("stale") });
      await stale.promise;
    });
    await flushPromises();

    expect(canvas.querySelector("svg")?.textContent).toBe("initial");
    expect(screen.getByText("140%")).toBeInTheDocument();
    expect(canvas.scrollLeft).toBe(42);
    expect(canvas.scrollTop).toBe(24);

    await act(async () => {
      current.resolve({ svg: diagram("current") });
      await current.promise;
    });
    await flushPromises();

    expect(screen.getByTestId("mermaid-canvas")).toBe(canvas);
    expect(canvas.querySelector("svg")?.textContent).toBe("current");
    expect(renderedBox(canvas)).toHaveStyle({
      width: "100px",
      height: "50px",
    });
    expect(screen.getByText("100%")).toBeInTheDocument();
    expect(canvas.scrollLeft).toBe(0);
    expect(canvas.scrollTop).toBe(0);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  test("keeps a healthy block mounted when another block fails", async () => {
    mermaidMock.render
      .mockResolvedValueOnce({ svg: diagram("healthy") })
      .mockRejectedValueOnce(new Error("other block failed"));
    render(
      <>
        <MermaidBlock code="healthy source" />
        <MermaidBlock code="broken source" />
      </>,
    );

    await advanceTimers(100);
    await flushPromises();

    const canvas = screen.getByTestId("mermaid-canvas");
    expect(canvas.querySelector("svg")?.textContent).toBe("healthy");
    expect(screen.getByText("broken source")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Mermaid render failed: other block failed",
    );
  });
});
