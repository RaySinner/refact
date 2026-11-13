#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuddyIssueCreateResult {
    pub url: String,
    pub provider: String,
    pub repo: String,
}
