//! Engine-independent buddy jobs.
//!
//! Each module here only reaches `AppState`, `refact-buddy-core` types and the
//! scheduler seam in `crate::buddy_scheduler`; the engine buddy job modules
//! re-export them and keep their tests.

pub mod buddy_briefing;
pub mod buddy_handbook_distiller;
pub mod config_watcher;
pub mod error_triage;
pub mod health_watcher;
pub mod proactive_suggestions;
pub mod autonomous_chats;
pub mod buddy_test_coverage_watcher;
pub mod buddy_pr_issue_matchmaker;
pub mod buddy_onboarding;
pub mod buddy_daily_digest;
pub mod buddy_friday_retro;
pub mod refact_self_critic;
pub mod buddy_idle_suggester;
pub mod buddy_refactor_hunter;
pub mod buddy_skill_author;
pub mod quest_prompt;
pub mod chat_phrase_bank;
pub mod stats_watcher;
pub mod greeting;
pub mod speaker_win;
pub mod tour;
pub mod speaker_insight;
pub mod speaker_memory_pulse_commentary;
