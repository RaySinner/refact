use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::signal;
use tracing::info;

use refact_core::scheduler_config::SchedulerConfig;
use refact_ext::hooks::HooksConfig;
use refact_files::path_utils::canonical_path;

use crate::command_line::CommandLine;
use crate::global_context::EngineGlobalConfig;
use crate::terminal_security::TerminalSecurityConfig;

pub async fn load_engine_global_config(
    config_dir: &PathBuf,
    cmdline: &CommandLine,
) -> (SchedulerConfig, HooksConfig, TerminalSecurityConfig) {
    let path = if cmdline.privacy_yaml.is_empty() {
        config_dir.join("privacy.yaml")
    } else {
        canonical_path(cmdline.privacy_yaml.clone())
    };
    let config = match tokio::fs::read_to_string(&path).await {
        Ok(content) => {
            serde_yaml::from_str::<EngineGlobalConfig>(&content).unwrap_or_else(|error| {
                tracing::warn!(
                    "failed to parse engine global config from {}: {error}",
                    path.display()
                );
                EngineGlobalConfig::default()
            })
        }
        Err(error) => {
            tracing::warn!(
                "failed to read engine global config from {}: {error}",
                path.display()
            );
            EngineGlobalConfig::default()
        }
    };
    let scheduler = config
        .scheduler
        .with_startup_overrides(cmdline.no_scheduler);
    (scheduler, config.hooks, config.terminal_security)
}

pub async fn migrate_to_config_folder(config_dir: &PathBuf, cache_dir: &PathBuf) -> io::Result<()> {
    let mut entries = tokio::fs::read_dir(cache_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
        let file_type = entry.file_type().await?;
        let is_yaml_cfg =
            file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("yaml");
        if is_yaml_cfg {
            let new_path = config_dir.join(&file_name);
            if new_path.exists() {
                tracing::info!(
                    "cannot migrate {:?} to {:?}: destination exists",
                    path,
                    new_path
                );
                continue;
            }
            tokio::fs::rename(&path, &new_path).await?;
            tracing::info!("migrated {:?} to {:?}", path, new_path);
        }
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn get_app_searchable_id(workspace_folders: &[PathBuf]) -> String {
    let mac = pnet_datalink::interfaces()
        .into_iter()
        .find(|iface: &pnet_datalink::NetworkInterface| !iface.is_loopback() && iface.mac.is_some())
        .and_then(|iface| iface.mac)
        .map(|mac| mac.to_string().replace(":", ""))
        .unwrap_or_else(|| "no-mac".to_string());

    let folders = workspace_folders
        .iter()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(";");

    format!("{}-{}", mac, folders)
}

#[cfg(target_os = "windows")]
pub fn get_app_searchable_id(workspace_folders: &[PathBuf]) -> String {
    use winreg::enums::*;
    use winreg::RegKey;
    let machine_guid = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\Microsoft\\Cryptography")
        .and_then(|key| key.get_value::<String, _>("MachineGuid"))
        .unwrap_or_else(|_| "no-machine-guid".to_string());
    let folders = workspace_folders
        .iter()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(";");
    format!("{}-{}", machine_guid, folders)
}

pub async fn block_until_signal(
    ask_shutdown_receiver: std::sync::mpsc::Receiver<String>,
    shutdown_flag: Arc<AtomicBool>,
) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let sigterm = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    #[cfg(unix)]
    let sigusr1 = async {
        signal::unix::signal(signal::unix::SignalKind::user_defined1())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let sigusr1 = std::future::pending::<()>();

    let shutdown_flag_clone = shutdown_flag.clone();
    tokio::select! {
        _ = ctrl_c => {
            info!("SIGINT signal received");
            shutdown_flag_clone.store(true, Ordering::SeqCst);
        },
        _ = sigterm => {
            info!("SIGTERM signal received");
            shutdown_flag_clone.store(true, Ordering::SeqCst);
        },
        _ = sigusr1 => {
            info!("SIGUSR1 signal received");
        },
        _ = tokio::task::spawn_blocking(move || {
            let _ = ask_shutdown_receiver.recv();
            shutdown_flag.store(true, Ordering::SeqCst);
        }) => {
            info!("graceful shutdown requested");
        }
    }
}
