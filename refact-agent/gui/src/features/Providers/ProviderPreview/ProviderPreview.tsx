import React from "react";
<<<<<<< HEAD
import { Button, Flex, Heading } from "@radix-ui/themes";
import { CopyIcon } from "@radix-ui/react-icons";

=======
import { ArrowLeft, Copy } from "lucide-react";

import { Button } from "../../../components/ui";
>>>>>>> upstream/main
import { ProviderForm } from "../ProviderForm";

import { getProviderName } from "../getProviderName";

import type { ProviderListItem } from "../../../services/refact";
import { DeletePopover } from "../../../components/DeletePopover";
import { useDeleteProviderMutation } from "../../../hooks/useProvidersQuery";
import { useAppDispatch } from "../../../hooks";
import { setInformation } from "../../Errors/informationSlice";
import { providersApi } from "../../../services/refact";
<<<<<<< HEAD
=======
import { SettingsSection } from "../../Settings/SettingsSection";
import styles from "./ProviderPreview.module.css";
>>>>>>> upstream/main

export type ProviderPreviewProps = {
  configuredProviders: ProviderListItem[];
  currentProvider: ProviderListItem;
  handleSetCurrentProvider: (provider: ProviderListItem | null) => void;
  onDuplicateProvider?: (provider: ProviderListItem) => void;
<<<<<<< HEAD
=======
  onBack?: () => void;
  sectioned?: boolean;
>>>>>>> upstream/main
};

export const ProviderPreview: React.FC<ProviderPreviewProps> = ({
  currentProvider,
  handleSetCurrentProvider,
  onDuplicateProvider,
<<<<<<< HEAD
=======
  onBack,
  sectioned = false,
>>>>>>> upstream/main
}) => {
  const dispatch = useAppDispatch();
  const [deleteProvider, { isLoading: isDeletingProvider }] =
    useDeleteProviderMutation();
<<<<<<< HEAD

  const handleDeleteProvider = async (providerName: string) => {
    const response = await deleteProvider(providerName);
    if (response.error) return;
    dispatch(
      setInformation(
        `${getProviderName(
          currentProvider,
        )}'s Provider configuration was deleted successfully`,
=======
  const providerName = getProviderName(currentProvider);

  const handleDeleteProvider = async (providerNameToDelete: string) => {
    const response = await deleteProvider(providerNameToDelete);
    if (response.error) return;
    dispatch(
      setInformation(
        `${providerName}'s Provider configuration was deleted successfully`,
>>>>>>> upstream/main
      ),
    );
    dispatch(providersApi.util.resetApiState());
    handleSetCurrentProvider(null);
  };

<<<<<<< HEAD
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
=======
  const actions = (
    <div className={styles.actions}>
      {onDuplicateProvider ? (
        <Button
          type="button"
          size="md"
          variant="soft"
          leftIcon={Copy}
          onClick={() => onDuplicateProvider(currentProvider)}
        >
          Duplicate instance
        </Button>
      ) : null}
      <DeletePopover
        itemName={providerName}
        isDisabled={currentProvider.readonly}
        isDeleting={isDeletingProvider}
        deleteBy={currentProvider.name}
        handleDelete={(providerNameToDelete: string) =>
          void handleDeleteProvider(providerNameToDelete)
        }
      />
    </div>
  );

  if (sectioned) {
    return (
      <SettingsSection
        title={`${providerName} Configuration`}
        description="Edit credentials, model availability, and default model routing for this provider instance."
        width="wide"
        subNav={
          onBack ? (
            <Button variant="ghost" leftIcon={ArrowLeft} onClick={onBack}>
              Back
            </Button>
          ) : null
        }
        actions={actions}
      >
        <ProviderForm currentProvider={currentProvider} />
      </SettingsSection>
    );
  }

  return (
    <section className={styles.preview}>
      <div className={styles.headerRow}>
        <h2 className={styles.title}>{providerName} Configuration</h2>
        {actions}
      </div>
      <ProviderForm currentProvider={currentProvider} />
    </section>
>>>>>>> upstream/main
  );
};
