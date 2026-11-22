export type MessageCreatedAtSource = {
  created_at_ms?: unknown;
  extra?: Record<string, unknown>;
};

export function epochMsOrNull(value: unknown): number | null {
  if (typeof value !== "number") return null;
  if (!Number.isFinite(value) || value <= 0) return null;
  return value;
}

export function decodeMessageCreatedAtMs(
  message: MessageCreatedAtSource | null | undefined,
): number | null {
  if (!message) return null;
  return (
    epochMsOrNull(message.created_at_ms) ??
    epochMsOrNull(message.extra?.created_at_ms)
  );
}

export function withCanonicalCreatedAtMs<T extends MessageCreatedAtSource>(
  message: T,
): T {
  const createdAtMs = decodeMessageCreatedAtMs(message);
  if (createdAtMs === null) return message;
  return Object.assign({}, message, {
    created_at_ms: createdAtMs,
    extra: {
      ...(message.extra ?? {}),
      created_at_ms: createdAtMs,
    },
  });
}
