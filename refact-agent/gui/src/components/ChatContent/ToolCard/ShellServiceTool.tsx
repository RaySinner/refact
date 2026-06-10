import React from "react";

import { ToolCall } from "../../../services/refact/types";
import { ExecToolCard } from "./ExecToolCard";

interface ShellServiceToolProps {
  toolCall: ToolCall;
}

export const ShellServiceTool: React.FC<ShellServiceToolProps> = ({
  toolCall,
}) => {
  return <ExecToolCard toolCall={toolCall} toolName="shell_service" />;
};

export default ShellServiceTool;
