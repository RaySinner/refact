"""AC-1 mover: settle the `execute_autonomous_spec` seam, then move the buddy job files.

    python tools/dev/ac1-move.py <worktree-root>

Deterministic, one-shot; rewrites the worktree it is pointed at and never the repo
it is run from. Routine used everywhere else in this task:

  * the production body moves verbatim into `refact-runtime-api`, with only forward
    `crate::` path rewrites and `pub(super)`/`pub(crate)` -> `pub`;
  * a top-level buddy job file becomes a glob `pub use` shim plus its **verbatim**
    `#[cfg(test)]` block;
  * `autonomous_chats/*.rs` children are only reachable through the facade, so the
    engine keeps the facade and deletes the moved children;
  * items the kept tests call are widened to `pub` in the crate (struct fields too).

Step 1 - the seam extends traits that already exist, no parallel seam:

  * `SubchatLauncher::resolve_subchat_config` - the `run_*` half was already on the
    trait (`run_subchat`), the parameter-resolution half was not;
  * `BuddyEventSink::render_autonomous_report_title` /
    `::render_autonomous_result_summary` - thin wrappers over the engine-only
    `buddy::speech::{render_buddy_activity_title,render_buddy_runtime_event}`;
  * `TrajectoryStore::all_trajectory_dirs` - wraps the engine-only
    `chat::trajectories::get_all_trajectories_dirs`.

Moved into `refact-runtime-api/src/buddy_jobs/`:
  * `autonomous_chats/{runtime,local_jobs,memory_jobs,behavior,model_cost}.rs`
  * `buddy_test_coverage_watcher`, `buddy_pr_issue_matchmaker`, `buddy_onboarding`,
    `buddy_daily_digest`, `buddy_friday_retro`, `refact_self_critic`,
    `buddy_idle_suggester`, `buddy_refactor_hunter`, `buddy_skill_author`

Not moved: `refact_compile_sniffer` - it calls
`refact_tools::tools::tool_buddy_get_logs::{is_log_candidate,resolve_log_dir}` and
`refact-tools` already depends on `refact-runtime-api`, so that is a cargo cycle.
"""
import os
import re
import sys

WT = os.path.abspath(sys.argv[1])
ENG = os.path.join(WT, "refact-agent", "engine")
SRC = os.path.join(ENG, "src")
JOBS = os.path.join(SRC, "buddy", "jobs")
AC = os.path.join(JOBS, "autonomous_chats")
CRATE = os.path.join(ENG, "crates", "refact-runtime-api", "src")
CRATE_JOBS = os.path.join(CRATE, "buddy_jobs")
CRATE_AC = os.path.join(CRATE_JOBS, "autonomous_chats")

TOP_LEVEL_JOBS = [
    "buddy_test_coverage_watcher",
    "buddy_pr_issue_matchmaker",
    "buddy_onboarding",
    "buddy_daily_digest",
    "buddy_friday_retro",
    "refact_self_critic",
    "buddy_idle_suggester",
    "buddy_refactor_hunter",
    "buddy_skill_author",
]

