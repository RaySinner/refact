import { selectMessages } from "../features/Chat";
import {
  getTotalTokenMeteringForMessages,
  getTotalUsdMeteringForMessages,
} from "../utils/getMetering";
import { useAppSelector } from "./useAppSelector";

export const useTotalTokenMeteringForChat = () => {
  const messages = useAppSelector(selectMessages);
  return getTotalTokenMeteringForMessages(messages);
};

export const useTotalUsdForChat = () => {
  const messages = useAppSelector(selectMessages);
  return getTotalUsdMeteringForMessages(messages);
};
