import React from "react";
import { Text } from "@radix-ui/themes";
import styles from "./SuccessGauge.module.css";

type SuccessGaugeProps = {
  successful: number;
  total: number;
};

export const SuccessGauge: React.FC<SuccessGaugeProps> = ({
  successful,
  total,
}) => {
  if (total === 0) {
    return (
      <div className={styles.gauge}>
        <Text size="2" color="gray">
          —
        </Text>
        <div className={styles.bar}>
          <div
            className={styles.fill}
            style={{ width: "0%", background: "var(--gray-7)" }}
          />
        </div>
      </div>
    );
  }

  const rate = Math.round((successful / total) * 100);
  const color =
    rate >= 95
      ? "var(--green-9)"
      : rate >= 80
        ? "var(--amber-9)"
        : "var(--red-9)";

  return (
    <div className={styles.gauge}>
      <Text size="3" weight="bold" style={{ color }}>
        {rate}%
      </Text>
      <div className={styles.bar}>
        <div
          className={styles.fill}
          style={{ width: `${rate}%`, background: color }}
        />
      </div>
    </div>
  );
};
