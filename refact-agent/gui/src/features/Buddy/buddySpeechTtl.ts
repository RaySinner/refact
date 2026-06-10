import { isAnyOf, type ListenerMiddlewareInstance } from "@reduxjs/toolkit";
import {
  setActiveSpeech,
  setBuddySnapshot,
  clearActiveSpeech,
} from "./buddySlice";
import type { BuddySpeechItem } from "./types";

/**
 * Shape of the part of the Redux state this listener reads. Kept narrow on
 * purpose so the listener (and its tests) don't have to depend on the full
 * RootState type.
 */
export interface BuddySpeechTtlState {
  buddy: { activeSpeech: BuddySpeechItem | null };
}

/**
 * Wires a listener that honors `BuddySpeechItem.persistent` / `ttl_seconds`
 * onto the supplied `listenerMiddleware`.
 *
 * The Rust engine emits speech items with a TTL but never auto-clears them on
 * the server, and the GUI used to leave non-persistent speeches up forever
 * (e.g. care actions like Play left "Played together with bug. Mischief
 * pressure reduced." stuck in the speech cloud). This listener schedules a
 * dispatch of `clearActiveSpeech` once the TTL elapses, accounting for any
 * time already spent in `created_at` so a stale snapshot replay doesn't get
 * a fresh full TTL.
 *
 * Exposed as a helper so `app/middleware.ts` and tests can reuse the exact
 * same registration without re-implementing the effect.
 */
export function registerBuddySpeechTtlListener(
  lm: ListenerMiddlewareInstance,
): void {
  lm.startListening({
    matcher: isAnyOf(setActiveSpeech, setBuddySnapshot),
    effect: async (_action, listenerApi) => {
      // A new speech (or snapshot carrying one) should always cancel any
      // pending clear so we never fire a stale timer against a different
      // speech.
      listenerApi.cancelActiveListeners();

      const speech = (listenerApi.getState() as BuddySpeechTtlState).buddy
        .activeSpeech;
      if (!speech || speech.persistent || speech.ttl_seconds <= 0) return;

      const ttlMs = speech.ttl_seconds * 1000;
      const createdAtMs = Date.parse(speech.created_at);
      const elapsedMs = Number.isFinite(createdAtMs)
        ? Math.max(0, Date.now() - createdAtMs)
        : 0;
      const remainingMs = ttlMs - elapsedMs;

      if (remainingMs <= 0) {
        listenerApi.dispatch(clearActiveSpeech());
        return;
      }

      try {
        await listenerApi.delay(remainingMs);
      } catch {
        return;
      }

      const after = (listenerApi.getState() as BuddySpeechTtlState).buddy
        .activeSpeech;
      if (after && after.id === speech.id) {
        listenerApi.dispatch(clearActiveSpeech());
      }
    },
  });
}
