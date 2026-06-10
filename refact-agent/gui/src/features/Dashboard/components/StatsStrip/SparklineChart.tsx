import React from "react";
import type { DayStats } from "../../../StatsDashboard/types";

type SparklineChartProps = {
  days: DayStats[];
};

export const SparklineChart: React.FC<SparklineChartProps> = ({ days }) => {
  const sorted = [...days].sort((a, b) => a.date.localeCompare(b.date));
  const last7 = sorted.slice(-7);
  if (last7.length === 0) {
    return <svg width="80" height="28" />;
  }

  const maxCalls = Math.max(...last7.map((d) => d.total_calls), 1);
  const barWidth = 8;
  const gap = 3;
  const height = 28;
  const totalWidth = last7.length * (barWidth + gap) - gap;

  return (
    <svg width={totalWidth} height={height} aria-label="7-day activity chart">
      {last7.map((day, i) => {
        const barHeight = Math.max(
          (day.total_calls / maxCalls) * (height - 2),
          1,
        );
        const x = i * (barWidth + gap);
        const y = height - barHeight;
        return (
          <rect
            key={day.date}
            x={x}
            y={y}
            width={barWidth}
            height={barHeight}
            rx={2}
            fill="var(--accent-9)"
            opacity={0.7 + (i / last7.length) * 0.3}
          >
            <title>{`${day.date}: ${day.total_calls} calls`}</title>
          </rect>
        );
      })}
    </svg>
  );
};
