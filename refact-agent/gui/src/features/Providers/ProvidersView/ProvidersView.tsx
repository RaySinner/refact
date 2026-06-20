import React, { useCallback, useState } from "react";
<<<<<<< HEAD
import { Button, Flex } from "@radix-ui/themes";
import { ArrowLeftIcon } from "@radix-ui/react-icons";

=======
import { ArrowLeft, Plus } from "lucide-react";
import classNames from "classnames";

import { Button } from "../../../components/ui";
>>>>>>> upstream/main
import { ConfiguredProvidersView } from "./ConfiguredProvidersView";
import { AddProviderInstanceModal } from "./AddProviderInstanceModal";

import type { ProviderListItem } from "../../../services/refact";
import { ProviderPreview } from "../ProviderPreview";
import {
  ErrorCallout,
  InformationCallout,
} from "../../../components/Callout/Callout";
<<<<<<< HEAD
import classNames from "classnames";
=======
>>>>>>> upstream/main
import { useAppDispatch, useAppSelector } from "../../../hooks";
import { clearError, getErrorMessage } from "../../Errors/errorsSlice";
import {
  clearInformation,
  getInformationMessage,
} from "../../Errors/informationSlice";
<<<<<<< HEAD
=======
import { SettingsSection } from "../../Settings/SettingsSection";
import { DefaultModels } from "../../DefaultModels";
>>>>>>> upstream/main

import styles from "./ProvidersView.module.css";
import { selectConfig } from "../../Config/configSlice";

export type ProvidersViewProps = {
  configuredProviders: ProviderListItem[];
  backFromProviders: () => void;
<<<<<<< HEAD
=======
  embedded?: boolean;
>>>>>>> upstream/main
};

export const ProvidersView: React.FC<ProvidersViewProps> = ({
  configuredProviders,
  backFromProviders,
<<<<<<< HEAD
}) => {
  const dispatch = useAppDispatch();

  const currentHost = useAppSelector(selectConfig).host;
=======
  embedded,
}) => {
  const dispatch = useAppDispatch();

  const currentConfig = useAppSelector(selectConfig);
  const currentHost = currentConfig.host;
>>>>>>> upstream/main
  const globalError = useAppSelector(getErrorMessage);
  const information = useAppSelector(getInformationMessage);

  const [currentProvider, setCurrentProvider] =
    useState<ProviderListItem | null>(null);
  const [instanceModalOpen, setInstanceModalOpen] = useState(false);
  const [initialBaseProvider, setInitialBaseProvider] = useState<string | null>(
    null,
  );
  const handleSetCurrentProvider = useCallback(
    (provider: ProviderListItem | null) => {
      setCurrentProvider(provider);
    },
    [],
  );

  const handleAddInstance = useCallback(() => {
    setInitialBaseProvider(null);
    setInstanceModalOpen(true);
  }, []);

  const handleDuplicateProvider = useCallback((provider: ProviderListItem) => {
    setInitialBaseProvider(provider.base_provider);
    setInstanceModalOpen(true);
  }, []);

  const handleInstanceCreated = useCallback((provider: ProviderListItem) => {
    setCurrentProvider(provider);
  }, []);

  const handleBackClick = useCallback(() => {
    if (currentProvider) {
      setCurrentProvider(null);
    } else {
      backFromProviders();
    }
  }, [currentProvider, backFromProviders]);

  return (
<<<<<<< HEAD
    <Flex px="1" direction="column" minHeight="100%" width="100%">
      {currentHost === "vscode" ? (
        <Flex gap="2" pb="3">
          <Button variant="surface" onClick={handleBackClick}>
            <ArrowLeftIcon width="16" height="16" />
            Back
          </Button>
        </Flex>
      ) : (
        <Button mr="auto" variant="outline" onClick={handleBackClick} mb="4">
          Back
        </Button>
      )}
      {!currentProvider && (
        <ConfiguredProvidersView
          configuredProviders={configuredProviders}
          handleSetCurrentProvider={handleSetCurrentProvider}
          onAddInstance={handleAddInstance}
          onDuplicateProvider={handleDuplicateProvider}
        />
      )}
      {currentProvider && (
=======
    <div className={styles.view}>
      {!currentProvider ? (
        <SettingsSection
          title="Providers"
          description="Manage model provider instances, credentials, defaults, and available models."
          width="wide"
          actions={
            <Button
              variant="soft"
              size="md"
              leftIcon={Plus}
              onClick={handleAddInstance}
            >
              Add instance
            </Button>
          }
          subNav={
            !embedded ? (
              <Button
                variant="ghost"
                leftIcon={ArrowLeft}
                onClick={handleBackClick}
              >
                Back
              </Button>
            ) : null
          }
        >
          <ConfiguredProvidersView
            configuredProviders={configuredProviders}
            handleSetCurrentProvider={handleSetCurrentProvider}
            onAddInstance={handleAddInstance}
            onDuplicateProvider={handleDuplicateProvider}
          />
          <DefaultModels
            embedded
            host={currentConfig.host}
            tabbed={currentConfig.tabbed}
            backFromDefaultModels={backFromProviders}
          />
        </SettingsSection>
      ) : null}
      {currentProvider ? (
>>>>>>> upstream/main
        <ProviderPreview
          currentProvider={currentProvider}
          configuredProviders={configuredProviders}
          handleSetCurrentProvider={handleSetCurrentProvider}
          onDuplicateProvider={handleDuplicateProvider}
<<<<<<< HEAD
        />
      )}
=======
          onBack={handleBackClick}
          sectioned
        />
      ) : null}
>>>>>>> upstream/main
      <AddProviderInstanceModal
        isOpen={instanceModalOpen}
        configuredProviders={configuredProviders}
        initialBaseProvider={initialBaseProvider}
        onOpenChange={setInstanceModalOpen}
        onCreated={handleInstanceCreated}
      />
<<<<<<< HEAD
      {information && (
=======
      {information ? (
>>>>>>> upstream/main
        <InformationCallout
          timeout={3000}
          mx="0"
          onClick={() => dispatch(clearInformation())}
          className={classNames(styles.popup, {
            [styles.popup_ide]: currentHost !== "web",
          })}
        >
          {information}
        </InformationCallout>
<<<<<<< HEAD
      )}
      {globalError && (
=======
      ) : null}
      {globalError ? (
>>>>>>> upstream/main
        <ErrorCallout
          mx="0"
          timeout={3000}
          onClick={() => dispatch(clearError())}
          className={classNames(styles.popup, {
            [styles.popup_ide]: currentHost !== "web",
          })}
        >
          {globalError}
        </ErrorCallout>
<<<<<<< HEAD
      )}
    </Flex>
=======
      ) : null}
    </div>
>>>>>>> upstream/main
  );
};
