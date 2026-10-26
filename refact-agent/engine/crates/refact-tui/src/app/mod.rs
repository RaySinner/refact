use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{json, Map, Value};

use crate::approvals::{ApprovalModalState, ApprovalQueue};
use crate::ask_questions::{
    AskQuestionType, AskQuestionsForm, AskQuestionsOutcome, AskQuestionsRequest,
};
use crate::client::{
    BrowserContextDecision, ChatEvent, DaemonStatus, CompetitorImportInfoResponse,
    CompetitorImportRunResponse, GoalBudget, GoalControlAction, HooksResponse,
    KnowledgeGraphResponse, McpViewData, OpenProjectResponse, ProjectEntry, ProviderListResponse,
    ProviderOAuthLogoutResponse, SlashCommandsListResponse, TaskBoardViewData, ToolDecision,
    WorkerInfo,
};
use crate::commands::{command_by_name, session, workflow, CommandAction, InfoTopic, LocalToggle};
use crate::composer::queue::{InputQueue, QueuedInput, INPUT_QUEUE_CAPACITY};
use crate::composer::{load_history, save_history, ComposerState, EnterDecision, HistorySearchView};
use crate::events_pane::{DaemonEventRecord, EventsPaneState};
use crate::history::cells::HistoryRenderMode;
use crate::history::{
    insert_history, HistoryBuffer, HistoryInsertion, ResizeReflowState,
    RESIZE_REFLOW_PENDING_CELL_CAP,
};
use crate::keymap::{
    HelpRow, KeyAction, KeyContext, KeyDispatch, KeymapRegistry, VimEffect, VimMode, VimState,
};
use crate::notifications::{NotificationConfig, NotificationKind, NotificationManager};
use crate::overlay::{PagerAction, PagerOverlay};
use crate::pickers::{
    file_mention_items_from_completions, ModePickerItem, PickerItem, PickerKind, PickerState,
};
use crate::protocol::{
    DeltaOp, InboundEventState, SseEvent, TranscriptMessage, TranscriptRole, TranscriptState,
};
use crate::read_only_views::ReadOnlyView;
use crate::render::highlight;
use crate::sessions::{
    last_branch_message_id, session_tab_from_picker_item, SessionTab, TrajectoryMeta,
};
use crate::streaming::{
    run_commit_tick, AdaptiveChunkingPolicy, CommitTickScope, PlanStreamController,
    StreamController,
};
use crate::terminal::{terminal_title, TerminalSession};
use crate::text_safety::{sanitize_tool_inline, sanitize_tool_text, truncate_graphemes};
use crate::theme::TuiTheme;
use crate::tools::{
    now_ms, ToolCard, ToolStatus, MAX_SUBCHAT_ATTACHED_FILES, MAX_SUBCHAT_DEPTH,
    MAX_SUBCHAT_PROGRESS_CHARS,
};

mod browser;
mod chat_events;
mod command_results;
mod goal;
pub(crate) mod input;
mod runtime;
#[path = "session.rs"]
mod session_lifecycle;
mod state;
pub(crate) mod surfaces;
mod transcript;
mod workers;
use self::command_results::CommandOrigin;
pub use self::goal::GoalCommandKind;
pub use self::session_lifecycle::{ClipboardCopySource, SessionState, SubscriptionStatus, UsageSummary};
pub use self::state::{
    App, BrowserContextDecisionOptions, BrowserContextPromptState, BrowserState,
    ClientMessageCorrelation, ComposerMode,
};
pub use surfaces::ProjectPickerState;
pub use transcript::TranscriptItem;
use self::session_lifecycle::{resolve_chat_model_id, ReasoningModelCaps};
use self::state::{
    BacktrackRollback, BacktrackTarget, HistorySaveRequest, InFlightSend, LocalInputHandoff,
    LocalInputOwner, PendingApprovalClear, PendingReasoningRollback, PendingSendRetry,
    ReasoningStateSnapshot, ToolDecisionRollback,
};
use chat_events::SubagentSummary;
#[cfg(test)]
use transcript::line_to_plain_string;
use transcript::{current_goal_cell_data, current_plan_cell_data, session_header_subtitle};
pub use runtime::run;

