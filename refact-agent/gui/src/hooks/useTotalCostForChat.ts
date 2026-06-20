<<<<<<< HEAD
import { selectMessages } from "../features/Chat";
=======
import { selectMessagesById, useThreadId } from "../features/Chat/Thread";
>>>>>>> upstream/main
import {
  getTotalTokenMeteringForMessages,
  getTotalUsdMeteringForMessages,
} from "../utils/getMetering";
import { useAppSelector } from "./useAppSelector";

export const useTotalTokenMeteringForChat = () => {
<<<<<<< HEAD
  const messages = useAppSelector(selectMessages);
=======
  const chatId = useThreadId();
  const messages = useAppSelector((state) => selectMessagesById(state, chatId));
>>>>>>> upstream/main
  return getTotalTokenMeteringForMessages(messages);
};

export const useTotalUsdForChat = () => {
<<<<<<< HEAD
  const messages = useAppSelector(selectMessages);
=======
  const chatId = useThreadId();
  const messages = useAppSelector((state) => selectMessagesById(state, chatId));
>>>>>>> upstream/main
  return getTotalUsdMeteringForMessages(messages);
};
