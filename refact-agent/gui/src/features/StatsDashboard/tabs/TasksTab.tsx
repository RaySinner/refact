import React from "react";
<<<<<<< HEAD
import { Box, Flex, Text } from "@radix-ui/themes";
=======
import { Surface } from "../../../components/ui";
>>>>>>> upstream/main
import { useGetStatsSummaryQuery } from "../../../services/refact/stats";
import { Spinner } from "../../../components/Spinner";
import { ErrorCallout } from "../../../components/Callout";
import { formatTokenCount, formatCostDisplay } from "../utils/formatters";
import { dateRangeToApiArgs } from "../utils/dateRange";
import type { DateRange } from "../types";
import styles from "./TasksTab.module.css";

type Props = { dateRange: DateRange };

export const TasksTab: React.FC<Props> = ({ dateRange }) => {
  const { data, isLoading, isError } = useGetStatsSummaryQuery(
    dateRangeToApiArgs(dateRange),
  );

  if (isLoading) return <Spinner spinning />;
  if (isError) return <ErrorCallout>Failed to load stats</ErrorCallout>;

  const allModes = data?.by_mode ?? [];

  if (!data || allModes.length === 0) {
<<<<<<< HEAD
    return <Text className={styles.emptyText}>No usage data by mode yet.</Text>;
  }

  return (
    <Flex direction="column" gap="3">
      <Box className={styles.tableWrapper}>
=======
    return <p className={styles.emptyText}>No usage data by mode yet.</p>;
  }

  return (
    <div className={styles.root}>
      <Surface animated="rise" className={styles.tableWrapper} variant="glass">
>>>>>>> upstream/main
        <table className={styles.table}>
          <thead>
            <tr>
              <th className={styles.th}>Mode</th>
              <th className={styles.th}>Calls</th>
              <th className={styles.th}>Total Tokens</th>
              <th className={styles.th}>Cost</th>
            </tr>
          </thead>
<<<<<<< HEAD
          <tbody>
            {allModes.map((m) => (
              <tr key={m.mode}>
=======
          <tbody className="rf-stagger">
            {allModes.map((m) => (
              <tr key={m.mode} className="rf-enter-rise">
>>>>>>> upstream/main
                <td className={styles.td}>{m.mode}</td>
                <td className={styles.td}>{m.total_calls}</td>
                <td className={styles.td}>
                  {formatTokenCount(m.total_tokens)}
                </td>
                <td className={styles.td}>
                  {formatCostDisplay(m.total_cost_usd)}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
<<<<<<< HEAD
      </Box>
    </Flex>
=======
      </Surface>
    </div>
>>>>>>> upstream/main
  );
};
