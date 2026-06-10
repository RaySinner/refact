import { useState, useEffect, useCallback, useRef, RefObject } from "react";
import type { DashboardBreakpoint } from "../types";

const NARROW_MAX = 450;
const MEDIUM_MAX = 700;

function getBreakpoint(width: number): DashboardBreakpoint {
  if (width <= NARROW_MAX) return "narrow";
  if (width <= MEDIUM_MAX) return "medium";
  return "wide";
}

export function useDashboardLayout(
  containerRef: RefObject<HTMLDivElement | null>,
): DashboardBreakpoint {
  const [breakpoint, setBreakpoint] = useState<DashboardBreakpoint>("medium");
  const prevBreakpointRef = useRef<DashboardBreakpoint>("medium");

  const measure = useCallback(() => {
    if (!containerRef.current) return;
    const width = containerRef.current.clientWidth;
    const next = getBreakpoint(width);
    if (next !== prevBreakpointRef.current) {
      prevBreakpointRef.current = next;
      setBreakpoint(next);
    }
  }, [containerRef]);

  useEffect(() => {
    measure();
    const el = containerRef.current;
    if (!el) return;

    const observer = new ResizeObserver(() => measure());
    observer.observe(el);
    return () => observer.disconnect();
  }, [containerRef, measure]);

  return breakpoint;
}
