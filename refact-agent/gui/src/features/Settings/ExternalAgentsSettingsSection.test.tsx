import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import { render, screen, waitFor } from "../../utils/test-utils";
import { server } from "../../utils/mockServer";
import { setUpStore } from "../../app/store";
import {
  makeExternalActions,
  makeExternalDetails,
  makeExternalParticipant,
} from "../../__fixtures__/externalAgents";
import type {
  ExternalParticipantDetails,
  ExternalParticipantRead,
} from "../../services/refact/externalAgents";
import { ExternalAgentsSettingsSection } from "./ExternalAgentsSettingsSection";

HTMLElement.prototype.hasPointerCapture = () => false;
HTMLElement.prototype.releasePointerCapture = () => undefined;

const CONFIG_STATE = {
  config: {
    apiKey: "test",
    lspPort: 8001,
    themeProps: {},
    host: "vscode" as const,
  },
};

interface CapturedRequest {
  method: string;
  path: string;
  body: unknown;
}

function installHandlers(
  participants: ExternalParticipantRead[],
  details: Partial<Record<string, ExternalParticipantDetails>> = {},
) {
  const requests: CapturedRequest[] = [];
  server.use(
    http.get("*/v1/external-participants", () =>
      HttpResponse.json({
        mode: participants.length ? "external_participants" : "internal_agents",
        participants,
      }),
    ),
    http.get("*/v1/external-participants/:participantId", ({ params }) => {
      const participantId = String(params.participantId);
      const participant = participants.find(
        (candidate) => candidate.id === participantId,
      );
      return HttpResponse.json(
        details[participantId] ??
          makeExternalDetails(participant ?? makeExternalParticipant()),
      );
    }),
    http.post("*/v1/external-participants", async ({ request }) => {
      const body = await request.json();
      requests.push({
        method: "POST",
        path: new URL(request.url).pathname,
        body,
      });
      return HttpResponse.json(makeExternalDetails());
    }),
    http.put(
      "*/v1/external-participants/:participantId",
      async ({ request }) => {
        const body = await request.json();
        requests.push({
          method: "PUT",
          path: new URL(request.url).pathname,
          body,
        });
        return HttpResponse.json(makeExternalDetails());
      },
    ),
    http.post(
      "*/v1/external-participants/:participantId/deliveries/:deliveryId/retry",
      async ({ request }) => {
        const body = await request.json();
        requests.push({
          method: "POST",
          path: new URL(request.url).pathname,
          body,
        });
        return HttpResponse.json(makeExternalDetails());
      },
    ),
    http.post(
      "*/v1/external-participants/:participantId/:action",
      async ({ request }) => {
        const body =
          request.headers.get("content-length") === "0"
            ? undefined
            : await request.json().catch(() => undefined);
        requests.push({
          method: "POST",
          path: new URL(request.url).pathname,
          body,
        });
        return HttpResponse.json(makeExternalDetails());
      },
    ),
  );
  return requests;
}

function renderSection() {
  const store = setUpStore({ ...CONFIG_STATE });
  return render(<ExternalAgentsSettingsSection />, { store });
}

async function fillRequiredCreateFields(
  user: ReturnType<typeof renderSection>["user"],
) {
  await user.type(screen.getByLabelText(/^Name/), "Reviewer");
  await user.type(screen.getByLabelText(/^Participant ID/), "participant-1");
  await user.type(
    screen.getByLabelText(/^UHP server root/),
    "https://uhp.example.test",
  );
  await user.type(screen.getByLabelText(/^Base/), "server-base");
  await user.type(screen.getByLabelText(/^Model/), "model-1");
  await user.type(
    screen.getByLabelText(/^Credential profile reference/),
    "credential/external-1",
  );
}

