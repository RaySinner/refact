#[allow(unused_imports)]
use super::*;

pub use refact_chat_api::post_merge_check::*;

const STALE_POST_MERGE_WRITE: &str = "stale post-merge write";

pub(super) async fn store_regression_result(
    ctx: &ChatCtx,
    task_id: &str,
    card_id: &str,
    expected_card_state: Option<&ExpectedCardState>,
    command: &str,
    output_tail: &str,
    merge_commit: &str,
    revert_commit: &str,
    first_error: &str,
    revert_skipped_reason: Option<&str>,
    diagnostic: Option<&str>,
) -> Result<Option<String>, String> {
    let card_id_owned = card_id.to_string();
    let task_id_owned = task_id.to_string();
    let expected_card_state = expected_card_state.cloned();
    let command_owned = command.to_string();
    let output_owned = output_tail.to_string();
    let merge_commit_owned = merge_commit.to_string();
    let revert_commit_owned = revert_commit.to_string();
    let first_error_owned = first_error.to_string();
    let revert_skipped_reason_owned = revert_skipped_reason.map(str::to_string);
    let diagnostic_owned = diagnostic.map(str::to_string);
    let result = storage::update_board_atomic_for_ctx(ctx, task_id, move |board| {
        let Some(source_card_ref) = board.get_card(&card_id_owned) else {
            if expected_card_state.is_some() {
                return Err(format!(
                    "{} for task {} card {}: card no longer exists",
                    STALE_POST_MERGE_WRITE, task_id_owned, card_id_owned
                ));
            }
            return Err(format!("Card {} not found", card_id_owned));
        };
        if let Some(expected) = expected_card_state.as_ref() {
            if !expected.matches_board_card(board.rev, source_card_ref) {
                return Err(format!(
                    "{} for task {} card {}: current card state no longer matches post-merge request",
                    STALE_POST_MERGE_WRITE, task_id_owned, card_id_owned
                ));
            }
        }
        let source_card = source_card_ref.clone();
        ensure_regressed_column(board);
        let fix_card_id = next_fix_card_id(board, &card_id_owned);
        let now = Utc::now().to_rfc3339();
        let original = board
            .get_card_mut(&card_id_owned)
            .ok_or_else(|| format!("Card {} not found", card_id_owned))?;
        original.column = "regressed".to_string();
        original.status_updates.push(StatusUpdate {
            timestamp: now.clone(),
            message: match revert_skipped_reason_owned.as_deref() {
                Some(reason) => format!("Auto-revert skipped: {}", reason),
                None => format!("Auto-reverted: {}", first_error_owned),
            },
        });
        board.cards.push(build_fix_card(
            &source_card,
            &fix_card_id,
            &command_owned,
            &output_owned,
            &merge_commit_owned,
            &revert_commit_owned,
            revert_skipped_reason_owned.as_deref(),
            diagnostic_owned.as_deref(),
            &now,
        ));
        Ok(fix_card_id)
    })
    .await;
    let fix_card_id = match result {
        Ok((_, fix_card_id)) => fix_card_id,
        Err(error) if error.starts_with(STALE_POST_MERGE_WRITE) => {
            tracing::info!("{}", error);
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let _ = storage::update_task_stats_for_ctx(ctx, task_id).await;
    Ok(Some(fix_card_id))
}
