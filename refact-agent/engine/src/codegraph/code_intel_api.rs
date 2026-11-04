use serde::Serialize;

pub use refact_code_intel_api::*;

#[derive(Serialize)]
pub struct GraphNodeResponse {
    pub id: i64,
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Serialize)]
pub struct GraphEdgeResponse {
    pub source: i64,
    pub target: i64,
    pub kind: String,
}

#[derive(Serialize)]
pub struct GraphResponse {
    pub index_state: PrBlastIndexState,
    pub nodes: Vec<GraphNodeResponse>,
    pub edges: Vec<GraphEdgeResponse>,
}

#[derive(Serialize)]
pub struct CommunityResponse {
    pub id: usize,
    pub label: String,
    pub member_count: usize,
    pub cohesion: f64,
    pub index_state: PrBlastIndexState,
}
