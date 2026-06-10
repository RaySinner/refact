import { useCallback, useMemo } from "react";
import { useCapsForToolUse } from "./useCapsForToolUse";
import { useAppSelector } from "./useAppSelector";
import {
  selectChatId,
  selectIsStreaming,
  selectIsWaiting,
  selectThreadBoostReasoning,
  selectModel,
  setBoostReasoning,
} from "../features/Chat";
import { useAppDispatch } from "./useAppDispatch";

export function useThinking() {
  const dispatch = useAppDispatch();

  const isStreaming = useAppSelector(selectIsStreaming);
  const isWaiting = useAppSelector(selectIsWaiting);
  const chatId = useAppSelector(selectChatId);
  const threadModel = useAppSelector(selectModel);

  const isBoostReasoningEnabled = useAppSelector(selectThreadBoostReasoning);

  const caps = useCapsForToolUse();

  const currentModel = threadModel || caps.currentModel;

  const supportsBoostReasoning = useMemo(() => {
    const models = caps.data?.chat_models;
    const item = models?.[currentModel];
    if (!item) return false;
    return (
      !!item.reasoning_effort_options?.length ||
      !!item.supports_thinking_budget ||
      !!item.supports_adaptive_thinking_budget
    );
  }, [caps.data?.chat_models, currentModel]);

  const shouldBeDisabled = useMemo(() => {
    return !supportsBoostReasoning || isStreaming || isWaiting;
  }, [supportsBoostReasoning, isStreaming, isWaiting]);

  const noteText = useMemo(() => {
    if (!supportsBoostReasoning)
      return `Note: ${currentModel} doesn't support thinking`;
    if (isStreaming || isWaiting)
      return `Note: you can't ${
        isBoostReasoningEnabled ? "disable" : "enable"
      } reasoning while stream is in process`;
  }, [
    supportsBoostReasoning,
    isStreaming,
    isWaiting,
    isBoostReasoningEnabled,
    currentModel,
  ]);

  const handleReasoningChange = useCallback(
    (event: React.MouseEvent<HTMLButtonElement>, checked: boolean) => {
      event.stopPropagation();
      event.preventDefault();
      dispatch(setBoostReasoning({ chatId, value: checked }));
    },
    [dispatch, chatId],
  );

  return {
    handleReasoningChange,
    shouldBeDisabled,
    noteText,
    areCapsInitialized: !caps.uninitialized,
    supportsBoostReasoning,
  };
}