REWRITES = [
    ("crate::buddy::jobs::autonomous_chats::", "crate::buddy_jobs::autonomous_chats::"),
    ("crate::buddy::actor::BuddyMutation", "refact_buddy_core::actor::BuddyMutation"),
    ("crate::buddy::actor::make_runtime_event",
     "refact_buddy_core::actor::make_runtime_event"),
    ("crate::buddy::actor::validate_workflow_id",
     "refact_buddy_core::actor::validate_workflow_id"),
    ("crate::buddy::actor::", "crate::buddy_actor_runtime::"),
    ("crate::buddy::workflows::", "refact_buddy_core::workflows::"),
    ("crate::buddy::scheduler::", "crate::buddy_scheduler::"),
    ("crate::buddy::autonomous_workflows::", "refact_buddy_core::autonomous_workflows::"),
    ("crate::buddy::settings::", "refact_buddy_core::settings::"),
    ("crate::buddy::types::", "refact_buddy_core::types::"),
    ("crate::buddy::conversation_ledger::", "refact_buddy_core::conversation_ledger::"),
    ("crate::buddy::user_activity::", "refact_buddy_core::user_activity::"),
    ("crate::buddy::memory_lifecycle::",
     "refact_buddy_core::memory_lifecycle_model::"),
    ("crate::buddy::storage::enqueue_memory_op",
     "refact_buddy_core::storage::enqueue_memory_op"),
    ("crate::worktrees::git::run_git_lossy", "refact_worktrees::git::run_git_lossy"),
    ("crate::llm::safe_truncate", "refact_llm::logging::safe_truncate"),
    ("crate::caps::resolve_model", "refact_caps_core::model_resolve::resolve_model"),
    ("crate::memories::touch_memory_observation",
     "crate::memories_runtime::touch_memory_observation"),
    ("crate::memories::normalize_preference_text_for_dedupe",
     "refact_core::memories_format::normalize_preference_text_for_dedupe"),
    ("crate::memories::preference_statement_is_safe",
     "refact_core::memories_format::preference_statement_is_safe"),
    ("crate::yaml_configs::customization_registry::get_subagent_config",
     "crate::customization_registry::get_subagent_config"),
    ("crate::ext::competitor_import::markdown::sanitize_skill_id",
     "refact_ext::competitor_import::markdown::sanitize_skill_id"),
    ("crate::tasks::storage::get_all_tasks_dirs", "crate::task_storage::get_all_tasks_dirs"),
    ("crate::stats::reader::", "refact_stats::reader::"),
    ("crate::chat::trajectories::get_all_trajectories_dirs(&gcx.chat_ctx()).await",
     "gcx.chat_ctx().trajectories.all_trajectory_dirs().await"),
]

# a kept test reaches this helper; it must exist (and be reachable) outside `test`
HELPER_GATES = [
    ("#[cfg(test)]\npub(super) async fn execute_dependency_radar_with_evidence",
     '#[cfg(any(test, feature = "test-util"))]\n'
     "pub async fn execute_dependency_radar_with_evidence"),
]