#[cfg(test)]
use crate::client::{ChatSeqDecision, ChatSeqTracker, DaemonClient};
#[cfg(test)]
use std::process::Command;
#[cfg(test)]
use tokio::sync::mpsc;

#[cfg(test)]
use runtime::{
    edit_text_with_editor_command, execute_history_save_request,
    external_editor_command_from_values, load_git_diff, parse_editor_command, run_action,
    DaemonEventSubscription, RuntimeEvent, SubscriptionManager, SubscriptionTask,
};

const PATCH_LIKE_FUNCTIONS: &[&str] = &[
    "patch",
    "text_edit",
    "create_textdoc",
    "update_textdoc",
    "replace_textdoc",
    "update_textdoc_regex",
    "update_textdoc_by_lines",
    "update_textdoc_anchored",
    "apply_patch",
    "undo_textdoc",
];
const CHAT_RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const CHAT_RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(4);
const CHAT_MAX_RECONNECT_ATTEMPTS: u32 = 8;
const DAEMON_EVENTS_RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const DAEMON_EVENTS_RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(5);
const DAEMON_EVENTS_MAX_RECONNECT_ATTEMPTS: u32 = 8;
const ABORT_BEFORE_QUIT_TIMEOUT: Duration = Duration::from_secs(1);
const LIVE_TRANSCRIPT_ITEM_LIMIT: usize = 10_000;
const LIVE_TRANSCRIPT_RETENTION_NOTICE: &str =
    "Older live transcript items dropped after reaching 10000 live items";
