use std::time::Duration;

use tokio::task::JoinHandle;

const ABORT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct BackgroundTasksHolder {
    tasks: Vec<JoinHandle<()>>,
}

impl Default for BackgroundTasksHolder {
    fn default() -> Self {
        BackgroundTasksHolder { tasks: vec![] }
    }
}

impl BackgroundTasksHolder {
    pub fn new(tasks: Vec<JoinHandle<()>>) -> Self {
        BackgroundTasksHolder { tasks }
    }

    pub fn push_back(&mut self, task: JoinHandle<()>) {
        self.tasks.push(task);
    }

    pub fn extend<T>(&mut self, tasks: T)
    where
        T: IntoIterator<Item = JoinHandle<()>>,
    {
        self.tasks.extend(tasks);
    }

    pub async fn abort(&mut self) {
        for task in self.tasks.iter_mut() {
            task.abort();
        }
        let join_all = futures::future::join_all(self.tasks.drain(..));
        if tokio::time::timeout(ABORT_TIMEOUT, join_all).await.is_err() {
            tracing::warn!(
                "background_tasks: some tasks did not finish within {:?} after abort, continuing shutdown",
                ABORT_TIMEOUT
            );
        }
    }
}
