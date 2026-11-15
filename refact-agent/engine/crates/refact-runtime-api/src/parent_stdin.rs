use std::sync::Arc;

use crate::global_context::GlobalContext;

pub use refact_core::parent_stdin::watch_parent_stdin_for_eof;

pub async fn run_parent_stdin_watch(gcx: Arc<GlobalContext>) {
    let on_eof_sender = gcx.ask_shutdown_sender.clone();
    watch_parent_stdin_for_eof(
        tokio::io::stdin(),
        Arc::new(move || {
            if let Err(error) = on_eof_sender
                .lock()
                .unwrap()
                .send("parent stdin EOF".to_string())
            {
                tracing::debug!("failed to request shutdown after parent stdin EOF: {error}");
            }
        }),
    )
    .await;
}

pub fn spawn_parent_stdin_watch_if_requested(
    gcx: Arc<GlobalContext>,
) -> Option<tokio::task::JoinHandle<()>> {
    if !gcx.cmdline.watch_parent_stdin {
        return None;
    }
    if gcx.cmdline.lsp_stdin_stdout != 0 {
        eprintln!("--watch-parent-stdin ignored because --lsp-stdin-stdout owns stdin");
        return None;
    }
    Some(tokio::spawn(run_parent_stdin_watch(gcx)))
}
