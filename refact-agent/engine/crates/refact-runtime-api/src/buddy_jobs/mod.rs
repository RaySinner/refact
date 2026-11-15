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
