import type { EventMessage } from "../../services/refact/types";

const BUDDY_SOURCE_PREFIX = "buddy.";

export function isBuddySystemNotice(event: EventMessage): boolean {
  return (
    event.subkind === "system_notice" &&
    event.source.length > BUDDY_SOURCE_PREFIX.length &&
    event.source.startsWith(BUDDY_SOURCE_PREFIX)
  );
}
