import React from "react";
import { Button, Flex, Heading } from "@radix-ui/themes";
import { CopyIcon } from "@radix-ui/react-icons";

import { ProviderForm } from "../ProviderForm";

import { getProviderName } from "../getProviderName";

import type { ProviderListItem } from "../../../services/refact";
import { DeletePopover } from "../../../components/DeletePopover";
import { useDeleteProviderMutation } from "../../../hooks/useProvidersQuery";
import { useAppDispatch } from "../../../hooks";
import { setInformation } from "../../Errors/informationSlice";
import { providersApi } from "../../../services/refact";

export type ProviderPreviewProps = {
  configuredProviders: ProviderListItem[];
  currentProvider: ProviderListItem;
  handleSetCurrentProvider: (provider: ProviderListItem | null) => void;
  onDuplicateProvider?: (provider: ProviderListItem) => void;
};

export const ProviderPreview: React.FC<ProviderPreviewProps> = ({
  currentProvider,
  handleSetCurrentProvider,
  onDuplicateProvider,
}) => {
  const dispatch = useAppDispatch();
  const [deleteProvider, { isLoading: isDeletingProvider }] =
    useDeleteProviderMutation();

  const handleDeleteProvider = async (providerName: string) => {
    const response = await deleteProvider(providerName);
    if (response.error) return;
    dispatch(
      setInformation(
        `${getProviderName(
          currentProvider,
        )}'s Provider configuration was deleted successfully`,
      ),
    );
    dispatch(providersApi.util.resetApiState());
    handleSetCurrentProvider(null);
  };

  return (
    <Flex direction="column" align="start" minHeight="100%">
      <Flex justify="between" align="center" width="100%" mb="4">
        <Heading as="h2" size="3">
          {getProviderName(currentProvider)} Configuration
        </Heading>
        <Flex gap="2" align="center">
          {onDuplicateProvider && (
            <Button
              type="button"
              size="2"
              variant="soft"
              onClick={() => onDuplicateProvider(currentProvider)}
            >
              <CopyIcon /> Duplicate instance
            </Button>
          )}
          <DeletePopover
            itemName={getProviderName(currentProvider)}
            isDisabled={currentProvider.readonly}
            isDeleting={isDeletingProvider}
            deleteBy={currentProvider.name}
            handleDelete={(providerName: string) =>
              void handleDeleteProvider(providerName)
            }
          />
        </Flex>
      </Flex>
      <ProviderForm currentProvider={currentProvider} />
    </Flex>
  );
};
