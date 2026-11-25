import { configureStore } from "@reduxjs/toolkit";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  makeExternalDetails,
  makeExternalWrite,
} from "../../__fixtures__/externalAgents";
import type { EngineApiConfig } from "./apiUrl";
import {
  externalAgentsApi,
  saveExternalParticipant,
  type ExternalAgentsState,
} from "./externalAgents";

type FetchLike = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;
type TestConfig = EngineApiConfig & { apiKey: string | null };

function createTestStore(config: TestConfig) {
  return configureStore({
    reducer: {
      config: (state: TestConfig = config) => state,
      [externalAgentsApi.reducerPath]: externalAgentsApi.reducer,
    },
    middleware: (getDefaultMiddleware) =>
      getDefaultMiddleware().concat(externalAgentsApi.middleware),
  });
}

function jsonResponse(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function requestAt(
  fetchMock: ReturnType<typeof vi.fn<FetchLike>>,
  index: number,
): Request {
  const [input, init] = fetchMock.mock.calls[index];
  return input instanceof Request ? input : new Request(input, init);
}

function testConfig(): TestConfig {
  return { host: "ide", lspPort: 8123, apiKey: "local-token" };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("externalAgentsApi", () => {
  it("uses typed discovery, provision, cancel, and retry routes", async () => {
    const fetchMock = vi.fn<FetchLike>();
    vi.stubGlobal("fetch", fetchMock);
    fetchMock.mockImplementation(() =>
      Promise.resolve(jsonResponse(makeExternalDetails())),
    );
    const store = createTestStore(testConfig());

    await store
      .dispatch(
        externalAgentsApi.endpoints.discoverExternalParticipant.initiate({
          participantId: "participant/1",
        }),
      )
      .unwrap();
    const discoveryRequest = requestAt(fetchMock, 0);
    expect(discoveryRequest.method).toBe("POST");
    expect(new URL(discoveryRequest.url).pathname).toBe(
      "/v1/external-participants/participant%2F1/discovery",
    );
    expect(discoveryRequest.headers.get("Authorization")).toBe(
      "Bearer local-token",
    );

    await store
      .dispatch(
        externalAgentsApi.endpoints.provisionExternalParticipant.initiate({
          participantId: "participant-1",
          idempotencyKey: "provision-key-1",
        }),
      )
      .unwrap();
    const provisionRequest = requestAt(fetchMock, 1);
    await expect(provisionRequest.clone().json()).resolves.toEqual({
      idempotency_key: "provision-key-1",
    });

    await store
      .dispatch(
        externalAgentsApi.endpoints.cancelExternalParticipant.initiate({
          participantId: "participant-1",
          target: { kind: "session", id: "session-1" },
        }),
      )
      .unwrap();
    await expect(requestAt(fetchMock, 2).clone().json()).resolves.toEqual({
      kind: "session",
      id: "session-1",
    });

    const request = {
      input: "public room body",
      model: "model-1",
      metadata: { harness_id: "harness-1", environment: "environment-1" },
      stream: false,
      store: true,
    };
    await store
      .dispatch(
        externalAgentsApi.endpoints.retryExternalParticipantDelivery.initiate({
          participantId: "participant-1",
          deliveryId: "delivery/1",
          request,
        }),
      )
      .unwrap();
    const retryRequest = requestAt(fetchMock, 3);
    expect(new URL(retryRequest.url).pathname).toBe(
      "/v1/external-participants/participant-1/deliveries/delivery%2F1/retry",
    );
    await expect(retryRequest.clone().json()).resolves.toEqual(request);
  });

  it("sends write-only credentials directly without retaining them in Redux", async () => {
    const secret = "SECRET-UI-1-CANARY";
    const fetchMock = vi.fn<FetchLike>(() =>
      Promise.resolve(jsonResponse(makeExternalDetails())),
    );
    vi.stubGlobal("fetch", fetchMock);
    const config = testConfig();
    const store = createTestStore(config);

    await saveExternalParticipant(
      { config } as ExternalAgentsState,
      null,
      makeExternalWrite({ credential: { bearer: secret } }),
    );

    const request = requestAt(fetchMock, 0);
    expect(request.method).toBe("POST");
    await expect(request.clone().json()).resolves.toMatchObject({
      writes: false,
      credential: { bearer: secret },
    });
    expect(JSON.stringify(store.getState())).not.toContain(secret);
  });

  it("returns only sanitized server errors", async () => {
    const secret = "SECRET-UI-1-CANARY";
    const fetchMock = vi.fn<FetchLike>(() =>
      Promise.resolve(
        jsonResponse(
          {
            code: "unsupported_base",
            message: "external UHP operation failed",
          },
          400,
        ),
      ),
    );
    vi.stubGlobal("fetch", fetchMock);
    const config = testConfig();

    await expect(
      saveExternalParticipant(
        { config } as ExternalAgentsState,
        "participant-1",
        makeExternalWrite({ credential: { bearer: secret } }),
      ),
    ).rejects.toMatchObject({
      code: "unsupported_base",
      message: "external UHP operation failed",
    });
    expect(JSON.stringify(fetchMock.mock.results)).not.toContain(secret);
  });
});
