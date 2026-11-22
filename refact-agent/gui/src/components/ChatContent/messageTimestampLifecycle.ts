export type MessageTimestampLifecycle = {
  chatId: string | null;
  firstSeenMs: Map<string, number>;
  preexistingKeys: Set<string>;
  primedChats: Set<string>;
  observedMessages: unknown;
  observedSeq: string | undefined;
  awaitingSnapshot: boolean;
};

export function createMessageTimestampLifecycle(): MessageTimestampLifecycle {
  return {
    chatId: null,
    firstSeenMs: new Map<string, number>(),
    preexistingKeys: new Set<string>(),
    primedChats: new Set<string>(),
    observedMessages: null,
    observedSeq: undefined,
    awaitingSnapshot: false,
  };
}

export function updateMessageTimestampLifecycle(
  state: MessageTimestampLifecycle,
  input: {
    chatId: string;
    messages: unknown;
    itemKeys: readonly string[];
    snapshotReceived: boolean;
    eventSeq: string | undefined;
    sseStatus: "disconnected" | "connecting" | "connected" | null;
    nowMs: number;
  },
): void {
  const {
    chatId,
    messages,
    itemKeys,
    snapshotReceived,
    eventSeq,
    sseStatus,
    nowMs,
  } = input;
  const reconnecting =
    sseStatus === "connecting" || sseStatus === "disconnected";
  const connected = sseStatus === "connected";

  if (state.chatId !== chatId) {
    state.chatId = chatId;
    state.firstSeenMs = new Map<string, number>();
    state.preexistingKeys = new Set(itemKeys);
    state.awaitingSnapshot = reconnecting;
    if (snapshotReceived) {
      state.primedChats.add(chatId);
    } else {
      state.primedChats.delete(chatId);
    }
  } else {
    if (reconnecting) state.awaitingSnapshot = true;
    if (
      snapshotReceived &&
      eventSeq === "0" &&
      (state.observedSeq !== "0" ||
        (state.awaitingSnapshot &&
          connected &&
          state.observedMessages !== messages))
    ) {
      state.preexistingKeys = new Set(itemKeys);
      state.primedChats.add(chatId);
      state.awaitingSnapshot = false;
    }
  }
  state.observedMessages = messages;
  state.observedSeq = eventSeq;

  const primed = state.primedChats.has(chatId);
  const liveKeys = new Set<string>();
  for (const key of itemKeys) {
    if (liveKeys.has(key)) continue;
    liveKeys.add(key);
    if (
      primed &&
      !state.firstSeenMs.has(key) &&
      !state.preexistingKeys.has(key)
    ) {
      state.firstSeenMs.set(key, nowMs);
    }
  }
  for (const key of state.firstSeenMs.keys()) {
    if (!liveKeys.has(key)) state.firstSeenMs.delete(key);
  }
}
