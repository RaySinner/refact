import React from "react";

import { Button, Flex, Heading, Text } from "@radix-ui/themes";
import { PlusIcon } from "@radix-ui/react-icons";
import { ProviderCard } from "../ProviderCard/ProviderCard";

import type { ProviderListItem } from "../../../services/refact";
import { useGetConfiguredProvidersView } from "./useConfiguredProvidersView";

export type ConfiguredProvidersViewProps = {
  configuredProviders: ProviderListItem[];
  handleSetCurrentProvider: (provider: ProviderListItem) => void;
  onAddInstance: () => void;
  onDuplicateProvider: (provider: ProviderListItem) => void;
};

export const ConfiguredProvidersView: React.FC<
  ConfiguredProvidersViewProps
> = ({
  configuredProviders,
  handleSetCurrentProvider,
  onAddInstance,
  onDuplicateProvider,
}) => {
  const { sortedConfiguredProviders } = useGetConfiguredProvidersView({
    configuredProviders,
  });

  return (
    <Flex direction="column" gap="2" justify="between" height="100%">
      <Flex direction="column" gap="2">
        <Flex justify="between" align="start" gap="3">
          <Flex direction="column" gap="1">
            <Heading as="h2" size="3">
              Configured Providers
            </Heading>
            <Text as="p" size="2" color="gray">
              Here you can navigate through the list of configured and available
              providers
            </Text>
          </Flex>
          <Button variant="soft" size="2" onClick={onAddInstance}>
            <PlusIcon /> Add instance
          </Button>
        </Flex>
        {sortedConfiguredProviders.map((provider, idx) => (
          <ProviderCard
            key={`${provider.name}_${idx}`}
            provider={provider}
            setCurrentProvider={handleSetCurrentProvider}
            onDuplicateProvider={onDuplicateProvider}
          />
        ))}
      </Flex>
    </Flex>
  );
};
