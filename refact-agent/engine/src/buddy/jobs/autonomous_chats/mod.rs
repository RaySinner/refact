use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::buddy::actor::{
    buddy_apply, make_runtime_event, redact_sensitive, render_buddy_activity_title,
    render_buddy_runtime_event, validate_workflow_id, BuddyMutation,
};
use crate::buddy::voice_service::VoiceIntent;
use crate::buddy::autonomous_workflows::{
    autonomous_workflow_meta, is_autonomous_workflow_id, AutonomousWorkflowMeta,
    ARCHITECTURE_DRIFT_WORKFLOW_ID, BEHAVIOR_LEARNER_WORKFLOW_ID, DEPENDENCY_RADAR_WORKFLOW_ID,
    DOCS_GARDENER_WORKFLOW_ID, ERROR_DETECTIVE_WORKFLOW_ID, KNOWLEDGE_CONFLICT_WORKFLOW_ID,
    MEMORY_GARDENER_WORKFLOW_ID, MODEL_COST_OPTIMIZER_WORKFLOW_ID, SECURITY_WHISPERER_WORKFLOW_ID,
    SETUP_COACH_WORKFLOW_ID, USER_HABIT_COACH_WORKFLOW_ID,
};
use crate::buddy::diagnostics::{DiagnosticContext, DiagnosticSeverity};
use crate::buddy::memory_lifecycle::{MemoryCandidate, MemoryCandidateStatus, MemorySource};
use crate::buddy::scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};
use crate::buddy::settings::load_settings;
use crate::buddy::types::{BuddyActivity, BuddyFact, BuddyFactKind, BuddyRuntimeEvent, BuddyThreadMeta};
use crate::buddy::workflows::{WorkflowFailureCategory, WorkflowFailureReport};
use crate::call_validation::{ChatMessage, ChatMeta};
use crate::app_state::AppState;
use crate::stats::event::LlmCallEvent;

mod behavior;
mod local_evidence;
mod local_jobs;
mod memory_jobs;
mod model_cost;
mod redaction;
mod runtime;
mod spec;

pub use memory_jobs::{
    BuddyBehaviorLearnerJob, BuddyKnowledgeConflictResolverJob, BuddyMemoryGardenerJob,
    BuddyModelCostOptimizerJob, BuddyUserHabitCoachJob,
};
pub use local_evidence::{
    ArchitectureEvidence, DependencyManifestEvidence, DocsEvidence, PathGroup, PathStatus,
    SecurityFinding,
};
pub use local_jobs::{
    ArchitectureDriftWatcherJob, DependencyRadarJob, DocsGardenerJob, ErrorDetectiveJob,
    SecurityWhispererJob, SetupCoachJob,
};
pub use redaction::{
    AUTONOMOUS_EVIDENCE_CAP_CHARS, AUTONOMOUS_PROMPT_CAP_CHARS, redact_and_cap_evidence,
    redact_and_cap_prompt, redact_and_cap_text,
};
pub use runtime::{run_autonomous_buddy_chat, sum_llm_usage};
pub(crate) use runtime::{execute_autonomous_spec, job_finding_opportunities};
pub use spec::{
    AutonomousBuddyChatSpec, AutonomousLastResult, parse_last_autonomous_result, same_signal,
    serialize_last_autonomous_result, signal_hash,
};

pub(super) use behavior::*;
pub(super) use local_evidence::*;
pub(super) use local_jobs::*;
pub(super) use memory_jobs::*;
pub(super) use model_cost::*;
pub(super) use redaction::*;
pub(super) use runtime::*;
pub(super) use spec::*;

#[cfg(test)]
mod tests;
