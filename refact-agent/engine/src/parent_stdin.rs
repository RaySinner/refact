use std::sync::Arc;

use tokio::io::AsyncRead;

use crate::global_context::GlobalContext;

pub async fn watch_parent_stdin_for_eof<R>(
    mut reader: R,
    on_eof: Arc<dyn Fn() + Send + Sync>,
) where
    R: AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => {
                on_eof();
                return;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!("parent stdin watch read error, treating as EOF: {error}");
                on_eof();
                return;
            }
        }
    }
}

pub async fn run_parent_stdin_watch(gcx: Arc<GlobalContext>) {
    let on_eof_sender = gcx.ask_shutdown_sender.clone();
    watch_parent_stdin_for_eof(
        tokio::io::stdin(),
        Arc::new(move || {
            if let Err(error) = on_eof_sender.lock().unwrap().send("parent stdin EOF".to_string()) {
                tracing::debug!("failed to request shutdown after parent stdin EOF: {error}");
            }
        }),
    )
    .await;
}

pub fn spawn_parent_stdin_watch_if_requested(gcx: Arc<GlobalContext>) -> Option<tokio::task::JoinHandle<()>> {
    if !gcx.cmdline.watch_parent_stdin {
        return None;
    }
    if gcx.cmdline.lsp_stdin_stdout != 0 {
        tracing::warn!("--watch-parent-stdin ignored because --lsp-stdin-stdout owns stdin");
        return None;
    }
    Some(tokio::spawn(run_parent_stdin_watch(gcx)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn eof_triggers_on_eof_callback() {
        let (client, server) = tokio::io::duplex(64);
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = fired.clone();
        let reader_task = tokio::spawn(watch_parent_stdin_for_eof(
            client,
            Arc::new(move || {
                fired_clone.store(true, Ordering::SeqCst);
            }),
        ));
        drop(server);
        reader_task.await.unwrap();
        assert!(fired.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn open_pipe_does_not_trigger_eof() {
        let (client, mut server) = tokio::io::duplex(64);
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = fired.clone();
        let reader_task = tokio::spawn(watch_parent_stdin_for_eof(
            client,
            Arc::new(move || {
                fired_clone.store(true, Ordering::SeqCst);
            }),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        server.write_all(b"hello").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!fired.load(Ordering::SeqCst), "EOF must not fire while the parent holds the pipe open");
        drop(server);
        reader_task.await.unwrap();
        assert!(fired.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn read_error_triggers_eof_callback() {
        struct BrokenReader;
        impl AsyncRead for BrokenReader {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "broken",
                )))
            }
        }

        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = fired.clone();
        watch_parent_stdin_for_eof(
            BrokenReader,
            Arc::new(move || {
                fired_clone.store(true, Ordering::SeqCst);
            }),
        )
        .await;
        assert!(fired.load(Ordering::SeqCst));
    }
}
