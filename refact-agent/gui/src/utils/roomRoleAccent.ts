import type { TeamMember, TeamMemberStatus } from "../services/refact/tasks";

/**
 * One role accent: a thin stripe colour plus a readable label colour, both as
 * CSS *variable names* rather than literal colours. Keeping the names here and
 * the values in `tokens.css` means a theme change is a token change, not a
 * logic change, and every surface that shows a room member gets the same colour
 * for the same role without re-deriving it.
 */
export interface RoomRoleAccent {
  stripe: string;
  label: string;
}

const ACCENT_SLOTS = 8;

/**
 * Named roles get a fixed slot so a role's colour never changes when other
 * roles appear on the card. The list mirrors the engine's `TeamRole` plus the
 * `planner` role of the orchestrator chat.
 */
const NAMED_ROLE_SLOTS: Record<string, number | undefined> = {
  architect: 1,
  coder: 2,
  reviewer: 3,
  researcher: 4,
  specialist: 5,
  planner: 6,
};

/** First slot free for hash-assigned (unrecognised) roles. */
const PLANNER_SLOT = 6;
const HASH_SLOT_FIRST = PLANNER_SLOT + 1;

/**
 * FNV-1a, 32-bit. Used only to give an unknown role a stable slot: the same
 * role string always lands on the same colour. Two different unknown roles may
 * collide, which is the right trade — a named role never loses its slot.
 */
export function fnv1a(input: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i += 1) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash >>> 0;
}

export function normalizeRole(role: string): string {
  return role.trim().toLowerCase();
}

/** 1-based slot for a role, stable across renders and across cards. */
export function roleAccentSlot(role: string): number {
  const normalized = normalizeRole(role);
  const named = NAMED_ROLE_SLOTS[normalized];
  if (named !== undefined) return named;
  const hashed = fnv1a(normalized);
  const hashSlots = ACCENT_SLOTS - HASH_SLOT_FIRST + 1;
  return HASH_SLOT_FIRST + (hashed % hashSlots);
}

export function roleAccent(role: string): RoomRoleAccent {
  const slot = roleAccentSlot(role);
  return {
    stripe: `var(--room-stripe-${slot})`,
    label: `var(--room-label-${slot})`,
  };
}

const STATUS_ALIASES: Record<string, TeamMemberStatus | undefined> = {
  pending: "pending",
  planned: "pending",
  idle: "pending",
  starting: "pending",
  running: "running",
  doing: "running",
  active: "running",
  done: "done",
  completed: "done",
  complete: "done",
  partial: "partial",
  "partially-done": "partial",
  failed: "failed",
  error: "failed",
  cancelled: "failed",
  canceled: "failed",
};

/**
 * Mirrors the engine's `TeamStatus::parse` and `TeamMember::typed_status`:
 * the typed `member_status` wins, and only then do we fall back to parsing the
 * legacy free-form `status` scalar. An unrecognised value is `pending`, which
 * is what the engine defaults to as well.
 */
export function memberStatusLabel(member: TeamMember): TeamMemberStatus {
  const typed = member.member_status;
  if (typed) return typed;
  const legacy = member.status?.trim().toLowerCase().replace(/_/g, "-");
  if (!legacy) return "pending";
  return STATUS_ALIASES[legacy] ?? "pending";
}

const STATUS_TEXT: Record<TeamMemberStatus, string> = {
  pending: "pending",
  running: "running",
  done: "done",
  partial: "partial",
  failed: "failed",
};

export function memberStatusText(member: TeamMember): string {
  return STATUS_TEXT[memberStatusLabel(member)];
}

const ROLE_LABELS: Record<string, string | undefined> = {
  architect: "architect",
  coder: "coder",
  reviewer: "reviewer",
  researcher: "researcher",
  specialist: "specialist",
  planner: "planner",
};

/** Short, human-readable role; unknown roles keep their raw text. */
export function roleLabel(role: string): string {
  const normalized = normalizeRole(role);
  if (ROLE_LABELS[normalized] !== undefined) return ROLE_LABELS[normalized];
  return normalized || "agent";
}

/**
 * Up to two uppercase initials for the room badge. Falls back to a dot when the
 * role has no letters at all, so the badge is never blank.
 */
export function roleInitials(role: string): string {
  const letters = normalizeRole(role)
    .replace(/[^a-z0-9]+/g, " ")
    .trim();
  if (!letters) return "·";
  const words = letters.split(" ").filter(Boolean);
  if (words.length === 1) {
    return words[0].slice(0, 2).toUpperCase();
  }
  return `${words[0][0]}${words[1][0]}`.toUpperCase();
}

export const ROOM_ACCENT_SLOTS = ACCENT_SLOTS;
