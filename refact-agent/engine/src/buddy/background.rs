use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use tracing::{info, warn};

use refact_buddy_core::actor::{chat_phrase_bank_is_fresh, parse_commit_activity_from_log};
use refact_buddy_core::runtime_writer::{run_runtime_queue_writer, RuntimeQueueWriteOp};
use refact_buddy_core::user_action::UserAction;

use crate::app_state::AppState;

use super::facts::FactStore;
use super::humor::{HumorPlan, HumorService};
use super::memory_lifecycle::detect_memory_lifecycle_ops_from_knowledge_dirs;
use super::observers::{build_observer_registry, BuddyObserver, ObserverContext};
use super::opportunities::{primary_fact_kind_for_opportunity, OpportunityDetector};
use super::policy::{evaluate_with_mutes, PolicyDecision};
use super::types::{
    BuddyFact, BuddyFactKind, BuddyOpportunity, BuddyPulse, BuddySuggestion,
};

const PET_DECAY_INTERVAL_SECS: u64 = 15;
const OBSERVER_CONCURRENCY: usize = 4;
const MEMORY_OPS_ARCHIVE_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;
const MEMORY_OPS_COMPACT_INTERVAL_SECS: u64 = 6 * 60 * 60;
const MEMORY_OPS_DRAIN_INTERVAL_SECS: u64 = 600;
const MEMORY_OPS_DRAIN_MAX_APPLIES: usize = 200;

pub(crate) async fn observe_buddy_facts_parallel(
    due_observers: Vec<Arc<dyn BuddyObserver>>,
    app: AppState,
    project_root: std::path::PathBuf,
    now: DateTime<Utc>,
) -> Vec<(u64, Vec<BuddyFact>)> {
    use futures::stream::{FuturesUnordered, StreamExt};

    let mut pending = FuturesUnordered::new();
    let mut all_facts = Vec::new();
    for obs in due_observers {
        let app = app.clone();
        let project_root = project_root.clone();
        pending.push(async move {
            let ctx = ObserverContext { project_root, now };
            let refresh_ttl = obs.emission_refresh_ttl_seconds();
            let facts =
                tokio::time::timeout(tokio::time::Duration::from_secs(5), obs.observe(app, &ctx))
                    .await
                    .unwrap_or_default();
            (refresh_ttl, facts)
        });
        if pending.len() >= OBSERVER_CONCURRENCY {
            if let Some(group) = pending.next().await {
                all_facts.push(group);
            }
        }
    }
    while let Some(group) = pending.next().await {
        all_facts.push(group);
    }
    all_facts
}

async fn poll_commit_activity_once(app: AppState, project_root: PathBuf) {
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .arg("log")
            .arg("-10")
            .arg("--since=1h ago")
            .arg("--pretty=format:%H|%f|%s")
            .current_dir(project_root)
            .output()
    })
    .await;
    let Ok(Ok(output)) = output else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let actions = parse_commit_activity_from_log(&text);
    if actions.is_empty() {
        return;
    }
    let user_activity = app.chat_ctx().buddy.user_activity.clone();
    if let Ok(mut ring) = user_activity.try_lock() {
        let mut existing = ring
            .snapshot()
            .iter()
            .filter_map(|action| match action {
                UserAction::CommitMade { sha, .. } => Some(sha.clone()),
                _ => None,
            })
            .collect::<HashSet<_>>();
        for action in actions {
            if let UserAction::CommitMade { sha, .. } = &action {
                if existing.contains(sha) {
                    continue;
                }
                existing.insert(sha.clone());
            }
            ring.push(action);
        }
    };
}

async fn commit_activity_poller(
    app: AppState,
    project_root: PathBuf,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(300)) => {
                if shutdown_flag.load(Ordering::SeqCst) {
                    break;
                }
                poll_commit_activity_once(app.clone(), project_root.clone()).await;
            }
            _ = wait_for_shutdown(shutdown_flag.clone()) => {
                break;
            }
        }
    }
}

