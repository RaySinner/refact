#[cfg(test)]
use std::cmp::Reverse;
#[cfg(test)]
use std::collections::{BinaryHeap, HashMap, HashSet};
#[cfg(test)]
use std::io::{Read, Seek, SeekFrom};
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use crate::buddy::actor::{
    buddy_apply, make_runtime_event, render_buddy_activity_title,
    render_buddy_runtime_event, validate_workflow_id, BuddyMutation,
};
#[cfg(test)]
use crate::buddy::voice_service::VoiceIntent;
#[cfg(test)]
use crate::buddy::autonomous_workflows::{
    autonomous_workflow_meta, is_autonomous_workflow_id, ARCHITECTURE_DRIFT_WORKFLOW_ID,
    BEHAVIOR_LEARNER_WORKFLOW_ID, DEPENDENCY_RADAR_WORKFLOW_ID,
    DOCS_GARDENER_WORKFLOW_ID, ERROR_DETECTIVE_WORKFLOW_ID, KNOWLEDGE_CONFLICT_WORKFLOW_ID,
    MEMORY_GARDENER_WORKFLOW_ID, MODEL_COST_OPTIMIZER_WORKFLOW_ID, SECURITY_WHISPERER_WORKFLOW_ID,
    SETUP_COACH_WORKFLOW_ID, USER_HABIT_COACH_WORKFLOW_ID,
};
#[cfg(test)]
use crate::buddy::memory_lifecycle::{MemoryCandidate, MemoryCandidateStatus, MemorySource};
#[cfg(test)]
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
#[cfg(test)]
use crate::buddy::settings::load_settings;
#[cfg(test)]
use crate::buddy::types::{BuddyActivity, BuddyFact, BuddyFactKind, BuddyRuntimeEvent, BuddyThreadMeta};
#[cfg(test)]
use crate::buddy::workflows::{WorkflowFailureCategory, WorkflowFailureReport};
#[cfg(test)]
use crate::call_validation::{ChatMessage, ChatMeta};
#[cfg(test)]
use crate::app_state::AppState;
#[cfg(test)]
use crate::stats::event::LlmCallEvent;

mod local_evidence;
mod redaction;
mod spec;

pub use local_evidence::{
    ArchitectureEvidence, DependencyManifestEvidence, DocsEvidence, PathGroup, PathStatus,
    SecurityFinding,
};
pub use refact_runtime_api::buddy_jobs::autonomous_chats::*;

pub use redaction::{
    AUTONOMOUS_EVIDENCE_CAP_CHARS, AUTONOMOUS_PROMPT_CAP_CHARS, redact_and_cap_evidence,
    redact_and_cap_prompt, redact_and_cap_text,
};
pub use spec::{
    AutonomousBuddyChatSpec, AutonomousLastResult, parse_last_autonomous_result, same_signal,
    serialize_last_autonomous_result, signal_hash,
};

pub(super) use local_evidence::*;
pub(super) use redaction::*;
pub(super) use spec::*;

#[cfg(test)]
mod tests;
