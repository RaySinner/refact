import React, { useState, useCallback } from "react";
import {
  Flex,
  Button,
  Dialog,
  TextField,
  Text,
  SegmentedControl,
  Badge,
} from "@radix-ui/themes";
import { GlobeIcon, FileIcon } from "@radix-ui/react-icons";
import {
  useCreateSkillMutation,
  useCreateCommandMutation,
} from "../../../services/refact/extensions";

type ItemType = "skill" | "command";

type CreateItemDialogProps = {
  type: ItemType;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (name: string) => void;
  hasProjectRoot: boolean;
};

function validateName(name: string): string | null {
  if (!name.trim()) return "Name is required";
  if (/[\s/.]/.test(name))
    return "Name must not contain spaces, slashes, or dots";
  return null;
}

export const CreateItemDialog: React.FC<CreateItemDialogProps> = ({
  type,
  open,
  onOpenChange,
  onCreated,
  hasProjectRoot,
}) => {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [scope, setScope] = useState<"global" | "local">(
    hasProjectRoot ? "local" : "global",
  );
  const [error, setError] = useState<string | null>(null);

  const [createSkill, { isLoading: isCreatingSkill }] =
    useCreateSkillMutation();
  const [createCommand, { isLoading: isCreatingCommand }] =
    useCreateCommandMutation();
  const isLoading = isCreatingSkill || isCreatingCommand;

  React.useEffect(() => {
    setScope(hasProjectRoot ? "local" : "global");
  }, [hasProjectRoot]);

  React.useEffect(() => {
    if (open) {
      setName("");
      setDescription("");
      setError(null);
    }
  }, [open]);

  const handleCreate = useCallback(async () => {
    setError(null);
    const validationError = validateName(name);
    if (validationError) {
      setError(validationError);
      return;
    }
    try {
      if (type === "skill") {
        await createSkill({ name, scope, description, body: "" }).unwrap();
      } else {
        await createCommand({ name, scope, description }).unwrap();
      }
      onOpenChange(false);
      onCreated(name);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [
    type,
    name,
    scope,
    description,
    createSkill,
    createCommand,
    onOpenChange,
    onCreated,
  ]);

  const title = type === "skill" ? "Create Skill" : "Create Command";

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Content style={{ maxWidth: 400 }}>
        <Dialog.Title>{title}</Dialog.Title>
        <Flex direction="column" gap="3">
          <Flex direction="column" gap="1">
            <Text size="1">Name</Text>
            <TextField.Root
              placeholder="my_skill"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </Flex>
          <Flex direction="column" gap="1">
            <Text size="1">Description (optional)</Text>
            <TextField.Root
              placeholder="Brief description"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </Flex>
          <Flex direction="column" gap="1">
            <Text size="1">Save to:</Text>
            {hasProjectRoot ? (
              <SegmentedControl.Root
                size="1"
                value={scope}
                onValueChange={(v) => setScope(v as "global" | "local")}
              >
                <SegmentedControl.Item value="global">
                  <Flex align="center" gap="1">
                    <GlobeIcon width={12} height={12} />
                    Global
                  </Flex>
                </SegmentedControl.Item>
                <SegmentedControl.Item value="local">
                  <Flex align="center" gap="1">
                    <FileIcon width={12} height={12} />
                    Project
                  </Flex>
                </SegmentedControl.Item>
              </SegmentedControl.Root>
            ) : (
              <Badge size="1" color="blue" variant="soft">
                <Flex align="center" gap="1">
                  <GlobeIcon width={10} height={10} />
                  Global only (no project open)
                </Flex>
              </Badge>
            )}
          </Flex>
          {error && (
            <Text size="2" color="red">
              {error}
            </Text>
          )}
        </Flex>
        <Flex gap="3" mt="4" justify="end">
          <Dialog.Close>
            <Button variant="soft" color="gray">
              Cancel
            </Button>
          </Dialog.Close>
          <Button onClick={() => void handleCreate()} disabled={isLoading}>
            {isLoading ? "Creating..." : "Create"}
          </Button>
        </Flex>
      </Dialog.Content>
    </Dialog.Root>
  );
};