async fn wait_for_shutdown(shutdown_flag: Arc<std::sync::atomic::AtomicBool>) {
    while !shutdown_flag.load(Ordering::SeqCst) {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
}

pub async fn buddy_background_task(app: AppState) {
    let ctx = app.chat_ctx();
    let project_root = loop {
        if ctx.runtime.shutdown_flag.load(Ordering::SeqCst) {
            return;
        }
        let dirs = crate::files_correction::get_unscoped_project_dirs_for_ctx(&ctx).await;
        if let Some(root) = dirs.into_iter().next() {
            break root;
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    };

    if let Err(e) = super::storage::bootstrap_buddy_storage(&project_root).await {
        warn!("buddy: failed to bootstrap storage: {}", e);
        return;
    }
    super::storage::gc_stale_memory_ops_tmp_files(&project_root).await;
    match super::storage::archive_memory_ops_if_oversized(
        &project_root,
        MEMORY_OPS_ARCHIVE_THRESHOLD_BYTES,
    )
    .await
    {
        Ok(true) => info!("buddy: archived oversized memory ops queue"),
        Ok(false) => {}
        Err(err) => warn!(
            "buddy: failed to archive oversized memory ops queue: {}",
            err
        ),
    }

    let state = super::state::load_state(&project_root).await;
    let settings = super::settings::load_settings(&project_root).await;
    let recent_diagnostics =
        refact_runtime_api::buddy_actor_runtime::load_diagnostics_for_service(&project_root).await;
    let mut memory_ops = super::storage::load_memory_ops_repairing(&project_root).await;
    let before_memory_ops = memory_ops.pending_count + memory_ops.approved_count;
    if before_memory_ops > 5000 {
        match super::storage::compact_memory_ops(&project_root).await {
            Ok(compacted) => {
                let after_memory_ops = compacted.pending_count + compacted.approved_count;
                info!(
                    "buddy: compacted large memory ops queue at startup: {} -> {} pending/approved ops",
                    before_memory_ops, after_memory_ops
                );
                memory_ops = compacted;
            }
            Err(err) => warn!(
                "buddy: failed to compact large memory ops queue at startup: {}",
                err
            ),
        }
    }
    let runtime_queue = super::storage::load_runtime_queue(&project_root).await;

    let events_tx = ctx.buddy.buddy_events_tx.clone();

    let (queue_tx, queue_rx) = mpsc::unbounded_channel::<RuntimeQueueWriteOp>();
    let writer_root = project_root.clone();
    let writer_handle = tokio::spawn(async move {
        run_runtime_queue_writer(writer_root, queue_rx).await;
    });

    let mut service = refact_buddy_core::actor::BuddyService::new(
        project_root.clone(),
        state,
        settings,
        recent_diagnostics,
        runtime_queue,
        events_tx,
        Some(queue_tx),
    );
    service.set_memory_ops(memory_ops);
    service
        .draft_store
        .restore(super::storage::load_drafts(&project_root).await);
    let humor_service = Arc::new(tokio::sync::Mutex::new(HumorService::new()));
    let observers = build_observer_registry();

    let buddy_arc = ctx.buddy.buddy.clone();
    *buddy_arc.lock().await = Some(service);
    let initial_pulse =
        super::pulse::build_pulse(app.clone(), &project_root, &FactStore::new()).await;
    {
        let mut buddy = buddy_arc.lock().await;
        if let Some(svc) = buddy.as_mut() {
            svc.set_pulse(initial_pulse);
            if svc
                .state
                .chat_phrase_bank
                .as_ref()
                .is_some_and(|bank| !chat_phrase_bank_is_fresh(bank, Utc::now()))
            {
                svc.state.chat_phrase_bank = None;
                svc.dirty = true;
            }
        }
    }

    let agents_md = project_root.join("AGENTS.md");
    let setup_done = tokio::fs::try_exists(&agents_md).await.unwrap_or(false);
    if !setup_done {
        let mut guard = buddy_arc.lock().await;
        if let Some(svc) = guard.as_mut() {
            let already = svc
                .state
                .suggestion_state
                .iter()
                .any(|s| s.suggestion_type == "setup");
            if !already {
                let suggestion = BuddySuggestion {
                    id: "setup".to_string(),
                    suggestion_type: "setup".to_string(),
                    title: "Set up this project".to_string(),
                    description:
                        "Run setup to generate guidelines, integrations, and toolbox commands."
                            .to_string(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    dismissed: false,
                    controls: vec![],
                    quest: None,
                };
                svc.add_suggestion(suggestion);
            }
        }
    }

    info!("buddy: service started for {:?}", project_root);

    let scheduler = super::scheduler::BuddyScheduler::new(super::scheduler::default_buddy_jobs());
    let shutdown_flag = ctx.runtime.shutdown_flag.clone();
    let commit_poller_handle = tokio::spawn(commit_activity_poller(
        app.clone(),
        project_root.clone(),
        shutdown_flag.clone(),
    ));
    let supervision_advisor_handle = tokio::spawn(
        refact_runtime_api::buddy_supervision_advisor::run_supervision_advisor(app.clone()),
    );
    let mut expiry_tick: u64 = 0;

    loop {
        if shutdown_flag.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        expiry_tick += 1;
        if expiry_tick % 30 == 0 {
            app.buddy.supervision.evaluate_all_non_fatal().await;
        }
        if expiry_tick % PET_DECAY_INTERVAL_SECS == 0 {
            let mut buddy = buddy_arc.lock().await;
            if let Some(svc) = buddy.as_mut() {
                svc.apply_pet_tick(PET_DECAY_INTERVAL_SECS);
            }
        }
        if expiry_tick % 60 == 0 {
            let mut buddy = buddy_arc.lock().await;
            if let Some(svc) = buddy.as_mut() {
                svc.expire_suggestions();
            }
        }
        // Pulse refresh + opportunity expiry every 60s
        if expiry_tick % 60 == 0 {
            let now = Utc::now();
            let fact_snap = {
                let buddy = buddy_arc.lock().await;
                buddy
                    .as_ref()
                    .map(|svc| svc.fact_store.iter().cloned().collect::<Vec<_>>())
            };
            if let Some(facts) = fact_snap {
                let mut tmp_store = FactStore::new();
                for f in facts {
                    tmp_store.ingest(f);
                }
                let knowledge_dirs =
                    crate::files_correction::get_unscoped_project_dirs_for_ctx(&ctx)
                        .await
                        .into_iter()
                        .map(|dir| dir.join(crate::file_filter::KNOWLEDGE_FOLDER_NAME))
                        .filter(|dir| dir.exists())
                        .collect::<Vec<_>>();
                let lifecycle_ops =
                    detect_memory_lifecycle_ops_from_knowledge_dirs(&knowledge_dirs, now).await;
                if !lifecycle_ops.is_empty() {
                    let mut memory_ops = super::storage::load_memory_ops(&project_root).await;
                    for op in lifecycle_ops {
                        match super::storage::enqueue_memory_op(&project_root, op).await {
                            Ok(updated) => memory_ops = updated,
                            Err(err) => {
                                warn!("buddy: failed to enqueue memory lifecycle op: {}", err)
                            }
                        }
                    }
                    let mut buddy = buddy_arc.lock().await;
                    if let Some(svc) = buddy.as_mut() {
                        svc.set_memory_ops(memory_ops);
                    }
                }
                let new_pulse =
                    super::pulse::build_pulse(app.clone(), &project_root, &tmp_store).await;
                let mut buddy = buddy_arc.lock().await;
                if let Some(svc) = buddy.as_mut() {
                    svc.set_pulse(new_pulse);
                    svc.expire_opportunities();
                    svc.opportunity_queue.refresh_cooldowns(now);
                    svc.expire_drafts(now);
                    svc.expire_runtime_events_at(now);
                }
            }
        }
        {
            let window = {
                let actions = {
                    let ring = ctx.buddy.user_activity.lock().await;
                    ring.snapshot()
                };
                super::speech_policy::auto_quiet_window_from_actions(&actions)
            };
            let mut buddy = buddy_arc.lock().await;
            if let Some(svc) = buddy.as_mut() {
                svc.auto_quiet_window = window;
            }
        }
        // Observer ticking — each observer respects its own cadence
        {
            let now = Utc::now();
            let due_observers = {
                let buddy = buddy_arc.lock().await;
                match buddy.as_ref() {
                    Some(svc) => {
                        let s = svc.settings.clone();
                        let lt = svc.last_observer_tick.clone();
                        let due: Vec<Arc<dyn BuddyObserver>> = observers
                            .iter()
                            .filter(|obs| {
                                if !obs.requires_setting(&s) {
                                    return false;
                                }
                                match lt.get(obs.id()) {
                                    Some(t) => {
                                        (now - *t).num_seconds() as u64 >= obs.cadence_seconds()
                                    }
                                    None => true,
                                }
                            })
                            .cloned()
                            .collect();
                        due
                    }
                    None => vec![],
                }
            };
            if !due_observers.is_empty() {
                // Run observers without holding buddy lock (prevents deadlock with
                // DiagnosticClusterObserver which also locks buddy).
                let all_facts = observe_buddy_facts_parallel(
                    due_observers.clone(),
                    app.clone(),
                    project_root.clone(),
                    now,
                )
                .await;
                // Phase 1: ingest facts, detect candidates, add non-humor opps — all under buddy lock.
                let (humor_tasks, pulse_for_humor, humor_arc) = {
                    let mut buddy = buddy_arc.lock().await;
                    if let Some(svc) = buddy.as_mut() {
                        for obs in &due_observers {
                            svc.last_observer_tick.insert(obs.id(), now);
                        }
                        for (refresh_ttl_secs, facts) in all_facts {
                            svc.fact_store.ingest_many_with_refresh_ttl(
                                facts,
                                chrono::Duration::seconds(refresh_ttl_secs as i64),
                            );
                        }
                        let candidates = OpportunityDetector::new().detect(
                            &svc.fact_store,
                            &svc.pulse,
                            &svc.opportunity_queue,
                        );
                        let mut humor_needed: Vec<(BuddyOpportunity, BuddyFactKind, u64)> = vec![];
                        for (opp, cooldown_secs) in candidates {
                            match evaluate_with_mutes(
                                &opp,
                                &svc.settings,
                                &svc.opportunity_queue,
                                &svc.state.muted_rules,
                            ) {
                                PolicyDecision::Drop { reason } => {
                                    tracing::debug!("buddy: opp dropped by policy: {}", reason);
                                }
                                PolicyDecision::Surface { humor_allowed } => {
                                    if humor_allowed {
                                        let kind = primary_fact_kind_for_opportunity(
                                            &opp,
                                            &svc.fact_store,
                                        );
                                        humor_needed.push((opp, kind, cooldown_secs));
                                    } else {
                                        svc.surface_opportunity_with_cooldown(opp, cooldown_secs);
                                    }
                                }
                            }
                        }
                        let pulse = svc.pulse.clone();
                        let humor_arc = humor_service.clone();
                        (humor_needed, pulse, humor_arc)
                    } else {
                        (
                            vec![],
                            BuddyPulse::default(),
                            humor_service.clone(),
                        )
                    }
                }; // buddy lock released — LLM humor calls happen outside the lock

                // Phase 1.5: Buddy may speak into a live agent chat. Runs after
                // the facts are ingested (they are the trigger) and outside the
                // buddy lock (it awaits boards and chat sessions).
                let interjection_plan = {
                    let buddy = buddy_arc.lock().await;
                    buddy.as_ref().map(|svc| {
                        let facts = svc.fact_store.iter().cloned().collect::<Vec<_>>();
                        (
                            super::chat_interjection::stuck_task_ids(&facts),
                            svc.settings.clone(),
                            svc.state.speech_rotation.clone(),
                            svc.auto_quiet_window,
                            svc.llm_budget_exhausted(),
                        )
                    })
                };
                if let Some((stuck, settings, rotation, auto_quiet_window, llm_exhausted)) =
                    interjection_plan.filter(|(stuck, ..)| !stuck.is_empty())
                {
                    let targets =
                        super::chat_interjection::collect_interjection_targets(&app, &stuck).await;
                    for target in targets {
                        let text =
                            super::chat_interjection::build_interjection_text(&target);
                        match super::chat_interjection::maybe_interject_with_agent(
                            app.clone(),
                            &target,
                            settings.clone(),
                            rotation.clone(),
                            auto_quiet_window,
                            llm_exhausted,
                        )
                        .await
                        {
                            Ok(_) => {
                                let mut buddy = buddy_arc.lock().await;
                                if let Some(svc) = buddy.as_mut() {
                                    svc.record_interjection_emission(&target.chat_id, &text);
                                }
                            }
                            Err(reason) => {
                                tracing::debug!(
                                    target: "buddy.chat_interjection",
                                    task_id = %target.task_id,
                                    card_id = %target.card_id,
                                    agent_chat_id = %target.chat_id,
                                    reason = reason.as_str(),
                                    "buddy interjection skipped"
                                );
                            }
                        }
                    }
                }

                // Phase 1.6: Buddy answers a task agent's question the planner has
                // left hanging. Deliberately independent of the interjection pass:
                // a question can be overdue while every agent is busy, and "the agent
                // is working" is not "the answer arrived". Off by default, so this
                // whole block is skipped unless the user asked for it.
                let backstop_plan = {
                    let buddy = buddy_arc.lock().await;
                    buddy.as_ref().and_then(|svc| {
                        svc.settings.planner_backstop_enabled.then(|| {
                            (
                                svc.settings.clone(),
                                svc.state.speech_rotation.clone(),
                                svc.auto_quiet_window,
                            )
                        })
                    })
                };
                if let Some((settings, rotation, auto_quiet_window)) = backstop_plan {
                    let task_ids: Vec<String> = crate::tasks::storage::list_tasks_for_ctx(&ctx)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|meta| {
                            !matches!(
                                meta.status,
                                crate::tasks::types::TaskStatus::Completed
                                    | crate::tasks::types::TaskStatus::Abandoned
                            )
                        })
                        .map(|meta| meta.id)
                        .collect();
                    let candidates =
                        super::planner_backstop_runner::collect_backstop_candidates(
                            &app, &task_ids,
                        )
                        .await;
                    for candidate in candidates {
                        let card_id = candidate.question.card_id.clone();
                        let outcome = super::planner_backstop_runner::maybe_answer_overdue_question(
                            &app,
                            settings.clone(),
                            rotation.clone(),
                            auto_quiet_window,
                            &candidate.task_id,
                            &candidate.question,
                        )
                        .await;
                        match &outcome {
                            super::planner_backstop_runner::BackstopOutcome::Answered { .. }
                            | super::planner_backstop_runner::BackstopOutcome::NotifiedHuman {
                                ..
                            } => {
                                // Charge the interjection budget: the agent paid a
                                // whole turn to read this, same as any other message
                                // Buddy puts in an agent's transcript.
                                let mut buddy = buddy_arc.lock().await;
                                if let Some(svc) = buddy.as_mut() {
                                    svc.record_interjection_emission(
                                        &format!("backstop:{card_id}"),
                                        "planner backstop",
                                    );
                                }
                            }
                            super::planner_backstop_runner::BackstopOutcome::Skipped {
                                question_id,
                                reason,
                            } => {
                                tracing::debug!(
                                    target: "buddy.planner_backstop",
                                    task_id = %candidate.task_id,
                                    card_id = %card_id,
                                    question_id = %question_id,
                                    reason = %reason,
                                    "buddy planner backstop skipped"
                                );
                            }
                        }
                    }
                }

                // Phase 2: attach humor outside the buddy lock.
                let mut ready: Vec<(BuddyOpportunity, u64)> = Vec::with_capacity(humor_tasks.len());
                for (mut opp, kind, cooldown_secs) in humor_tasks {
                    let plan = {
                        let mut humor = humor_arc.lock().await;
                        humor.plan_humor(kind, &pulse_for_humor)
                    };
                    match plan {
                        HumorPlan::Ready(line) => {
                            opp.humor = Some(line);
                        }
                        HumorPlan::Generate(reservation) => {
                            let lines = reservation.generate(app.clone()).await;
                            let line = {
                                let mut humor = humor_arc.lock().await;
                                humor.complete_humor(reservation, lines)
                            };
                            if let Some(line) = line {
                                opp.humor = Some(line);
                            }
                        }
                        HumorPlan::Skip => {}
                    }
                    ready.push((opp, cooldown_secs));
                }

                // Phase 3: re-acquire buddy lock to add humor-processed opps.
                if !ready.is_empty() {
                    let mut buddy = buddy_arc.lock().await;
                    if let Some(svc) = buddy.as_mut() {
                        for (opp, cooldown_secs) in ready {
                            svc.surface_opportunity_with_cooldown(opp, cooldown_secs);
                        }
                    }
                }
            }
        }
        if expiry_tick % 30 == 0 {
            scheduler
                .tick(app.clone(), buddy_arc.clone(), &project_root)
                .await;
        }
        let state_to_save = {
            let mut buddy = buddy_arc.lock().await;
            buddy.as_mut().and_then(|svc| {
                if svc.dirty {
                    svc.dirty = false;
                    Some(svc.state.clone())
                } else {
                    None
                }
            })
        };
        if let Some(s) = state_to_save {
            if let Err(e) = super::state::save_state(&project_root, &s).await {
                warn!("buddy: failed to save state: {}", e);
                if let Some(svc) = buddy_arc.lock().await.as_mut() {
                    svc.dirty = true;
                }
            }
        }
        let drafts_to_save = {
            let mut buddy = buddy_arc.lock().await;
            buddy.as_mut().and_then(|svc| {
                if svc.drafts_dirty {
                    svc.drafts_dirty = false;
                    Some(svc.draft_store.snapshot())
                } else {
                    None
                }
            })
        };
        if let Some(drafts) = drafts_to_save {
            if let Err(e) = super::storage::save_drafts(&project_root, &drafts).await {
                warn!("buddy: failed to save drafts: {}", e);
                if let Some(svc) = buddy_arc.lock().await.as_mut() {
                    svc.drafts_dirty = true;
                }
            }
        }

        // Periodic compaction: ask the writer task to rewrite the JSONL from
        // the in-memory queue every 5 minutes. The append-on-mutation log can
        // grow unbounded under churn (progress updates, repeated coalesces);
        // this collapses it back to one line per surviving event. Routing
        // through the same channel keeps writes strictly ordered.
        if expiry_tick % 300 == 0 {
            let buddy = buddy_arc.lock().await;
            if let Some(svc) = buddy.as_ref() {
                if let Some(tx) = &svc.queue_writer {
                    let _ = tx.send(RuntimeQueueWriteOp::Compact(svc.runtime_queue.clone()));
                }
            }
        }
        if expiry_tick % MEMORY_OPS_COMPACT_INTERVAL_SECS == 0 {
            match super::storage::compact_memory_ops(&project_root).await {
                Ok(memory_ops) => {
                    let mut buddy = buddy_arc.lock().await;
                    if let Some(svc) = buddy.as_mut() {
                        svc.set_memory_ops(memory_ops);
                    }
                }
                Err(err) => warn!("buddy: failed to compact memory ops queue: {}", err),
            }
        }
        if expiry_tick % MEMORY_OPS_DRAIN_INTERVAL_SECS == 0 {
            let drain_settings = {
                let buddy = buddy_arc.lock().await;
                buddy
                    .as_ref()
                    .map(|svc| (svc.settings.autonomy_level, svc.settings.enabled))
            };
            if let Some((autonomy, enabled)) = drain_settings {
                if enabled {
                    match super::storage::drain_memory_ops(
                        &project_root,
                        app.clone(),
                        autonomy,
                        MEMORY_OPS_DRAIN_MAX_APPLIES,
                    )
                    .await
                    {
                        Ok((memory_ops, changed)) => {
                            let mut buddy = buddy_arc.lock().await;
                            if let Some(svc) = buddy.as_mut() {
                                svc.set_memory_ops(memory_ops);
                            }
                            if changed > 0 {
                                info!("buddy: drained {} memory ops", changed);
                            }
                        }
                        Err(err) => warn!("buddy: failed to drain memory ops queue: {}", err),
                    }
                }
            }
        }
    }

    let state_opt = {
        let buddy = buddy_arc.lock().await;
        buddy.as_ref().map(|s| s.state.clone())
    };
    if let Some(s) = state_opt {
        let _ = super::state::save_state(&project_root, &s).await;
    }
    let drafts_opt = {
        let buddy = buddy_arc.lock().await;
        buddy
            .as_ref()
            .filter(|s| s.drafts_dirty)
            .map(|s| s.draft_store.snapshot())
    };
    if let Some(drafts) = drafts_opt {
        let _ = super::storage::save_drafts(&project_root, &drafts).await;
    }

    // Final compaction on shutdown so the JSONL on disk is canonical, then
    // drop the writer sender (replacing the service slot does that for us)
    // and wait for the writer task to drain.
    // Take the whole service out of the slot atomically FIRST so any concurrent add_diagnostic
    // observes None and cannot spawn a new tracked task or enqueue new runtime records. Send the
    // final compaction from the taken service so nothing can mutate Buddy state after it.
    // Dropping the taken service also drops the writer sender so the writer task can finish.
    let mut service = buddy_arc.lock().await.take();
    if let Some(svc) = service.as_ref() {
        if let Some(tx) = &svc.queue_writer {
            let _ = tx.send(RuntimeQueueWriteOp::Compact(svc.runtime_queue.clone()));
        }
    }
    let mut background_tasks = service
        .as_mut()
        .map(|svc| svc.take_background_tasks())
        .unwrap_or_default();
    let drain = async {
        for handle in background_tasks.iter_mut() {
            let _ = handle.await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(10), drain)
        .await
        .is_err()
    {
        warn!("buddy: background tasks did not drain within 10s, aborting remaining");
        for handle in &background_tasks {
            handle.abort();
        }
    }
    drop(service);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), writer_handle).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), commit_poller_handle).await;
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        supervision_advisor_handle,
    )
    .await;

    info!("buddy: background task stopped");
}
