pub use refact_runtime_api::buddy_scheduler::*;

pub fn default_buddy_jobs() -> Vec<Box<dyn BuddyJob>> {
    vec![
        Box::new(super::jobs::greeting::GreetingJob) as Box<dyn BuddyJob>,
        Box::new(super::jobs::tour::TourJob),
        Box::new(refact_runtime_api::buddy_jobs::error_triage::ErrorTriageJob),
        Box::new(refact_runtime_api::buddy_jobs::config_watcher::ConfigWatcherJob),
        Box::new(super::jobs::stats_watcher::StatsWatcherJob),
        Box::new(refact_runtime_api::buddy_jobs::health_watcher::HealthWatcherJob),
        Box::new(super::jobs::autonomous_chats::BuddyMemoryGardenerJob),
        Box::new(super::jobs::autonomous_chats::BuddyKnowledgeConflictResolverJob),
        Box::new(super::jobs::autonomous_chats::BuddyBehaviorLearnerJob),
        Box::new(super::jobs::autonomous_chats::BuddyUserHabitCoachJob),
        Box::new(super::jobs::autonomous_chats::BuddyModelCostOptimizerJob),
        Box::new(super::jobs::quest_prompt::QuestPromptJob),
        Box::new(super::jobs::autonomous_chats::ErrorDetectiveJob),
        Box::new(super::jobs::refact_compile_sniffer::RefactCompileSnifferJob),
        Box::new(super::jobs::refact_self_critic::RefactSelfCriticJob),
        Box::new(super::jobs::buddy_daily_digest::BuddyDailyDigestJob),
        Box::new(refact_runtime_api::buddy_jobs::buddy_briefing::BuddyBriefingJob),
        Box::new(refact_runtime_api::buddy_jobs::buddy_handbook_distiller::BuddyHandbookDistillerJob),
        Box::new(super::jobs::buddy_friday_retro::BuddyFridayRetroJob),
        Box::new(super::jobs::chat_phrase_bank::ChatPhraseBankJob),
        Box::new(super::jobs::buddy_idle_suggester::BuddyIdleSuggesterJob),
        Box::new(super::jobs::buddy_pr_issue_matchmaker::BuddyPrIssueMatchmakerJob),
        Box::new(super::jobs::autonomous_chats::SecurityWhispererJob),
        Box::new(super::jobs::autonomous_chats::SetupCoachJob),
        Box::new(super::jobs::autonomous_chats::DependencyRadarJob),
        Box::new(super::jobs::autonomous_chats::DocsGardenerJob),
        Box::new(super::jobs::autonomous_chats::ArchitectureDriftWatcherJob),
        Box::new(super::jobs::buddy_onboarding::BuddyOnboardingJob),
        Box::new(super::jobs::buddy_refactor_hunter::BuddyRefactorHunterJob),
        Box::new(super::jobs::buddy_skill_author::BuddySkillAuthorJob),
        Box::new(super::jobs::buddy_test_coverage_watcher::BuddyTestCoverageWatcherJob),
        Box::new(refact_runtime_api::buddy_jobs::proactive_suggestions::ProactiveSuggestionsJob),
        Box::new(super::jobs::speaker_insight::SpeakerInsightJob),
        Box::new(super::jobs::speaker_win::SpeakerWinJob),
        Box::new(super::jobs::speaker_memory_pulse_commentary::SpeakerMemoryPulseCommentaryJob),
    ]
}

