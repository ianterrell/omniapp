use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use thiserror::Error;

use crate::Workspace;

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("could not start filesystem watcher: {0}")]
    Notify(#[from] notify::Error),
}

pub struct WorkspaceWatcher {
    _watcher: RecommendedWatcher,
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl WorkspaceWatcher {
    pub fn start(workspace: Workspace) -> Result<Self, WatcherError> {
        let (events_tx, events_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let _ = events_tx.send(event);
        })?;
        watcher.watch(workspace.root(), RecursiveMode::Recursive)?;

        let worker = thread::spawn(move || {
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }
                let first = match events_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Ok(event)) => event,
                    Ok(Err(error)) => {
                        eprintln!("OmniApp filesystem watcher error: {error}");
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                let mut paths = first.paths;
                loop {
                    match events_rx.recv_timeout(Duration::from_millis(125)) {
                        Ok(Ok(event)) => paths.extend(event.paths),
                        Ok(Err(error)) => {
                            eprintln!("OmniApp filesystem watcher error: {error}");
                        }
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                paths.sort();
                paths.dedup();
                if let Err(error) = workspace.refresh_cache_paths(&paths) {
                    eprintln!("OmniApp incremental index update failed: {error}");
                }
            }
        });

        Ok(Self {
            _watcher: watcher,
            stop: stop_tx,
            worker: Some(worker),
        })
    }
}

impl Drop for WorkspaceWatcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
