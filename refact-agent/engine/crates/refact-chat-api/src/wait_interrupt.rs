/// Why an interruptible wait was interrupted. Tool results surface the label so
/// the model can tell a real incoming message apart from a user abort.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitInterruptCause {
    MessageArrived,
    RunnerMessageArrived,
}

impl WaitInterruptCause {
    pub fn label(self) -> &'static str {
        match self {
            Self::MessageArrived => "message arrived",
            Self::RunnerMessageArrived => "background agent message arrived",
        }
    }
}