# production items the kept tests call -> widen to `pub` (struct fields too)
WIDEN = {
    "local_jobs.rs": [],
    "memory_jobs.rs": [],
    "behavior.rs": ["MAX_BEHAVIOR_TRAJECTORY_BYTES", "MAX_BEHAVIOR_TRAJECTORY_META_BYTES",
                    "MAX_BEHAVIOR_TRAJECTORY_SCAN_FILES",
                    "MAX_BEHAVIOR_TRAJECTORY_SCAN_ENTRIES",
                    "MAX_BEHAVIOR_TASK_DIR_SCAN_ENTRIES", "BEHAVIOR_COOLDOWN_SECS",
                    "MAX_TRAJECTORY_SNIPPETS", "MAX_BEHAVIOR_PREFERENCE_WRITES",
                    "TrajectoryUserSnippet", "BehaviorTrajectoryMeta",
                    "BehaviorTrajectoryCandidate", "BehaviorTrajectoryScanStats",
                    "first_sentence", "explicit_preference_confidence", "preference_like",
                    "behavior_candidate_status", "behavior_preference_candidates",
                    "enqueue_behavior_preferences", "extract_text_from_trajectory_content",
                    "read_bounded_trajectory_json", "parse_json_string_at", "skip_json_ws",
                    "skip_json_value", "top_level_json_strings", "top_level_json_string",
                    "parse_behavior_trajectory_meta", "behavior_trajectory_is_internal",
                    "collect_behavior_trajectory_metas_from_dir", "system_time_key",
                    "push_behavior_trajectory_candidate",
                    "collect_behavior_trajectory_candidates_from_dir",
                    "collect_behavior_trajectory_candidates_from_dirs",
                    "collect_behavior_trajectory_candidates_from_dirs_with_stats",
                    "parse_behavior_trajectory_meta_from_path",
                    "collect_behavior_trajectory_metas_from_candidates",
                    "collect_behavior_task_trajectory_dirs", "collect_behavior_trajectory_metas",
                    "collect_recent_user_snippets", "habit_evidence"],
    "model_cost.rs": ["MODEL_COST_RECENT_EVENT_LIMIT", "is_autonomous_buddy_workflow_identifier",
                      "is_buddy_autonomous_report_event", "model_cost_input_events",
                      "model_cost_evidence_from_events", "model_cost_evidence"],
    "buddy_daily_digest.rs": ["COOLDOWN_SECONDS", "date_key", "should_run_at"],
    "buddy_friday_retro.rs": ["COOLDOWN_SECONDS", "week_key_at", "should_run_at"],
    "buddy_refactor_hunter.rs": ["COOLDOWN_SECONDS", "week_key_at", "week_key_recorded",
                                 "build_refactor_hunter_spec"],
    "buddy_onboarding.rs": ["OnboardingScanResult", "OnboardingCandidate", "serialize_scan",
                            "scan_onboarding", "build_onboarding_spec"],
    "buddy_skill_author.rs": ["SkillScanResult", "SkillCandidate", "serialize_scan",
                              "scan_skill_author", "build_skill_author_spec"],
    "refact_self_critic.rs": ["is_refact_engine_workspace", "hash_prompt_tree_at",
                              "build_self_critic_spec"],
    "buddy_pr_issue_matchmaker.rs": ["PrIssueMatchmakerCache", "store_pending_diff_cache",
                                     "take_pending_diff_cache", "serialize_cache", "fresh_cache",
                                     "parse_shortstat_changed_lines",
                                     "git_diff_shortstat_changed_lines", "checked_diff_cache"],
    "buddy_test_coverage_watcher.rs": ["COOLDOWN_SECONDS", "MAX_CANDIDATES", "CoverageCandidate",
                                       "CoverageScanResult", "CoverageScanCache",
                                       "CoverageAutonomousResult", "serialize_scan",
                                       "serialize_scan_with_autonomous", "cached_scan",
                                       "parse_git_status_line", "modified_rust_files",
                                       "source_has_test_coverage", "scan_test_coverage",
                                       "build_test_coverage_spec"],
}

CRATE_AC_MOD = '''use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use uuid::Uuid;

use refact_buddy_core::actor::{make_runtime_event, validate_workflow_id, BuddyMutation};
use refact_buddy_core::autonomous_redaction::*;
use refact_buddy_core::autonomous_workflows::*;
use refact_buddy_core::jobs::autonomous_chats::local_evidence::*;
use refact_buddy_core::memory_lifecycle_model::{MemoryCandidateStatus, MemorySource};
use refact_buddy_core::settings::load_settings;
use refact_buddy_core::types::{
    BuddyActivity, BuddyFact, BuddyFactKind, BuddyRuntimeEvent, BuddyThreadMeta,
};
use refact_buddy_core::workflows::{WorkflowFailureCategory, WorkflowFailureReport};
use refact_chat_api::subchat_types::ChatMeta;
use refact_chat_api::ChatMessage;
use refact_stats::event::LlmCallEvent;

use crate::app_state::AppState;
use crate::buddy_autonomous_spec::*;
use crate::buddy_runtime::buddy_apply;
use crate::buddy_memory_lifecycle::MemoryCandidate;
use crate::buddy_scheduler::{BuddyJob, BuddyJobContext, BuddyJobResult};

pub use crate::buddy_autonomous_spec::*;

pub mod behavior;
pub mod local_jobs;
pub mod memory_jobs;
pub mod model_cost;
pub mod runtime;

pub use behavior::*;
pub use local_jobs::*;
pub use memory_jobs::*;
pub use model_cost::*;
pub use runtime::*;
'''

