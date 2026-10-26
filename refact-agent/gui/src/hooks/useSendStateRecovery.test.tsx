import React from "react";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { Provider } from "react-redux";
import { renderHook } from "@testing-library/react";
import { act } from "react-dom/test-utils";
import { setUpStore, type AppStore } from "../app/store";
import {
  createChatWithId,
  enableSend,
  setIsWaitingForResponse,
  setPreventSend,
  resetStuckSendState,
} from "../features/Chat/Thread/actions";
import { useSendStateRecovery } from "./useSendStateRecovery";

const CHAT_ID = "recovery-test-chat";

function makeStore() {
  const store: AppStore = setUpStore();
  store.dispatch(createChatWithId({ id: CHAT_ID }));
  return store;
}

const wrapper = (store: AppStore) =>
  function Wrapper({ children }: { children: React.ReactNode }) {
    return <Provider store={store}>{children}</Provider>;
  };

describe("resetStuckSendState reducer", () => {
  it("clears stuck busy and prevent_send flags", () => {
    const store = makeStore();
    store.dispatch(setPreventSend({ id: CHAT_ID }));
    store.dispatch(setIsWaitingForResponse({ id: CHAT_ID, value: true }));
    store.dispatch(enableSend({ id: CHAT_ID }));
    store.dispatch(setPreventSend({ id: CHAT_ID }));

    store.dispatch(resetStuckSendState({ id: CHAT_ID }));

    const runtime = store.getState().chat.threads[CHAT_ID];
    expect(runtime?.prevent_send).toBe(false);
    expect(runtime?.waiting_for_response).toBe(false);
    expect(runtime?.streaming).toBe(false);
  });
});

describe("useSendStateRecovery", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("re-enables send when prevent_send is stuck with no busy state", async () => {
    const store = makeStore();
    store.dispatch(setPreventSend({ id: CHAT_ID }));

    renderHook(() => useSendStateRecovery(CHAT_ID), {
      wrapper: wrapper(store),
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(6_000);
    });

    expect(store.getState().chat.threads[CHAT_ID]?.prevent_send).toBe(false);
  });

  it("does not clear prevent_send while waiting", async () => {
    const store = makeStore();
    store.dispatch(setPreventSend({ id: CHAT_ID }));
    store.dispatch(setIsWaitingForResponse({ id: CHAT_ID, value: true }));

    renderHook(() => useSendStateRecovery(CHAT_ID), {
      wrapper: wrapper(store),
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(store.getState().chat.threads[CHAT_ID]?.prevent_send).toBe(true);
  });

  it("resets stuck waiting_for_response after prolonged inactivity", async () => {
    const store = makeStore();
    store.dispatch(setIsWaitingForResponse({ id: CHAT_ID, value: true }));

    renderHook(() => useSendStateRecovery(CHAT_ID), {
      wrapper: wrapper(store),
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(150_000);
    });

    expect(store.getState().chat.threads[CHAT_ID]?.waiting_for_response).toBe(
      false,
    );
    expect(store.getState().chat.threads[CHAT_ID]?.streaming).toBe(false);
  });

  it("does not reset while there is recent activity", async () => {
    const store = makeStore();
    store.dispatch(setIsWaitingForResponse({ id: CHAT_ID, value: true }));

    renderHook(() => useSendStateRecovery(CHAT_ID), {
      wrapper: wrapper(store),
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });

    expect(store.getState().chat.threads[CHAT_ID]?.waiting_for_response).toBe(
      true,
    );
  });
});
