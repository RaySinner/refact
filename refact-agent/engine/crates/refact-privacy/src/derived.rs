use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct DerivedZone {
    pub zone: String,
    pub origin: Option<String>,
}

pub type DerivedPrivacyZones = Arc<RwLock<HashMap<PathBuf, DerivedZone>>>;

pub fn new_derived_privacy_zones() -> DerivedPrivacyZones {
    Arc::new(RwLock::new(HashMap::new()))
}
