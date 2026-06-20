import { useCallback, useMemo, type FC } from "react";
import classNames from "classnames";
<<<<<<< HEAD
import {
  Badge,
  Card,
  DropdownMenu,
  Flex,
  IconButton,
  Text,
  Tooltip,
} from "@radix-ui/themes";
import { DotsVerticalIcon } from "@radix-ui/react-icons";

=======
import { MoreVertical } from "lucide-react";

import {
  Badge,
  IconButton,
  Menu,
  Surface,
  Tooltip,
} from "../../../../components/ui";
>>>>>>> upstream/main
import { ModelCardPopup } from "./components/ModelCardPopup";
import {
  CapabilityIcons,
  ContextWindowIcon,
  ModelDetailIcon,
  PricingIcon,
} from "./components/CapabilityIcons";
import { useModelDialogState } from "./hooks/useModelDialogState";

import type { ModelType } from "../../../../services/refact";
import type { UiModel } from "./utils/groupModelsWithPricing";

import styles from "./ModelCard.module.css";
import { useEventsBusForIDE } from "../../../../hooks";

export type ModelCardProps = {
  model: UiModel;
  providerName: string;
  modelType: ModelType;
  isReadonlyProvider: boolean;
  currentModelNames: string[];
};

<<<<<<< HEAD
/**
 * Card component that displays model information and provides access to model settings
 */
