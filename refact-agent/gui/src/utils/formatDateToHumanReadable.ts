export function formatDateToHumanReadable(
  date: string,
  timeZone: string,
  locale?: string,
): string {
  const utcDate = new Date(date);
  return new Intl.DateTimeFormat(locale, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
    timeZone: timeZone,
  })
    .format(utcDate)
    .replace(",", "");
}

export const formatDateOrTimeBasedOnToday = (
  isoString: string | null,
  timezone: string,
) => {
  if (!isoString) return "";

  const date = new Date(isoString);
  const now = new Date();
  const isToday =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();

  if (isToday) {
    return new Intl.DateTimeFormat(undefined, {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
      timeZone: timezone,
    }).format(date);
  }

  return formatDateToHumanReadable(isoString, timezone);
};

/**
 * Exact wall-clock time as `HH:MM:SS`, 24-hour, leading zeros guaranteed.
 *
 * `hourCycle: "h23"` rather than `hour12: false`: the two are not equivalent. With
 * `hour12: false` some ICU builds render midnight as `24:00:00`, which is not a clock
 * time anybody reads. `h23` is the only form that means "midnight is zero".
 *
 * Returns `""` for a missing or unparseable instant so a caller can omit the label
 * rather than print a wrong one.
 */
export function formatExactClockTime(
  epochMs: number | null | undefined,
  timeZone: string,
  locale?: string,
): string {
  if (epochMs === null || epochMs === undefined) return "";
  if (!Number.isFinite(epochMs)) return "";

  const date = new Date(epochMs);
  if (Number.isNaN(date.getTime())) return "";

  return new Intl.DateTimeFormat(locale, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
    timeZone,
  }).format(date);
}
