import React, { useCallback, useEffect, useMemo, useState } from "react";
import {
  isCommitLink,
  isPostChatLink,
  isUserMessage,
  linksApi,
  type ChatLink,
} from "../services/refact";
import { useAppDispatch } from "./useAppDispatch";
import { useAppSelector } from "./useAppSelector";
import { useGetCapsQuery } from "./useGetCapsQuery";
import { useChatActions } from "./useChatActions";
import {
  selectAreFollowUpsEnabled,
<<<<<<< HEAD
  selectChatId,
  selectIntegration,
  selectIsStreaming,
  selectIsWaiting,
  selectMessages,
  selectModel,
  selectThreadMode,
=======
  selectIntegrationById,
  selectIsStreamingById,
  selectIsWaitingById,
  selectMessagesById,
  selectModelById,
  selectThreadModeById,
>>>>>>> upstream/main
  setIncreaseMaxTokens,
  setIntegrationData,
  setIsNewChatSuggested,
} from "../features/Chat";
<<<<<<< HEAD
=======
import { useThreadId } from "../features/Chat/Thread";
>>>>>>> upstream/main
import { DEFAULT_MODE } from "../features/Chat/Thread/types";
import { useGoToLink } from "./useGoToLink";
import { setError } from "../features/Errors/errorsSlice";
import { setInformation } from "../features/Errors/informationSlice";
import { debugIntegrations, debugRefact } from "../debugConfig";
import { isAbsolutePath } from "../utils";

export function useGetLinksFromLsp() {
  const dispatch = useAppDispatch();
<<<<<<< HEAD

  const isStreaming = useAppSelector(selectIsStreaming);
  const isWaiting = useAppSelector(selectIsWaiting);
  const messages = useAppSelector(selectMessages);
  const chatId = useAppSelector(selectChatId);
  const maybeIntegration = useAppSelector(selectIntegration);
  const threadMode = useAppSelector(selectThreadMode);
=======
  const contextId = useThreadId();

  const isStreaming = useAppSelector((state) =>
    selectIsStreamingById(state, contextId),
  );
  const isWaiting = useAppSelector((state) =>
    selectIsWaitingById(state, contextId),
  );
  const messages = useAppSelector((state) =>
    selectMessagesById(state, contextId),
  );
  const maybeIntegration = useAppSelector((state) =>
    selectIntegrationById(state, contextId),
  );
  const threadMode = useAppSelector((state) =>
    selectThreadModeById(state, contextId),
  );
>>>>>>> upstream/main
  const areFollowUpsEnabled = useAppSelector(selectAreFollowUpsEnabled);

  // TODO: add the model
  const caps = useGetCapsQuery();

<<<<<<< HEAD
  const model = useAppSelector(selectModel) || caps.data?.chat_default_model;
=======
  const model =
    useAppSelector((state) => selectModelById(state, contextId)) ||
    caps.data?.chat_default_model;
>>>>>>> upstream/main

  const unCalledTools = React.useMemo(() => {
    if (messages.length === 0) return false;
    const last = messages[messages.length - 1];
    //TODO: handle multiple tool calls in last assistant message
    if (last.role !== "assistant") return false;
    const maybeTools = last.tool_calls;
    if (maybeTools && maybeTools.length > 0) return true;
    return false;
  }, [messages]);

  const skipLinksRequest = useMemo(() => {
    const lastMessageIsUserMessage =
      messages.length > 0 && isUserMessage(messages[messages.length - 1]);
    if (!model) return true;
    if (!caps.data) return true;
    return (
      !areFollowUpsEnabled ||
      isStreaming ||
      isWaiting ||
      unCalledTools ||
      lastMessageIsUserMessage
    );
  }, [
    caps.data,
    areFollowUpsEnabled,
    isStreaming,
    isWaiting,
    messages,
    model,
    unCalledTools,
  ]);

  const linksResult = linksApi.useGetLinksForChatQuery(
    {
<<<<<<< HEAD
      chat_id: chatId,
=======
      chat_id: contextId,
>>>>>>> upstream/main
      messages,
      model: model ?? "",
      mode: threadMode ?? DEFAULT_MODE,
      current_config_file: maybeIntegration?.path,
    },
    { skip: skipLinksRequest },
  );

  useEffect(() => {
    if (linksResult.data?.new_chat_suggestion) {
      dispatch(
        setIsNewChatSuggested({
<<<<<<< HEAD
          chatId,
=======
          chatId: contextId,
>>>>>>> upstream/main
          value: linksResult.data.new_chat_suggestion,
        }),
      );
    }
<<<<<<< HEAD
  }, [dispatch, linksResult.data, chatId]);
=======
  }, [dispatch, linksResult.data, contextId]);
>>>>>>> upstream/main

  return linksResult;
}

