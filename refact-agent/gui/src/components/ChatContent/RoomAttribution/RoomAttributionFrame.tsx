import React from "react";
import { roleAccent, roleLabel } from "../../../utils/roomRoleAccent";
import type { RoomAttribution } from "./roomAttribution";
import styles from "./RoomAttributionFrame.module.css";

type RoomAttributionFrameProps = {
  attribution: RoomAttribution;
  children: React.ReactNode;
};

/**
 * Wraps one message of a room transcript in a Continue-style card that keeps two
 * agents apart at a glance.
 *
 * The card is a softly-rounded bubble with a hairline border and a pastel left
 * stripe in the speaker's role accent. A byline bar crowns it: an uppercase role
 * pill, the agent's name in the role colour, a tabular timestamp, and — only when
 * the speaker changed — a hairline boundary marking the start of a new turn.
 *
 * The colours arrive as CSS *variable names* from `roleAccent`, so a theme swap
 * is a token change in `tokens.css` and not a re-derivation of every message. The
 * inline custom-property writes below are the only place the accent reaches the
 * DOM; the pill tint, card border and stripe all derive from those variables in
 * the stylesheet.
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
  const roleText = roleLabel(attribution.author.role);

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
          <span className={styles.name} data-testid="room-attribution-name">
            {attribution.author.displayName}
          </span>
          {attribution.timeLabel !== "" && (
            <span className={styles.time} data-testid="room-attribution-time">
              {attribution.timeLabel}
            </span>
          )}
        </div>
        {children}
      </div>
    </div>
  );
};

RoomAttributionFrame.displayName = "RoomAttributionFrame";