MOVE_TO_CRATE_MODS = ["behavior", "local_jobs", "memory_jobs", "model_cost", "runtime"]
DROP_LINES = [
    "pub use memory_jobs::",
    "pub use local_jobs::",
    "pub use runtime::",
    "pub(crate) use runtime::",
    "pub(super) use behavior::*;",
    "pub(super) use local_jobs::*;",
    "pub(super) use memory_jobs::*;",
    "pub(super) use model_cost::*;",
    "pub(super) use runtime::*;",
]


def engine_ac_mod(original):
    """The engine facade: original prelude kept, moved children replaced by the glob.

    The prelude matters: `autonomous_chats/tests.rs` is a child module and its
    `use super::*` still resolves every private `use` of this module, so dropping
    them would break the kept tests.
    """
    kept = []
    dropping = False
    for line in original.split("\n"):
        stripped = line.strip()
        if dropping:
            if stripped.endswith(";"):
                dropping = False
            continue
        if any(stripped.startswith("mod %s;" % m) for m in MOVE_TO_CRATE_MODS):
            continue
        if any(stripped.startswith(d) for d in DROP_LINES):
            if not stripped.endswith(";"):
                dropping = True
            continue
        kept.append(line)
    anchor = "pub use redaction::{"
    body = kept_text(kept)
    if anchor not in body:
        raise SystemExit("ac1-move: engine autonomous_chats/mod.rs: no redaction anchor")
    body = body.replace(
        anchor,
        "pub use refact_runtime_api::buddy_jobs::autonomous_chats::*;\n\n" + anchor,
        1,
    )
    gated = []
    for line in body.split("\n"):
        if line.startswith("use "):
            gated.append("#[cfg(test)]")
        gated.append(line)
    return "\n".join(gated)


def kept_text(lines):
    return "\n".join(lines)


SUBTCHAT_RESOLVE_TRAIT = '''    async fn resolve_subchat_config(
        &self,
        tool_name: &str,
        stateful: bool,
        chat_id: Option<String>,
        title: Option<String>,
        tools: Option<Vec<String>>,
        max_steps: usize,
        prepend_system_prompt: bool,
        mode: String,
    ) -> Result<SubchatConfig, String>;

'''

SUBTCHAT_RESOLVE_IMPL = '''    async fn resolve_subchat_config(
        &self,
        tool_name: &str,
        stateful: bool,
        chat_id: Option<String>,
        title: Option<String>,
        tools: Option<Vec<String>>,
        max_steps: usize,
        prepend_system_prompt: bool,
        mode: String,
    ) -> Result<SubchatConfig, String> {
        crate::subchat::resolve_subchat_config(
            self.gcx.clone(),
            tool_name,
            stateful,
            chat_id,
            title,
            None,
            None,
            None,
            tools,
            max_steps,
            prepend_system_prompt,
            None,
            mode,
        )
        .await
    }

'''

BUDDY_SINK_SPEECH_TRAIT = '''
    /// Seam over the engine-only `buddy::speech::render_buddy_activity_title`.
    async fn render_autonomous_report_title(
        &self,
        workflow_id: &str,
        workflow_summary: String,
        fallback_title: String,
    ) -> String;

    /// Seam over the engine-only `buddy::speech::render_buddy_runtime_event`.
    /// `None` means the buddy service is unavailable, so the caller keeps its own
    /// fallback title and description.
    async fn render_autonomous_result_summary(
        &self,
        workflow_id: &str,
        workflow_summary: String,
        fallback_title: String,
        fallback_description: String,
    ) -> Option<(String, Option<String>)>;
'''

