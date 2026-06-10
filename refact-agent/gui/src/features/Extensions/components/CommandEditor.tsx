import React, { useState, useCallback, useEffect } from "react";
import {
  Flex,
  Button,
  Text,
  TextField,
  TextArea,
  SegmentedControl,
  Callout,
} from "@radix-ui/themes";
import {
  ArrowLeftIcon,
  MixerHorizontalIcon,
  CodeIcon,
  InfoCircledIcon,
} from "@radix-ui/react-icons";
import { skipToken } from "@reduxjs/toolkit/query";
import {
  useGetCommandQuery,
  useSaveCommandMutation,
  type CommandDetail,
} from "../../../services/refact/extensions";
import { useGetDraftQuery } from "../../../services/refact/buddy";
import { StringListEditor } from "../../Customization/components/StringListEditor";
import { Spinner } from "../../../components/Spinner";
import { BuddyDraftPreview } from "../../Buddy/BuddyDraftPreview";
import styles from "./CommandEditor.module.css";

type EditorView = "form" | "raw";

type CommandFormProps = {
  data: CommandDetail;
  onChange: (patch: Partial<CommandDetail>) => void;
  disabled: boolean;
};

const CommandForm: React.FC<CommandFormProps> = ({
  data,
  onChange,
  disabled,
}) => {
  return (
    <Flex direction="column" gap="3" className={styles.formContent}>
      <Flex direction="column" gap="1">
        <Text size="1" weight="medium">
          Name
        </Text>
        <TextField.Root size="1" value={data.name} disabled />
      </Flex>

      <Flex direction="column" gap="1">
        <Text size="1" weight="medium">
          Description
        </Text>
        <TextArea
          size="1"
          value={data.description}
          onChange={(e) => onChange({ description: e.target.value })}
          placeholder="Describe what this command does..."
          disabled={disabled}
        />
      </Flex>

      <Flex direction="column" gap="1">
        <Text size="1" weight="medium">
          Argument Hint
        </Text>
        <TextField.Root
          size="1"
          value={data.argument_hint}
          onChange={(e) => onChange({ argument_hint: e.target.value })}
          placeholder="e.g., [file_path]"
          disabled={disabled}
        />
      </Flex>

      <StringListEditor
        value={data.allowed_tools}
        onChange={(tools) => onChange({ allowed_tools: tools })}
        label="Allowed Tools"
        placeholder="Add tool..."
      />

      <Flex direction="column" gap="1">
        <Text size="1" weight="medium">
          Model (optional)
        </Text>
        <TextField.Root
          size="1"
          value={data.model ?? ""}
          onChange={(e) => onChange({ model: e.target.value || null })}
          placeholder="Leave blank to use default"
          disabled={disabled}
        />
      </Flex>

      <Flex direction="column" gap="1">
        <Text size="1" weight="medium">
          Body
        </Text>
        <Text size="1" color="gray">
          Placeholders: $ARGUMENTS, $1, $2, $3
        </Text>
        <textarea
          className={styles.bodyTextarea}
          value={data.body}
          onChange={(e) => onChange({ body: e.target.value })}
          placeholder="Markdown template with $ARGUMENTS placeholder..."
          disabled={disabled}
          spellCheck={false}
        />
      </Flex>
    </Flex>
  );
};

type CommandEditorProps = {
  name: string;
  onBack: () => void;
  draftId?: string;
};

