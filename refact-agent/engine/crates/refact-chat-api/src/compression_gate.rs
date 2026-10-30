use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::types::{ChatSession, CompressionPhase};

/// How long an attempt may stay "active" before the gate treats it as stale.
///
/// Also the point at which a user waiting on that attempt deserves to be told
/// something is wrong, so `queue.rs` reuses this exact threshold instead of
/// inventing a second one.
pub const COMPRESSION_ATTEMPT_STALE_AFTER: Duration = Duration::from_secs(15 * 60);

/// A missing timestamp cannot be aged out, so it stays active and fails closed.
fn compression_attempt_is_stale(session: &ChatSession) -> bool {
    let Some(started_at_ms) = session.compression_attempt_started_at_ms else {
        return false;
    };
    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    now_ms.saturating_sub(started_at_ms) >= COMPRESSION_ATTEMPT_STALE_AFTER.as_millis() as u64
}

pub fn compression_attempt_active(session: &ChatSession) -> bool {
    // Guard cancellation is synchronous even if same-owner status cleanup awaits a lock.
    !session
        .compression_abort_flag
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::SeqCst))
        && !compression_attempt_is_stale(session)
        && (session.active_compression_attempt.is_some()
            || session.is_compressing
            || session.runtime.is_compressing
            || matches!(
                session.compression_phase,
                Some(CompressionPhase::Checking | CompressionPhase::Running)
            )
            || matches!(
                session.runtime.compression_phase,
                Some(CompressionPhase::Checking | CompressionPhase::Running)
            ))
}
