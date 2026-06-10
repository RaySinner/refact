import React from "react";
import { Badge, Button, Flex, Text } from "@radix-ui/themes";
import { Spinner } from "../../Spinner";

type ConnectionStatusValue = string | Record<string, unknown>;

type MCPConnectionStatusProps = {
  status: ConnectionStatusValue;
  onReconnect: () => void;
  isReconnecting: boolean;
};

function getStatusLabel(status: ConnectionStatusValue): string {
  if (typeof status === "string") return status;
  if ("status" in status && typeof status.status === "string")
    return status.status;
  return "unknown";
}

function getStatusColor(label: string): "green" | "yellow" | "red" | "gray" {
  const lower = label.toLowerCase();
  if (lower === "connected") return "green";
  if (lower === "connecting" || lower === "reconnecting") return "yellow";
  if (lower === "error") return "red";
  if (lower === "disconnected") return "red";
  return "gray";
}

function isSpinnerVisible(label: string, isReconnecting: boolean): boolean {
  const lower = label.toLowerCase();
  return isReconnecting || lower === "connecting" || lower === "reconnecting";
}

function getAttemptInfo(status: ConnectionStatusValue): string | null {
  if (typeof status !== "object") return null;
  const attempt =
    "attempt" in status && typeof status.attempt === "number"
      ? status.attempt
      : null;
  const maxAttempts =
    "max_attempts" in status && typeof status.max_attempts === "number"
      ? status.max_attempts
      : null;
  if (attempt !== null && maxAttempts !== null)
    return `Attempt ${attempt}/${maxAttempts}`;
  return null;
}

function getNextRetryInfo(status: ConnectionStatusValue): string | null {
  if (typeof status !== "object") return null;
  if (
    "next_retry_seconds" in status &&
    typeof status.next_retry_seconds === "number"
  ) {
    return `Next retry in ${status.next_retry_seconds}s`;
  }
  return null;
}

export const MCPConnectionStatus: React.FC<MCPConnectionStatusProps> = ({
  status,
  onReconnect,
  isReconnecting,
}) => {
  const label = getStatusLabel(status);
  const color = getStatusColor(label);
  const showSpinner = isSpinnerVisible(label, isReconnecting);
  const attemptInfo = getAttemptInfo(status);
  const nextRetryInfo = getNextRetryInfo(status);

  return (
    <Flex align="center" gap="3" wrap="wrap">
      <Flex align="center" gap="2">
        <Badge color={color} radius="full" size="2">
          {label}
        </Badge>
        {showSpinner && <Spinner spinning />}
      </Flex>
      {attemptInfo && (
        <Text size="1" color="gray">
          {attemptInfo}
        </Text>
      )}
      {nextRetryInfo && (
        <Text size="1" color="gray">
          {nextRetryInfo}
        </Text>
      )}
      <Button
        size="1"
        variant="soft"
        onClick={onReconnect}
        disabled={isReconnecting}
      >
        {isReconnecting ? "Reconnecting..." : "Reconnect"}
      </Button>
      {typeof status === "object" &&
        "error" in status &&
        typeof status.error === "string" && (
          <Text size="1" color="red">
            {status.error}
          </Text>
        )}
    </Flex>
  );
};