export const CommandEditor: React.FC<CommandEditorProps> = ({
  name,
  onBack,
  draftId,
}) => {
  const { data, isLoading, error } = useGetCommandQuery({ name });
  const {
    data: draft,
    isLoading: draftLoading,
    error: draftError,
  } = useGetDraftQuery(draftId ?? skipToken);
  const [saveCommand, { isLoading: isSaving }] = useSaveCommandMutation();
  const [view, setView] = useState<EditorView>("form");
  const [localData, setLocalData] = useState<CommandDetail | null>(null);
  const [rawContent, setRawContent] = useState("");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [draftExpired, setDraftExpired] = useState(false);

  useEffect(() => {
    if (draftError) {
      setDraftExpired(true);
    }
  }, [draftError]);

  useEffect(() => {
    if (draft && draft.kind === "command") {
      setRawContent(draft.yaml_or_json);
      setView("raw");
    }
  }, [draft]);

  useEffect(() => {
    if (data) {
      setLocalData(data);
      if (!draft || draft.kind !== "command") {
        setRawContent(data.raw_content);
      }
    }
  }, [data, draft]);

  const handleFormChange = useCallback((patch: Partial<CommandDetail>) => {
    setLocalData((prev) => (prev ? { ...prev, ...patch } : prev));
  }, []);

  const handleSave = useCallback(async () => {
    setSaveError(null);
    if (!localData) return;
    try {
      if (view === "raw") {
        await saveCommand({
          name,
          body: { raw_content: rawContent, draft_id: draftId },
        }).unwrap();
      } else {
        await saveCommand({
          name,
          body: {
            description: localData.description,
            argument_hint: localData.argument_hint,
            allowed_tools: localData.allowed_tools,
            model: localData.model,
            body: localData.body,
            draft_id: draftId,
          },
        }).unwrap();
      }
    } catch (e) {
      setSaveError(e instanceof Error ? e.message : String(e));
    }
  }, [name, view, localData, rawContent, saveCommand, draftId]);

  if (isLoading || draftLoading) return <Spinner spinning />;
  if (!localData) {
    return (
      <Callout.Root color="red">
        <Callout.Icon>
          <InfoCircledIcon />
        </Callout.Icon>
        <Callout.Text>
          {error !== undefined ? "Failed to load command" : "Loading..."}
        </Callout.Text>
      </Callout.Root>
    );
  }

  if (draft && draft.kind !== "command") {
    return (
      <Callout.Root color="red">
        <Callout.Icon>
          <InfoCircledIcon />
        </Callout.Icon>
        <Callout.Text>Draft kind mismatch: expected command draft</Callout.Text>
      </Callout.Root>
    );
  }

  const isReadOnly = localData.source.startsWith("plugin:");

  return (
    <Flex direction="column" gap="2" className={styles.editor}>
      <Button
        variant="ghost"
        size="1"
        onClick={onBack}
        className={styles.backButton}
      >
        <ArrowLeftIcon /> Back to list
      </Button>

      {draftExpired && (
        <Callout.Root color="orange">
          <Callout.Icon>
            <InfoCircledIcon />
          </Callout.Icon>
          <Callout.Text>Draft expired</Callout.Text>
        </Callout.Root>
      )}

      {draft && <BuddyDraftPreview draft={draft} />}

      {isReadOnly && (
        <Callout.Root color="blue">
          <Callout.Icon>
            <InfoCircledIcon />
          </Callout.Icon>
          <Callout.Text>
            This item is from an installed plugin and cannot be edited.
          </Callout.Text>
        </Callout.Root>
      )}

      <Flex justify="between" align="center" gap="2" wrap="wrap">
        <Text size="2" weight="bold">
          {name}
        </Text>
        <Flex gap="1" align="center">
          <SegmentedControl.Root
            size="1"
            value={view}
            onValueChange={(v) => setView(v as EditorView)}
          >
            <SegmentedControl.Item value="form">
              <MixerHorizontalIcon width={12} height={12} />
            </SegmentedControl.Item>
            <SegmentedControl.Item value="raw">
              <CodeIcon width={12} height={12} />
            </SegmentedControl.Item>
          </SegmentedControl.Root>
          {!isReadOnly && (
            <Button
              size="1"
              onClick={() => void handleSave()}
              disabled={isSaving}
            >
              {isSaving ? "..." : "Save"}
            </Button>
          )}
        </Flex>
      </Flex>

      {saveError && (
        <Text size="1" color="red">
          {saveError}
        </Text>
      )}

      {view === "form" ? (
        <CommandForm
          data={localData}
          onChange={handleFormChange}
          disabled={isReadOnly}
        />
      ) : (
        <textarea
          className={styles.rawTextarea}
          value={rawContent}
          onChange={(e) => setRawContent(e.target.value)}
          disabled={isReadOnly}
          spellCheck={false}
        />
      )}
    </Flex>
  );
};
