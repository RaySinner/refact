import React from "react";
import { roleAccent, roleLabel } from "../../../utils/roomRoleAccent";
import type { RoomAttribution } from "./roomAttribution";
import styles from "./RoomAttributionFrame.module.css";
import type { Usage } from "../../../services/refact";
import {
  MessageActionButtons,
  MessageUsageDisplay,
} from "../MessageFooter";
import type { Checkpoint } from "../../../features/Checkpoints/types";

export type RoomAttributionFrameProps = {
  attribution: RoomAttribution;
  children: React.ReactNode;
  messageId?: string;
  onCopy?: () => void;
  onBranch?: (messageId: string) => void;
  onDelete?: (messageId: string) => void;
  usage?: Usage | null;
  checkpoints?: Checkpoint[] | null;
  messageIndex?: number;
  actions?: React.ReactNode;
};
export const RoomAttributionFrame: React.FC<RoomAttributionFrameProps> = ({
  attribution,
  children,
  messageId,
  onCopy,
  onBranch,
  onDelete,
  usage,
  checkpoints,
  messageIndex,
  actions,
}) => {
  const accent = roleAccent(attribution.author.role);
  const accentStyle = {
    "--room-stripe-fallback": accent.stripe,
    "--room-label-fallback": accent.label,
  } as React.CSSProperties;
  const roleText = roleLabel(attribution.author.role);
  const mirrored = attribution.author.role === "user";

  const isStandardRole =
    attribution.author.role === "user" || attribution.author.role === "assistant";
  const showName =
    !isStandardRole &&
    attribution.author.displayName.trim().toLowerCase() !== roleText.trim().toLowerCase();

  const renderedActions = actions ?? (
    <MessageActionButtons
      messageId={messageId}
      onCopy={onCopy}
      onBranch={onBranch}
      onDelete={onDelete}
      checkpoints={checkpoints}
      messageIndex={messageIndex}
    />
  );

  return (
    <div
      className={styles.roomFrame}
      data-testid="room-attribution"
      data-author-chat-id={attribution.author.chatId}
      data-author-role={attribution.author.role}
      data-divider={attribution.divider ? "true" : "false"}
      data-mirrored={mirrored ? "true" : "false"}
      style={accentStyle}
    >
      <span
        aria-hidden="true"
        className={styles.stripe}
        data-testid="room-attribution-stripe"
      />
      <div className={styles.body}>
        {attribution.divider && (
          <hr
            aria-hidden="true"
            className={styles.divider}
            data-testid="room-attribution-divider"
          />
        )}
        <div className={styles.byline} data-testid="room-attribution-byline">
          <span aria-hidden="true" className={styles.rolePill}>
            {roleText}
          </span>
          {showName && (
            <span className={styles.name} data-testid="room-attribution-name">
              {attribution.author.displayName}
            </span>
          )}
          <div className={styles.bylineControls}>
            <div
              className={styles.bylineActions}
              data-testid="room-attribution-actions"
            >
              {renderedActions}
            </div>
            {usage && <MessageUsageDisplay usage={usage} />}
            {attribution.timeLabel !== "" && (
              <span className={styles.time} data-testid="room-attribution-time">
                {attribution.timeLabel}
              </span>
            )}
          </div>
        </div>
        {children}
      </div>
    </div>
  );
};
RoomAttributionFrame.displayName = "RoomAttributionFrame";
