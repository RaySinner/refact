pub use refact_http::routers::v1::sidebar::*;

#[cfg(test)]
#[allow(unused_imports)]
use std::collections::VecDeque;
#[cfg(test)]
#[allow(unused_imports)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
#[allow(unused_imports)]
use serde_json::Value;
#[cfg(test)]
#[allow(unused_imports)]
use tokio::sync::mpsc;
#[cfg(test)]
#[allow(unused_imports)]
use refact_tasks::events::TaskEvent;

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_part(section: SidebarSection, status: SidebarSectionStatus) -> InitialSidebarPart {
        let error = (status == SidebarSectionStatus::Error).then(|| "boom".to_string());
        match section {
            SidebarSection::Workspace => InitialSidebarPart::Workspace {
                workspace_roots: Vec::new(),
                status,
                error,
            },
            SidebarSection::Chats => InitialSidebarPart::Chats {
                trajectories: Vec::new(),
                pagination: None,
                status,
                error,
            },
            SidebarSection::Tasks => InitialSidebarPart::Tasks {
                tasks: Vec::new(),
                status,
                error,
            },
            SidebarSection::Buddy => InitialSidebarPart::Buddy {
                buddy: serde_json::Value::Null,
                status,
                error,
            },
        }
    }

    fn buffered_notification() -> SidebarEvent {
        SidebarEvent::Notification {
            notification: NotificationEvent::TaskDone {
                chat_id: "chat".to_string(),
                tool_call_id: "tool".to_string(),
                summary: "done".to_string(),
                knowledge_path: None,
            },
        }
    }

    #[test]
    fn sidebar_event_envelope_uses_v2_nested_event_shape() {
        let seq = AtomicU64::new(0);
        let raw = make_event(
            &seq,
            "sub-1",
            section_snapshot_event(
                SidebarSection::Tasks,
                SidebarSectionStatus::Ready,
                SidebarSectionSnapshot::Tasks { tasks: Vec::new() },
                Some(7),
                None,
            ),
        )
        .expect("event should serialize");
        let json = raw
            .trim_start_matches("data: ")
            .trim()
            .parse::<serde_json::Value>()
            .expect("valid json");

        assert_eq!(json["protocol_version"], 2);
        assert_eq!(json["seq"], 0);
        assert_eq!(json["subscription_id"], "sub-1");
        assert_eq!(json["event"]["type"], "section_snapshot");
        assert_eq!(json["event"]["section"], "tasks");
        assert_eq!(json["event"]["status"], "ready");
        assert_eq!(
            json["event"]["snapshot"]["tasks"].as_array().unwrap().len(),
            0
        );
        assert!(json.get("category").is_none());
    }

    #[test]
    fn sidebar_error_snapshot_is_terminal_and_carries_empty_data() {
        let event = section_snapshot_event(
            SidebarSection::Chats,
            SidebarSectionStatus::Error,
            SidebarSectionSnapshot::Chats {
                trajectories: Vec::new(),
                pagination: None,
            },
            None,
            Some("boom".to_string()),
        );
        let json = serde_json::to_value(event).expect("event should serialize");

        assert_eq!(json["type"], "section_snapshot");
        assert_eq!(json["section"], "chats");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"], "boom");
        assert_eq!(
            json["snapshot"]["trajectories"].as_array().unwrap().len(),
            0
        );
    }

    #[test]
    fn sidebar_bootstrap_settles_for_all_ready_error_combinations_and_flushes() {
        let statuses = [SidebarSectionStatus::Ready, SidebarSectionStatus::Error];

        for workspace_status in statuses {
            for chats_status in statuses {
                for tasks_status in statuses {
                    for buddy_status in statuses {
                        let mut workspace_settled = false;
                        let mut chats_settled = false;
                        let mut tasks_settled = false;
                        let mut buddy_settled = false;

                        for part in [
                            empty_part(SidebarSection::Workspace, workspace_status),
                            empty_part(SidebarSection::Chats, chats_status),
                            empty_part(SidebarSection::Tasks, tasks_status),
                            empty_part(SidebarSection::Buddy, buddy_status),
                        ] {
                            assert!(matches!(
                                part.status(),
                                SidebarSectionStatus::Ready | SidebarSectionStatus::Error
                            ));
                            mark_section_settled(
                                part.section(),
                                &mut workspace_settled,
                                &mut chats_settled,
                                &mut tasks_settled,
                                &mut buddy_settled,
                            );
                        }

                        let (_tx, rx) = mpsc::unbounded_channel();
                        let mut initial_rx = Some(rx);
                        let mut bootstrap_complete = false;
                        let mut buffered_live_events = VecDeque::from([buffered_notification()]);
                        let seq = AtomicU64::new(0);
                        let emitted = try_finish_bootstrap(
                            workspace_settled,
                            chats_settled,
                            tasks_settled,
                            buddy_settled,
                            &mut bootstrap_complete,
                            &mut initial_rx,
                            &mut buffered_live_events,
                            &seq,
                            "sub-1",
                        );

                        assert!(bootstrap_complete);
                        assert!(initial_rx.is_none());
                        assert!(buffered_live_events.is_empty());
                        assert_eq!(emitted.len(), 1);
                        assert_eq!(seq.load(Ordering::SeqCst), 1);
                    }
                }
            }
        }
    }

    #[test]
    fn settled_section_live_updates_flush_before_full_bootstrap() {
        let mut initial_rx = Some(mpsc::unbounded_channel().1);
        let mut bootstrap_complete = false;
        let mut buffered_live_events = VecDeque::from([
            SidebarEvent::SectionUpdate {
                section: SidebarSection::Tasks,
                update: SidebarSectionUpdate::Task(TaskEvent::BoardChanged {
                    task_id: "task-1".to_string(),
                    rev: 2,
                }),
            },
            buffered_notification(),
        ]);
        let seq = AtomicU64::new(0);

        let emitted = try_finish_bootstrap(
            false,
            false,
            true,
            false,
            &mut bootstrap_complete,
            &mut initial_rx,
            &mut buffered_live_events,
            &seq,
            "sub-1",
        );

        assert!(!bootstrap_complete);
        assert!(initial_rx.is_some());
        assert_eq!(emitted.len(), 1);
        assert_eq!(seq.load(Ordering::SeqCst), 1);
        assert_eq!(buffered_live_events.len(), 1);
        assert!(matches!(
            buffered_live_events.front(),
            Some(SidebarEvent::Notification { .. })
        ));
    }

    #[test]
    fn sidebar_stale_initial_part_generation_is_ignored() {
        let stale = InitialSidebarPartMessage {
            generation: 1,
            part: empty_part(SidebarSection::Workspace, SidebarSectionStatus::Ready),
        };
        let current = InitialSidebarPartMessage {
            generation: 2,
            part: empty_part(SidebarSection::Workspace, SidebarSectionStatus::Error),
        };

        assert!(current_initial_sidebar_part(stale, 2).is_none());
        let part = current_initial_sidebar_part(current, 2).expect("current generation accepted");

        assert_eq!(part.section(), SidebarSection::Workspace);
        assert_eq!(part.status(), SidebarSectionStatus::Error);
    }
}

