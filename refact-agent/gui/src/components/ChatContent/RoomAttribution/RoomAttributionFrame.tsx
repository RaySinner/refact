import React from "react";
import { roleAccent } from "../../../utils/roomRoleAccent";
import type { RoomAttribution } from "./roomAttribution";
import styles from "./RoomAttributionFrame.module.css";

type RoomAttributionFrameProps = {
  attribution: RoomAttribution;
  children: React.ReactNode;
};

/**
 * Wraps one message of a room transcript with the marks that keep two agents apart: a
 * thin left stripe in that agent's accent, a small byline naming it, and a hairline
 * when the speaker changed.
 *
 * The colours arrive as CSS *variable names* from `roleAccent`, so a theme swap is a
 * token change in `tokens.css` and not a re-derivation of every message. The inline
 * custom-property writes below are the only place the accent reaches the DOM.
 */
export const RoomAttributionFrame: React.FC<RoomAttributionFrameProps> = ({
  attribution,
  children,
}) => {
  const accent = roleAccent(attribution.author.role);
  const accentStyle = {
    "--room-stripe-fallback": accent.stripe,
    "--room-label-fallback": accent.label,
  } as React.CSSProperties;

  return (
    <div
      className={styles.roomFrame}
      data-testid="room-attribution"
      data-author-chat-id={attribution.author.chatId}
      data-author-role={attribution.author.role}
      data-divider={attribution.divider ? "true" : "false"}
      style={accentStyle}
    >
      <span
        aria-hidden="true"
        className={styles.stripe}
        data-testid="room-attribution-stripe"
      />
      <div className={styles.body}>
        <div className={styles.byline} data-testid="room-attribution-byline">
          <span className={styles.name} data-testid="room-attribution-name">
            {attribution.author.displayName}
          </span>
          {attribution.timeLabel !== "" && (
            <span className={styles.time} data-testid="room-attribution-time">
              {attribution.timeLabel}
            </span>
          )}
          {attribution.divider && (
            <hr
              aria-hidden="true"
              className={styles.divider}
              data-testid="room-attribution-divider"
            />
          )}
        </div>
        {children}
      </div>
    </div>
  );
};

RoomAttributionFrame.displayName = "RoomAttributionFrame";
