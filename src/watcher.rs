use anyhow::Result;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub repo_index: usize,
}

pub fn start_watching(
    repo_paths: Arc<RwLock<Vec<PathBuf>>>,
    tx: mpsc::UnboundedSender<WatchEvent>,
) -> Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
    let debouncer = new_debouncer(
        Duration::from_millis(300),
        move |result: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            let events = match result {
                Ok(events) => events,
                Err(_) => return,
            };

            let repo_paths = repo_paths.read().unwrap();
            let mut seen = HashSet::new();

            for event in events {
                if event.kind != DebouncedEventKind::Any {
                    continue;
                }

                if is_git_internal_path(&event.path) {
                    continue;
                }

                if let Some(idx) = find_repo_index(&repo_paths, &event.path) {
                    if seen.insert(idx) {
                        let _ = tx.send(WatchEvent { repo_index: idx });
                    }
                }
            }
        },
    )?;

    Ok(debouncer)
}

pub fn watch_repo(
    debouncer: &mut notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    path: &Path,
) -> Result<()> {
    use notify::RecursiveMode;
    debouncer
        .watcher()
        .watch(path.as_ref(), RecursiveMode::Recursive)?;
    Ok(())
}

/// Returns true for `.git` paths that are noisy and irrelevant to diff state.
/// Allows through key files that change on commit/checkout/stage/rebase:
/// - HEAD, index, refs/*, MERGE_HEAD, REBASE_HEAD, CHERRY_PICK_HEAD
fn is_git_internal_path(path: &Path) -> bool {
    let components: Vec<_> = path.components().collect();
    let git_pos = components.iter().position(|c| c.as_os_str() == ".git");
    let Some(pos) = git_pos else {
        return false; // not inside .git at all
    };

    // Get the path after `.git/`
    let remaining: Vec<_> = components[pos + 1..].iter().collect();
    if remaining.is_empty() {
        return true; // bare `.git` directory event
    }

    let first = remaining[0].as_os_str().to_string_lossy();
    match first.as_ref() {
        "HEAD" | "index" | "MERGE_HEAD" | "REBASE_HEAD" | "CHERRY_PICK_HEAD" => false,
        "refs" => false, // refs/heads/*, refs/tags/* change on commit/branch ops
        _ => true,       // objects/, logs/, COMMIT_EDITMSG, hooks/, etc.
    }
}

fn find_repo_index(repo_paths: &[PathBuf], event_path: &Path) -> Option<usize> {
    repo_paths
        .iter()
        .position(|repo_path| event_path.starts_with(repo_path))
}
