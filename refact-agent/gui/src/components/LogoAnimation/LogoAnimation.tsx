import React from "react";
import styles from "./LogoAnimation.module.css";

export type LogoAnimationProps = {
  isWaiting: boolean;
  isStreaming: boolean;
  size?: "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9";
};

export const LogoAnimation: React.FC<LogoAnimationProps> = ({
  isWaiting,
  isStreaming,
  size = "8",
}) => {
  if (!isStreaming && !isWaiting) return false;

  const style = { fontSize: `var(--font-size-${size})` };

  if (isStreaming) {
    return (
      <span className={`${styles.root} ${styles.streaming}`} style={style} />
    );
  }

  return (
    <span className={styles.waiting} style={style}>
      <span className={styles.dot} />
      <span className={styles.dot} />
    </span>
  );
};
