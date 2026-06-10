import { FC, useCallback } from "react";
import {
  type LinkProps as RadixLinkProps,
  Link as RadixLink,
} from "@radix-ui/themes";
import classNames from "classnames";

import { useConfig, useOpenUrl } from "../../hooks";
import styles from "./Link.module.css";

interface LinkProps extends RadixLinkProps {
  href?: string;
  children?: React.ReactNode;
  className?: string;
  onClick?: React.MouseEventHandler<HTMLAnchorElement>;
}

export const Link: FC<LinkProps> = ({ onClick, ...props }) => {
  const config = useConfig();
  const openUrl = useOpenUrl();

  const href = props.href ?? "";
  const isExternalUrl =
    href.startsWith("http://") || href.startsWith("https://");

  const handleClick: React.MouseEventHandler<HTMLAnchorElement> = useCallback(
    (e) => {
      if (onClick) {
        onClick(e);
      }
      if (config.host === "jetbrains" && isExternalUrl && !e.defaultPrevented) {
        e.preventDefault();
        openUrl(href);
      }
    },
    [onClick, config.host, isExternalUrl, openUrl, href],
  );

  return (
    <RadixLink
      className={classNames(
        styles.link,
        { [styles.jetbrains]: config.host === "jetbrains" },
        props.className,
      )}
      onClick={handleClick}
      {...props}
    />
  );
};
