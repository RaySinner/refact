import { Button, Flex, Text } from "@radix-ui/themes";

import { FC } from "react";
import {
  IntegrationWithIconRecord,
  NotConfiguredIntegrationWithIconRecord,
} from "../../../services/refact";
import { GlobalIntegrations } from "./GlobalIntegrations";
import { NewIntegrations } from "./NewIntegrations";
import { ProjectIntegrations } from "./ProjectIntegrations";
import { useAppDispatch } from "../../../hooks";
import { push } from "../../../features/Pages/pagesSlice";

type IntegrationsListProps = {
  globalIntegrations?: IntegrationWithIconRecord[];
  groupedProjectIntegrations?: Record<string, IntegrationWithIconRecord[]>;
  availableIntegrationsToConfigure?: NotConfiguredIntegrationWithIconRecord[];
  handleIntegrationShowUp: (
    integration:
      | IntegrationWithIconRecord
      | NotConfiguredIntegrationWithIconRecord,
  ) => void;
};

export const IntegrationsList: FC<IntegrationsListProps> = ({
  globalIntegrations,
  groupedProjectIntegrations,
  availableIntegrationsToConfigure,
  handleIntegrationShowUp,
}) => {
  const dispatch = useAppDispatch();

  return (
    <Flex direction="column" width="100%" gap="4">
      <Flex align="center" justify="between">
        <Text my="2">
          Integrations allow Refact.ai Agent to interact with other services and
          tools
        </Text>
        <Button
          variant="outline"
          size="2"
          onClick={() => dispatch(push({ name: "mcp marketplace" }))}
        >
          Browse MCP Marketplace
        </Button>
      </Flex>
      <GlobalIntegrations
        globalIntegrations={globalIntegrations}
        handleIntegrationShowUp={handleIntegrationShowUp}
      />
      <ProjectIntegrations
        groupedProjectIntegrations={groupedProjectIntegrations}
        handleIntegrationShowUp={handleIntegrationShowUp}
      />
      <NewIntegrations
        availableIntegrationsToConfigure={availableIntegrationsToConfigure}
        handleIntegrationShowUp={handleIntegrationShowUp}
      />
    </Flex>
  );
};
