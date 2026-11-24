use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeliveryAttempt {
    pub delivery_id: String,
    pub idempotency_key: String,
    pub room_id: String,
    pub event_id: String,
    pub recipient_id: String,
    pub logical_attempt: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_delivery_id: Option<String>,
}

impl Default for DeliveryAttempt {
    fn default() -> Self {
        Self {
            delivery_id: String::new(),
            idempotency_key: String::new(),
            room_id: String::new(),
            event_id: String::new(),
            recipient_id: String::new(),
            logical_attempt: 0,
            previous_delivery_id: None,
        }
    }
}

impl DeliveryAttempt {
    pub fn new(
        room_id: impl Into<String>,
        event_id: impl Into<String>,
        recipient_id: impl Into<String>,
    ) -> Self {
        Self::with_attempt(room_id, event_id, recipient_id, 0, None)
    }

    fn with_attempt(
        room_id: impl Into<String>,
        event_id: impl Into<String>,
        recipient_id: impl Into<String>,
        logical_attempt: u64,
        previous_delivery_id: Option<String>,
    ) -> Self {
        let room_id = room_id.into();
        let event_id = event_id.into();
        let recipient_id = recipient_id.into();
        let mut hasher = Sha256::new();
        hasher.update(room_id.as_bytes());
        hasher.update([0]);
        hasher.update(event_id.as_bytes());
        hasher.update([0]);
        hasher.update(recipient_id.as_bytes());
        hasher.update([0]);
        hasher.update(logical_attempt.to_string().as_bytes());
        let idempotency_key = format!("refact-room-v1-{}", hex::encode(hasher.finalize()));
        Self {
            delivery_id: idempotency_key.clone(),
            idempotency_key,
            room_id,
            event_id,
            recipient_id,
            logical_attempt,
            previous_delivery_id,
        }
    }

    pub fn transport_retry(&self) -> Self {
        self.clone()
    }

    pub fn linked_attempt(&self) -> Self {
        Self::with_attempt(
            self.room_id.clone(),
            self.event_id.clone(),
            self.recipient_id.clone(),
            self.logical_attempt + 1,
            Some(self.delivery_id.clone()),
        )
    }
}
