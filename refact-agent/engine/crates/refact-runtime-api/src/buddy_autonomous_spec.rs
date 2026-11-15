pub use refact_buddy_core::jobs::autonomous_chats::spec::*;

use crate::buddy_scheduler::{BuddyJobContext, BuddyJobResult};

#[cfg_attr(not(test), allow(dead_code))]
pub fn same_signal(ctx: &BuddyJobContext, hash: &str) -> bool {
    parse_last_autonomous_result(ctx.job_state.last_result.as_deref())
        .map(|last| last.signal_hash == hash && last.status.as_deref() != Some("failed"))
        .unwrap_or(false)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn same_signal_or_failed_backoff(
    ctx: &BuddyJobContext,
    hash: &str,
    cooldown_seconds: u64,
) -> bool {
    parse_last_autonomous_result(ctx.job_state.last_result.as_deref())
        .map(|last| {
            if last.status.as_deref() == Some("failed") {
                return failure_backoff_active(&last, cooldown_seconds);
            }
            last.signal_hash == hash
        })
        .unwrap_or(false)
}

pub fn preserve_last_result(ctx: &BuddyJobContext) -> BuddyJobResult {
    BuddyJobResult {
        last_result: ctx.job_state.last_result.clone(),
        ..Default::default()
    }
}
