import React, { useMemo } from "react";
<<<<<<< HEAD
import { Box, Flex, Text, Badge } from "@radix-ui/themes";
=======
import { Badge, Card, Icon, StatusDot, Surface } from "../../../components/ui";
import {
  Bot,
  CalendarClock,
  CheckCircle2,
  Clock3,
  Coins,
  Gauge,
  Hash,
  MessageSquareText,
  PiggyBank,
  RefreshCw,
  ServerCog,
  Sparkles,
  Zap,
} from "lucide-react";
>>>>>>> upstream/main
import { useGetStatsSummaryQuery } from "../../../services/refact/stats";
import {
  useGetClaudeCodeUsageQuery,
  useGetOpenAICodexUsageQuery,
<<<<<<< HEAD
=======
  useGetOpenCodeUsageQuery,
  type OpenCodeUsageData,
>>>>>>> upstream/main
} from "../../../services/refact/providers";
import { useGetConfiguredProvidersQuery } from "../../../hooks";
import { Spinner } from "../../../components/Spinner";
import { ErrorCallout } from "../../../components/Callout";
import { StatCard } from "../components/StatCard";
import {
  formatTokenCount,
  formatCostDisplay,
  formatDuration,
} from "../utils/formatters";
import { dateRangeToApiArgs } from "../utils/dateRange";
<<<<<<< HEAD
import type { DateRange } from "../types";
import styles from "./OverviewTab.module.css";

const formatResetAt = (resetAt: string | null | undefined): string | null => {
  if (!resetAt) return null;
  const d = new Date(resetAt);
  if (isNaN(d.getTime())) return null;
  return `Resets ${d.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  })}`;
};

