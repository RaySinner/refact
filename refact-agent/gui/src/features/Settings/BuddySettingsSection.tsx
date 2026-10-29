import React, { useCallback, useEffect, useRef, useState } from "react";

import { useAppSelector } from "../../hooks";
import {
  FieldSwitch,
  FieldText,
  SaveStatus,
  SettingItem,
} from "../../components/ui";
import type { SaveStatusState } from "../../components/ui";
import { selectBuddySettings } from "../Buddy/buddySlice";
import { useUpdateBuddySettingsMutation } from "../../services/refact/buddy";
import type { BuddySettings } from "../Buddy/types";
import { SettingsGroup, SettingsSection } from "./SettingsSection";
import styles from "./BuddySettingsSection.module.css";

const SAVE_STATUS_RESET_MS = 2000;

const STALLED_TURN_INTERVAL_MIN = 0;
const STALLED_TURN_INTERVAL_MAX = 86400;
const STALLED_TURN_MAX_PER_HOUR_MIN = 0;
const STALLED_TURN_MAX_PER_HOUR_MAX = 1000;

interface StalledTurnNumberFieldProps {
  value: number;
  min: number;
  max: number;
  placeholder: string;
  "aria-label": string;
  "data-testid": string;
  disabled: boolean;
  onCommit: (value: number) => void;
}

function StalledTurnNumberField({
  value,
  min,
  max,
  disabled,
  onCommit,
  ...rest
}: StalledTurnNumberFieldProps) {
  const [draft, setDraft] = useState<string | null>(null);

  const commitDraft = (raw: string) => {
    const parsed = /^\d+$/.test(raw) ? Number(raw) : Number.NaN;
    if (
      Number.isInteger(parsed) &&
      parsed >= min &&
      parsed <= max &&
      parsed !== value
    ) {
      onCommit(parsed);
    }
    setDraft(null);
  };

  return (
    <FieldText
      {...rest}
      className={styles.numberInput}
      disabled={disabled}
      inputMode="numeric"
      min={min}
      max={max}
      step={1}
      type="number"
      value={draft ?? String(value)}
      onChange={setDraft}
      onCommit={commitDraft}
    />
  );
}

export const BuddySettingsSection: React.FC = () => {
  const settings = useAppSelector(selectBuddySettings);
  const [updateSettingsMutation] = useUpdateBuddySettingsMutation();
  const [saveState, setSaveState] = useState<SaveStatusState>("idle");
  const saveSeqRef = useRef(0);
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    return () => {
      if (savedTimerRef.current !== null) clearTimeout(savedTimerRef.current);
    };
  }, []);

  const autoSave = useCallback(
    async (patch: Partial<BuddySettings>) => {
      const requestSeq = saveSeqRef.current + 1;
      saveSeqRef.current = requestSeq;
      setSaveState("saving");
      if (savedTimerRef.current !== null) clearTimeout(savedTimerRef.current);
      try {
        await updateSettingsMutation(patch).unwrap();
        if (saveSeqRef.current === requestSeq) {
          setSaveState("saved");
          savedTimerRef.current = setTimeout(() => {
            if (saveSeqRef.current === requestSeq) setSaveState("idle");
          }, SAVE_STATUS_RESET_MS);
        }
      } catch {
        if (saveSeqRef.current === requestSeq) setSaveState("error");
      }
    },
    [updateSettingsMutation],
  );

  const interventionEnabled = settings?.stalled_turn_intervention ?? false;

  return (
    <SettingsSection
      title="Buddy"
      description="Controls for Pixel's supervision of stalled agent turns."
      actions={<SaveStatus state={saveState} />}
    >
      {!settings ? (
        <p>Buddy settings are unavailable. Open a project and try again.</p>
      ) : (
        <SettingsGroup title="Stalled turn supervision">
          <SettingItem
            className="rf-enter"
            title="Intervene on stalled turns"
            description="When an agent ends a turn without calling any tool while its card, plan, or goal is still unfinished, Buddy nudges it to keep going instead of asking you."
            control={
              <FieldSwitch
                aria-label="Intervene on stalled turns"
                data-testid="buddy-settings-stalled-turn-toggle"
                checked={settings.stalled_turn_intervention}
                onChange={(checked) =>
                  void autoSave({ stalled_turn_intervention: checked })
                }
              />
            }
          />
          <SettingItem
            className="rf-enter"
            title="Minimum seconds between nudges"
            description="0 disables the cooldown."
            control={
              <StalledTurnNumberField
                aria-label="minimum seconds between nudges"
                data-testid="buddy-settings-stalled-turn-interval"
                value={settings.stalled_turn_min_interval_secs}
                min={STALLED_TURN_INTERVAL_MIN}
                max={STALLED_TURN_INTERVAL_MAX}
                placeholder="300"
                disabled={!interventionEnabled}
                onCommit={(value) =>
                  void autoSave({ stalled_turn_min_interval_secs: value })
                }
              />
            }
          />
          <SettingItem
            className="rf-enter"
            title="Maximum nudges per hour"
            description="0 turns the nudge off entirely."
            control={
              <StalledTurnNumberField
                aria-label="maximum nudges per hour"
                data-testid="buddy-settings-stalled-turn-max-per-hour"
                value={settings.stalled_turn_max_per_hour}
                min={STALLED_TURN_MAX_PER_HOUR_MIN}
                max={STALLED_TURN_MAX_PER_HOUR_MAX}
                placeholder="6"
                disabled={!interventionEnabled}
                onCommit={(value) =>
                  void autoSave({ stalled_turn_max_per_hour: value })
                }
              />
            }
          />
        </SettingsGroup>
      )}
    </SettingsSection>
  );
};