BUDDY_SINK_SPEECH_IMPL = '''    async fn render_autonomous_report_title(
        &self,
        workflow_id: &str,
        workflow_summary: String,
        fallback_title: String,
    ) -> String {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        let Some(snapshot) = self.snapshot().await else {
            return fallback_title;
        };
        crate::buddy::speech::render_buddy_activity_title(
            app,
            snapshot.state.personality,
            snapshot.state.identity.name,
            snapshot.pulse,
            Some(workflow_id.to_string()),
            workflow_summary,
            crate::buddy::voice_service::VoiceIntent::AutonomousReportSaved,
            fallback_title,
        )
        .await
    }

    async fn render_autonomous_result_summary(
        &self,
        workflow_id: &str,
        workflow_summary: String,
        fallback_title: String,
        fallback_description: String,
    ) -> Option<(String, Option<String>)> {
        let app = app_state_from_gcx(self.gcx.clone()).await;
        let snapshot = self.snapshot().await?;
        Some(
            crate::buddy::speech::render_buddy_runtime_event(
                app,
                snapshot.state.personality,
                snapshot.state.identity.name,
                snapshot.pulse,
                Some(workflow_id.to_string()),
                workflow_summary,
                "completed",
                fallback_title,
                Some(fallback_description),
            )
            .await,
        )
    }

'''

TRAJECTORY_TRAIT = '''
    /// Directories holding trajectory files (workspace + global). Engine half:
    /// `chat::trajectories::get_all_trajectories_dirs`.
    async fn all_trajectory_dirs(&self) -> Vec<PathBuf>;
'''

TRAJECTORY_IMPL = '''    async fn all_trajectory_dirs(&self) -> Vec<PathBuf> {
        trajectories::get_all_trajectories_dirs(&self.gcx.chat_ctx(self.gcx.clone())).await
    }

'''

RUNTIME_ANCHORS = [
    ("""    let title = match crate::buddy::actor::buddy_snapshot(gcx.clone()).await {
        Some(snapshot) => {
            render_buddy_activity_title(
                gcx,
                snapshot.state.personality,
                snapshot.state.identity.name,
                snapshot.pulse,
                Some(spec.workflow_id.clone()),
                spec.title.clone(),
                VoiceIntent::AutonomousReportSaved,
                fallback_title,
            )
            .await
        }
        None => fallback_title,
    };""",
     """    let title = gcx
        .buddy_event_sink
        .render_autonomous_report_title(&spec.workflow_id, spec.title.clone(), fallback_title)
        .await;""",
     "autonomous_activity title seam"),

    ("""    let (title, description) = match crate::buddy::actor::buddy_snapshot(gcx.clone()).await {
        Some(snapshot) => {
            render_buddy_runtime_event(
                gcx,
                snapshot.state.personality,
                snapshot.state.identity.name,
                snapshot.pulse,
                Some(spec.workflow_id.clone()),
                fallback_description.clone(),
                "completed",
                fallback_title,
                Some(fallback_description.clone()),
            )
            .await
        }
        None => (fallback_title, Some(fallback_description.clone())),
    };""",
     """    let (title, description) = gcx
        .buddy_event_sink
        .render_autonomous_result_summary(
            &spec.workflow_id,
            fallback_description.clone(),
            fallback_title.clone(),
            fallback_description.clone(),
        )
        .await
        .unwrap_or_else(|| (fallback_title, Some(fallback_description.clone())));""",
     "runtime_event summary seam"),

    ("""        system_prompt = crate::chat::prompts::system_prompt_add_extra_instructions(
            gcx,
            system_prompt,""",
     """        system_prompt = gcx
            .chat_ctx()
            .prompts
            .system_prompt_add_extra_instructions(
            system_prompt,""",
     "prompts seam"),

    ("""    let mut config = crate::subchat::resolve_subchat_config(
        gcx.gcx.clone(),
        &spec.workflow_id,
        true,
        Some(format!("buddy-{}-{}", spec.workflow_id, Uuid::new_v4())),
        Some(spec.title.clone()),
        None,
        None,
        None,
        Some(tools),
        max_steps,
        false,
        None,
        pick_available_buddy_mode(gcx.clone()).await,
    )
    .await?;""",
     """    let mut config = gcx
        .chat_ctx()
        .subchat
        .resolve_subchat_config(
            &spec.workflow_id,
            true,
            Some(format!("buddy-{}-{}", spec.workflow_id, Uuid::new_v4())),
            Some(spec.title.clone()),
            Some(tools),
            max_steps,
            false,
            pick_available_buddy_mode(gcx.clone()).await,
        )
        .await?;""",
     "subchat launch seam"),

    ("    let result = crate::subchat::run_subchat(gcx.gcx.clone(), messages, config).await?;",
     "    let result = gcx.chat_ctx().subchat.run_subchat(messages, config).await?;",
     "subchat run seam"),
]


