import React from "react";
<<<<<<< HEAD
import styles from "./Tasks.module.css";
=======
import { StatusDot } from "../../components/ui";
>>>>>>> upstream/main

interface AgentStatusDotProps {
  status: "doing" | "done" | "failed";
  size?: "small" | "medium";
}

export const AgentStatusDot: React.FC<AgentStatusDotProps> = ({
  status,
  size = "medium",
}) => {
<<<<<<< HEAD
  const sizeClass =
    size === "small" ? styles.agentDotSmall : styles.agentDotMedium;
  const statusClass =
    status === "doing"
      ? styles.agentDotDoing
      : status === "done"
        ? styles.agentDotDone
        : styles.agentDotFailed;

  return <div className={`${sizeClass} ${statusClass}`} />;
=======
  const dotStatus =
    status === "doing" ? "running" : status === "done" ? "success" : "error";

  return (
    <StatusDot
      status={dotStatus}
      size={size === "small" ? "small" : "medium"}
      pulse={status !== "failed"}
    />
  );
>>>>>>> upstream/main
};