export function useLinksFromLsp() {
  const dispatch = useAppDispatch();
<<<<<<< HEAD
  const { handleGoTo } = useGoToLink();
  const { submit, setParams } = useChatActions();

  const [applyCommit, _applyCommitResult] = linksApi.useSendCommitMutation();

  const isStreaming = useAppSelector(selectIsStreaming);
  const isWaiting = useAppSelector(selectIsWaiting);
  const messages = useAppSelector(selectMessages);
  const maybeIntegration = useAppSelector(selectIntegration);
=======
  const contextId = useThreadId();
  const { handleGoTo } = useGoToLink();
  const { submit, setParams } = useChatActions(contextId);

  const [applyCommit, _applyCommitResult] = linksApi.useSendCommitMutation();

  const isStreaming = useAppSelector((state) =>
    selectIsStreamingById(state, contextId),
  );
  const isWaiting = useAppSelector((state) =>
    selectIsWaitingById(state, contextId),
  );
  const messages = useAppSelector((state) =>
    selectMessagesById(state, contextId),
  );
  const maybeIntegration = useAppSelector((state) =>
    selectIntegrationById(state, contextId),
  );
>>>>>>> upstream/main

  const unCalledTools = React.useMemo(() => {
    if (messages.length === 0) return false;
    const last = messages[messages.length - 1];
    //TODO: handle multiple tool calls in last assistant message
    if (last.role !== "assistant") return false;
    const maybeTools = last.tool_calls;
    if (maybeTools && maybeTools.length > 0) return true;
    return false;
  }, [messages]);

  // TODO: think of how to avoid batching and this useless state
  const [pendingIntegrationGoto, setPendingIntegrationGoto] = useState<
    string | null
  >(null);

  useEffect(() => {
    if (
      maybeIntegration?.shouldIntermediatePageShowUp !== undefined &&
      pendingIntegrationGoto
    ) {
      handleGoTo({ goto: pendingIntegrationGoto });
      setPendingIntegrationGoto(null);
    }
  }, [pendingIntegrationGoto, handleGoTo, maybeIntegration]);

  const handleLinkAction = useCallback(
    (link: ChatLink) => {
      if (!("link_action" in link)) return;
      if (
        link.link_action === "goto" &&
        "link_goto" in link &&
        link.link_goto !== undefined
      ) {
        const [action, ...payloadParts] = link.link_goto.split(":");
        const payload = payloadParts.join(":");
        if (action.toLowerCase() === "settings") {
          debugIntegrations(
            `[DEBUG]: this goto is integrations one, dispatching integration data`,
          );
          if (!isAbsolutePath(payload)) {
            dispatch(
              setIntegrationData({
<<<<<<< HEAD
                name: payload,
                path: undefined,
                shouldIntermediatePageShowUp: payload !== "DEFAULT",
=======
                chatId: contextId,
                value: {
                  name: payload,
                  path: undefined,
                  shouldIntermediatePageShowUp: payload !== "DEFAULT",
                },
>>>>>>> upstream/main
              }),
            );
          } else {
            dispatch(
              setIntegrationData({
<<<<<<< HEAD
                path: payload,
                shouldIntermediatePageShowUp: false,
=======
                chatId: contextId,
                value: {
                  path: payload,
                  shouldIntermediatePageShowUp: false,
                },
>>>>>>> upstream/main
              }),
            );
          }
          setPendingIntegrationGoto(link.link_goto);
        }
        handleGoTo({
          goto: link.link_goto,
        });
        return;
      }

      if (link.link_action === "patch-all") {
        // TBD: smart links for patches
        // void applyPatches(messages).then(() => {
        //   if ("link_goto" in link) {
        //     handleGoTo({ goto: link.link_goto });
        //   }
        // });
        if ("link_goto" in link) {
          handleGoTo({ goto: link.link_goto });
        }
        return;
      }

      if (link.link_action === "follow-up") {
        void submit(link.link_text);
        return;
      }

      // TBD: It should be safe to remove this now?
      if (link.link_action === "regenerate-with-increased-context-size") {
<<<<<<< HEAD
        dispatch(setIncreaseMaxTokens(true));
=======
        dispatch(setIncreaseMaxTokens({ chatId: contextId, value: true }));
>>>>>>> upstream/main
        return;
      }

      if (isCommitLink(link)) {
        void applyCommit(link.link_payload)
          .unwrap()
          .then((res) => {
            const commits = res.commits_applied;

            if (commits.length > 0) {
              const commitInfo = commits
                .map((commit, index) => `${index + 1}: ${commit.project_name}`)
                .join("\n");
              const message = `Successfully committed: ${commits.length}\n${commitInfo}`;
              dispatch(setInformation(message));
            }

            const errors = res.error_log
              .map((err, index) => {
                return `${index + 1}: ${err.project_name}\n${
                  err.project_path
                }\n${err.error_message}`;
              })
              .join("\n");
            if (errors) {
              dispatch(setError(`Commit errors: ${errors}`));
            }
          });

        return;
      }

      if (isPostChatLink(link)) {
        dispatch(
          setIntegrationData({
<<<<<<< HEAD
            path: link.link_payload.chat_meta.current_config_file,
=======
            chatId: contextId,
            value: { path: link.link_payload.chat_meta.current_config_file },
>>>>>>> upstream/main
          }),
        );
        debugRefact(`[DEBUG]: link messages: `, link.link_payload.messages);
        const lastMsg =
          link.link_payload.messages[link.link_payload.messages.length - 1];
        if (lastMsg.role === "user") {
          const content =
            typeof lastMsg.content === "string" ? lastMsg.content : "";
          void setParams({ mode: link.link_payload.chat_meta.chat_mode }).then(
            () => {
              void submit(content);
            },
          );
        }
        return;
      }

      // eslint-disable-next-line no-console
      console.warn(`unknown action: ${JSON.stringify(link)}`);
    },
<<<<<<< HEAD
    [applyCommit, dispatch, handleGoTo, submit, setParams],
=======
    [applyCommit, contextId, dispatch, handleGoTo, submit, setParams],
>>>>>>> upstream/main
  );

  const linksResult = useGetLinksFromLsp();

  return {
    linksResult,
    handleLinkAction,
    streaming: isWaiting || isStreaming || unCalledTools,
  };
}
