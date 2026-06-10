import React from "react";
import { Text } from "@radix-ui/themes";
import { formatTokenCount } from "../../../StatsDashboard/utils/formatters";

type TokenDonutProps = {
  prompt: number;
  completion: number;
  cache: number;
};

export const TokenDonut: React.FC<TokenDonutProps> = ({
  prompt,
  completion,
  cache,
}) => {
  const total = prompt + completion + cache;
  if (total === 0) {
    return (
      <Text size="2" weight="bold" color="gray">
        0
      </Text>
    );
  }

  const size = 36;
  const strokeWidth = 5;
  const radius = (size - strokeWidth) / 2;
  const circumference = 2 * Math.PI * radius;
  const cx = size / 2;
  const cy = size / 2;

  const promptFraction = prompt / total;
  const completionFraction = completion / total;
  const cacheFraction = cache / total;

  const promptOffset = 0;
  const completionOffset = promptFraction * circumference;
  const cacheOffset = (promptFraction + completionFraction) * circumference;

  return (
    <div style={{ display: "flex", alignItems: "center", gap: 4 }}>
      <svg width={size} height={size} aria-label="Token distribution">
        <circle
          cx={cx}
          cy={cy}
          r={radius}
          fill="none"
          stroke="var(--blue-8)"
          strokeWidth={strokeWidth}
          strokeDasharray={`${promptFraction * circumference} ${circumference}`}
          strokeDashoffset={-promptOffset}
          transform={`rotate(-90 ${cx} ${cy})`}
        >
          <title>{`Prompt: ${formatTokenCount(prompt)}`}</title>
        </circle>
        <circle
          cx={cx}
          cy={cy}
          r={radius}
          fill="none"
          stroke="var(--green-8)"
          strokeWidth={strokeWidth}
          strokeDasharray={`${
            completionFraction * circumference
          } ${circumference}`}
          strokeDashoffset={-completionOffset}
          transform={`rotate(-90 ${cx} ${cy})`}
        >
          <title>{`Completion: ${formatTokenCount(completion)}`}</title>
        </circle>
        {cacheFraction > 0 && (
          <circle
            cx={cx}
            cy={cy}
            r={radius}
            fill="none"
            stroke="var(--amber-8)"
            strokeWidth={strokeWidth}
            strokeDasharray={`${
              cacheFraction * circumference
            } ${circumference}`}
            strokeDashoffset={-cacheOffset}
            transform={`rotate(-90 ${cx} ${cy})`}
          >
            <title>{`Cache: ${formatTokenCount(cache)}`}</title>
          </circle>
        )}
      </svg>
      <Text size="1" weight="bold">
        {formatTokenCount(total)}
      </Text>
    </div>
  );
};
