import { FC, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Box,
  Button,
  Card,
  Flex,
  RadioGroup,
  Text,
  TextField,
} from "@radix-ui/themes";
import { useGetAutoNameMutation } from "../../../services/refact/mcpMarketplace";
import { NotConfiguredIntegrationWithIconRecord } from "../../../services/refact";
import { validateSnakeCase } from "../../../utils/validateSnakeCase";
import { createProjectLabelsWithConflictMarkers } from "../../../utils/createProjectLabelsWithConflictMarkers";
import { IntegrationPathField } from "../IntermediateIntegration/IntegrationPathField";
import styles from "./MCPSetupWizard.module.css";

type MCPSetupWizardProps = {
  integration: NotConfiguredIntegrationWithIconRecord;
  onSubmit: (
    configPath: string,
    integrName: string,
    initialInput?: { input: string; transport: string },
  ) => void;
};

function detectTransport(input: string): "stdio" | "http" | "sse" {
  const trimmed = input.trim();
  if (trimmed.startsWith("http://") || trimmed.startsWith("https://")) {
    return "http";
  }
  return "stdio";
}

function getConfigPrefix(transport: "stdio" | "http" | "sse"): string {
  if (transport === "http") return "mcp_http_";
  if (transport === "sse") return "mcp_sse_";
  return "mcp_stdio_";
}

export const MCPSetupWizard: FC<MCPSetupWizardProps> = ({
  integration,
  onSubmit,
}) => {
  const [input, setInput] = useState("");
  const [suggestedName, setSuggestedName] = useState("");
  const [nameError, setNameError] = useState("");
  const [transport, setTransport] = useState<"stdio" | "http" | "sse">("stdio");
  const [useSSE, setUseSSE] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [selectedConfigPath, setSelectedConfigPath] = useState(
    integration.integr_config_path[0] ?? "",
  );

  const [getAutoName] = useGetAutoNameMutation();
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const projectLabels = useMemo(() => {
    const validProjectPaths = integration.project_path.filter((p) => p !== "");
    return createProjectLabelsWithConflictMarkers(validProjectPaths);
  }, [integration.project_path]);

  const effectiveTransport = useSSE ? "sse" : transport;
  const configPrefix = getConfigPrefix(effectiveTransport);

  const transportLabel =
    effectiveTransport === "stdio"
      ? "Local server (stdio)"
      : effectiveTransport === "sse"
        ? "Remote server (SSE)"
        : "Remote server (HTTP)";

  const handleInputChange = useCallback(
    (value: string) => {
      setInput(value);
      const detected = detectTransport(value);
      setTransport(detected);
      if (detected !== "stdio") {
        setUseSSE(false);
      }

      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }

      if (!value.trim()) {
        setSuggestedName("");
        return;
      }

      debounceRef.current = setTimeout(() => {
        void getAutoName({ input: value.trim() })
          .unwrap()
          .then((result) => {
            setSuggestedName(result.suggested_name);
            setTransport(result.transport);
            if (!validateSnakeCase(result.suggested_name)) {
              setNameError("The name must be in snake_case!");
            } else {
              setNameError("");
            }
          })
          .catch(() => {
            const trimmed = value.trim();
            const fallback = trimmed
              .split(/[^a-z0-9]+/i)
              .filter(Boolean)
              .map((s) => s.toLowerCase())
              .join("_")
              .replace(/^_+|_+$/g, "")
              .slice(0, 40);
            setSuggestedName(fallback || "mcp_server");
          });
      }, 300);
    },
    [getAutoName],
  );

  useEffect(() => {
    return () => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
    };
  }, []);

  const handleNameChange = (value: string) => {
    setSuggestedName(value);
    if (!validateSnakeCase(value)) {
      setNameError("The name must be in snake_case!");
    } else {
      setNameError("");
    }
  };

  const handleSubmit = () => {
    if (!suggestedName || nameError) return;
    const basePath = selectedConfigPath;
    const configPath = basePath
      .replace(
        /mcp_(?:stdio|sse|http)_TEMPLATE/,
        `${configPrefix}${suggestedName}`,
      )
      .replace(/mcp_TEMPLATE/, `${configPrefix}${suggestedName}`);
    const integrName = `${configPrefix}${suggestedName}`;
    onSubmit(configPath, integrName, {
      input: input.trim(),
      transport: effectiveTransport,
    });
  };

  const canSubmit = !!input.trim() && !!suggestedName && !nameError;

  return (
    <Flex direction="column" gap="4" width="100%">
      <Text size="2" color="gray">
        Enter the command or URL for your MCP server:
      </Text>

      <TextField.Root
        size="2"
        placeholder="npx -y @modelcontextprotocol/server-github"
        value={input}
        onChange={(e) => handleInputChange(e.target.value)}
        className={styles.inputField}
        data-testid="mcp-wizard-input"
      />

      {input.trim() && (
        <Flex direction="column" gap="2">
          <Text size="2" color="gray">
            Detected: {transportLabel}
          </Text>

          <Flex align="center" gap="2">
            <Text size="2" color="gray">
              Name:
            </Text>
            <Box style={{ flex: 1 }}>
              <TextField.Root
                size="2"
                value={suggestedName}
                onChange={(e) => handleNameChange(e.target.value)}
                color={nameError ? "red" : undefined}
                data-testid="mcp-wizard-name"
              />
            </Box>
          </Flex>
          {nameError && (
            <Text color="red" size="1">
              {nameError}
            </Text>
          )}
        </Flex>
      )}

      <Card>
        <RadioGroup.Root
          name="integr_config_path"
          value={selectedConfigPath}
          onValueChange={setSelectedConfigPath}
        >
          {integration.integr_config_path.map((configPath, index) => {
            const shouldPathBeFormatted =
              integration.project_path[index] !== "";
            return (
              <Text as="label" size="2" key={configPath}>
                <IntegrationPathField
                  configPath={configPath}
                  projectPath={integration.project_path[index] ?? ""}
                  projectLabels={projectLabels}
                  shouldBeFormatted={shouldPathBeFormatted}
                />
              </Text>
            );
          })}
        </RadioGroup.Root>
      </Card>

      {transport === "stdio" && (
        <Box>
          <button
            type="button"
            className={styles.advancedToggle}
            onClick={() => setAdvancedOpen((v) => !v)}
          >
            <Text size="2" color="gray">
              {advancedOpen ? "▼" : "▶"} Advanced: Use SSE transport instead
            </Text>
          </button>
          {advancedOpen && (
            <Flex align="center" gap="2" mt="2">
              <input
                type="checkbox"
                id="use-sse"
                checked={useSSE}
                onChange={(e) => setUseSSE(e.target.checked)}
                data-testid="mcp-wizard-sse-checkbox"
              />
              <Text as="label" htmlFor="use-sse" size="2">
                Use SSE transport
              </Text>
            </Flex>
          )}
        </Box>
      )}

      <Button
        type="button"
        variant="surface"
        color="green"
        disabled={!canSubmit}
        onClick={handleSubmit}
        data-testid="mcp-wizard-submit"
      >
        Continue with setup
      </Button>
    </Flex>
  );
};
