use anyhow::Result;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub repo_index: usize,
}

pub fn start_watching(
    repo_paths: &[PathBuf],
    tx: mpsc::UnboundedSender<WatchEvent>,
) -> Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
    let repo_paths_owned: Vec<PathBuf> = repo_paths.to_vec();

    let debouncer = new_debouncer(
        Duration::from_millis(50),
        move |result: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            let events = match result {
                Ok(events) => events,
                Err(_) => return,
            };

            for event in events {
                if event.kind != DebouncedEventKind::Any {
                    continue;
                }

                // Skip .git directory changes
                if is_git_internal_path(&event.path) {
                    continue;
                }

                // Determine which repo this event belongs to
                if let Some(idx) = find_repo_index(&repo_paths_owned, &event.path) {
                    let _ = tx.send(WatchEvent { repo_index: idx });
                }
            }
        },
    )?;

    Ok(debouncer)
}

pub fn watch_paths(
    debouncer: &mut notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    repo_paths: &[PathBuf],
) -> Result<()> {
    use notify::RecursiveMode;
    for path in repo_paths {
        debouncer
            .watcher()
            .watch(path.as_ref(), RecursiveMode::Recursive)?;
    }
    Ok(())
}

fn is_git_internal_path(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".git")
}

fn find_repo_index(repo_paths: &[PathBuf], event_path: &Path) -> Option<usize> {
    repo_paths
        .iter()
        .position(|repo_path| event_path.starts_with(repo_path))
}
