import React, { useMemo } from "react";

import { Flex } from "@radix-ui/themes";

import { BuddyCanvas, useBuddyState } from "../../features/Buddy";

const HELLO_TEMPLATES = [
  (name: string) => `Hi! I'm ${name}. What should we build today?`,
  () => "Hello! I'm ready when you are.",
  (name: string) => `Hey, I'm ${name}. Tell me what's on your mind.`,
  () => "Hi there! Want to explore some code together?",
  (name: string) => `Hello from ${name}. Let's make something nice.`,
  (name: string) =>
    `${name} reporting for snack-driven development. What's next?`,
  () => "I brought zero opinions and one tiny pixel sword.",
  (name: string) => `${name} has entered the chat. The bugs look nervous.`,
  () => "Ask me anything. If I don't know, I'll squint professionally.",
  (name: string) => `${name} is awake, caffeinated, and probably over-indexed.`,
  () => "Ready to turn mysterious red text into slightly less mysterious text.",
  (name: string) => `${name} found a loose semicolon. It claims innocence.`,
  () => "Let's make the computer do the thing on purpose this time.",
  (name: string) => `${name} is listening. Logs feared this day would come.`,
  () => "Drop a task here. I'll poke it with a tiny stick first.",
  (name: string) => `${name} warmed up the rubber duck. Begin confession.`,
  () => "Today's forecast: scattered TODOs with a chance of refactor.",
  (name: string) => `${name} is ready to negotiate with the compiler.`,
];

const pickHello = (name: string) =>
  HELLO_TEMPLATES[Math.floor(Math.random() * HELLO_TEMPLATES.length)](name);

export const PlaceHolderText: React.FC = () => {
  const buddy = useBuddyState();
  const name = buddy.state.name.trim() || "your companion";
  const speech = useMemo(() => pickHello(name), [name]);

  return (
    <Flex
      direction="column"
      align="center"
      justify="center"
      width="100%"
      height="100%"
      minHeight="100%"
    >
      <BuddyCanvas
        state={buddy.state}
        onEvent={buddy.handleCanvasEvent}
        displaySize={220}
        speechOverride={speech}
        bubblePosition="top"
      />
    </Flex>
  );
};
