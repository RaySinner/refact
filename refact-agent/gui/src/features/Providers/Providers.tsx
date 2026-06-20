import React from "react";
<<<<<<< HEAD
import { Flex } from "@radix-ui/themes";
=======
>>>>>>> upstream/main

import { ScrollArea } from "../../components/ScrollArea";
import { PageWrapper } from "../../components/PageWrapper";
import { Spinner } from "../../components/Spinner";
import { ProvidersView } from "./ProvidersView";
import styles from "./Providers.module.css";

import { useGetConfiguredProvidersQuery } from "../../hooks/useProvidersQuery";

import type { Config } from "../Config/configSlice";

export type ProvidersProps = {
  backFromProviders: () => void;
  host: Config["host"];
  tabbed: Config["tabbed"];
<<<<<<< HEAD
=======
  embedded?: boolean;
>>>>>>> upstream/main
};
export const Providers: React.FC<ProvidersProps> = ({
  backFromProviders,
  host,
<<<<<<< HEAD
=======
  embedded,
>>>>>>> upstream/main
}) => {
  const { data: configuredProvidersData, isSuccess } =
    useGetConfiguredProvidersQuery();

  if (!isSuccess) return <Spinner spinning />;
<<<<<<< HEAD
  return (
    <PageWrapper
      host={host}
      style={{
        padding: 0,
        marginTop: 0,
      }}
    >
      <ScrollArea
        scrollbars="vertical"
        fullHeight
        className={styles.scrollArea}
      >
        <Flex
          direction="column"
          justify="between"
          flexGrow="1"
          style={{
            width: "inherit",
            minHeight: "100%",
          }}
        >
          <ProvidersView
            configuredProviders={configuredProvidersData.providers}
            backFromProviders={backFromProviders}
          />
        </Flex>
      </ScrollArea>
=======

  const providersView = (
    <ProvidersView
      configuredProviders={configuredProvidersData.providers}
      backFromProviders={backFromProviders}
      embedded={embedded}
    />
  );

  if (embedded) {
    return (
      <div className={styles.page}>
        <div className={styles.content}>{providersView}</div>
      </div>
    );
  }

  const content = (
    <ScrollArea scrollbars="vertical" fullHeight className={styles.scrollArea}>
      <div className={styles.content}>{providersView}</div>
    </ScrollArea>
  );

  return (
    <PageWrapper host={host} className={styles.page} noPadding>
      {content}
>>>>>>> upstream/main
    </PageWrapper>
  );
};
