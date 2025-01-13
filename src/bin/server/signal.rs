use crate::error::DaemonError;

use log::{error, info};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};

#[derive(Debug)]
pub struct SignalHandler {}

impl SignalHandler {
    pub fn new() -> Self {
        SignalHandler {}
    }

    pub async fn handle_signals(&self) -> Result<(), DaemonError> {
        let mut sigterm = signal(SignalKind::terminate()).map_err(|e| {
            DaemonError::InternalError(format!("Failed to register to handle SIGTERM {}", e))
        })?;
        let mut sigint = signal(SignalKind::interrupt()).map_err(|e| {
            DaemonError::InternalError(format!("Failed to register to handle SIGINT {}", e))
        })?;

        let force_quit = Arc::new(AtomicBool::new(false));

        // We loop here so we catch a second signal if one arrives - for example
        // because we hang on the first exit attempt (which is handled in the main
        // select).
        loop {
            tokio::select! {
                _ = sigterm.recv() => {
                    info!("SIGTERM received");
                    if force_quit.load(Ordering::SeqCst) {
                        error!("Second signal received - force quitting");
                        std::process::exit(1);
                    }
                    force_quit.store(true, Ordering::SeqCst);
                    return Ok(());
                }
                _ = sigint.recv() => {
                    info!("SIGINT received");
                    if force_quit.load(Ordering::SeqCst) {
                        error!("Second signal received - force quitting");
                        std::process::exit(1);
                    }
                    force_quit.store(true, Ordering::SeqCst);
                    return Ok(());
                }
            }
        }
    }
}
