import React, { useMemo } from "react";
import { Box, Card, Flex, Text, HoverCard } from "@radix-ui/themes";
import { Usage } from "../../services/refact";
import { formatNumberToFixed } from "../../utils/formatNumberToFixed";
import {
  calculateUsageInputTokens,
  getCacheCreationTokens,
  getCacheReadTokens,
} from "../../utils/calculateUsageInputTokens";
import { formatUsd } from "../../utils/getMetering";

type MessageUsageInfoProps = {
  usage?: Usage | null;
};

const TokenDisplay: React.FC<{ label: string; value: number }> = ({
  label,
  value,
}) => (
  <Flex align="center" justify="between" width="100%" gap="4">
    <Text size="1" weight="bold">
      {label}
    </Text>
    <Text size="1">{formatNumberToFixed(value)}</Text>
  </Flex>
);

const UsdDisplay: React.FC<{ label: string; value: number | undefined }> = ({
  label,
  value,
}) => (
  <Flex align="center" justify="between" width="100%" gap="4">
    <Text size="1" weight="bold">
      {label}
    </Text>
    <Text size="1">{formatUsd(value)}</Text>
  </Flex>
);

export const MessageUsageInfo: React.FC<MessageUsageInfoProps> = ({
  usage,
}) => {
  const outputTokens = useMemo(() => {
    return calculateUsageInputTokens({
      usage,
      keys: ["completion_tokens"],
    });
  }, [usage]);

  // Context tokens includes prompt + cache tokens for accurate context size
  const contextTokens = useMemo(() => {
    return calculateUsageInputTokens({
      usage,
      keys: [
        "prompt_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
      ],
    });
  }, [usage]);

  const cacheReadTokens = getCacheReadTokens(usage);
  const cacheCreationTokens = getCacheCreationTokens(usage);

  const meteringUsd = usage?.metering_usd;
  const hasUsd = meteringUsd !== undefined && meteringUsd.total_usd > 0;

  if (!usage && !hasUsd) return null;

  return (
    <Flex justify="end" mt="2">
      <HoverCard.Root>
        <HoverCard.Trigger>
          <Card
            style={{
              padding: "var(--space-1) var(--space-2)",
              opacity: 0.5,
              cursor: "pointer",
            }}
          >
            <Flex align="center" gap="3">
              {contextTokens > 0 && (
                <Flex align="center" gap="1">
                  <Text size="1" color="gray">
                    ctx:
                  </Text>
                  <Text size="1">{formatNumberToFixed(contextTokens)}</Text>
                </Flex>
              )}
              {hasUsd && (
                <Flex align="center" gap="1">
                  <Text size="1">{formatUsd(meteringUsd.total_usd)}</Text>
                </Flex>
              )}
            </Flex>
          </Card>
        </HoverCard.Trigger>
        <HoverCard.Content size="1" maxWidth="300px">
          <Flex direction="column" gap="2">
            <Text size="2" weight="bold" mb="1">
              This Message
            </Text>

            {usage && (
              <>
                <TokenDisplay label="Context size" value={contextTokens} />
                {cacheReadTokens > 0 && (
                  <TokenDisplay label="Cache read" value={cacheReadTokens} />
                )}
                {cacheCreationTokens > 0 && (
                  <TokenDisplay
                    label="Cache creation"
                    value={cacheCreationTokens}
                  />
                )}
                <TokenDisplay label="Output tokens" value={outputTokens} />
                {usage.completion_tokens_details?.reasoning_tokens !== null &&
                  usage.completion_tokens_details?.reasoning_tokens !==
                    undefined &&
                  usage.completion_tokens_details.reasoning_tokens > 0 && (
                    <TokenDisplay
                      label="Reasoning tokens"
                      value={usage.completion_tokens_details.reasoning_tokens}
                    />
                  )}
              </>
            )}

            {hasUsd && (
              <>
                <Box my="2" style={{ borderTop: "1px solid var(--gray-a6)" }} />
                <Flex align="center" justify="between" width="100%" mb="1">
                  <Text size="2" weight="bold">
                    Cost
                  </Text>
                  <Text size="2">{formatUsd(meteringUsd.total_usd)}</Text>
                </Flex>
                <UsdDisplay label="Prompt" value={meteringUsd.prompt_usd} />
                <UsdDisplay
                  label="Completion"
                  value={meteringUsd.generated_usd}
                />
                {meteringUsd.cache_read_usd !== undefined &&
                  meteringUsd.cache_read_usd > 0 && (
                    <UsdDisplay
                      label="Cache read"
                      value={meteringUsd.cache_read_usd}
                    />
                  )}
                {meteringUsd.cache_creation_usd !== undefined &&
                  meteringUsd.cache_creation_usd > 0 && (
                    <UsdDisplay
                      label="Cache creation"
                      value={meteringUsd.cache_creation_usd}
                    />
                  )}
              </>
            )}
          </Flex>
        </HoverCard.Content>
      </HoverCard.Root>
    </Flex>
  );
};
