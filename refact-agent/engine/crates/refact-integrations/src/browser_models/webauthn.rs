use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAuthenticatorProtocol {
    U2f,
    #[default]
    Ctap2,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAuthenticatorTransport {
    #[default]
    Usb,
    Nfc,
    Ble,
    Cable,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserWebAuthnCredential {
    pub credential_id: String,
    #[serde(default)]
    pub is_resident_credential: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rp_id: Option<String>,
    pub private_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_handle: Option<String>,
    #[serde(default)]
    pub sign_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub large_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_eligibility: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_state: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_display_name: Option<String>,
}
