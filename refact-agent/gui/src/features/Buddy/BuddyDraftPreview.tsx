import React from "react";
import { Callout, Text, Flex } from "@radix-ui/themes";
import { InfoCircledIcon } from "@radix-ui/react-icons";
import { useAppSelector } from "../../hooks";
import { selectBuddySnapshot } from "./buddySlice";
import type { BuddyDraft } from "./types";

type Props = {
  draft: BuddyDraft;
};

export const BuddyDraftPreview: React.FC<Props> = ({ draft }) => {
  const name = useAppSelector(selectBuddySnapshot)?.state.identity.name ?? "";
  const titlePrefix = name ? `${name} draft` : "Draft";

  return (
    <Callout.Root color="blue" mb="2">
      <Callout.Icon>
        <InfoCircledIcon />
      </Callout.Icon>
      <Text as="div">
        <Flex direction="column" gap="1">
          <Text size="2" weight="bold">
            {titlePrefix}: {draft.title}
          </Text>
          {draft.explanation && <Text size="1">{draft.explanation}</Text>}
        </Flex>
      </Text>
    </Callout.Root>
  );
};
