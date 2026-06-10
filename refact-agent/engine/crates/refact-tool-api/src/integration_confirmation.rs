use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone, Default, Debug)]
pub struct IntegrationConfirmation {
    #[serde(default)]
    pub ask_user: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}
