import type { HistoryTreeNode } from "../../../History/historySlice";

export type TrailDot = {
  id: string;
  chatId: string;
  type: "user" | "assistant" | "subagent" | "fork" | "active" | "completed";
  label?: string;
  depth: number;
  hasBranch: boolean;
};

export function buildDotTrail(node: HistoryTreeNode, maxDots = 8): TrailDot[] {
  const dots: TrailDot[] = [];

  function addDot(n: HistoryTreeNode, depth: number) {
    if (dots.length >= maxDots) return;
    const hasBranch = n.bubbleChildren.length > 0;

    dots.push({
      id: n.id,
      chatId: n.id,
      type: "subagent",
      label: n.link_type ?? undefined,
      depth,
      hasBranch,
    });

    for (const child of n.bubbleChildren) {
      if (dots.length >= maxDots) break;
      addDot(child, depth + 1);
    }
  }

  for (const child of node.bubbleChildren) {
    if (dots.length >= maxDots) break;
    addDot(child, 0);
  }
  return dots;
}
