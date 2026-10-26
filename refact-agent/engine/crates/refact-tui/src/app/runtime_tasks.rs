use super::*;

pub(super) fn spawn_daemon_events_task(
    client: DaemonClient,
    tx: mpsc::Sender<RuntimeEvent>,
    generation: u64,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reconnect_attempts = 0u32;
        loop {
            match client.subscribe_daemon_events().await {
                Ok(mut stream) => {
                    let mut disconnected = false;
                    while let Some(event) = stream.next().await {
                        match event {
                            Ok(event) => {
                                reconnect_attempts = 0;
                                if tx
                                    .send(RuntimeEvent::DaemonEvent { generation, event })
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Err(error) => {
                                disconnected = true;
                                reconnect_attempts = reconnect_attempts.saturating_add(1);
                                let retrying =
                                    reconnect_attempts <= DAEMON_EVENTS_MAX_RECONNECT_ATTEMPTS;
                                if tx
                                    .send(RuntimeEvent::DaemonEventsDisconnected {
                                        generation,
                                        message: error.to_string(),
                                        retrying,
                                    })
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                if !retrying {
                                    return;
                                }
                                break;
                            }
                        }
                    }
                    if disconnected {
                        continue;
                    }
                    reconnect_attempts = reconnect_attempts.saturating_add(1);
                    let retrying = reconnect_attempts <= DAEMON_EVENTS_MAX_RECONNECT_ATTEMPTS;
                    if tx
                        .send(RuntimeEvent::DaemonEventsDisconnected {
                            generation,
                            message: "stream ended".to_string(),
                            retrying,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if !retrying {
                        return;
                    }
                }
                Err(error) => {
                    reconnect_attempts = reconnect_attempts.saturating_add(1);
                    let retrying = reconnect_attempts <= DAEMON_EVENTS_MAX_RECONNECT_ATTEMPTS;
                    if tx
                        .send(RuntimeEvent::DaemonEventsDisconnected {
                            generation,
                            message: error.to_string(),
                            retrying,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if !retrying {
                        return;
                    }
                }
            }
            if reconnect_attempts == 0 {
                reconnect_attempts = 1;
            }
            let delay = reconnect_backoff(
                DAEMON_EVENTS_RECONNECT_INITIAL_BACKOFF,
                DAEMON_EVENTS_RECONNECT_MAX_BACKOFF,
                reconnect_attempts,
                generation,
            );
            tokio::time::sleep(delay).await;
        }
    })
}

pub(super) fn spawn_subscription_task(
    client: DaemonClient,
    project_id: String,
    chat_id: String,
    tx: mpsc::Sender<RuntimeEvent>,
    generation: u64,
    initial_delay: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if !initial_delay.is_zero() {
            tokio::time::sleep(initial_delay).await;
        }
        match client.subscribe_chat(&project_id, &chat_id).await {
            Ok(mut stream) => {
                let mut seq = ChatSeqTracker::new();
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(event) => match seq.observe(&event) {
                            ChatSeqDecision::Apply => {
                                if tx
                                    .send(RuntimeEvent::Chat { generation, event })
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            ChatSeqDecision::Suppress => {}
                            ChatSeqDecision::Resubscribe(message) => {
                                let _ = tx
                                    .send(RuntimeEvent::ChatResubscribe {
                                        generation,
                                        message,
                                    })
                                    .await;
                                return;
                            }
                        },
                        Err(error) => {
                            let unreachable = error.is_unreachable();
                            let auth_stale = error.is_auth_stale();
                            let _ = tx
                                .send(RuntimeEvent::ChatDisconnected {
                                    generation,
                                    message: error.to_string(),
                                    unreachable,
                                    auth_stale,
                                })
                                .await;
                            return;
                        }
                    }
                }
                let _ = tx
                    .send(RuntimeEvent::ChatDisconnected {
                        generation,
                        message: "stream ended".to_string(),
                        unreachable: ClientError::SseDisconnect("stream ended".to_string())
                            .is_unreachable(),
                        auth_stale: false,
                    })
                    .await;
            }
            Err(error) => {
                let unreachable = error.is_unreachable();
                let auth_stale = error.is_auth_stale();
                let _ = tx
                    .send(RuntimeEvent::ChatDisconnected {
                        generation,
                        message: error.to_string(),
                        unreachable,
                        auth_stale,
                    })
                    .await;
            }
        }
    })
}
