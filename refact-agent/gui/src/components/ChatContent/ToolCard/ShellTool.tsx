import React from "react";

import { ToolCall } from "../../../services/refact/types";
import { ExecToolCard } from "./ExecToolCard";

interface ShellToolProps {
  toolCall: ToolCall;
}

export const ShellTool: React.FC<ShellToolProps> = ({ toolCall }) => {
  return <ExecToolCard toolCall={toolCall} toolName="shell" />;
};

export default ShellTool;
