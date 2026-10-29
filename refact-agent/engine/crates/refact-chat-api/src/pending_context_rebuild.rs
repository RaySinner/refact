#[derive(Clone, Debug)]
pub struct PendingContextRebuild {
    pub model: Option<String>,
    pub trigger: String,
}
