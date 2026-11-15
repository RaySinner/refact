pub use refact_runtime_api::buddy_jobs::buddy_briefing::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buddy::scheduler::BuddyJob;

    #[test]
    fn briefing_job_metadata() {
        let job = BuddyBriefingJob;
        assert_eq!(job.id(), "buddy_briefing");
        assert_eq!(job.cooldown_seconds(), 3600);
        assert!(!job.records_empty_result());
    }
}
