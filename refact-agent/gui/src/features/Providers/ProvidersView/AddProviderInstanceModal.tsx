import React, { useCallback, useEffect, useMemo, useState } from "react";
import {
  Button,
  Dialog,
  Flex,
  Select,
  Text,
  TextField,
} from "@radix-ui/themes";

import type { ProviderListItem } from "../../../services/refact";
import {
  type CreateProviderInstanceRequest,
  providersApi,
  useUpdateProviderMutation,
} from "../../../services/refact";
import { useAppDispatch } from "../../../hooks";
import {
  nextInstanceId,
  providerBaseOptions,
  providerInstanceDisplayName,
  validateProviderInstanceId,
} from "./providerInstanceUtils";

import styles from "./AddProviderInstanceModal.module.css";

export type AddProviderInstanceModalProps = {
  isOpen: boolean;
  configuredProviders: ProviderListItem[];
  initialBaseProvider: string | null;
  onOpenChange: (open: boolean) => void;
  onCreated: (provider: ProviderListItem) => void;
};

function getErrorMessage(error: unknown) {
  if (typeof error === "object" && error !== null) {
    const record = error as Record<string, unknown>;
    const data = record.data;
    if (typeof data === "object" && data !== null) {
      const dataRecord = data as Record<string, unknown>;
      if (typeof dataRecord.detail === "string") return dataRecord.detail;
      if (typeof dataRecord.error === "string") return dataRecord.error;
    }
    if (typeof data === "string") return data;
    if (typeof record.error === "string") return record.error;
    if (typeof record.message === "string") return record.message;
  }
  return "Failed to create provider instance.";
}

export const AddProviderInstanceModal: React.FC<
  AddProviderInstanceModalProps