def read(path):
    with open(path, "r", encoding="utf-8", errors="surrogateescape") as fh:
        return fh.read()


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", errors="surrogateescape", newline="\n") as fh:
        fh.write(text)


def sub1(path, old, new, label):
    text = read(path)
    if old not in text:
        raise SystemExit("ac1-move: %s: anchor not found: %r" % (label, old[:100]))
    write(path, text.replace(old, new, 1))


USE_RE = re.compile(r"(?m)^use [^;]+;\n")


def prod_uses(prod):
    """The production `use` lines, gated on `test`.

    A kept `#[cfg(test)] mod tests` says `use super::*` and a child module sees the
    parent's private imports through it, so the shim has to carry them; nothing in
    the shim consumes them at build time, so they must not leak `unused_imports`
    into a normal lib build.
    """
    return "".join("#[cfg(test)]\n" + use for use in USE_RE.findall(prod))


def split_prod(text):
    lines = text.split("\n")
    for idx, line in enumerate(lines):
        if line.strip().startswith("#[cfg(test)]"):
            return "\n".join(lines[:idx]), "\n".join(lines[idx:])
    return text, ""


def rewrite(text):
    for old, new in REWRITES:
        text = text.replace(old, new)
    return text


def demote_visibility(text):
    return (text.replace("pub(crate) ", "pub ")
                .replace("pub(super) ", "pub ")
                .replace("pub(in crate::buddy) ", "pub "))


DEF_RE = re.compile(r"(?m)^(?:pub(?:\([^)]*\))? )?(?:async )?"
                    r"(?:fn|struct|enum|const|type|static)\s+([A-Za-z_][A-Za-z0-9_]*)\s*[(<{]")

DEF_ATTR_RE = re.compile(r"(?m)^#\[cfg\(test\)\]\n"
                         r"(?:pub(?:\([^)]*\))? )?(?:async )?"
                         r"(?:fn|struct|enum|const|type|static)\s+([A-Za-z_][A-Za-z0-9_]*)")


def widen_names_for(text, tests_text, extra):
    """Items the kept tests reach, plus items this file gates on `test`."""
    names = {m.group(1) for m in DEF_ATTR_RE.finditer(text)}
    for m in DEF_RE.finditer(text):
        name = m.group(1)
        if re.search(r"\b%s\b" % re.escape(name), tests_text):
            names.add(name)
    names.update(extra)
    return sorted(names)


def widen_items(text, names):
    for name in names:
        pattern = (r"(?m)^(?:#\[cfg\(test\)\]\n)?(?:pub(?:\([^)]*\))? )?((?:async )?)"
                   r"(fn|struct|enum|const|type|static) %s\b")
        text, n = re.subn(pattern % re.escape(name), r"pub \1\2 %s" % name, text)
        if not n:
            raise SystemExit("ac1-move: could not widen %s" % name)
    return text


