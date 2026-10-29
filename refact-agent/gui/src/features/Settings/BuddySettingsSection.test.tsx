import { http, HttpResponse } from "msw";
import { describe, expect, it } from "vitest";

import { render, screen, waitFor } from "../../utils/test-utils";
import { server } from "../../utils/mockServer";
import { setUpStore } from "../../app/store";
import { defaultBuddySettings, setBuddySnapshot } from "../Buddy/buddySlice";
import type { BuddySettings, BuddySnapshot } from "../Buddy/types";
import { BuddySettingsSection } from "./BuddySettingsSection";

const CONFIG_STATE = {
  config: {
    apiKey: "test",
    lspPort: 8001,
    themeProps: {},
    host: "vscode" as const,
  },
};

function makeSnapshot(overrides?: Partial<BuddySnapshot>): BuddySnapshot {
  const settings = overrides?.settings ?? defaultBuddySettings();
  return {
    state: {
      identity: { name: "Buddy", created_at: "", palette_index: 0 },
      progression: {
        stage: 0,
        stage_name: "Egg",
        level: 1,
        xp: 8,
        xp_next: 20,
      },
      skills: { unlocked: [], locked: [] },
      workflow_summaries: [],
      semantic: {
        mood: "idle",
        focus: "helping",
        headline: "Keeping watch",
        last_active: "2024-01-01T00:00:00Z",
      },
      recent_activities: [],
      suggestion_state: [],
      pet: {
        needs: {
          hunger: 80,
          energy: 85,
          hygiene: 80,
          boredom: 15,
          affection: 75,
        },
        condition: {
          sleeping: false,
          hungry: false,
          sleepy: false,
          dirty: false,
          bored: false,
          lonely: false,
        },
        evolution: {
          care_score: 4,
          neglect_score: 0,
          open_seconds: 0,
          last_evolved_at: null,
        },
      },
      personality: {
        archetype_id: "helper_sprite",
        archetype_label: "Helper Sprite",
        vibe: "Playful",
        summary: "An energetic helper.",
        prompt: "Playful",
        traits: {
          playfulness: 70,
          chaos: 35,
          sociability: 72,
          curiosity: 78,
          resilience: 66,
        },
      },
      active_quest: null,
      opportunities: [],
    },
    settings,
    enabled: settings.enabled,
    opportunities: [],
    runtime_queue: [],
    recent_diagnostics: [],
    active_speech: null,
    now_playing: null,
    active_drafts: [],
    ...overrides,
  };
}

interface CapturedRequest {
  count: number;
  body: unknown;
}

function installSettingsPostHandler(): CapturedRequest {
  const captured: CapturedRequest = { count: 0, body: undefined };
  server.use(
    http.post("*/v1/buddy/settings", async ({ request }) => {
      captured.count += 1;
      const body = (await request.json()) as Partial<BuddySettings>;
      captured.body = body;
      return HttpResponse.json({ ...defaultBuddySettings(), ...body });
    }),
  );
  return captured;
}

function renderSection() {
  const store = setUpStore({ ...CONFIG_STATE });
  return render(<BuddySettingsSection />, { store });
}

describe("BuddySettingsSection", () => {
  it("renders the Buddy title and the three stalled-turn controls", () => {
    const store = setUpStore({ ...CONFIG_STATE });
    store.dispatch(setBuddySnapshot(makeSnapshot()));

    render(<BuddySettingsSection />, { store });

    expect(screen.getByRole("heading", { name: "Buddy" })).toBeInTheDocument();
    expect(
      screen.getByTestId("buddy-settings-stalled-turn-toggle"),
    ).toBeInTheDocument();
    expect(
      screen.getByTestId("buddy-settings-stalled-turn-interval"),
    ).toBeInTheDocument();
    expect(
      screen.getByTestId("buddy-settings-stalled-turn-max-per-hour"),
    ).toBeInTheDocument();
  });

  it("posts only the toggled stalled-turn key when the switch is clicked", async () => {
    const captured = installSettingsPostHandler();
    const store = setUpStore({ ...CONFIG_STATE });
    store.dispatch(setBuddySnapshot(makeSnapshot()));

    const { user } = render(<BuddySettingsSection />, { store });

    await user.click(
      screen.getByRole("switch", { name: "Intervene on stalled turns" }),
    );

    await waitFor(() => expect(captured.count).toBe(1));
    expect(captured.body).toEqual({ stalled_turn_intervention: false });
  });

  it("posts the interval value on blur", async () => {
    const captured = installSettingsPostHandler();
    const store = setUpStore({ ...CONFIG_STATE });
    store.dispatch(setBuddySnapshot(makeSnapshot()));

    const { user } = render(<BuddySettingsSection />, { store });

    const interval = screen.getByTestId("buddy-settings-stalled-turn-interval");
    await user.clear(interval);
    await user.type(interval, "120");
    await user.tab();

    await waitFor(() => expect(captured.count).toBe(1));
    expect(captured.body).toEqual({ stalled_turn_min_interval_secs: 120 });
  });

  it("does not post when the interval is out of range", async () => {
    const captured = installSettingsPostHandler();
    const store = setUpStore({ ...CONFIG_STATE });
    store.dispatch(setBuddySnapshot(makeSnapshot()));

    const { user } = render(<BuddySettingsSection />, { store });

    const interval = screen.getByTestId("buddy-settings-stalled-turn-interval");
    await user.clear(interval);
    await user.type(interval, "99999");
    await user.tab();

    expect(captured.count).toBe(0);
    expect(captured.body).toBeUndefined();
    expect(interval).toHaveValue(300);
  });

  it("disables the numeric fields while intervention is off", () => {
    const store = setUpStore({ ...CONFIG_STATE });
    store.dispatch(
      setBuddySnapshot(
        makeSnapshot({
          settings: {
            ...defaultBuddySettings(),
            stalled_turn_intervention: false,
          },
        }),
      ),
    );

    render(<BuddySettingsSection />, { store });

    expect(
      screen.getByTestId("buddy-settings-stalled-turn-interval"),
    ).toBeDisabled();
    expect(
      screen.getByTestId("buddy-settings-stalled-turn-max-per-hour"),
    ).toBeDisabled();
    expect(
      screen.getByTestId("buddy-settings-stalled-turn-toggle"),
    ).not.toBeDisabled();
  });

  it("renders an unavailable message when there are no buddy settings", () => {
    renderSection();

    expect(
      screen.getByText(
        "Buddy settings are unavailable. Open a project and try again.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.queryByTestId("buddy-settings-stalled-turn-toggle"),
    ).not.toBeInTheDocument();
  });
});
