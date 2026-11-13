use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;

use refact_core::chat_types::ChatMessage;

use crate::compression_gate::compression_attempt_active;
use crate::types::{ChatSession, CompressionPhase, CompressionReason, SessionState};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ManualCompressionPreview {
    pub eligible: bool,
    pub trajectory_version: Option<u64>,
    pub resolved_model: Option<String>,
    pub context_window: Option<usize>,
    pub source_messages: usize,
    pub approximate_source_tokens: usize,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ManualCompressionApplyResult {
    pub applied: bool,
    pub resolved_model: Option<String>,
    pub context_window: Option<usize>,
    pub source_messages: usize,
    pub approximate_source_tokens: usize,
    pub before_message_count: usize,
    pub after_message_count: usize,
    pub before_approx_tokens: usize,
    pub after_approx_tokens: usize,
    pub reason: Option<String>,
}

pub fn pending_tools(messages: &[ChatMessage]) -> bool {
    let mut pending = std::collections::HashSet::new();
    for message in messages {
        for call in message.tool_calls.iter().flatten() {
            pending.insert(call.id.clone());
        }
        if matches!(message.role.as_str(), "tool" | "diff" | "context_file") {
            pending.remove(&message.tool_call_id);
        }
    }
    !pending.is_empty()
}

pub fn at_auto_cap(used: usize, cap: usize) -> bool {
    cap > 0 && used >= cap
}

pub fn idle(session: &ChatSession) -> bool {
    session.runtime.state == SessionState::Idle
        && session.draft_message.is_none()
        && session.draft_usage.is_none()
        && session.stream_started_at.is_none()
        && !session.closed
        && !session.abort_flag.load(Ordering::SeqCst)
}

/// Deterministic recovery runs right after a failed model rebuild, which has
/// already left the session in the Error state, so only a live stream, draft,
/// closed session, or abort must block it — not the Error state itself.
pub fn compaction_safe(session: &ChatSession) -> bool {
    matches!(
        session.runtime.state,
        SessionState::Idle | SessionState::Error | SessionState::Completed
    ) && session.draft_message.is_none()
        && session.draft_usage.is_none()
        && session.stream_started_at.is_none()
        && !session.closed
        && !session.abort_flag.load(Ordering::SeqCst)
}

pub fn status(
    session: &mut ChatSession,
    phase: CompressionPhase,
    reason: Option<CompressionReason>,
) {
    let running = matches!(
        phase,
        CompressionPhase::Checking | CompressionPhase::Running
    );
    session.is_compressing = running;
    session.runtime.is_compressing = running;
    session.compression_phase = Some(phase);
    session.runtime.compression_phase = Some(phase);
    session.compression_reason = reason;
    session.runtime.compression_reason = reason;
    if !running {
        session.active_compression_attempt = None;
        session.compression_attempt_started_at_ms = None;
        session.compression_abort_flag = None;
        session.clear_compression_gate_diagnostics();
        // This is the single authoritative post-compression finalisation point: every
        // terminal phase (Applied, Failed, Skipped) passes through here. Summarizing
        // rewrote the history the model sees, so any repeated-call chain counted
        // against the old transcript is meaningless — the next call starts fresh.
        session.tool_loop_guard.clear();
        session.queue_notify.notify_waiters();
    }
    session.refresh_goal_runtime_mirror();
    let event = session.runtime_update_event(
        session.runtime.state,
        session.runtime.error.clone(),
        running,
        Some(phase),
        reason,
    );
    session.emit(event);
}

pub fn emit_compression_skipped_status(session: &mut ChatSession, reason: CompressionReason) {
    if !compression_attempt_active(session) {
        status(session, CompressionPhase::Skipped, Some(reason));
    }
}

/// Releases the reservation even when the caller drops the provider future.
pub struct AttemptGuard {
    pub session: Arc<Mutex<ChatSession>>,
    pub attempt: u64,
    pub abort: Arc<AtomicBool>,
}
impl Drop for AttemptGuard {
    fn drop(&mut self) {
        let session = self.session.clone();
        let attempt = self.attempt;
        let release = {
            let attempt = self.attempt;
            move |s: &mut ChatSession| {
                if s.active_compression_attempt == Some(attempt) {
                    status(
                        s,
                        CompressionPhase::Failed,
                        Some(CompressionReason::TransientFailure),
                    );
                }
            }
        };
        // Ordering matters. `compression_attempt_active` short-circuits on the abort
        // flag, so raising it first opens a window where the attempt reads as
        // cancelled while `is_compressing` is still true — stale UI that outlives a
        // snapshot. Whenever the lock is free we therefore publish the terminal
        // phase FIRST and only then raise the flag, which closes that window
        // outright. When the lock is contended we cannot publish synchronously, so
        // the flag goes up first to cancel in-flight provider work and the spawned
        // task follows as soon as it wins the lock.
        let Ok(mut s) = session.try_lock() else {
            self.abort.store(true, Ordering::SeqCst);
            match tokio::runtime::Handle::try_current() {
                Ok(runtime) => {
                    let session = session.clone();
                    runtime.spawn(async move {
                        release(&mut *session.lock().await);
                    });
                }
                Err(_) => {
                    // No runtime and a contended lock: nothing can write the terminal
                    // phase here. Record it loudly rather than fail silently; the
                    // COMPRESSION_ATTEMPT_STALE_AFTER predicate is what keeps this
                    // from wedging the session once the attempt ages out.
                    tracing::warn!(
                        attempt,
                        "compression attempt guard dropped with no runtime and a contended \
                         session lock; terminal phase deferred to compression attempt staleness"
                    );
                }
            }
            return;
        };
        release(&mut s);
        self.abort.store(true, Ordering::SeqCst);
    }
}

pub fn prepare_explicit_idle(session: &mut ChatSession) {
    if matches!(
        session.runtime.state,
        SessionState::Idle | SessionState::Error | SessionState::Completed
    ) && !session.closed
        && !compression_attempt_active(session)
        && session.draft_message.is_none()
        && session.draft_usage.is_none()
        && session.stream_started_at.is_none()
    {
        // An explicit operation is a retry, never a request to resume generation.
        session.abort_flag.store(false, Ordering::SeqCst);
        session.set_runtime_state(SessionState::Idle, None);
    }
}

/// The gate's resolved cap and which input won it. Diagnostic only: the value is
/// byte-identical to the inline `min()` this replaced, so triggering is unchanged.
/// `model_window_fallback` means every positive candidate was absent and the cap
/// fell back to `n_ctx` — including the degenerate all-zero case.
pub fn resolve_effective_compression_cap(
    n_ctx: usize,
    context_tokens_cap: Option<usize>,
    auto_compression_cap: Option<usize>,
) -> (usize, &'static str) {
    let candidate = |value: usize, source: &'static str| (value > 0).then_some((value, source));
    [
        candidate(n_ctx, "model_window"),
        context_tokens_cap
            .filter(|value| *value > 0)
            .map(|value| (value, "context_tokens_cap")),
        auto_compression_cap
            .filter(|value| *value > 0)
            .map(|value| (value, "auto_compression_cap")),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|(value, _)| *value)
    .unwrap_or((n_ctx, "model_window_fallback"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionOutcome {
    Applied,
    NothingToCompact,
    LlmUnavailable,
}
impl CompactionOutcome {
    pub fn applied(self) -> bool {
        self == Self::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use refact_core::active_context::{make_reconstruction_report, ReconstructionMetadata};

    fn epoch_ms_now() -> u64 {
        chrono::Utc::now().timestamp_millis().max(0) as u64
    }

    fn fixture() -> (ChatSession, crate::ThreadParams, u64, ChatMessage) {
        let mut s = ChatSession::new("transaction-test".into());
        s.add_message(ChatMessage::new(
            "user".into(),
            "ARCHIVED_SENTINEL ".repeat(100),
        ));
        s.active_compression_attempt = Some(1);
        status(&mut s, CompressionPhase::Running, None);
        let mut context = ChatMessage::new("user".into(), "active request".into());
        context.message_id = "rebuilt-request".into();
        let report =
            make_reconstruction_report(vec![context], ReconstructionMetadata::default()).unwrap();
        let thread = s.thread.clone();
        let version = s.trajectory_version;
        (s, thread, version, report)
    }

    #[test]
    fn completed_and_aborted_manual_retry_is_idle_without_resuming() {
        let mut s = ChatSession::new("manual-completed".into());
        s.runtime.state = SessionState::Completed;
        s.abort_flag.store(true, Ordering::SeqCst);
        prepare_explicit_idle(&mut s);
        assert!(idle(&s));
        assert!(s.command_queue.is_empty());
        s.runtime.state = SessionState::ExecutingTools;
        s.abort_flag.store(true, Ordering::SeqCst);
        prepare_explicit_idle(&mut s);
        assert!(!idle(&s));
    }

    #[test]
    fn dropped_attempt_clears_compression_before_raising_abort() {
        // When the session lock is free the guard publishes the terminal phase
        // first, so there must be no observable state where the abort flag is
        // raised while the session still advertises an active compression phase.
        let (s, _, _, _) = fixture();
        let session = Arc::new(Mutex::new(s));
        let abort = Arc::new(AtomicBool::new(false));
        drop(AttemptGuard {
            session: session.clone(),
            attempt: 1,
            abort: abort.clone(),
        });
        let s = &*session
            .try_lock()
            .expect("guard must release synchronously");
        assert!(abort.load(Ordering::SeqCst));
        assert!(s.active_compression_attempt.is_none());
        assert!(!s.is_compressing);
        assert!(!s.runtime.is_compressing);
        assert!(!compression_attempt_active(s));
        assert_eq!(
            s.compression_phase,
            Some(CompressionPhase::Failed),
            "a dropped attempt must reach a terminal phase"
        );
    }

    #[test]
    fn compression_attempt_ages_out_instead_of_wedging_generation() {
        use crate::compression_gate::COMPRESSION_ATTEMPT_STALE_AFTER;

        let age_ms = |ms: u64| Some(epoch_ms_now().saturating_sub(ms));

        // No reservation at all.
        assert!(!compression_attempt_active(&ChatSession::new(
            "no-attempt".into()
        )));

        // A live attempt still blocks.
        let mut live = ChatSession::new("live-attempt".into());
        live.active_compression_attempt = Some(1);
        live.compression_abort_flag = Some(Arc::new(AtomicBool::new(false)));
        live.compression_attempt_started_at_ms = age_ms(0);
        status(&mut live, CompressionPhase::Running, None);
        assert!(
            compression_attempt_active(&live),
            "a fresh reservation must keep generation blocked"
        );

        // One millisecond short of the threshold it still blocks.
        let mut almost = ChatSession::new("almost-stale".into());
        almost.active_compression_attempt = Some(1);
        almost.compression_abort_flag = Some(Arc::new(AtomicBool::new(false)));
        almost.compression_attempt_started_at_ms = age_ms(14 * 60 * 1000 + 59_000);
        status(&mut almost, CompressionPhase::Running, None);
        assert!(
            compression_attempt_active(&almost),
            "an attempt inside the threshold must keep generation blocked"
        );

        // Past the threshold the reservation can no longer be trusted to reach a
        // terminal phase, so it must stop blocking instead of wedging forever.
        let mut stale = ChatSession::new("stale-attempt".into());
        stale.active_compression_attempt = Some(1);
        stale.compression_abort_flag = Some(Arc::new(AtomicBool::new(false)));
        stale.compression_attempt_started_at_ms =
            age_ms(COMPRESSION_ATTEMPT_STALE_AFTER.as_millis() as u64);
        status(&mut stale, CompressionPhase::Running, None);
        assert!(
            !compression_attempt_active(&stale),
            "a stale reservation must not block generation forever"
        );
        assert!(
            stale.start_stream().is_some(),
            "a stale reservation must leave the session startable"
        );

        // A missing timestamp cannot be aged out: fail closed and keep blocking.
        let mut untimed = ChatSession::new("untimed-attempt".into());
        untimed.active_compression_attempt = Some(1);
        untimed.compression_abort_flag = Some(Arc::new(AtomicBool::new(false)));
        status(&mut untimed, CompressionPhase::Running, None);
        untimed.compression_attempt_started_at_ms = None;
        assert!(
            compression_attempt_active(&untimed),
            "an untimed reservation must stay active rather than age out"
        );
    }

    #[test]
    fn terminal_compression_phase_clears_the_tool_loop_guard() {
        use refact_tool_api::tool_call_loop_guard::ToolCallLoopGuard;

        let mut s = ChatSession::new("loop-guard-compression".into());
        s.tool_loop_guard = ToolCallLoopGuard::new();
        // Build up a chain of identical repeats so there is something to clear.
        s.tool_loop_guard.observe("cat\u{1}a.rs", 7, 3);
        s.tool_loop_guard.observe("cat\u{1}a.rs", 7, 3);
        assert_eq!(s.tool_loop_guard.consecutive(), 2);

        // A running phase is not finalisation: compression is still under way, and
        // clearing here would let a loop survive across a rebuild.
        status(&mut s, CompressionPhase::Running, None);
        assert_eq!(
            s.tool_loop_guard.consecutive(),
            2,
            "Running must not clear the loop guard"
        );

        for phase in [
            CompressionPhase::Applied,
            CompressionPhase::Failed,
            CompressionPhase::Skipped,
        ] {
            // Re-seed a chain. Two calls are needed: after `clear()` the first
            // observe() is a FirstCall (consecutive == 1), not yet a repeat. The
            // exact count varies with what the previous iteration left behind,
            // so assert the chain is live rather than pinning a number.
            s.tool_loop_guard.observe("cat\u{1}a.rs", 7, 3);
            s.tool_loop_guard.observe("cat\u{1}a.rs", 7, 3);
            assert!(s.tool_loop_guard.consecutive() >= 2);
            status(&mut s, phase, None);
            assert_eq!(
                s.tool_loop_guard.consecutive(),
                0,
                "{phase:?} must clear the loop guard"
            );
            assert!(s.tool_loop_guard.last_key().is_none(), "{phase:?}");
        }
    }

    #[test]
    fn idle_excludes_live_draft_and_tool_execution() {
        let mut s = ChatSession::new("rebuild-test".into());
        assert!(idle(&s));
        s.draft_message = Some(ChatMessage::new("assistant".into(), "partial".into()));
        assert!(!idle(&s));
        s.draft_message = None;
        s.runtime.state = SessionState::ExecutingTools;
        assert!(!idle(&s));
    }
}
