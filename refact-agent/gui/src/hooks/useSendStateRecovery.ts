import { useEffect, useRef } from "react";
import { useAppDispatch } from "./useAppDispatch";
import { useAppSelector } from "./useAppSelector";
import {
  enableSend,
  requestSseRefresh,
  resetStuckSendState,
  selectHasUncalledToolsById,
  selectIsStreamingById,
  selectIsWaitingById,
  selectMessagesById,
  selectPreventSendById,
  selectQueuedItemsById,
  selectStreamVersion,
} from "../features/Chat/Thread";

const BUSY_STALL_REFRESH_MS = 90_000;
const BUSY_STALL_RESET_MS = 45_000;
const STALE_PREVENT_SEND_MS = 5_000;
const WATCHDOG_TICK_MS = 15_000;

export function useSendStateRecovery(chatId: string | null) {
  const dispatch = useAppDispatch();
  const isStreaming = useAppSelector((state) =>
    selectIsStreamingById(state, chatId ?? ""),
  );
  const isWaiting = useAppSelector((state) =>
    selectIsWaitingById(state, chatId ?? ""),
  );
  const preventSend = useAppSelector((state) =>
    selectPreventSendById(state, chatId ?? ""),
  );
  const hasUncalledTools = useAppSelector((state) =>
    selectHasUncalledToolsById(state, chatId ?? ""),
  );
  const messages = useAppSelector((state) =>
    selectMessagesById(state, chatId ?? ""),
  );
  const queuedItems = useAppSelector((state) =>
    selectQueuedItemsById(state, chatId ?? ""),
  );
  const streamVersion = useAppSelector(selectStreamVersion);

  const busy = isWaiting || isStreaming;
  const lastActivityRef = useRef(Date.now());
  const refreshRequestedRef = useRef(false);

  useEffect(() => {
    lastActivityRef.current = Date.now();
    refreshRequestedRef.current = false;
  }, [busy, messages, queuedItems, streamVersion]);

  useEffect(() => {
    if (!busy || !chatId) return;
    const timer = window.setInterval(() => {
      const idleFor = Date.now() - lastActivityRef.current;
      if (idleFor >= BUSY_STALL_REFRESH_MS + BUSY_STALL_RESET_MS) {
        refreshRequestedRef.current = true;
        dispatch(resetStuckSendState({ id: chatId }));
      } else if (
        idleFor >= BUSY_STALL_REFRESH_MS &&
        !refreshRequestedRef.current
      ) {
        refreshRequestedRef.current = true;
        dispatch(requestSseRefresh({ chatId }));
      }
    }, WATCHDOG_TICK_MS);
    return () => window.clearInterval(timer);
  }, [busy, chatId, dispatch]);

  useEffect(() => {
    if (!preventSend || busy || hasUncalledTools || !chatId) return;
    const timer = window.setTimeout(() => {
      dispatch(enableSend({ id: chatId }));
    }, STALE_PREVENT_SEND_MS);
    return () => window.clearTimeout(timer);
  }, [preventSend, busy, hasUncalledTools, chatId, dispatch]);
}

export default useSendStateRecovery;
