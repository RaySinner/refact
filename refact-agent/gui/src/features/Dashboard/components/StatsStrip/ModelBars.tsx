import React from "react";
import { Text, Tooltip } from "@radix-ui/themes";
import type { ModelStats } from "../../../StatsDashboard/types";
import { formatTokenCount } from "../../../StatsDashboard/utils/formatters";
import styles from "./ModelBars.module.css";

const MODEL_COLORS = [
  "var(--blue-8)",
  "var(--green-8)",
  "var(--amber-8)",
  "var(--purple-8)",
  "var(--red-8)",
];

type ModelBarsProps = {
  models: ModelStats[];
};

export const ModelBars: React.FC<ModelBarsProps> = ({ models }) => {
  const sorted = [...models]
    .sort((a, b) => b.total_tokens - a.total_tokens)
    .slice(0, 4);

  if (sorted.length === 0) {
    return (
      <Text size="1" color="gray">
        No data
      </Text>
    );
  }

  const maxTokens = Math.max(...sorted.map((m) => m.total_tokens), 1);

  return (
    <div className={styles.bars}>
      {sorted.map((model, i) => {
        const width = Math.max((model.total_tokens / maxTokens) * 100, 4);
        const shortName = model.model.split("/").pop() ?? model.model;
        return (
          <Tooltip
            key={model.model_id || model.model}
            content={`${shortName}: ${formatTokenCount(
              model.total_tokens,
            )} tokens`}
          >
            <div className={styles.barRow}>
              <div
                className={styles.barFill}
                style={{
                  width: `${width}%`,
                  background: MODEL_COLORS[i % MODEL_COLORS.length],
                }}
              />
              <Text size="1" className={styles.barLabel} truncate>
                {shortName}
              </Text>
            </div>
          </Tooltip>
        );
      })}
    </div>
  );
};