=======
>>>>>>> upstream/main
export const ModelCard: FC<ModelCardProps> = ({
  model,
  modelType,
  providerName,
  isReadonlyProvider,
  currentModelNames,
}) => {
  const { enabled, name, removable, user_configured } = model;
  const {
    isOpen: dialogOpen,
    setIsOpen: setDialogOpen,
    dropdownOpen,
    setDropdownOpen,
    openDialogSafely,
    isSavingModel,
    handleToggleModelEnabledState,
    handleRemoveModel,
    handleResetModel,
    handleSaveModel,
    handleUpdateModel,
  } = useModelDialogState({
    initialState: false,
    modelType,
    providerName,
  });

  const { setCodeCompletionModel } = useEventsBusForIDE();

  const handleSetCompletionModelForIDE = useCallback(() => {
    const formattedModelName = `${providerName}/${model.name}`;
    setCodeCompletionModel(formattedModelName);
  }, [model, providerName, setCodeCompletionModel]);

  const dropdownOptions = useMemo(() => {
    const shouldOptionsBeDisabled = isReadonlyProvider || isSavingModel;
    return [
      {
        label: "Edit model's settings",
        onClick: openDialogSafely,
        visible: !shouldOptionsBeDisabled,
      },
      {
        label: enabled ? "Disable model" : "Enable model",
        onClick: () => void handleToggleModelEnabledState(model),
        visible: !shouldOptionsBeDisabled,
      },
      {
        label: "Reset model",
        onClick: () => void handleResetModel(model),
        visible: !removable && user_configured,
      },
      {
        label: "Remove model",
        onClick: () => void handleRemoveModel({ model }),
        visible: removable,
      },
      {
        label: "Use as completion model in IDE",
        onClick: handleSetCompletionModelForIDE,
        visible: modelType === "completion",
      },
    ];
  }, [
    isReadonlyProvider,
    isSavingModel,
    enabled,
    removable,
    user_configured,
    model,
    modelType,
    openDialogSafely,
    handleToggleModelEnabledState,
    handleResetModel,
    handleRemoveModel,
    handleSetCompletionModelForIDE,
  ]);

<<<<<<< HEAD
  const dropdownOptionsCount = useMemo(() => {
    return dropdownOptions.filter((option) => option.visible).length;
  }, [dropdownOptions]);

  return (
    <Card className={classNames({ [styles.disabledCard]: isSavingModel })}>
      {dialogOpen && (
=======
  const visibleDropdownOptions = useMemo(() => {
    return dropdownOptions.filter((option) => option.visible);
  }, [dropdownOptions]);

  return (
    <Surface
      variant="glass"
      animated="rise"
      className={classNames(styles.modelCard, {
        [styles.disabledCard]: isSavingModel,
      })}
    >
      {dialogOpen ? (
>>>>>>> upstream/main
        <ModelCardPopup
          minifiedModel={model}
          isOpen={dialogOpen}
          isSaving={isSavingModel}
          setIsOpen={setDialogOpen}
          modelName={name}
          modelType={modelType}
          providerName={providerName}
          onSave={handleSaveModel}
          onUpdate={handleUpdateModel}
          isRemovable={removable}
          currentModelNames={currentModelNames}
        />
<<<<<<< HEAD
      )}

      <Flex align="center" justify="between">
        <Flex direction="column" gap="1" style={{ flex: 1, minWidth: 0 }}>
          <Flex gap="2" align="center" wrap="wrap">
            <Text as="span" size="2" weight="medium">
              {name}
            </Text>
            <Badge size="1" color={enabled ? "green" : "gray"}>
              {enabled ? "Active" : "Inactive"}
            </Badge>
          </Flex>

          <Flex gap="2" align="center" wrap="wrap">
            {model.pricingLabel && (
              <Tooltip content="Price per 1M tokens (prompt/output)">
                <ModelDetailIcon icon={<PricingIcon />}>
                  {model.pricingLabel}
                </ModelDetailIcon>
              </Tooltip>
            )}
            {model.nCtxLabel && (
              <Tooltip
                content={`Context window: ${model.nCtx?.toLocaleString()} tokens`}
              >
                <ModelDetailIcon icon={<ContextWindowIcon />}>
                  {model.nCtxLabel}
                </ModelDetailIcon>
              </Tooltip>
            )}
            {model.capabilities && (
              <CapabilityIcons capabilities={model.capabilities} size="1" />
            )}
          </Flex>
        </Flex>

        {dropdownOptionsCount > 0 && (
          <DropdownMenu.Root open={dropdownOpen} onOpenChange={setDropdownOpen}>
            <DropdownMenu.Trigger>
              <IconButton size="1" variant="outline" color="gray">
                <DotsVerticalIcon />
              </IconButton>
            </DropdownMenu.Trigger>
            <DropdownMenu.Content side="bottom" align="end" size="1">
              {dropdownOptions.map(({ label, visible, onClick }) => {
                if (!visible) return null;
                return (
                  <DropdownMenu.Item
                    key={label}
                    onClick={onClick}
                    title={label}
                  >
                    {label}
                  </DropdownMenu.Item>
                );
              })}
            </DropdownMenu.Content>
          </DropdownMenu.Root>
        )}
      </Flex>
    </Card>
=======
      ) : null}

      <div className={styles.modelHeader}>
        <div className={styles.modelCopy}>
          <div className={styles.modelTitleRow}>
            <span className={styles.modelName}>{name}</span>
            <Badge tone={enabled ? "success" : "muted"}>
              {enabled ? "Active" : "Inactive"}
            </Badge>
          </div>

          <div className={styles.modelMetaRow}>
            {model.pricingLabel ? (
              <Tooltip>
                <Tooltip.Trigger asChild>
                  <span>
                    <ModelDetailIcon icon={<PricingIcon />}>
                      {model.pricingLabel}
                    </ModelDetailIcon>
                  </span>
                </Tooltip.Trigger>
                <Tooltip.Content>
                  Price per 1M tokens (prompt/output)
                </Tooltip.Content>
              </Tooltip>
            ) : null}
            {model.nCtxLabel ? (
              <Tooltip>
                <Tooltip.Trigger asChild>
                  <span>
                    <ModelDetailIcon icon={<ContextWindowIcon />}>
                      {model.nCtxLabel}
                    </ModelDetailIcon>
                  </span>
                </Tooltip.Trigger>
                <Tooltip.Content>
                  Context window: {model.nCtx?.toLocaleString()} tokens
                </Tooltip.Content>
              </Tooltip>
            ) : null}
            {model.capabilities ? (
              <CapabilityIcons capabilities={model.capabilities} size="1" />
            ) : null}
          </div>
        </div>

        {visibleDropdownOptions.length > 0 ? (
          <Menu open={dropdownOpen} onOpenChange={setDropdownOpen}>
            <Menu.Trigger asChild>
              <IconButton
                size="sm"
                variant="ghost"
                aria-label="Model actions"
                icon={MoreVertical}
              />
            </Menu.Trigger>
            <Menu.Content side="bottom" align="end" maxWidth="260px">
              {visibleDropdownOptions.map(({ label, onClick }) => (
                <Menu.Item key={label} onClick={onClick} title={label}>
                  {label}
                </Menu.Item>
              ))}
            </Menu.Content>
          </Menu>
        ) : null}
      </div>
    </Surface>
>>>>>>> upstream/main
  );
};
