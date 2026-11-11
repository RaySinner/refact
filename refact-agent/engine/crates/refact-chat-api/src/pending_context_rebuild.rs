use crate::compression_gate::compression_attempt_active;
use crate::types::ChatSession;

#[derive(Clone, Debug)]
pub struct PendingContextRebuild {
    pub model: Option<String>,
    pub trigger: String,
}

pub fn request_rebuild(
    session: &mut ChatSession,
    request: PendingContextRebuild,
) -> Result<(), String> {
    if session.closed
        || compression_attempt_active(session)
        || session.pending_context_rebuild.is_some()
        || session.pending_mode_handoff.is_some()
    {
        return Err("A context operation is already pending or the session is closed".into());
    }
    session.pending_context_rebuild = Some(request);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_rejects_second_owner() {
        let mut s = ChatSession::new("rebuild-test".into());
        assert!(request_rebuild(
            &mut s,
            PendingContextRebuild {
                model: None,
                trigger: "tool".into()
            }
        )
        .is_ok());
        assert!(request_rebuild(
            &mut s,
            PendingContextRebuild {
                model: None,
                trigger: "tool".into()
            }
        )
        .is_err());
    }
}