describe("ExternalAgentsSettingsSection", () => {
  it("keeps internal agents as the empty default until explicit opt-in", async () => {
    installHandlers([]);
    renderSection();

    expect(
      await screen.findByText("Internal agents are active by default"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Adding one is an explicit opt-in/),
    ).toBeInTheDocument();
    expect(screen.queryByText("Live verified")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Add external advisor" }),
    ).toBeEnabled();
  });

  it("creates through keyboard-accessible fields without retaining the bearer", async () => {
    const requests = installHandlers([]);
    const secret = "SECRET-UI-1-CANARY";
    const { user, store } = renderSection();
    const add = await screen.findByRole("button", {
      name: "Add external advisor",
    });

    add.focus();
    await user.keyboard("{Enter}");
    const name = screen.getByLabelText(/^Name/);
    name.focus();
    await user.tab();
    expect(screen.getByLabelText(/^Participant ID/)).toHaveFocus();
    await fillRequiredCreateFields(user);
    const bearer = screen.getByTestId("external-agent-bearer");
    await user.type(bearer, secret);
    await user.click(screen.getByRole("button", { name: "Create advisor" }));

    expect(bearer).toHaveValue("");
    await waitFor(() => expect(requests).toHaveLength(1));
    expect(requests[0].body).toMatchObject({
      id: "participant-1",
      writes: false,
      enabled: false,
      credential: { bearer: secret },
    });
    expect(JSON.stringify(store.getState())).not.toContain(secret);
    expect(document.body.innerHTML).not.toContain(secret);
    expect(screen.queryByDisplayValue(secret)).not.toBeInTheDocument();
  });

  it("validates create/edit values and keeps saved credentials write-only", async () => {
    const participant = makeExternalParticipant();
    const requests = installHandlers([participant]);
    const { user } = renderSection();

    await user.click(await screen.findByRole("button", { name: "Configure" }));
    expect(screen.getByLabelText(/^Participant ID/)).toBeDisabled();
    expect(screen.getByTestId("external-agent-bearer")).toHaveValue("");
    await user.clear(screen.getByLabelText(/^UHP server root/));
    await user.type(screen.getByLabelText(/^UHP server root/), "not-a-url");
    await user.clear(screen.getByLabelText("Maximum steps"));
    await user.type(screen.getByLabelText("Maximum steps"), "0");
    await user.click(screen.getByRole("button", { name: "Save changes" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Server root must be a valid HTTP(S) URL",
    );
    expect(requests).toHaveLength(0);

    await user.clear(screen.getByLabelText(/^UHP server root/));
    await user.type(
      screen.getByLabelText(/^UHP server root/),
      "https://uhp.example.test",
    );
    await user.clear(screen.getByLabelText("Maximum steps"));
    await user.type(screen.getByLabelText("Maximum steps"), "30");
    await user.clear(screen.getByLabelText(/^Name/));
    await user.type(screen.getByLabelText(/^Name/), "Second reviewer");
    await user.click(screen.getByRole("button", { name: "Save changes" }));

    await waitFor(() => expect(requests).toHaveLength(1));
    expect(requests[0].method).toBe("PUT");
    expect(requests[0].body).toMatchObject({
      name: "Second reviewer",
      max_step: 30,
      writes: false,
    });
    expect(requests[0].body).not.toHaveProperty("credential");
  });

  it("disables unsupported capabilities with reasons and calls allowed actions", async () => {
    const participant = makeExternalParticipant({
      actions: makeExternalActions({
        test: {
          enabled: false,
          reason: "configure or provision a harness first",
        },
        provision: {
          enabled: false,
          reason: "server does not advertise harness management",
        },
      }),
    });
    const requests = installHandlers([participant]);
    const { user } = renderSection();

    expect(
      await screen.findByRole("button", { name: "Provision" }),
    ).toBeDisabled();
    expect(screen.getByRole("button", { name: "Test model" })).toBeDisabled();
    expect(
      screen.getByText("server does not advertise harness management"),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Discover" }));

    await waitFor(() => expect(requests).toHaveLength(1));
    expect(requests[0].path).toBe(
      "/v1/external-participants/participant-1/discovery",
    );
    expect(requests).toHaveLength(1);
  });

  it("provisions with an idempotency key and shows sanitized server errors", async () => {
    const participant = makeExternalParticipant();
    const requests = installHandlers([participant]);
    server.use(
      http.post("*/v1/external-participants/:participantId/discovery", () =>
        HttpResponse.json(
          {
            code: "unsupported_base",
            message: "external UHP operation failed",
          },
          { status: 400 },
        ),
      ),
    );
    const { user } = renderSection();

    await user.click(await screen.findByRole("button", { name: "Provision" }));
    await waitFor(() => expect(requests).toHaveLength(1));
    expect(requests[0].path).toBe(
      "/v1/external-participants/participant-1/provision",
    );
    const provisionBody = requests[0].body as Record<string, unknown>;
    expect(typeof provisionBody.idempotency_key).toBe("string");
    expect(provisionBody.idempotency_key).not.toBe("");

    await user.click(screen.getByRole("button", { name: "Discover" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "external UHP operation failed",
    );
    expect(document.body.innerHTML).not.toContain("unsupported_base");
  });

  it("requires matching all-pass live evidence for production GO", async () => {
    const verified = makeExternalParticipant({
      id: "verified",
      name: "Live reviewer",
      state: "verified",
      server_version: "0.41.25",
    });
    const unproven = makeExternalParticipant({
      id: "unproven",
      name: "Unproven reviewer",
      state: "verified",
    });
    const staging = makeExternalParticipant({
      id: "staging",
      name: "Staging reviewer",
      harness_id: "harness-1",
      environment_id: "environment-1",
      environment_version: 2,
      protocol_version: "2026-10-19",
    });
    installHandlers([verified, unproven, staging], {
      verified: makeExternalDetails(verified, {
        evidence: [
          {
            smoke_suite_id: "live-smoke",
            server_version: "0.41.25",
            cli_version: verified.cli_pin,
            model_version: verified.model,
            checks: [{ name: "live_read_only_smoke", status: "pass" }],
            generated_at: 1,
            sanitized_report_location: "evidence/live-smoke.json",
          },
        ],
      }),
      unproven: makeExternalDetails(unproven),
      staging: makeExternalDetails(staging),
    });
    renderSection();

    expect(await screen.findByText("Live verified")).toBeInTheDocument();
    expect(screen.getByText("Unverified")).toBeInTheDocument();
    expect(
      screen.getByText("Staging ready — not live verified"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        /Discovery, model tests, and mock checks never grant production GO/,
      ),
    ).toBeInTheDocument();
  });

  it("cancels sessions and retries only with an exact CreateResponse payload", async () => {
    const participant = makeExternalParticipant({ harness_id: "harness-1" });
    const details = makeExternalDetails(participant, {
      sessions: [
        {
          participant_id: participant.id,
          session_id: "session-1",
          last_response_id: "response-1",
          last_terminal_status: null,
          busy: true,
          queued: false,
          created_at: 1,
          updated_at: 2,
          replay_cursor: null,
        },
      ],
      deliveries: [
        {
          delivery_id: "delivery-1",
          room_id: "room-1",
          event_id: "event-1",
          participant_id: participant.id,
          status: "terminal",
          request_state: "failed",
          logical_attempt: 2,
          response_id: "response-1",
          session_id: "session-1",
          source_revision: "revision-1",
          source_digest: "digest-1",
          source_freshness: "stale",
          environment_version: 1,
          error: null,
          created_at: 1,
          updated_at: 2,
        },
      ],
    });
    const requests = installHandlers([participant], {
      [participant.id]: details,
    });
    const { user } = renderSection();

    await user.click(
      await screen.findByText("Sessions, deliveries, and evidence"),
    );
    await user.click(screen.getByRole("button", { name: "Cancel session" }));
    await waitFor(() => expect(requests).toHaveLength(1));
    expect(requests[0].body).toEqual({ kind: "session", id: "session-1" });

    await user.click(screen.getByRole("button", { name: "Retry delivery" }));
    const input = screen.getByLabelText(
      "Original CreateResponse JSON for delivery-1",
    );
    await user.click(input);
    await user.paste("not-json");
    await user.click(screen.getByRole("button", { name: "Verify and retry" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "must be valid JSON",
    );
    expect(requests).toHaveLength(1);

    const request = {
      input: "public room body",
      model: "model-1",
      metadata: { harness_id: "harness-1" },
      stream: false,
      store: true,
    };
    await user.clear(input);
    await user.paste(JSON.stringify(request));
    await user.click(screen.getByRole("button", { name: "Verify and retry" }));
    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[1].body).toEqual(request);
  });
});
