pub use refact_buddy_core::events::BuddyEvent as CoreBuddyEvent;

pub type BuddyEvent = CoreBuddyEvent<super::diagnostics::DiagnosticContext>;
