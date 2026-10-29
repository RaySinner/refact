use std::path::PathBuf;
use tokio::sync::mpsc;

pub const TRAJECTORY_VECTORIZATION_QUEUE_CAPACITY: usize = 64;

/// Channel holder for best-effort trajectory VecDB vectorization. The enqueue
/// and worker loop live with the caller.
#[derive(Default)]
pub struct TrajectoryVectorizationQueue {
    pub sender: Option<mpsc::Sender<PathBuf>>,
    pub worker: Option<tokio::task::JoinHandle<()>>,
}