#[cfg(test)]
fn job_ids(scheduler: &BuddyScheduler) -> Vec<String> {
    scheduler.jobs.iter().map(|job| job.id().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex as AMutex;

    use crate::app_state::AppState;
    use crate::buddy::actor::BuddyService;
    use crate::buddy::autonomous_workflows::AUTONOMOUS_BUDDY_WORKFLOWS;
    use crate::buddy::settings::BuddySettings;
    use crate::buddy::types::{
        BuddyActivity, BuddyBubblePolicy, BuddyJobState, BuddyRuntimeEvent, BuddySpeechItem,
        BuddySuggestion,
    };
    use crate::buddy::voice_service::{SpeechIntent, SpeechIntentWireToken};

    struct NoOutputUnrecordedJob {
        last_result: Option<String>,
    }

    struct NoOutputRecordedJob {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    struct ReadyAutonomousJob {
        id: String,
    }

    #[async_trait::async_trait]
    impl BuddyJob for NoOutputUnrecordedJob {
        fn id(&self) -> &str {
            "no_output_unrecorded"
        }

        fn cooldown_seconds(&self) -> u64 {
            0
        }

        fn priority(&self) -> u32 {
            0
        }

        fn records_empty_result(&self) -> bool {
            false
        }

        async fn should_run(&self, _gcx: AppState, _ctx: &BuddyJobContext) -> bool {
            true
        }

        async fn execute(&self, _gcx: AppState, _ctx: BuddyJobContext) -> BuddyJobResult {
            BuddyJobResult {
                last_result: self.last_result.clone(),
                ..Default::default()
            }
        }
    }

    #[async_trait::async_trait]
    impl BuddyJob for NoOutputRecordedJob {
        fn id(&self) -> &str {
            "no_output_recorded"
        }

        fn cooldown_seconds(&self) -> u64 {
            60 * 60
        }

        fn priority(&self) -> u32 {
            0
        }

        fn records_empty_result(&self) -> bool {
            true
        }

        async fn should_run(&self, _gcx: AppState, _ctx: &BuddyJobContext) -> bool {
            true
        }

        async fn execute(&self, _gcx: AppState, _ctx: BuddyJobContext) -> BuddyJobResult {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            BuddyJobResult::default()
        }
    }

    #[async_trait::async_trait]
    impl BuddyJob for ReadyAutonomousJob {
        fn id(&self) -> &str {
            &self.id
        }

        fn cooldown_seconds(&self) -> u64 {
            0
        }

        fn priority(&self) -> u32 {
            0
        }

        fn is_autonomous(&self) -> bool {
            true
        }

        async fn should_run(&self, _gcx: AppState, _ctx: &BuddyJobContext) -> bool {
            true
        }

        async fn execute(&self, _gcx: AppState, _ctx: BuddyJobContext) -> BuddyJobResult {
            BuddyJobResult {
                xp: 4,
                activity: Some(BuddyActivity {
                    icon: "•".to_string(),
                    title: self.id.clone(),
                    description: self.id.clone(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    activity_type: "test".to_string(),
                    chat_id: None,
                    failure_category: None,
                    failure_summary: None,
                }),
                ..Default::default()
            }
        }
    }

    struct NoIntentSpeechJob;

    #[async_trait::async_trait]
    impl BuddyJob for NoIntentSpeechJob {
        fn id(&self) -> &str {
            "no_intent_speech"
        }

        fn cooldown_seconds(&self) -> u64 {
            0
        }

        fn priority(&self) -> u32 {
            0
        }

        async fn should_run(&self, _gcx: AppState, _ctx: &BuddyJobContext) -> bool {
            true
        }

        async fn execute(&self, _gcx: AppState, _ctx: BuddyJobContext) -> BuddyJobResult {
            BuddyJobResult {
                speech: Some(BuddySpeechItem {
                    id: "no-intent-speech".to_string(),
                    text: "no intent speech".to_string(),
                    mood: "happy".to_string(),
                    scope: "global".to_string(),
                    persistent: false,
                    ttl_seconds: 10,
                    dedupe_key: Some("no-intent-speech".to_string()),
                    speech_intent: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    controls: vec![],
                    chat_id: None,
                }),
                speech_intent: None,
                ..Default::default()
            }
        }
    }

    struct SpeechJob {
        id: String,
        priority: u32,
        intent: SpeechIntent,
        activity_title: String,
        runtime_event: bool,
    }

    #[async_trait::async_trait]
    impl BuddyJob for SpeechJob {
        fn id(&self) -> &str {
            &self.id
        }

        fn cooldown_seconds(&self) -> u64 {
            0
        }

        fn priority(&self) -> u32 {
            self.priority
        }

        async fn should_run(&self, _gcx: AppState, _ctx: &BuddyJobContext) -> bool {
            true
        }

        async fn execute(&self, _gcx: AppState, _ctx: BuddyJobContext) -> BuddyJobResult {
            BuddyJobResult {
                speech_intent: Some(self.intent),
                speech: Some(BuddySpeechItem {
                    id: format!("speech-{}", self.id),
                    text: format!("speech {}", self.id),
                    mood: "happy".to_string(),
                    scope: "global".to_string(),
                    persistent: false,
                    ttl_seconds: 10,
                    dedupe_key: Some(format!("speech-{}", self.id)),
                    speech_intent: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    controls: vec![],
                    chat_id: None,
                }),
                activity: Some(BuddyActivity {
                    icon: "•".to_string(),
                    title: self.activity_title.to_string(),
                    description: self.activity_title.to_string(),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    activity_type: "test".to_string(),
                    chat_id: None,
                    failure_category: None,
                    failure_summary: None,
                }),
                runtime_event: self.runtime_event.then(|| {
                    let title = format!("runtime {}", self.id);
                    let mut event = crate::buddy::actor::make_runtime_event(
                        "test_signal",
                        &title,
                        self.id(),
                        &format!("runtime-{}", self.id),
                        "completed",
                        None,
                    );
                    event.speech_text = Some(format!("runtime speech {}", self.id));
                    event
                }),
                ..Default::default()
            }
        }
    }

    async fn test_service(
        dir: &tempfile::TempDir,
        state: crate::buddy::types::BuddyState,
    ) -> Arc<AMutex<Option<BuddyService>>> {
        let (tx, _) = tokio::sync::broadcast::channel(16);
        Arc::new(AMutex::new(Some(BuddyService::new(
            dir.path().to_path_buf(),
            state,
            BuddySettings::default(),
            Vec::new(),
            crate::buddy::runtime_queue::RuntimeQueue::new(),
            tx,
            None,
        ))))
    }

    #[tokio::test]
    async fn speech_without_intent_is_not_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let scheduler = BuddyScheduler::new(vec![Box::new(NoIntentSpeechJob)]);
        let buddy_arc = test_service(&dir, crate::buddy::state::default_buddy_state()).await;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler.tick(gcx, buddy_arc.clone(), dir.path()).await;

        let buddy = buddy_arc.lock().await;
        let service = buddy.as_ref().unwrap();
        assert!(service.active_speech.is_some());
    }

    fn speech_item(intent: SpeechIntent) -> BuddySpeechItem {
        BuddySpeechItem {
            id: format!("speech-{}", crate::buddy::speech_policy::intent_key(intent)),
            text: "test speech".to_string(),
            mood: intent.mood().to_string(),
            scope: "global".to_string(),
            persistent: false,
            ttl_seconds: 10,
            dedupe_key: None,
            speech_intent: Some(intent.wire_token().to_string()),
            created_at: chrono::Utc::now().to_rfc3339(),
            controls: vec![],
            chat_id: None,
        }
    }

    #[test]
    fn speech_runtime_event_sets_ambient_policy_for_humor() {
        let event = speech_runtime_event(
            "test_job",
            SpeechIntent::Humor,
            &speech_item(SpeechIntent::Humor),
            "Humor".to_string(),
            None,
        );

        assert_eq!(event.bubble_policy, Some(BuddyBubblePolicy::Ambient));
    }

    #[test]
    fn speech_runtime_event_sets_durable_policy_for_greeting() {
        let event = speech_runtime_event(
            "test_job",
            SpeechIntent::Greeting,
            &speech_item(SpeechIntent::Greeting),
            "Greeting".to_string(),
            None,
        );

        assert_eq!(event.bubble_policy, Some(BuddyBubblePolicy::Durable));
    }

    #[tokio::test]
    async fn competing_speech_intents_pick_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let scheduler = BuddyScheduler::new(vec![
            Box::new(SpeechJob {
                id: "speech_one".to_string(),
                priority: 0,
                intent: SpeechIntent::Humor,
                activity_title: "one activity".to_string(),
                runtime_event: false,
            }),
            Box::new(SpeechJob {
                id: "speech_two".to_string(),
                priority: 1,
                intent: SpeechIntent::ErrorAlert,
                activity_title: "two activity".to_string(),
                runtime_event: false,
            }),
        ]);
        let buddy_arc = test_service(&dir, crate::buddy::state::default_buddy_state()).await;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler.tick(gcx, buddy_arc.clone(), dir.path()).await;

        let buddy = buddy_arc.lock().await;
        let service = buddy.as_ref().unwrap();
        let active_speech = service.active_speech.as_ref().unwrap();
        assert_eq!(active_speech.text, "speech speech_two");
        assert_eq!(active_speech.speech_intent.as_deref(), Some("error_alert"));
    }

    #[tokio::test]
    async fn humor_caps_at_5_per_hour() {
        let dir = tempfile::tempdir().unwrap();
        let scheduler = BuddyScheduler::new(
            (0..6)
                .map(|idx| {
                    Box::new(SpeechJob {
                        id: format!("humor_{idx}"),
                        priority: idx,
                        intent: SpeechIntent::Humor,
                        activity_title: "humor activity".to_string(),
                        runtime_event: false,
                    }) as Box<dyn BuddyJob>
                })
                .collect(),
        );
        let buddy_arc = test_service(&dir, crate::buddy::state::default_buddy_state()).await;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        for _ in 0..6 {
            scheduler
                .tick(gcx.clone(), buddy_arc.clone(), dir.path())
                .await;
            let mut buddy = buddy_arc.lock().await;
            let service = buddy.as_mut().unwrap();
            for job_id in service
                .state
                .job_cooldowns
                .keys()
                .cloned()
                .collect::<Vec<_>>()
            {
                if job_id.starts_with("humor_") {
                    service.state.job_cooldowns.remove(&job_id);
                }
            }
        }

        let buddy = buddy_arc.lock().await;
        let rotation = &buddy.as_ref().unwrap().state.speech_rotation;
        let state = rotation
            .by_intent
            .get(crate::buddy::speech_policy::intent_key(SpeechIntent::Humor))
            .unwrap();
        assert_eq!(state.hour_count, 5);
    }

    #[tokio::test]
    async fn scheduler_emits_one_speech_per_tick_but_other_outputs_apply() {
        let dir = tempfile::tempdir().unwrap();
        let scheduler = BuddyScheduler::new(vec![
            Box::new(SpeechJob {
                id: "speech_low".to_string(),
                priority: 0,
                intent: SpeechIntent::Humor,
                activity_title: "low activity".to_string(),
                runtime_event: false,
            }),
            Box::new(SpeechJob {
                id: "speech_high".to_string(),
                priority: 1,
                intent: SpeechIntent::ErrorAlert,
                activity_title: "high activity".to_string(),
                runtime_event: false,
            }),
        ]);
        let buddy_arc = test_service(&dir, crate::buddy::state::default_buddy_state()).await;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler.tick(gcx, buddy_arc.clone(), dir.path()).await;

        let buddy = buddy_arc.lock().await;
        let service = buddy.as_ref().unwrap();
        assert_eq!(
            service.active_speech.as_ref().unwrap().text,
            "speech speech_high"
        );
        assert_eq!(service.state.recent_activities.len(), 2);
        let rotation = &service.state.speech_rotation;
        assert!(rotation
            .by_intent
            .contains_key(crate::buddy::speech_policy::intent_key(
                SpeechIntent::ErrorAlert
            )));
        assert!(!rotation
            .by_intent
            .contains_key(crate::buddy::speech_policy::intent_key(SpeechIntent::Humor)));
    }

    #[tokio::test]
    async fn scheduler_preserves_runtime_events_from_unselected_speech_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let scheduler = BuddyScheduler::new(vec![
            Box::new(SpeechJob {
                id: "speech_low".to_string(),
                priority: 0,
                intent: SpeechIntent::Humor,
                activity_title: "low activity".to_string(),
                runtime_event: true,
            }),
            Box::new(SpeechJob {
                id: "speech_high".to_string(),
                priority: 1,
                intent: SpeechIntent::ErrorAlert,
                activity_title: "high activity".to_string(),
                runtime_event: true,
            }),
        ]);
        let buddy_arc = test_service(&dir, crate::buddy::state::default_buddy_state()).await;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler.tick(gcx, buddy_arc.clone(), dir.path()).await;

        let buddy = buddy_arc.lock().await;
        let service = buddy.as_ref().unwrap();
        assert_eq!(
            service.active_speech.as_ref().unwrap().text,
            "speech speech_high"
        );
        assert_eq!(service.runtime_queue.items.len(), 2);
        assert!(service
            .runtime_queue
            .items
            .iter()
            .any(|event| event.title == "runtime speech_low"
                && event.speech_text.as_deref() == Some("runtime speech speech_low")));
        assert!(service
            .runtime_queue
            .items
            .iter()
            .any(|event| event.title == "runtime speech_high"
                && event.speech_text.as_deref() == Some("runtime speech speech_high")));
    }

    #[tokio::test]
    async fn speech_rotation_allows_win_when_error_over_budget() {
        let dir = tempfile::tempdir().unwrap();
        let scheduler = BuddyScheduler::new(vec![
            Box::new(SpeechJob {
                id: "speech_error".to_string(),
                priority: 0,
                intent: SpeechIntent::ErrorAlert,
                activity_title: "error activity".to_string(),
                runtime_event: false,
            }),
            Box::new(SpeechJob {
                id: "speech_win".to_string(),
                priority: 1,
                intent: SpeechIntent::Win,
                activity_title: "win activity".to_string(),
                runtime_event: false,
            }),
        ]);
        let mut state = crate::buddy::state::default_buddy_state();
        let now = chrono::Utc::now();
        state.speech_rotation.by_intent.insert(
            crate::buddy::speech_policy::intent_key(SpeechIntent::ErrorAlert).to_string(),
            crate::buddy::state::IntentBudgetState {
                last_emitted_at: Some(now),
                hour_count: 2,
                day_count: 2,
                hour_window_start: Some(now),
                day_window_start: Some(now),
            },
        );
        let buddy_arc = test_service(&dir, state).await;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler.tick(gcx, buddy_arc.clone(), dir.path()).await;

        let buddy = buddy_arc.lock().await;
        let service = buddy.as_ref().unwrap();
        assert_eq!(
            service.active_speech.as_ref().unwrap().text,
            "speech speech_win"
        );
        assert!(service
            .state
            .speech_rotation
            .by_intent
            .contains_key(crate::buddy::speech_policy::intent_key(SpeechIntent::Win)));
    }

    #[test]
    fn next_last_result_preserves_existing_when_job_returns_none() {
        assert_eq!(
            next_last_result(Some("existing-json"), None).as_deref(),
            Some("existing-json")
        );
        assert_eq!(
            next_last_result(Some("existing-json"), Some("new-json")).as_deref(),
            Some("new-json")
        );
        assert_eq!(next_last_result(None, None), None);
    }

    #[tokio::test]
    async fn unrecorded_no_output_result_does_not_advance_job_state() {
        let dir = tempfile::tempdir().unwrap();
        let job_id = "no_output_unrecorded".to_string();
        let mut state = crate::buddy::state::default_buddy_state();
        state.job_cooldowns.insert(
            job_id.clone(),
            BuddyJobState {
                last_run: None,
                last_result: Some("existing-json".to_string()),
                run_count: 7,
                snoozed_until: None,
                dismissed: false,
            },
        );
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let service = BuddyService::new(
            dir.path().to_path_buf(),
            state,
            BuddySettings::default(),
            Vec::new(),
            crate::buddy::runtime_queue::RuntimeQueue::new(),
            tx,
            None,
        );
        let scheduler = BuddyScheduler::new(vec![Box::new(NoOutputUnrecordedJob {
            last_result: None,
        })]);
        let buddy_arc = Arc::new(AMutex::new(Some(service)));
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler.tick(gcx, buddy_arc.clone(), dir.path()).await;

        let buddy = buddy_arc.lock().await;
        let job_state = buddy
            .as_ref()
            .unwrap()
            .state
            .job_cooldowns
            .get(&job_id)
            .unwrap();
        assert!(job_state.last_run.is_none());
        assert_eq!(job_state.run_count, 7);
        assert_eq!(job_state.last_result.as_deref(), Some("existing-json"));
    }

    #[tokio::test]
    async fn no_output_last_result_advances_job_state_for_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let job_id = "no_output_unrecorded".to_string();
        let mut state = crate::buddy::state::default_buddy_state();
        state.job_cooldowns.insert(
            job_id.clone(),
            BuddyJobState {
                last_run: None,
                last_result: Some("existing-json".to_string()),
                run_count: 7,
                snoozed_until: None,
                dismissed: false,
            },
        );
        let scheduler = BuddyScheduler::new(vec![Box::new(NoOutputUnrecordedJob {
            last_result: Some("failed:today".to_string()),
        })]);
        let buddy_arc = test_service(&dir, state).await;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler.tick(gcx, buddy_arc.clone(), dir.path()).await;

        let buddy = buddy_arc.lock().await;
        let job_state = buddy
            .as_ref()
            .unwrap()
            .state
            .job_cooldowns
            .get(&job_id)
            .unwrap();
        assert!(job_state.last_run.is_some());
        assert_eq!(job_state.run_count, 8);
        assert_eq!(job_state.last_result.as_deref(), Some("failed:today"));
    }

    #[tokio::test]
    async fn recorded_no_output_result_advances_cooldown_and_blocks_next_tick() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::buddy::state::default_buddy_state();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let scheduler = BuddyScheduler::new(vec![Box::new(NoOutputRecordedJob {
            calls: calls.clone(),
        })]);
        let buddy_arc = test_service(&dir, state).await;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler
            .tick(gcx.clone(), buddy_arc.clone(), dir.path())
            .await;
        scheduler.tick(gcx, buddy_arc.clone(), dir.path()).await;

        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let buddy = buddy_arc.lock().await;
        let job_state = buddy
            .as_ref()
            .unwrap()
            .state
            .job_cooldowns
            .get("no_output_recorded")
            .unwrap();
        assert!(job_state.last_run.is_some());
        assert_eq!(job_state.run_count, 1);
    }

    #[tokio::test]
    async fn failed_phrase_bank_generation_records_scheduler_backoff() {
        let (service, renderer) = crate::buddy::voice_service::test_voice_service_with_responses(
            vec![Some("humor: only one usable kind".to_string())],
        );
        let _guard = crate::buddy::voice_service::install_test_voice_service(service).await;
        let dir = tempfile::tempdir().unwrap();
        let mut settings = BuddySettings::default();
        settings.daily_digest_hour = Some(0);
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let service = BuddyService::new(
            dir.path().to_path_buf(),
            crate::buddy::state::default_buddy_state(),
            settings,
            Vec::new(),
            crate::buddy::runtime_queue::RuntimeQueue::new(),
            tx,
            None,
        );
        let scheduler = BuddyScheduler::new(vec![Box::new(
            crate::buddy::jobs::chat_phrase_bank::ChatPhraseBankJob,
        )]);
        let buddy_arc = Arc::new(AMutex::new(Some(service)));
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler
            .tick(gcx.clone(), buddy_arc.clone(), dir.path())
            .await;
        scheduler.tick(gcx, buddy_arc.clone(), dir.path()).await;

        assert_eq!(renderer.calls(), 1);
        let buddy = buddy_arc.lock().await;
        let job_state = buddy
            .as_ref()
            .unwrap()
            .state
            .job_cooldowns
            .get("chat_phrase_bank")
            .unwrap();
        assert!(job_state.last_run.is_some());
        assert_eq!(job_state.run_count, 1);
        assert!(job_state
            .last_result
            .as_deref()
            .is_some_and(|result| result.starts_with("failed:")));
    }

    #[tokio::test]
    async fn tick_caps_autonomous_jobs_at_two() {
        let dir = tempfile::tempdir().unwrap();
        let scheduler = BuddyScheduler::new(
            (0..4)
                .map(|idx| {
                    Box::new(ReadyAutonomousJob {
                        id: format!("autonomous_{idx}"),
                    }) as Box<dyn BuddyJob>
                })
                .collect(),
        );
        let buddy_arc = test_service(&dir, crate::buddy::state::default_buddy_state()).await;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler.tick(gcx, buddy_arc.clone(), dir.path()).await;

        let buddy = buddy_arc.lock().await;
        let job_cooldowns = &buddy.as_ref().unwrap().state.job_cooldowns;
        assert_eq!(job_cooldowns.len(), MAX_AUTONOMOUS_JOBS_PER_TICK);
        assert!(job_cooldowns.contains_key("autonomous_0"));
        assert!(job_cooldowns.contains_key("autonomous_1"));
        assert!(!job_cooldowns.contains_key("autonomous_2"));
        assert!(!job_cooldowns.contains_key("autonomous_3"));
    }

    #[tokio::test]
    async fn autonomous_job_success_grants_bounded_xp() {
        let dir = tempfile::tempdir().unwrap();
        let scheduler = BuddyScheduler::new(vec![Box::new(ReadyAutonomousJob {
            id: "autonomous_xp".to_string(),
        })]);
        let buddy_arc = test_service(&dir, crate::buddy::state::default_buddy_state()).await;
        let gcx = crate::app_state::app_state_from_gcx(crate::global_context::tests::make_test_gcx().await).await;

        scheduler
            .tick(gcx.clone(), buddy_arc.clone(), dir.path())
            .await;

        {
            let buddy = buddy_arc.lock().await;
            let service = buddy.as_ref().unwrap();
            assert_eq!(service.state.progression.xp, 4);
            assert_eq!(service.state.progression.stage, 0);
        }

        for _ in 0..4 {
            {
                let mut buddy = buddy_arc.lock().await;
                buddy
                    .as_mut()
                    .unwrap()
                    .state
                    .job_cooldowns
                    .remove("autonomous_xp");
            }
            scheduler
                .tick(gcx.clone(), buddy_arc.clone(), dir.path())
                .await;
        }

        let buddy = buddy_arc.lock().await;
        let service = buddy.as_ref().unwrap();
        assert_eq!(service.state.progression.stage, 1);
        assert_eq!(service.state.progression.xp, 0);
    }

    fn active_suggestion(idx: usize) -> BuddySuggestion {
        BuddySuggestion {
            id: format!("suggestion-{idx}"),
            suggestion_type: "test".to_string(),
            title: format!("Suggestion {idx}"),
            description: "Test".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            dismissed: false,
            controls: vec![],
            quest: None,
        }
    }

    #[test]
    fn result_after_suggestion_policy_removes_only_suggestion_output() {
        let mut settings = BuddySettings::default();
        settings.proactive_enabled = false;
        let result = BuddyJobResult {
            suggestion: Some(active_suggestion(1)),
            runtime_event: Some(BuddyRuntimeEvent {
                id: "event".to_string(),
                signal_type: "health".to_string(),
                title: "Health".to_string(),
                description: None,
                source: "test".to_string(),
                status: "failed".to_string(),
                failure_category: None,
                failure_summary: None,
                progress: None,
                dedupe_key: None,
                priority: "normal".to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                ttl_ms: None,
                bubble_policy: None,
                speech_text: None,
                scene: None,
                duration_hint: None,
                persistent: false,
                controls: vec![],
                chat_id: None,
                dismissed: false,
            }),
            last_result: Some("unhealthy".to_string()),
            ..Default::default()
        };

        let filtered = result_after_suggestion_policy(result, &settings, &[]);

        assert!(filtered.suggestion.is_none());
        assert!(filtered.runtime_event.is_some());
        assert_eq!(filtered.last_result.as_deref(), Some("unhealthy"));
    }

    #[test]
    fn suggestions_allowed_requires_proactive_and_unread_budget() {
        let mut settings = BuddySettings::default();
        assert!(suggestions_allowed(&settings, &[]));

        settings.proactive_enabled = false;
        assert!(!suggestions_allowed(&settings, &[]));

        settings.proactive_enabled = true;
        let mut suggestions = (0..MAX_UNREAD_SUGGESTIONS)
            .map(active_suggestion)
            .collect::<Vec<_>>();
        assert!(!suggestions_allowed(&settings, &suggestions));
        suggestions[0].dismissed = true;
        assert!(suggestions_allowed(&settings, &suggestions));
    }

    #[test]
    fn mixed_suggestion_watchers_run_when_suggestions_blocked() {
        use crate::buddy::jobs::health_watcher::HealthWatcherJob;
        use crate::buddy::jobs::stats_watcher::StatsWatcherJob;

        assert!(StatsWatcherJob.produces_suggestion());
        assert!(StatsWatcherJob.runs_when_suggestions_blocked());
        assert!(HealthWatcherJob.produces_suggestion());
        assert!(HealthWatcherJob.runs_when_suggestions_blocked());
    }

    #[test]
    fn scheduler_registers_all_autonomous_registry_jobs() {
        let scheduler = BuddyScheduler::new(default_buddy_jobs());
        let ids = job_ids(&scheduler);

        for meta in AUTONOMOUS_BUDDY_WORKFLOWS {
            assert!(ids.iter().any(|id| id == meta.id), "missing {}", meta.id);
        }
    }
}
