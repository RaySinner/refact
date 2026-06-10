import React, { useState, useCallback, useEffect } from "react";
import {
  Flex,
  Button,
  Text,
  TextField,
  TextArea,
  Switch,
  Select,
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
  useGetSkillQuery,
  useSaveSkillMutation,
  type SkillDetail,
} from "../../../services/refact/extensions";
import { useGetDraftQuery } from "../../../services/refact/buddy";
import { StringListEditor } from "../../Customization/components/StringListEditor";
import { Spinner } from "../../../components/Spinner";
import { BuddyDraftPreview } from "../../Buddy/BuddyDraftPreview";
import styles from "./SkillEditor.module.css";

type EditorView = "form" | "raw";

type SkillFormProps = {
  data: SkillDetail;
  onChange: (patch: Partial<SkillDetail>) => void;
  disabled: boolean;
};

const SkillForm: React.FC<SkillFormProps> = ({ data, onChange, disabled }) => {
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
          placeholder="Describe what this skill does..."
          disabled={disabled}
        />
      </Flex>

      <Flex gap="4" wrap="wrap">
        <Flex align="center" gap="2">
          <Switch
            size="1"
            checked={data.user_invocable}
            onCheckedChange={(checked) => onChange({ user_invocable: checked })}
            disabled={disabled}
          />
          <Text size="1">User Invocable</Text>
        </Flex>
        <Flex align="center" gap="2">
          <Switch
            size="1"
            checked={data.disable_model_invocation}
            onCheckedChange={(checked) =>
              onChange({ disable_model_invocation: checked })
            }
            disabled={disabled}
          />
          <Text size="1">Disable Model Invocation</Text>
        </Flex>
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
          Context
        </Text>
        <Select.Root
          value={data.context ?? "none"}
          onValueChange={(v) => onChange({ context: v === "none" ? null : v })}
          disabled={disabled}
          size="1"
        >
          <Select.Trigger style={{ width: "100%" }} />
          <Select.Content>
            <Select.Item value="none">None</Select.Item>
            <Select.Item value="fork">Fork (run in subagent)</Select.Item>
          </Select.Content>
        </Select.Root>
      </Flex>

      {data.context === "fork" && (
        <Flex direction="column" gap="1">
          <Text size="1" weight="medium">
            Agent (optional)
          </Text>
          <TextField.Root
            size="1"
            value={data.agent ?? ""}
            onChange={(e) => onChange({ agent: e.target.value || null })}
            placeholder="subagent"
            disabled={disabled}
          />
        </Flex>
      )}

      <Flex direction="column" gap="1">
        <Text size="1" weight="medium">
          Body
        </Text>
        <textarea
          className={styles.bodyTextarea}
          value={data.body}
          onChange={(e) => onChange({ body: e.target.value })}
          placeholder="Markdown content for the skill..."
          disabled={disabled}
          spellCheck={false}
        />
      </Flex>
    </Flex>
  );
};

type SkillEditorProps = {
  name: string;
  onBack: () => void;
  draftId?: string;
};

export const SkillEditor: React.FC<SkillEditorProps> = ({
  name,
  onBack,
  draftId,
}) => {
  const { data, isLoading, error } = useGetSkillQuery({ name });
  const {
    data: draft,
    isLoading: draftLoading,
    error: draftError,
  } = useGetDraftQuery(draftId ?? skipToken);
  const [saveSkill, { isLoading: isSaving }] = useSaveSkillMutation();
  const [view, setView] = useState<EditorView>("form");
  const [localData, setLocalData] = useState<SkillDetail | null>(null);
  const [rawContent, setRawContent] = useState("");
  const [saveError, setSaveError] = useState<string | null>(null);
  const [draftExpired, setDraftExpired] = useState(false);

  useEffect(() => {
    if (draftError) {
      setDraftExpired(true);
    }
  }, [draftError]);

  useEffect(() => {
    if (draft && draft.kind === "skill") {
      setRawContent(draft.yaml_or_json);
      setView("raw");
    }
  }, [draft]);

  useEffect(() => {
    if (data) {
      setLocalData(data);
      if (!draft || draft.kind !== "skill") {
        setRawContent(data.raw_content);
      }
    }
  }, [data, draft]);

  const handleFormChange = useCallback((patch: Partial<SkillDetail>) => {
    setLocalData((prev) => (prev ? { ...prev, ...patch } : prev));
  }, []);

  const handleSave = useCallback(async () => {
    setSaveError(null);
    if (!localData) return;
    try {
      if (view === "raw") {
        await saveSkill({
          name,
          body: { raw_content: rawContent, draft_id: draftId },
        }).unwrap();
      } else {
        await saveSkill({
          name,
          body: {
            description: localData.description,
            user_invocable: localData.user_invocable,
            disable_model_invocation: localData.disable_model_invocation,
            argument_hint: localData.argument_hint,
            allowed_tools: localData.allowed_tools,
            model: localData.model,
            context: localData.context,
            agent: localData.agent,
            body: localData.body,
            draft_id: draftId,
          },
        }).unwrap();
      }
    } catch (e) {
      setSaveError(e instanceof Error ? e.message : String(e));
    }
  }, [name, view, localData, rawContent, saveSkill, draftId]);

  if (isLoading || draftLoading) return <Spinner spinning />;
  if (!localData) {
    return (
      <Callout.Root color="red">
        <Callout.Icon>
          <InfoCircledIcon />
        </Callout.Icon>
        <Callout.Text>
          {error !== undefined ? "Failed to load skill" : "Loading..."}
        </Callout.Text>
      </Callout.Root>
    );
  }

  if (draft && draft.kind !== "skill") {
    return (
      <Callout.Root color="red">
        <Callout.Icon>
          <InfoCircledIcon />
        </Callout.Icon>
        <Callout.Text>Draft kind mismatch: expected skill draft</Callout.Text>
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
        <SkillForm
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