def widen_struct_fields(text, struct_names):
    for name in struct_names:
        m = re.search(r"(?m)^pub struct %s \{\n" % re.escape(name), text)
        if not m:
            continue
        start = m.end()
        end = text.find("\n}\n", start)
        if end < 0:
            continue
        lines = []
        for line in text[start:end].split("\n"):
            stripped = line.strip()
            if (stripped
                    and not stripped.startswith(("pub ", "#", "//"))
                    and "(" not in stripped):
                indent = line[: len(line) - len(line.lstrip())]
                line = indent + "pub " + stripped
            lines.append(line)
        text = text[:start] + "\n".join(lines) + text[end:]
    return text


def engine_paths(text):
    pat = re.compile(r"\bcrate::(?:buddy|chat|subchat|llm|caps|ext|worktrees|yaml_configs|tools|"
                     r"memories|stats|files_correction|call_validation|global_context|agents|"
                     r"scratchpads|privacy|integrations|codegraph|vecdb)::[A-Za-z_][A-Za-z0-9_:]*")
    allowed = ("crate::buddy_jobs::", "crate::buddy_scheduler::", "crate::buddy_autonomous_spec::",
               "crate::buddy_actor_runtime::", "crate::buddy_runtime::",
               "crate::customization_registry::", "crate::files_correction::",
               "crate::memories_runtime::", "crate::task_storage::", "crate::stats::",
               "crate::app_state::AppState", "crate::app_state::app_state_from_gcx")
    return sorted(h for h in set(pat.findall(text)) if not h.startswith(allowed))


def seam():
    sub1(os.path.join(CRATE, "chat_ctx.rs"),
         "    async fn resolve_subchat_config_with_parent(",
         SUBTCHAT_RESOLVE_TRAIT + "    async fn resolve_subchat_config_with_parent(",
         "SubchatLauncher::resolve_subchat_config")

    sub1(os.path.join(CRATE, "lib.rs"),
         """    ) -> Option<(String, Option<String>)>;

    /// Seam over the engine buddy workflow wrapper.""",
         """    ) -> Option<(String, Option<String>)>;
""" + BUDDY_SINK_SPEECH_TRAIT + """
    /// Seam over the engine buddy workflow wrapper.""",
         "BuddyEventSink speech seam")

    sub1(os.path.join(CRATE, "chat_ctx.rs"),
         "    async fn list_trajectory_dirs(&self) -> Vec<PathBuf>;",
         "    async fn list_trajectory_dirs(&self) -> Vec<PathBuf>;\n" + TRAJECTORY_TRAIT,
         "TrajectoryStore::all_trajectory_dirs")

    sub1(os.path.join(SRC, "app_state", "trajectory_store.rs"),
         "    async fn list_trajectory_dirs(&self) -> Vec<PathBuf> {",
         TRAJECTORY_IMPL + "    async fn list_trajectory_dirs(&self) -> Vec<PathBuf> {",
         "EngineTrajectoryStore::all_trajectory_dirs")

    sub1(os.path.join(SRC, "app_state", "launchers.rs"),
         "    async fn resolve_subchat_config_with_parent(",
         SUBTCHAT_RESOLVE_IMPL + "    async fn resolve_subchat_config_with_parent(",
         "EngineSubchatLauncher::resolve_subchat_config")

    sub1(os.path.join(SRC, "app_state", "sinks.rs"),
         "    async fn render_runtime_event_fast(",
         BUDDY_SINK_SPEECH_IMPL + "    async fn render_runtime_event_fast(",
         "AppBuddyEventSink speech seam")


def prepare_body(text, widen_names, regate=False):
    body = demote_visibility(rewrite(text))
    for old, new in HELPER_GATES:
        body = body.replace(old, new)
    if regate:
        body = body.replace("#[cfg(test)]", '#[cfg(any(test, feature = "test-util"))]')
    body = widen_items(body, widen_names)
    body = widen_struct_fields(body, [n for n in widen_names if ("pub struct %s {" % n) in body])
    return body


