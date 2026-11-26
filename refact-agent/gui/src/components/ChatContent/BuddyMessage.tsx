import React from "react";
import { Box } from "@radix-ui/themes";
import type { EventMessage } from "../../services/refact/types";
import { Markdown } from "../Markdown";
import { MessageWrapper } from "./MessageFooter";

export type BuddyMessageProps = {
  event: EventMessage;
  onCopyClick?: (text: string) => void;
};

export const BuddyMessage: React.FC<BuddyMessageProps> = ({
  event,
  onCopyClick,
}) => {
  return (
    <MessageWrapper>
      <Box py="2" data-testid="buddy-message" data-source={event.source}>
        <Markdown canHaveInteractiveElements={true} onCopyClick={onCopyClick}>
          {event.content}
        </Markdown>
      </Box>
    </MessageWrapper>
  );
};

BuddyMessage.displayName = "BuddyMessage";