const WORKING_ANIMATION_INTERVAL: Duration = Duration::from_millis(100);
const ASSISTANT_STREAM_RESERVED_COLS: u16 = 4;

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("daemon client error: {0}")]
    Client(#[from] crate::client::ClientError),
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiOptions {
    pub daemon_url: Option<String>,
    pub daemon_url_source: Option<crate::client::DaemonUrlSource>,
    pub project_hint: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorCommand {
    program: String,
    args: Vec<String>,
}

#[derive(Debug, Clone)]
enum CommandContextTag {
    SendMessage {
        prompt: String,
        params: Value,
        correlation: ClientMessageCorrelation,
    },
    Reasoning {
        previous: ReasoningStateSnapshot,
    },
    RetryFromIndex {
        origin: CommandOrigin,
        rollback: Option<BacktrackRollback>,
    },
    ToolDecisions {
        origin: CommandOrigin,
        client_request_id: String,
        rollback: ToolDecisionRollback,
    },
    Abort {
        origin: CommandOrigin,
    },
    BrowserContextDecision {
        origin: CommandOrigin,
        prompt: BrowserContextPromptState,
    },
    Rename {
        origin: CommandOrigin,
        title: String,
    },
    Fork {
        origin: CommandOrigin,
        target_chat_id: String,
        title: Option<String>,
    },
    Archive {
        origin: CommandOrigin,
        chat_id: String,
    },
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppAction {
    None,
    LoadProjects,
    LoadModels,
    LoadModes,
    LoadFileMentions {
        query: String,
        cursor: i64,
    },
    LoadReadOnlyView {
        view: ReadOnlyView,
    },
    LoadTaskBoard {
        task_id: Option<String>,
    },
    LoadProviderLogoutChoices,
    ProviderOAuthLogout {
        provider: String,
    },
    LoadCompetitorImportSources,
    RunCompetitorImport {
        source: Option<String>,
        scope: String,
    },
    LoadSessions,
    RefreshRecentSessions,
    RefreshWorkers,
    LoadDaemonStatus,
    OpenProject(PathBuf),
    SubscribeCurrent,
    SendMessage {
        prompt: String,
        params: Value,
        correlation: ClientMessageCorrelation,
    },
    RetryFromIndex {
        index: usize,
        content: Value,
    },
    SetParams {
        patch: Value,
    },
    GoalCommand {
        kind: GoalCommandKind,
        content: Option<String>,
        budget: Option<GoalBudget>,
    },
    GoalControl {
        action: GoalControlAction,
    },
    RenameChat {
        title: String,
    },
    ForkChat {
        target_chat_id: String,
        source_chat_id: String,
        up_to_message_id: String,
        title: Option<String>,
    },
    ArchiveChat {
        chat_id: String,
        new_chat_id: String,
    },
    LoadDiff {
        root: PathBuf,
    },
    Worktree {
        action: surfaces::WorktreeAction,
    },
    CopyToClipboard {
        text: String,
        source: ClipboardCopySource,
    },
    OpenExternalEditor {
        draft: String,
    },
    OpenImageExternally {
        image: crate::terminal_image::InlineImage,
    },
    SendToolDecisions {
        client_request_id: String,
        decisions: Vec<ToolDecision>,
        patch: Option<Value>,
        rollback: ToolDecisionRollback,
    },
    SendBrowserContextDecision {
        decision: BrowserContextDecision,
        prompt: BrowserContextPromptState,
    },
    Abort,
    Quit {
        abort_active: bool,
    },
}

fn history_path_for_root(root: &std::path::Path) -> PathBuf {
    let hash = stable_path_hash(root);
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("refact")
        .join("tui_history")
        .join(format!("{hash}.json"))
}

fn load_tui_config_content() -> Option<String> {
    let path = default_tui_config_path()?;
    fs::read_to_string(path).ok()
}

fn default_tui_config_path() -> Option<PathBuf> {
    if cfg!(test) {
        None
    } else {
        KeymapRegistry::default_config_path()
    }
}

fn initialize_syntax_theme(theme: &TuiTheme, config_path: Option<&Path>) {
    let theme_home = config_path
        .and_then(|path| path.parent())
        .map(Path::to_path_buf);
    let _ = highlight::set_theme_override(Some(theme.syntax_theme_name().to_string()), theme_home);
}

fn stable_path_hash(path: &std::path::Path) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn is_empty_live_assistant(message: &TranscriptMessage) -> bool {
    message.role == TranscriptRole::Assistant
        && !message.stream_finished
        && message.content.is_empty()
        && message.images.is_empty()
        && message.reasoning.is_empty()
        && message.tool_calls.is_empty()
        && message.citations.is_empty()
        && message.thinking_blocks.is_empty()
        && message.server_content_blocks.is_empty()
        && message.extra.is_empty()
        && message.unknown_delta_ops.is_empty()
}

fn split_command_name_and_args(input: &str) -> (&str, &str) {
    let input = input.trim().trim_start_matches('/').trim_start();
    match input.find(char::is_whitespace) {
        Some(index) => {
            let (name, args) = input.split_at(index);
            (name, args.trim())
        }
        None => (input, ""),
    }
}

fn notice_transcript_state(text: String) -> TranscriptState {
    let mut state = TranscriptState::new();
    state.push_notice(text);
    state
}

fn thread_update_params(raw: &Value) -> &Value {
    raw.get("params")
        .filter(|value| value.is_object())
        .unwrap_or(raw)
}

fn worker_waking_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("status 502")
        || message.contains("status 503")
        || message.contains("status 504")
        || message.contains("bad gateway")
        || message.contains("service unavailable")
        || (message.contains("worker")
            && (message.contains("starting")
                || message.contains("waking")
                || message.contains("spawn")
                || message.contains("not ready")
                || message.contains("unavailable")))
}

fn retry_hint_from_message(message: &str) -> Option<String> {
    let lower = message.to_ascii_lowercase();
    if lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("retry-after")
        || lower.contains("retry_after")
    {
        return Some(
            retry_after_hint(message)
                .map(|retry_after| format!("rate limited; retry after {retry_after}"))
                .unwrap_or_else(|| "rate limited; retry later".to_string()),
        );
    }
    if worker_waking_message(message) {
        return Some("worker waking; retrying".to_string());
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return Some("request timed out; retrying".to_string());
    }
    None
}

fn retry_after_hint(message: &str) -> Option<String> {
    [
        "retry_after_ms",
        "retry-after",
        "retry_after",
        "retry after",
    ]
    .into_iter()
    .find_map(|needle| retry_value_after(message, needle))
}

fn retry_value_after(message: &str, needle: &str) -> Option<String> {
    let lower = message.to_ascii_lowercase();
    let start = lower.find(needle)? + needle.len();
    let rest = message
        .get(start..)?
        .trim_start_matches(|ch: char| ch == ':' || ch == '=' || ch == '"' || ch.is_whitespace());
    let value = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '.' || *ch == '-')
        .collect::<String>();
    (!value.is_empty()).then_some(value)
}

fn assistant_stream_width(width: u16) -> Option<usize> {
    crate::render::width::usable_content_width_u16(width, ASSISTANT_STREAM_RESERVED_COLS)
        .or(Some(1))
}

fn render_frame(terminal: &mut TerminalSession, app: &mut App) -> Result<(), TuiError> {
    terminal.set_title(&app.terminal_title())?;
    if app.native_scrollback() {
        let size = terminal.size()?;
        app.set_screen_height(size.height);
        app.update_stream_width_for_terminal(size.width);
        let fullscreen_layer = app.surface_layer().fills_screen();
        if fullscreen_layer && !terminal.alt_screen_active() {
            terminal.enter_alt_screen()?;
        } else if !fullscreen_layer && terminal.alt_screen_active() {
            terminal.leave_alt_screen()?;
        }
        if terminal.alt_screen_active() {
            terminal.begin_synchronized_update()?;
            let result = terminal
                .draw_target()
                .draw(|frame| crate::ui::render(frame, app))
                .map(|_| ());
            terminal.end_synchronized_update()?;
            result?;
            return Ok(());
        }
        if app.resize_reflow_pending() && !app.resize_reflow_is_due() {
            return Ok(());
        }
        terminal.begin_synchronized_update()?;
        if app.resize_reflow_pending() {
            let height = crate::ui::inline_viewport_height(app, size);
            terminal.restart_screen(height)?;
            let rows = usize::from(size.height.saturating_sub(height));
            for insertion in app.resize_reflow_insertions(size.width, rows) {
                insert_history(terminal.terminal_mut(), insertion)?;
            }
            for insertion in app.pending_history_insertions(size.width) {
                insert_history(terminal.terminal_mut(), insertion)?;
            }
            app.finish_resize_reflow();
        } else {
            for insertion in app.pending_history_insertions(size.width) {
                insert_history(terminal.terminal_mut(), insertion)?;
            }
            let height = crate::ui::inline_viewport_height(app, size);
            terminal.set_inline_height(height)?;
        }
        let result = terminal
            .terminal_mut()
            .draw(|frame| crate::ui::render(frame, app))
            .map(|_| ());
        terminal.end_synchronized_update()?;
        result?;
        return Ok(());
    }
    let completed_frame = terminal
        .terminal_mut()
        .draw(|frame| crate::ui::render(frame, app))?;
    let positions =
        crate::terminal_image::image_positions(completed_frame.buffer, app.visible_transcript());
    let mut images = crate::app::transcript::inline_images_for_visible_transcript(
        app.visible_transcript(),
        crate::terminal_probe::image_protocol_from_env(),
        &positions,
    );
    browser::append_inline_image(app, completed_frame.buffer, &mut images);
    terminal.write_inline_images(&images)?;
    Ok(())
}

fn approval_patch(modal: &ApprovalModalState) -> Value {
    let all_patch_like = modal.reasons().iter().all(|reason| {
        PATCH_LIKE_FUNCTIONS
            .iter()
            .any(|name| *name == reason.tool_name)
    });
    if all_patch_like {
        json!({"auto_approve_editing_tools": true})
    } else {
        json!({"auto_approve_dangerous_commands": true})
    }
}

fn composer_search_text(key: KeyEvent) -> Option<char> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    match key.code {
        KeyCode::Char(ch) => Some(ch),
        _ => None,
    }
}

fn is_ctrl_c_key(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Char('c' | 'C'))
}

fn is_plain_space_key(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && matches!(key.code, KeyCode::Char(' '))
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

mod update;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_2;
#[cfg(test)]
mod tests_3;
#[cfg(test)]
mod tests_4;
#[cfg(test)]
mod tests_util;