def move_ac_child(module):
    """`autonomous_chats/<module>.rs`: the whole file moves and the engine copy goes.

    The children are reachable only through `autonomous_chats/mod.rs`, which keeps
    a `pub use` of the crate module, so no per-child shim is needed. `mod tests`
    lives in `autonomous_chats/tests.rs` (the engine), so the children carry only
    item-level `#[cfg(test)]` helpers; those are re-gated on
    `any(test, feature = "test-util")` so the crate's kept tests keep seeing them.
    """
    src_path = os.path.join(AC, module + ".rs")
    text = read(src_path)
    if "mod tests" in text:
        raise SystemExit("ac1-move: %s declares its own tests module" % module)
    widen_names = widen_names_for(text, read(os.path.join(AC, "tests.rs")),
                                  WIDEN.get(module + ".rs", []))
    body = prepare_body(text, widen_names, regate=True)
    leftover = engine_paths(body)
    if leftover:
        print("WARN %s leftovers:" % module, leftover)
    write(os.path.join(CRATE_AC, module + ".rs"), body)
    os.remove(src_path)
    return (len(body.rstrip("\n").split("\n")), 0)


def move_top_level(name):
    src_path = os.path.join(JOBS, name + ".rs")
    text = read(src_path)
    prod, tests = split_prod(text)
    widen_names = widen_names_for(prod, tests, WIDEN.get(name + ".rs", []))
    body = prepare_body(prod, widen_names)
    leftover = engine_paths(body)
    if leftover:
        print("WARN %s leftovers:" % name, leftover)
    write(os.path.join(CRATE_JOBS, name + ".rs"), body)
    shim = prod_uses(prod) + "\npub use refact_runtime_api::buddy_jobs::%s::*;\n" % name
    if tests.strip():
        shim += "\n" + tests.strip() + "\n"
    write(src_path, shim)
    return (len(prod.rstrip("\n").split("\n")), len(tests.rstrip("\n").split("\n")))


def main():
    if not os.path.isdir(AC):
        raise SystemExit("ac1-move: no %s" % AC)

    seam()
    os.makedirs(CRATE_AC, exist_ok=True)
    write(os.path.join(CRATE_AC, "mod.rs"), CRATE_AC_MOD)
    facade_path = os.path.join(AC, "mod.rs")
    write(facade_path, engine_ac_mod(read(facade_path)))

    text = read(os.path.join(AC, "runtime.rs"))
    for old, new, label in RUNTIME_ANCHORS:
        if old not in text:
            raise SystemExit("ac1-move: runtime.rs %s: anchor not found:\n%s" % (label, old[:300]))
        text = text.replace(old, new, 1)
    runtime_body = prepare_body(text, [])
    leftover = engine_paths(runtime_body)
    if leftover:
        print("WARN runtime.rs leftovers:", leftover)
    write(os.path.join(CRATE_AC, "runtime.rs"), runtime_body)
    os.remove(os.path.join(AC, "runtime.rs"))

    report = [("autonomous_chats/runtime.rs", (708, 0))]
    for module in ("behavior", "local_jobs", "memory_jobs", "model_cost"):
        report.append(("autonomous_chats/%s.rs" % module, move_ac_child(module)))
    for name in TOP_LEVEL_JOBS:
        report.append((name + ".rs", move_top_level(name)))

    jobs_mod = os.path.join(CRATE_JOBS, "mod.rs")
    text = read(jobs_mod)
    if "pub mod autonomous_chats;" not in text:
        text = text.rstrip("\n") + "\npub mod autonomous_chats;\n"
    for name in TOP_LEVEL_JOBS:
        if ("pub mod %s;" % name) not in text:
            text = text.rstrip("\n") + "\npub mod %s;\n" % name
    write(jobs_mod, text)

    total = 0
    for module, (prod_lines, test_lines) in report:
        total += prod_lines
        print("moved %-42s prod %5d  tests-kept %5d" % (module, prod_lines, test_lines))
    print("TOTAL production lines moved: %d" % total)


main()