> = ({
  isOpen,
  configuredProviders,
  initialBaseProvider,
  onOpenChange,
  onCreated,
}) => {
  const dispatch = useAppDispatch();
  const [updateProvider, { isLoading }] = useUpdateProviderMutation();
  const providerNames = useMemo(
    () => configuredProviders.map((provider) => provider.name),
    [configuredProviders],
  );
  const baseOptions = useMemo(
    () => providerBaseOptions(configuredProviders),
    [configuredProviders],
  );
  const defaultBaseProvider = useMemo(() => {
    if (
      initialBaseProvider &&
      baseOptions.some((option) => option.id === initialBaseProvider)
    ) {
      return initialBaseProvider;
    }
    return baseOptions[0]?.id ?? "";
  }, [baseOptions, initialBaseProvider]);

  const [baseProvider, setBaseProvider] = useState(defaultBaseProvider);
  const [instanceId, setInstanceId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [idTouched, setIdTouched] = useState(false);
  const [displayNameTouched, setDisplayNameTouched] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen || !defaultBaseProvider) return;
    const nextId = nextInstanceId(defaultBaseProvider, providerNames);
    setBaseProvider(defaultBaseProvider);
    setInstanceId(nextId);
    setDisplayName(providerInstanceDisplayName(defaultBaseProvider, nextId));
    setIdTouched(false);
    setDisplayNameTouched(false);
    setLocalError(null);
  }, [defaultBaseProvider, isOpen, providerNames]);

  const idValidation = useMemo(
    () => validateProviderInstanceId(instanceId, providerNames),
    [instanceId, providerNames],
  );
  const displayNameValidation = displayName.trim()
    ? null
    : "Display name is required.";
  const canSubmit =
    Boolean(baseProvider) &&
    !idValidation &&
    !displayNameValidation &&
    !isLoading;

  const handleBaseProviderChange = useCallback(
    (nextBaseProvider: string) => {
      const generatedInstanceId = nextInstanceId(
        nextBaseProvider,
        providerNames,
      );
      const nextInstanceIdValue = idTouched ? instanceId : generatedInstanceId;
      setBaseProvider(nextBaseProvider);
      if (!idTouched) setInstanceId(generatedInstanceId);
      if (!displayNameTouched) {
        setDisplayName(
          providerInstanceDisplayName(nextBaseProvider, nextInstanceIdValue),
        );
      }
      setLocalError(null);
    },
    [displayNameTouched, idTouched, instanceId, providerNames],
  );

  const handleInstanceIdChange = useCallback(
    (event: React.ChangeEvent<HTMLInputElement>) => {
      const nextId = event.target.value;
      setInstanceId(nextId);
      setIdTouched(true);
      if (!displayNameTouched) {
        setDisplayName(providerInstanceDisplayName(baseProvider, nextId));
      }
      setLocalError(null);
    },
    [baseProvider, displayNameTouched],
  );

  const handleDisplayNameChange = useCallback(
    (event: React.ChangeEvent<HTMLInputElement>) => {
      setDisplayName(event.target.value);
      setDisplayNameTouched(true);
      setLocalError(null);
    },
    [],
  );

  const handleSubmit = useCallback(async () => {
    const trimmedInstanceId = instanceId.trim();
    const trimmedDisplayName = displayName.trim();
    const validation =
      validateProviderInstanceId(trimmedInstanceId, providerNames) ??
      (trimmedDisplayName ? null : "Display name is required.");
    if (!baseProvider || validation) {
      setLocalError(validation ?? "Select a base provider.");
      return;
    }

    try {
      const settings: CreateProviderInstanceRequest = {
        base_provider: baseProvider,
        display_name: trimmedDisplayName,
        enabled: false,
      };
      await updateProvider({
        providerName: trimmedInstanceId,
        settings,
      }).unwrap();
      dispatch(
        providersApi.util.invalidateTags([
          { type: "PROVIDERS", id: "LIST" },
          { type: "PROVIDER", id: trimmedInstanceId },
          { type: "PROVIDER_MODELS", id: trimmedInstanceId },
          { type: "AVAILABLE_MODELS", id: trimmedInstanceId },
        ]),
      );
      onOpenChange(false);
      onCreated({
        name: trimmedInstanceId,
        base_provider: baseProvider,
        display_name: trimmedDisplayName,
        enabled: false,
        readonly: false,
        has_credentials: false,
        status: "not_configured",
        model_count: 0,
      });
    } catch (error) {
      setLocalError(getErrorMessage(error));
    }
  }, [
    baseProvider,
    dispatch,
    displayName,
    instanceId,
    onCreated,
    onOpenChange,
    providerNames,
    updateProvider,
  ]);

  const handleOpenChange = useCallback(
    (open: boolean) => {
      if (!open && isLoading) return;
      onOpenChange(open);
    },
    [isLoading, onOpenChange],
  );

  return (
    <Dialog.Root open={isOpen} onOpenChange={handleOpenChange}>
      <Dialog.Content className={styles.dialogContent}>
        <Dialog.Title>Add provider instance</Dialog.Title>
        <Dialog.Description size="2" color="gray">
          Create a blank provider configuration using an existing base provider.
        </Dialog.Description>

        <Flex direction="column" gap="3" mt="4">
          {baseOptions.length > 0 ? (
            <Flex direction="column" gap="1">
              <Text
                as="label"
                htmlFor="provider-instance-base"
                className={styles.fieldLabel}
              >
                Base provider
              </Text>
              <Select.Root
                value={baseProvider}
                onValueChange={handleBaseProviderChange}
                disabled={isLoading}
              >
                <Select.Trigger
                  id="provider-instance-base"
                  aria-label="Base provider"
                />
                <Select.Content position="popper">
                  {baseOptions.map((option) => (
                    <Select.Item key={option.id} value={option.id}>
                      {option.label}
                    </Select.Item>
                  ))}
                </Select.Content>
              </Select.Root>
            </Flex>
          ) : (
            <Text size="2" className={styles.errorText}>
              No user-creatable base providers are available.
            </Text>
          )}

          <Flex direction="column" gap="1">
            <Text
              as="label"
              htmlFor="provider-instance-id"
              className={styles.fieldLabel}
            >
              Instance id
            </Text>
            <TextField.Root
              id="provider-instance-id"
              value={instanceId}
              onChange={handleInstanceIdChange}
              disabled={isLoading || baseOptions.length === 0}
              placeholder="openai_2"
            />
            <Text
              size="1"
              className={idValidation ? styles.errorText : styles.helperText}
            >
              {idValidation ?? "Use this id as the model prefix."}
            </Text>
          </Flex>

          <Flex direction="column" gap="1">
            <Text
              as="label"
              htmlFor="provider-display-name"
              className={styles.fieldLabel}
            >
              Display name
            </Text>
            <TextField.Root
              id="provider-display-name"
              value={displayName}
              onChange={handleDisplayNameChange}
              disabled={isLoading || baseOptions.length === 0}
              placeholder="OpenAI 2"
            />
            {displayNameValidation && (
              <Text size="1" className={styles.errorText}>
                {displayNameValidation}
              </Text>
            )}
          </Flex>

          {localError && (
            <Text size="2" className={styles.errorText}>
              {localError}
            </Text>
          )}
        </Flex>

        <Flex gap="3" mt="4" justify="end">
          <Dialog.Close>
            <Button variant="soft" color="gray" disabled={isLoading}>
              Cancel
            </Button>
          </Dialog.Close>
          <Button onClick={() => void handleSubmit()} disabled={!canSubmit}>
            {isLoading ? "Creating..." : "Create instance"}
          </Button>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  );
};
