import { act, render, screen } from "../../utils/test-utils";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

const appearanceMock = vi.hoisted(() => ({
  current: "light" as "light" | "dark",
}));

const mermaidMock = vi.hoisted(() => ({
  render: vi.fn(),
  initialize: vi.fn(),
  registerLayoutLoaders: vi.fn(),
  detectType: vi.fn(),
}));

vi.mock("mermaid", () => ({ default: mermaidMock }));
vi.mock("../../hooks/useAppearance", () => ({
  useAppearance: () => ({ appearance: appearanceMock.current }),
}));
vi.mock("../../features/Buddy/reportBuddyFrontendError", () => ({
  reportBuddyFrontendError: vi.fn(),
}));

import {
  MermaidBlock,
  resetMermaidRenderCacheForTests,
} from "./MermaidBlock";

type RenderedMermaid = { svg: string };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function diagram(label: string, id: string) {
  return `<svg id="${id}" viewBox="0 0 100 50" width="100" height="50"><text>${label}</text></svg>`;
}

async function advanceTimers(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function flushPromises() {
  await act(async () => {
    for (let index = 0; index < 8; index += 1) {
      await Promise.resolve();
    }
  });
}

describe("MermaidBlock render lifecycle", () => {
  beforeEach(() => {
    resetMermaidRenderCacheForTests();
    vi.useFakeTimers();
    appearanceMock.current = "light";
    mermaidMock.render.mockReset();
    mermaidMock.render.mockImplementation((id: string, code: string) =>
      Promise.resolve({ svg: diagram(code, id) }),
    );
    mermaidMock.initialize.mockClear();
    mermaidMock.registerLayoutLoaders.mockClear();
    mermaidMock.detectType.mockReset();
    mermaidMock.detectType.mockReturnValue("sequence");
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  test("does not render unchanged source after timers, unrelated props, or a virtualization remount", async () => {
    const source = "static-remount-source";
    const view = render(<MermaidBlock code={source} />);

    await advanceTimers(100);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(1);
    const initializeCalls = mermaidMock.initialize.mock.calls.length;

    await advanceTimers(30_000);
    await flushPromises();
    view.rerender(<MermaidBlock code={source} onCopyClick={() => undefined} />);
    await advanceTimers(30_000);
    await flushPromises();

    expect(mermaidMock.render).toHaveBeenCalledTimes(1);
    expect(mermaidMock.initialize).toHaveBeenCalledTimes(initializeCalls);

    view.unmount();
    render(<MermaidBlock code={source} />);
    await advanceTimers(100);
    await flushPromises();

    expect(mermaidMock.render).toHaveBeenCalledTimes(1);
    expect(mermaidMock.initialize).toHaveBeenCalledTimes(initializeCalls);
    expect(screen.getByText(source)).toBeInTheDocument();
  });

  test("renders a real source change exactly once", async () => {
    const { rerender } = render(<MermaidBlock code="source-change-before" />);
    await advanceTimers(100);
    await flushPromises();

    rerender(<MermaidBlock code="source-change-after" />);
    await advanceTimers(100);
    await flushPromises();

    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
    expect(mermaidMock.render).toHaveBeenNthCalledWith(
      2,
      expect.any(String),
      "source-change-after",
    );

    rerender(<MermaidBlock code="source-change-after" />);
    await advanceTimers(30_000);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
  });

  test("changes theme only when needed and reuses the previous theme render", async () => {
    const source = "theme-change-source";
    const lightCopyClick = () => undefined;
    const darkCopyClick = () => undefined;
    const { rerender } = render(
      <MermaidBlock code={source} onCopyClick={lightCopyClick} />,
    );
    await advanceTimers(100);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(1);

    appearanceMock.current = "dark";
    rerender(<MermaidBlock code={source} onCopyClick={darkCopyClick} />);
    await advanceTimers(100);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
    const initializeCalls = mermaidMock.initialize.mock.calls.length;

    rerender(<MermaidBlock code={source} onCopyClick={darkCopyClick} />);
    await advanceTimers(30_000);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
    expect(mermaidMock.initialize).toHaveBeenCalledTimes(initializeCalls);

    appearanceMock.current = "light";
    rerender(<MermaidBlock code={source} onCopyClick={lightCopyClick} />);
    await advanceTimers(100);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
    expect(mermaidMock.initialize).toHaveBeenCalledTimes(initializeCalls);
  });

  test("keeps diagrams independent when one render times out", async () => {
    const pending = deferred<RenderedMermaid>();
    mermaidMock.render.mockImplementationOnce(() => pending.promise);
    const { rerender } = render(
      <>
        <MermaidBlock code="independent-timeout-source" />
        <MermaidBlock code="independent-healthy-source" />
      </>,
    );

    await advanceTimers(100);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(1);

    await advanceTimers(15_000);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
    expect(mermaidMock.render).toHaveBeenNthCalledWith(
      2,
      expect.any(String),
      "independent-healthy-source",
    );
    expect(
      screen.getByText("independent-healthy-source"),
    ).toBeInTheDocument();

    rerender(
      <>
        <MermaidBlock code="independent-timeout-source" />
        <MermaidBlock code="independent-healthy-source" />
      </>,
    );
    await advanceTimers(30_000);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
    expect(
      screen.getByText("independent-healthy-source"),
    ).toBeInTheDocument();
  });

  test("skips a cancelled queued source instead of running stale and current revisions", async () => {
    const blocker = deferred<RenderedMermaid>();
    mermaidMock.render.mockImplementationOnce(() => blocker.promise);
    const { rerender } = render(
      <>
        <MermaidBlock code="queue-blocker-source" />
        <MermaidBlock code="cancelled-queued-source" />
      </>,
    );

    await advanceTimers(100);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(1);

    rerender(
      <>
        <MermaidBlock code="queue-blocker-source" />
        <MermaidBlock code="current-queued-source" />
      </>,
    );
    await advanceTimers(100);
    await flushPromises();

    await act(async () => {
      blocker.resolve({ svg: diagram("queue-blocker-source", "blocker") });
      await blocker.promise;
    });
    await flushPromises();

    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
    expect(mermaidMock.render).toHaveBeenNthCalledWith(
      2,
      expect.any(String),
      "current-queued-source",
    );
    expect(screen.getByText("current-queued-source")).toBeInTheDocument();

    await advanceTimers(30_000);
    await flushPromises();
    expect(mermaidMock.render).toHaveBeenCalledTimes(2);
  });

  test("shares an identical settled render without duplicate SVG ids", async () => {
    const source = "identical-diagram-source";
    render(
      <>
        <MermaidBlock code={source} />
        <MermaidBlock code={source} />
      </>,
    );

    await advanceTimers(100);
    await flushPromises();

    expect(mermaidMock.render).toHaveBeenCalledTimes(1);
    const svgIds = screen
      .getAllByTestId("mermaid-canvas")
      .map((canvas) => canvas.querySelector("svg")?.id);
    expect(svgIds[0]).toBeTruthy();
    expect(svgIds[1]).toBeTruthy();
    expect(svgIds[0]).not.toBe(svgIds[1]);
  });
});
