import React, { forwardRef } from "react";
import { IconButton, Button, Flex, HoverCard, Text } from "@radix-ui/themes";
import {
  PaperPlaneIcon,
  ExitIcon,
  Cross1Icon,
  FileTextIcon,
} from "@radix-ui/react-icons";
import classNames from "classnames";
import styles from "./button.module.css";
import iconStyles from "./iconButton.module.css";
import { PuzzleIcon } from "../../images/PuzzleIcon";

type IconButtonProps = React.ComponentProps<typeof IconButton>;
type ButtonProps = React.ComponentProps<typeof Button>;

export const PaperPlaneButton: React.FC<IconButtonProps> = (props) => (
  <IconButton variant="ghost" {...props}>
    <PaperPlaneIcon />
  </IconButton>
);
type PlainButtonProps = React.ButtonHTMLAttributes<HTMLButtonElement>;

export const AgentIntegrationsButton = forwardRef<
  HTMLButtonElement,
  PlainButtonProps
>((props, ref) => (
  <HoverCard.Root>
    <HoverCard.Trigger>
      <button
        type="button"
        className={iconStyles.iconButton}
        aria-label="Set up Agent Integrations"
        {...props}
        ref={ref}
      >
        <PuzzleIcon />
      </button>
    </HoverCard.Trigger>
    <HoverCard.Content size="1" side="top">
      <Text as="p" size="2">
        Set up Agent Integrations
      </Text>
    </HoverCard.Content>
  </HoverCard.Root>
));

AgentIntegrationsButton.displayName = "AgentIntegrationsButton";

export const ThreadHistoryButton: React.FC<IconButtonProps> = (props) => (
  <IconButton variant="ghost" {...props}>
    <FileTextIcon />
  </IconButton>
);

export const BackToSideBarButton: React.FC<PlainButtonProps> = (props) => (
  <HoverCard.Root>
    <HoverCard.Trigger>
      <button
        type="button"
        className={iconStyles.iconButton}
        aria-label="Return to sidebar"
        {...props}
      >
        <ExitIcon style={{ transform: "scaleX(-1)" }} />
      </button>
    </HoverCard.Trigger>
    <HoverCard.Content size="1" side="top">
      <Text as="p" size="2">
        Return to sidebar
      </Text>
    </HoverCard.Content>
  </HoverCard.Root>
);

export const CloseButton: React.FC<
  IconButtonProps & { iconSize?: number | string }
> = ({ iconSize, ...props }) => (
  <IconButton variant="ghost" {...props}>
    <Cross1Icon width={iconSize} height={iconSize} />
  </IconButton>
);

export const RightButton: React.FC<ButtonProps & { className?: string }> = (
  props,
) => {
  return (
    <Button
      size="1"
      variant="surface"
      {...props}
      className={classNames(styles.rightButton, props.className)}
    />
  );
};

type FlexProps = React.ComponentProps<typeof Flex>;

export const RightButtonGroup: React.FC<React.PropsWithChildren & FlexProps> = (
  props,
) => {
  return (
    <Flex
      {...props}
      gap="1"
      className={classNames(styles.rightButtonGroup, props.className)}
    />
  );
};