const UsageBar: React.FC<{ pct: number }> = ({ pct }) => {
  const clamped = Math.max(0, Math.min(pct, 100));
  const color =
    clamped >= 90
      ? "var(--red-9)"
      : clamped >= 70
        ? "var(--orange-9)"
        : "var(--green-9)";
  return (
    <div
      style={{
        height: "4px",
        width: "100%",
        borderRadius: "2px",
        background: "var(--gray-a4)",
        overflow: "hidden",
        marginTop: "4px",
      }}
    >
      <div
        style={{
          height: "100%",
          width: `${clamped}%`,
          borderRadius: "2px",
          background: color,
          transition: "width 0.3s ease",
        }}
=======
import {
  clampPercent,
  formatClaudeExtraUsage,
  formatCodexCreditsDetails,
  formatCodexCreditsSummary,
  formatLimitWindowSeconds,
  formatQuotaMeta,
  formatResetAt,
  formatUsagePercent,
} from "../../../utils/providerQuota";
import type { DateRange } from "../types";
import styles from "./OverviewTab.module.css";

type UsageTone = "danger" | "warning" | "success";

const usageTone = (pct: number): UsageTone => {
  if (pct >= 90) return "danger";
  if (pct >= 70) return "warning";
  return "success";
};

const usageStatus: Record<UsageTone, "error" | "warning" | "success"> = {
  danger: "error",
  warning: "warning",
  success: "success",
};

const UsageBar: React.FC<{ pct: number }> = ({ pct }) => {
  const clamped = clampPercent(pct);
  const tone = usageTone(clamped);
  const color =
    tone === "danger"
      ? "var(--rf-color-danger)"
      : tone === "warning"
        ? "var(--rf-color-warning)"
        : "var(--rf-color-success)";

  return (
    <div
      aria-label={`${Math.round(clamped)}% used`}
      className={styles.usageBar}
      role="meter"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={clamped}
    >
      <div
        className={styles.usageBarFill}
        style={
          {
            "--usage-pct": `${clamped}%`,
            "--usage-color": color,
          } as React.CSSProperties
        }
>>>>>>> upstream/main
      />
    </div>
  );
};
<<<<<<< HEAD
=======

const QuotaLine: React.FC<{
  label: string;
  pct: number;
  resetAt?: string | null;
  limitReached?: boolean;
  windowSeconds?: number | null;
}> = ({ label, pct, resetAt, limitReached = false, windowSeconds }) => {
  const clamped = clampPercent(pct);
  const windowText = formatLimitWindowSeconds(windowSeconds);
  const meta = formatQuotaMeta([
    formatUsagePercent(clamped),
    windowText ? `Window ${windowText}` : null,
    formatResetAt(resetAt),
  ]);

  return (
    <div className={styles.quotaLine}>
      <div className={styles.quotaLabel}>
        <StatusDot
          status={limitReached ? "error" : usageStatus[usageTone(clamped)]}
          pulse={limitReached}
        />
        <span className={styles.quotaMeta}>{label}</span>
        {limitReached && <Badge tone="danger">Limit reached</Badge>}
      </div>
      <span className={styles.quotaMeta}>{meta}</span>
    </div>
  );
};

type OpenCodeUsageWindowKey = keyof Pick<
  OpenCodeUsageData,
  "rolling" | "weekly" | "monthly"
>;

const OPENCODE_USAGE_WINDOWS: {
  key: OpenCodeUsageWindowKey;
  label: string;
}[] = [
  { key: "rolling", label: "Rolling" },
  { key: "weekly", label: "Weekly" },
  { key: "monthly", label: "Monthly" },
];

function hasOpenCodeQuotaData(data: OpenCodeUsageData): boolean {
  return (
    typeof data.balance === "number" ||
    OPENCODE_USAGE_WINDOWS.some(({ key }) => Boolean(data[key]))
  );
}

>>>>>>> upstream/main
const ClaudeCodeInstanceCard: React.FC<{
  providerName: string;
  displayName: string;
}> = ({ providerName, displayName }) => {
  const { data: claudeUsage } = useGetClaudeCodeUsageQuery(
    { providerName },
    { pollingInterval: 5 * 60_000 },
  );
  const data = claudeUsage?.data;
  if (!data) return null;
  if (!data.five_hour && !data.seven_day) return null;

  return (
<<<<<<< HEAD
    <Box
      style={{
        flex: "1 1 200px",
        minWidth: "180px",
        background: "var(--gray-a2)",
        borderRadius: "var(--radius-3)",
        padding: "var(--space-3)",
      }}
    >
      <Flex align="center" gap="2" mb="2">
        <Text size="2" weight="medium">
          {displayName}
        </Text>
        <Text size="1" color="gray">
          ({providerName})
        </Text>
      </Flex>
      {data.five_hour &&
        (() => {
          const pct = Math.max(0, Math.min(data.five_hour.percent_used, 100));
          return (
            <Box mb="3">
              <Flex justify="between">
                <Text size="1" color="gray">
                  Session (5h)
                </Text>
                <Text size="1" color="gray">
                  {Math.round(pct)}%
                  {formatResetAt(data.five_hour.resets_at)
                    ? ` · ${formatResetAt(data.five_hour.resets_at)}`
                    : ""}
                </Text>
              </Flex>
              <UsageBar pct={pct} />
            </Box>
          );
        })()}
      {data.seven_day &&
        (() => {
          const pct = Math.max(0, Math.min(data.seven_day.percent_used, 100));
          return (
            <Box mb="2">
              <Flex justify="between">
                <Text size="1" color="gray">
                  Weekly
                </Text>
                <Text size="1" color="gray">
                  {Math.round(pct)}%
                  {formatResetAt(data.seven_day.resets_at)
                    ? ` · ${formatResetAt(data.seven_day.resets_at)}`
                    : ""}
                </Text>
              </Flex>
              <UsageBar pct={pct} />
            </Box>
          );
        })()}
      {data.extra_usage && (
        <Text size="1" color="gray">
          Extra: {data.extra_usage.is_enabled ? "on" : "off"} · $
          {data.extra_usage.used_credits.toFixed(2)} spent
          {typeof data.extra_usage.monthly_limit === "number"
            ? ` / $${data.extra_usage.monthly_limit.toFixed(0)}`
            : ""}
        </Text>
      )}
    </Box>
=======
    <Card animated="rise" className={styles.quotaCard}>
      <div className={styles.quotaHeader}>
        <div className={styles.quotaLabel}>
          <Icon icon={ServerCog} size="md" tone="accent" />
          <span className={styles.quotaName}>{displayName}</span>
        </div>
        <span className={styles.quotaProvider}>{providerName}</span>
      </div>
      {data.five_hour && (
        <div>
          <QuotaLine
            label="Session (5h)"
            pct={data.five_hour.percent_used}
            resetAt={data.five_hour.resets_at}
          />
          <UsageBar pct={data.five_hour.percent_used} />
        </div>
      )}
      {data.seven_day && (
        <div>
          <QuotaLine
            label="Weekly"
            pct={data.seven_day.percent_used}
            resetAt={data.seven_day.resets_at}
          />
          <UsageBar pct={data.seven_day.percent_used} />
        </div>
      )}
      {data.extra_usage && (
        <span className={styles.quotaExtra}>
          Extra: {formatClaudeExtraUsage(data.extra_usage)}
        </span>
      )}
    </Card>
>>>>>>> upstream/main
  );
};

const OpenAICodexInstanceCard: React.FC<{
  providerName: string;
  displayName: string;
}> = ({ providerName, displayName }) => {
  const { data: codexUsage } = useGetOpenAICodexUsageQuery(
    { providerName },
    { pollingInterval: 5 * 60_000 },
  );
  const data = codexUsage?.data;
  if (!data?.rate_limit) return null;

<<<<<<< HEAD
  return (
    <Box
      style={{
        flex: "1 1 200px",
        minWidth: "180px",
        background: "var(--gray-a2)",
        borderRadius: "var(--radius-3)",
        padding: "var(--space-3)",
      }}
    >
      <Flex align="center" gap="2" mb="2">
        <Text size="2" weight="medium">
          {displayName}
        </Text>
        <Text size="1" color="gray">
          ({providerName})
        </Text>
        {data.plan_type && (
          <Badge color="blue" size="1">
            {data.plan_type}
          </Badge>
        )}
      </Flex>
      {data.rate_limit.primary_window &&
        (() => {
          const pct = Math.max(
            0,
            Math.min(data.rate_limit.primary_window.used_percent, 100),
          );
          return (
            <Box mb="3">
              <Flex justify="between" align="center">
                <Flex align="center" gap="1">
                  <Text size="1" color="gray">
                    Session (5h)
                  </Text>
                  {data.rate_limit.limit_reached && (
                    <Badge color="red" size="1">
                      Limit reached
                    </Badge>
                  )}
                </Flex>
                <Text size="1" color="gray">
                  {Math.round(pct)}%
                  {formatResetAt(data.rate_limit.primary_window.reset_at)
                    ? ` · ${formatResetAt(
                        data.rate_limit.primary_window.reset_at,
                      )}`
                    : ""}
                </Text>
              </Flex>
              <UsageBar pct={pct} />
            </Box>
          );
        })()}
      {data.rate_limit.secondary_window &&
        (() => {
          const pct = Math.max(
            0,
            Math.min(data.rate_limit.secondary_window.used_percent, 100),
          );
          return (
            <Box mb="2">
              <Flex justify="between">
                <Text size="1" color="gray">
                  Weekly
                </Text>
                <Text size="1" color="gray">
                  {Math.round(pct)}%
                  {formatResetAt(data.rate_limit.secondary_window.reset_at)
                    ? ` · ${formatResetAt(
                        data.rate_limit.secondary_window.reset_at,
                      )}`
                    : ""}
                </Text>
              </Flex>
              <UsageBar pct={pct} />
            </Box>
          );
        })()}
      {data.code_review_rate_limit?.primary_window &&
        (() => {
          const pct = Math.max(
            0,
            Math.min(
              data.code_review_rate_limit.primary_window.used_percent,
              100,
            ),
          );
          return (
            <Box mb="2">
              <Flex justify="between" align="center">
                <Flex align="center" gap="1">
                  <Text size="1" color="gray">
                    Code review
                  </Text>
                  {data.code_review_rate_limit.limit_reached && (
                    <Badge color="red" size="1">
                      Limit reached
                    </Badge>
                  )}
                </Flex>
                <Text size="1" color="gray">
                  {Math.round(pct)}%
                </Text>
              </Flex>
              <UsageBar pct={pct} />
            </Box>
          );
        })()}
      {data.credits && (
        <Text size="1" color="gray">
          Credits:{" "}
          {data.credits.unlimited
            ? "unlimited"
            : data.credits.has_credits
              ? `${data.credits.balance} remaining`
              : "none"}
        </Text>
      )}
    </Box>
=======
  const creditsDetails = data.credits
    ? formatCodexCreditsDetails(data.credits)
    : null;

  return (
    <Card animated="rise" className={styles.quotaCard}>
      <div className={styles.quotaHeader}>
        <div className={styles.quotaLabel}>
          <Icon icon={Bot} size="md" tone="accent" />
          <span className={styles.quotaName}>{displayName}</span>
          {data.plan_type && <Badge tone="accent">{data.plan_type}</Badge>}
        </div>
        <span className={styles.quotaProvider}>{providerName}</span>
      </div>
      {data.rate_limit.primary_window && (
        <div>
          <QuotaLine
            label="Session"
            pct={data.rate_limit.primary_window.used_percent}
            resetAt={data.rate_limit.primary_window.reset_at}
            limitReached={data.rate_limit.limit_reached}
            windowSeconds={data.rate_limit.primary_window.limit_window_seconds}
          />
          <UsageBar pct={data.rate_limit.primary_window.used_percent} />
        </div>
      )}
      {data.rate_limit.secondary_window && (
        <div>
          <QuotaLine
            label="Secondary"
            pct={data.rate_limit.secondary_window.used_percent}
            resetAt={data.rate_limit.secondary_window.reset_at}
            windowSeconds={
              data.rate_limit.secondary_window.limit_window_seconds
            }
          />
          <UsageBar pct={data.rate_limit.secondary_window.used_percent} />
        </div>
      )}
      {data.code_review_rate_limit?.primary_window && (
        <div>
          <QuotaLine
            label="Code review"
            pct={data.code_review_rate_limit.primary_window.used_percent}
            resetAt={data.code_review_rate_limit.primary_window.reset_at}
            limitReached={data.code_review_rate_limit.limit_reached}
            windowSeconds={
              data.code_review_rate_limit.primary_window.limit_window_seconds
            }
          />
          <UsageBar
            pct={data.code_review_rate_limit.primary_window.used_percent}
          />
        </div>
      )}
      {data.code_review_rate_limit?.secondary_window && (
        <div>
          <QuotaLine
            label="Code review secondary"
            pct={data.code_review_rate_limit.secondary_window.used_percent}
            resetAt={data.code_review_rate_limit.secondary_window.reset_at}
            windowSeconds={
              data.code_review_rate_limit.secondary_window.limit_window_seconds
            }
          />
          <UsageBar
            pct={data.code_review_rate_limit.secondary_window.used_percent}
          />
        </div>
      )}
      {data.credits && (
        <span className={styles.quotaExtra}>
          Credits: {formatCodexCreditsSummary(data.credits)}
          {creditsDetails ? ` · ${creditsDetails}` : ""}
        </span>
      )}
    </Card>
  );
};

const OpenCodeInstanceCard: React.FC<{
  providerName: string;
  displayName: string;
  data: OpenCodeUsageData;
}> = ({ providerName, displayName, data }) => {
  const windows = OPENCODE_USAGE_WINDOWS.map(({ key, label }) => ({
    key,
    label,
    window: data[key],
  })).filter(
    (
      row,
    ): row is {
      key: OpenCodeUsageWindowKey;
      label: string;
      window: NonNullable<OpenCodeUsageData[OpenCodeUsageWindowKey]>;
    } => Boolean(row.window),
  );
  if (!hasOpenCodeQuotaData(data)) return null;

  return (
    <Card animated="rise" className={styles.quotaCard}>
      <div className={styles.quotaHeader}>
        <div className={styles.quotaLabel}>
          <Icon icon={Bot} size="md" tone="accent" />
          <span className={styles.quotaName}>{displayName}</span>
          {data.plan_type && <Badge tone="accent">{data.plan_type}</Badge>}
        </div>
        <span className={styles.quotaProvider}>{providerName}</span>
      </div>
      {data.workspace_id && (
        <span className={styles.quotaExtra}>
          Workspace: {data.workspace_id}
        </span>
      )}
      {typeof data.balance === "number" && (
        <span className={styles.quotaExtra}>
          Zen balance:{" "}
          {data.balance.toLocaleString(undefined, { maximumFractionDigits: 2 })}
        </span>
      )}
      {windows.map(({ key, label, window }) => (
        <div key={key}>
          <QuotaLine
            label={
              formatLimitWindowSeconds(window.limit_window_seconds) ?? label
            }
            pct={window.used_percent}
            resetAt={window.reset_at}
            limitReached={window.status === "rate-limited"}
            windowSeconds={window.limit_window_seconds}
          />
          <UsageBar pct={window.used_percent} />
        </div>
      ))}
    </Card>
  );
};

const OpenCodeProviderQuotaCard: React.FC<{
  providerName: string;
  displayName: string;
}> = ({ providerName, displayName }) => {
  const { data: openCodeUsage } = useGetOpenCodeUsageQuery(
    { providerName },
    { pollingInterval: 5 * 60_000 },
  );
  const data = openCodeUsage?.data;
  if (!data || !hasOpenCodeQuotaData(data)) return null;

  return (
    <section className={styles.root}>
      <h3 className={styles.sectionTitle}>
        <Icon icon={Gauge} size="md" tone="accent" />
        OpenCode Quota
      </h3>
      <div className={`${styles.quotaGrid} rf-stagger`}>
        <OpenCodeInstanceCard
          providerName={providerName}
          displayName={displayName}
          data={data}
        />
      </div>
    </section>
  );
};

const OpenCodeProviderQuotaSections: React.FC = () => {
  const { data: providersData } = useGetConfiguredProvidersQuery();
  const providers = useMemo(
    () => providersData?.providers ?? [],
    [providersData],
  );
  const openCodeInstances = useMemo(
    () => providers.filter((p) => p.base_provider === "opencode" && p.enabled),
    [providers],
  );

  return (
    <>
      {openCodeInstances.map((p) => (
        <OpenCodeProviderQuotaCard
          key={`opencode:${p.name}`}
          providerName={p.name}
          displayName={p.display_name}
        />
      ))}
    </>
>>>>>>> upstream/main
  );
};

const ProviderQuotaSection: React.FC = () => {
  const { data: providersData } = useGetConfiguredProvidersQuery();
  const providers = useMemo(
    () => providersData?.providers ?? [],
    [providersData],
  );

  const claudeInstances = useMemo(
    () =>
      providers.filter((p) => p.base_provider === "claude_code" && p.enabled),
    [providers],
  );
  const codexInstances = useMemo(
    () =>
      providers.filter((p) => p.base_provider === "openai_codex" && p.enabled),
    [providers],
  );
<<<<<<< HEAD

  if (claudeInstances.length === 0 && codexInstances.length === 0) return null;

  return (
    <Box>
      <Text
        size="3"
        weight="medium"
        className={styles.sectionTitle}
        mb="3"
        as="p"
      >
        Provider Quotas
      </Text>
      <Flex gap="3" wrap="wrap">
=======
  if (claudeInstances.length === 0 && codexInstances.length === 0) return null;

  return (
    <section className={styles.root}>
      <h3 className={styles.sectionTitle}>
        <Icon icon={Gauge} size="md" tone="accent" />
        Provider Quotas
      </h3>
      <div className={`${styles.quotaGrid} rf-stagger`}>
>>>>>>> upstream/main
        {claudeInstances.map((p) => (
          <ClaudeCodeInstanceCard
            key={`claude:${p.name}`}
            providerName={p.name}
            displayName={p.display_name}
          />
        ))}
        {codexInstances.map((p) => (
          <OpenAICodexInstanceCard
            key={`codex:${p.name}`}
            providerName={p.name}
            displayName={p.display_name}
          />
        ))}
<<<<<<< HEAD
      </Flex>
    </Box>
=======
      </div>
    </section>
>>>>>>> upstream/main
  );
};

type Props = { dateRange: DateRange };

export const OverviewTab: React.FC<Props> = ({ dateRange }) => {
  const { data, isLoading, isError } = useGetStatsSummaryQuery(
    dateRangeToApiArgs(dateRange),
  );

  if (isLoading) return <Spinner spinning />;
  if (isError) return <ErrorCallout>Failed to load stats</ErrorCallout>;

  const t = data?.totals;
  const hasStats = !!(t && t.total_calls > 0);

  const avgPerConversation =
    t && t.total_conversations > 0
      ? Math.round(t.total_tokens / t.total_conversations)
      : 0;
  const avgPerMessage =
    t && t.total_messages_sent > 0
      ? Math.round(t.total_tokens / t.total_messages_sent)
      : 0;
  const completionPct =
    t && t.total_tokens > 0
      ? Math.round((t.total_completion_tokens / t.total_tokens) * 100)
      : 0;
  const successRate =
    t && t.total_calls > 0
      ? Math.round((t.successful_calls / t.total_calls) * 100)
      : 0;
  const cacheEfficiency =
    t && t.total_tokens > 0
      ? Math.round((t.total_cache_read_tokens / t.total_tokens) * 100)
      : 0;

  const topConversations = data?.top_conversations ?? [];

  return (
<<<<<<< HEAD
    <Flex direction="column" gap="4">
      <ProviderQuotaSection />
      {!hasStats && (
        <Text className={styles.emptyText}>
          No usage data yet. Start chatting to see stats!
        </Text>
      )}
      {hasStats && (
        <>
          <Flex className={styles.cardsRow}>
            <StatCard
=======
    <div className={styles.root}>
      <ProviderQuotaSection />
      <OpenCodeProviderQuotaSections />
      {!hasStats && (
        <p className={styles.emptyText}>
          No usage data yet. Start chatting to see stats!
        </p>
      )}
      {hasStats && (
        <>
          <div className={`${styles.cardsRow} rf-stagger`}>
            <StatCard
              icon={Zap}
>>>>>>> upstream/main
              title="Total Usage"
              value={formatTokenCount(t.total_tokens)}
              subtitle={`${formatTokenCount(
                t.total_prompt_tokens,
              )} read + ${formatTokenCount(t.total_completion_tokens)} written`}
            />
            <StatCard
<<<<<<< HEAD
=======
              icon={MessageSquareText}
>>>>>>> upstream/main
              title="Conversations"
              value={t.total_conversations.toString()}
              subtitle={`Each one used ~${formatTokenCount(
                avgPerConversation,
              )} tokens on average`}
            />
            <StatCard
<<<<<<< HEAD
=======
              icon={Hash}
>>>>>>> upstream/main
              title="Messages Sent"
              value={t.total_messages_sent.toString()}
              subtitle={`Each message cost ~${formatTokenCount(
                avgPerMessage,
              )} tokens on average`}
            />
            <StatCard
<<<<<<< HEAD
=======
              icon={Sparkles}
>>>>>>> upstream/main
              title="AI Wrote"
              value={formatTokenCount(t.total_completion_tokens)}
              subtitle={`${completionPct}% of total — most usage is from reading context`}
            />
            <StatCard
<<<<<<< HEAD
=======
              icon={CheckCircle2}
              tone="success"
>>>>>>> upstream/main
              title="Success Rate"
              value={`${successRate}%`}
              subtitle={`${t.successful_calls} of ${t.total_calls} calls succeeded`}
            />
            <StatCard
<<<<<<< HEAD
=======
              icon={Coins}
              tone="warning"
>>>>>>> upstream/main
              title="Total Cost"
              value={formatCostDisplay(t.total_cost_usd)}
              subtitle="across all providers"
            />
            <StatCard
<<<<<<< HEAD
=======
              icon={Clock3}
>>>>>>> upstream/main
              title="Avg Duration"
              value={formatDuration(t.avg_duration_ms)}
              subtitle="average per LLM call"
            />
            <StatCard
<<<<<<< HEAD
=======
              icon={PiggyBank}
              tone="success"
>>>>>>> upstream/main
              title="Cache Efficiency"
              value={`${cacheEfficiency}%`}
              subtitle={`${formatTokenCount(
                t.total_cache_read_tokens,
              )} tokens read from cache`}
            />
            <StatCard
<<<<<<< HEAD
=======
              icon={RefreshCw}
>>>>>>> upstream/main
              title="Cache Created"
              value={formatTokenCount(t.total_cache_creation_tokens)}
              subtitle="tokens written to cache for future reuse"
            />
<<<<<<< HEAD
          </Flex>

          {topConversations.length > 0 && (
            <Box>
              <Text
                size="3"
                weight="medium"
                className={styles.sectionTitle}
                mb="2"
                as="p"
              >
                Top Conversations by Token Usage
              </Text>
              <Box className={styles.tableWrapper}>
=======
          </div>

          {topConversations.length > 0 && (
            <section className={styles.root}>
              <h3 className={styles.sectionTitle}>
                <Icon icon={CalendarClock} size="md" tone="accent" />
                Top Conversations by Token Usage
              </h3>
              <Surface
                className={`${styles.tableWrapper} rf-enter-rise`}
                variant="plain"
              >
>>>>>>> upstream/main
                <table className={styles.table}>
                  <thead>
                    <tr>
                      <th className={styles.th}>Chat ID</th>
                      <th className={styles.th}>Model</th>
                      <th className={styles.th}>Calls</th>
                      <th className={styles.th}>Tokens</th>
                      <th className={styles.th}>Cost</th>
                    </tr>
                  </thead>
                  <tbody>
                    {topConversations.map((c) => (
                      <tr key={c.chat_id}>
                        <td className={styles.td}>
                          <span className={styles.chatId} title={c.chat_id}>
                            {c.chat_id.slice(0, 8)}
                          </span>
                        </td>
                        <td className={styles.td}>{c.model_id}</td>
                        <td className={styles.td}>{c.total_calls}</td>
                        <td className={styles.td}>
                          {formatTokenCount(c.total_tokens)}
                        </td>
                        <td className={styles.td}>
                          {formatCostDisplay(c.total_cost_usd)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
<<<<<<< HEAD
              </Box>
            </Box>
          )}
        </>
      )}
    </Flex>
=======
              </Surface>
            </section>
          )}
        </>
      )}
    </div>
>>>>>>> upstream/main
  );
};
