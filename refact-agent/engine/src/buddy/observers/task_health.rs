use chrono::Utc;

use crate::app_state::AppState;
use crate::buddy::observers::{BuddyObserver, ObserverContext};
use crate::buddy::settings::BuddySettings;
use crate::buddy::types::BuddyFact;

pub use refact_buddy_core::observers::task_health::*;

pub struct TaskHealthObserver;
#[async_trait::async_trait]
impl BuddyObserver for TaskHealthObserver {
    fn id(&self) -> &'static str {
        "task_health"
    }

    fn cadence_seconds(&self) -> u64 {
        60
    }

    fn requires_setting(&self, settings: &BuddySettings) -> bool {
        settings.observers.task_health
    }

    async fn observe(&self, gcx: AppState, ctx: &ObserverContext) -> Vec<BuddyFact> {
        let tasks = match crate::tasks::storage::list_tasks(gcx.gcx.clone()).await {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let mut entries = vec![];
        let mut memory_facts = vec![];
        for meta in tasks {
            let board = match crate::tasks::storage::load_board(gcx.gcx.clone(), &meta.id).await {
                Ok(b) => b,
                Err(_) => continue,
            };
            if let Ok(task_dir) =
                crate::tasks::storage::find_task_dir(gcx.gcx.clone(), &meta.id).await
            {
                memory_facts.extend(
                    detect_orphaned_task_memory_facts(
                        &meta.id,
                        &board,
                        &task_dir.join("memories"),
                        ctx.now,
                    )
                    .await,
                );
            }
            let mut latest_heartbeat: Option<chrono::DateTime<Utc>> = None;
            for card in &board.cards {
                if card.column == "doing" {
                    let live = if let Some(chat_id) = &card.agent_chat_id {
                        crate::chat::task_agent_monitor::get_last_agent_heartbeat(
gcx.chat_ctx(),
                            chat_id,
                        )
                        .await
                    } else {
                        None
                    };
                    let hb = live.or_else(|| {
                        card.last_heartbeat_at
                            .as_deref()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|t| t.with_timezone(&Utc))
                    });
                    if let Some(h) = hb {
                        latest_heartbeat = Some(match latest_heartbeat {
                            Some(t) if t > h => t,
                            _ => h,
                        });
                    }
                }
            }
            let touched_files: Vec<String> = board
                .cards
                .iter()
                .flat_map(|c| c.target_files.iter().cloned())
                .collect();
            entries.push(TaskHealthEntry {
                meta,
                board,
                last_heartbeat: latest_heartbeat,
                touched_files,
            });
        }
        let mut facts = detect_task_health_facts(&entries, ctx.now);
        facts.extend(memory_facts);
        facts
    }
}
