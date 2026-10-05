import { describe, expect, it } from "vitest";
import { formatExactClockTime } from "./formatDateToHumanReadable";

const UTC = "UTC";

// 2026-10-05T13:42:05Z
const AT_13_42_05 = Date.UTC(2026, 9, 5, 13, 42, 5);
const AT_MIDNIGHT = Date.UTC(2026, 9, 5, 0, 0, 0);

describe("formatExactClockTime", () => {
  it("shows exact time with seconds", () => {
    expect(formatExactClockTime(AT_13_42_05, UTC, "en-US")).toBe("13:42:05");
  });

  it("renders 24-hour time, never 12-hour", () => {
    const afternoon = formatExactClockTime(AT_13_42_05, UTC, "en-US");
    expect(afternoon).not.toContain("PM");
    expect(afternoon).not.toContain("AM");
    expect(afternoon.startsWith("13")).toBe(true);
  });

  it("renders midnight as 00, not 24", () => {
    expect(formatExactClockTime(AT_MIDNIGHT, UTC, "en-US")).toBe("00:00:00");
  });

  it("pads single-digit hours, minutes and seconds", () => {
    // 2026-10-05T07:04:09Z
    const early = Date.UTC(2026, 9, 5, 7, 4, 9);
    expect(formatExactClockTime(early, UTC, "en-US")).toBe("07:04:09");
  });

  it("honours the requested time zone", () => {
    expect(formatExactClockTime(AT_13_42_05, "Europe/Berlin", "en-US")).toBe(
      "15:42:05",
    );
  });

  it("returns empty string rather than a wrong clock for missing input", () => {
    expect(formatExactClockTime(null, UTC)).toBe("");
    expect(formatExactClockTime(undefined, UTC)).toBe("");
    expect(formatExactClockTime(Number.NaN, UTC)).toBe("");
    expect(formatExactClockTime(Number.POSITIVE_INFINITY, UTC)).toBe("");
  });
});
