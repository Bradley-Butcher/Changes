use anyhow::Result;
use git2::Repository;
use notify_debouncer_mini::notify::RecursiveMode;
use notify_debouncer_mini::{DebouncedEventKind, new_debouncer, notify};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::mpsc;

const DEBOUNCE_DURATION: Duration = Duration::from_millis(300);

#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub repo_path: PathBuf,
    pub base_refresh_needed: bool,
}

pub struct RepoWatcher {
    debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    repo_paths: Arc<RwLock<Vec<PathBuf>>>,
}

impl RepoWatcher {
    pub fn new(repo_paths: Vec<PathBuf>, tx: mpsc::Sender<WatchEvent>) -> Result<Self> {
        let repo_paths = Arc::new(RwLock::new(repo_paths));
        let mut debouncer = start_watching(repo_paths.clone(), tx)?;
        for path in repo_paths.read().unwrap_or_else(|e| e.into_inner()).iter() {
            debouncer.watcher().watch(path, RecursiveMode::Recursive)?;
        }
        Ok(Self {
            debouncer,
            repo_paths,
        })
    }

    pub fn add(&mut self, path: &Path) -> Result<()> {
        self.debouncer
            .watcher()
            .watch(path, RecursiveMode::Recursive)?;
        self.repo_paths
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(path.to_path_buf());
        Ok(())
    }

    pub fn remove(&mut self, path: &Path) {
        // The OS can remove a watch first when its directory disappears.
        // Cleanup must not make closing a repository tab fatal.
        let _ = self.debouncer.watcher().unwatch(path);
        self.repo_paths
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|repo_path| repo_path != path);
    }
}

fn start_watching(
    repo_paths: Arc<RwLock<Vec<PathBuf>>>,
    tx: mpsc::Sender<WatchEvent>,
) -> Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
    let debouncer = new_debouncer(
        DEBOUNCE_DURATION,
        move |result: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            let events = match result {
                Ok(events) => events,
                Err(_) => return,
            };

            let repo_paths = repo_paths.read().unwrap_or_else(|e| e.into_inner());
            let mut pending = HashMap::<PathBuf, bool>::new();
            let mut repositories = HashMap::<PathBuf, Option<Repository>>::new();

            for event in events {
                if event.kind != DebouncedEventKind::Any {
                    continue;
                }

                if is_git_internal_path(&event.path) {
                    continue;
                }

                let Some(repo_path) = find_repo_path(&repo_paths, &event.path) else {
                    continue;
                };
                let git_metadata_changed = is_git_path(&event.path);
                let repository = repositories
                    .entry(repo_path.clone())
                    .or_insert_with(|| Repository::open(&repo_path).ok());
                if !git_metadata_changed
                    && path_is_ignored(repository.as_ref(), &repo_path, &event.path)
                {
                    continue;
                }
                // A linked worktree keeps HEAD and its index outside the watched
                // directory. Its file events can therefore imply a branch change.
                let base_refresh_needed = git_metadata_changed
                    || repository
                        .as_ref()
                        .is_some_and(git2::Repository::is_worktree);

                pending
                    .entry(repo_path)
                    .and_modify(|needed| *needed |= base_refresh_needed)
                    .or_insert(base_refresh_needed);
            }
            drop(repo_paths);

            for (repo_path, base_refresh_needed) in pending {
                let _ = tx.blocking_send(WatchEvent {
                    repo_path,
                    base_refresh_needed,
                });
            }
        },
    )?;

    Ok(debouncer)
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

fn is_git_path(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".git")
}

fn path_is_ignored(repository: Option<&Repository>, repo_path: &Path, event_path: &Path) -> bool {
    let relative = event_path.strip_prefix(repo_path).unwrap_or(event_path);
    let Some(repository) = repository else {
        return false;
    };
    if !repository.status_should_ignore(relative).unwrap_or(false) {
        return false;
    }

    // Ignore rules can still match a file that was added with `git add -f`.
    // If index access fails, keep the event so the live diff cannot become stale.
    repository
        .index()
        .map(|index| !index_contains_path_or_child(&index, relative, event_path))
        .unwrap_or(false)
}

fn index_contains_path_or_child(index: &git2::Index, relative: &Path, event_path: &Path) -> bool {
    if index.get_path(relative, 0).is_some() {
        return true;
    }
    if event_path.is_file() {
        return false;
    }

    let Some(relative) = relative.to_str() else {
        return true;
    };
    let mut prefix = relative
        .replace(std::path::MAIN_SEPARATOR, "/")
        .into_bytes();
    if !prefix.is_empty() && !prefix.ends_with(b"/") {
        prefix.push(b'/');
    }
    index.iter().any(|entry| entry.path.starts_with(&prefix))
}

fn find_repo_path(repo_paths: &[PathBuf], event_path: &Path) -> Option<PathBuf> {
    repo_paths
        .iter()
        .find(|repo_path| event_path.starts_with(repo_path))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::{RepoWatcher, is_git_internal_path, is_git_path, path_is_ignored};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("changes-{label}-{}-{unique}", std::process::id()))
    }

    #[test]
    fn non_git_path_passes_through() {
        assert!(!is_git_internal_path(&PathBuf::from("/repo/src/main.rs")));
    }

    #[test]
    fn git_head_allowed() {
        assert!(!is_git_internal_path(&PathBuf::from("/repo/.git/HEAD")));
    }

    #[test]
    fn git_index_allowed() {
        assert!(!is_git_internal_path(&PathBuf::from("/repo/.git/index")));
    }

    #[test]
    fn git_refs_allowed() {
        assert!(!is_git_internal_path(&PathBuf::from(
            "/repo/.git/refs/heads/main"
        )));
    }

    #[test]
    fn git_merge_head_allowed() {
        assert!(!is_git_internal_path(&PathBuf::from(
            "/repo/.git/MERGE_HEAD"
        )));
    }

    #[test]
    fn git_objects_filtered() {
        assert!(is_git_internal_path(&PathBuf::from(
            "/repo/.git/objects/pack/abc123"
        )));
    }

    #[test]
    fn git_logs_filtered() {
        assert!(is_git_internal_path(&PathBuf::from("/repo/.git/logs/HEAD")));
    }

    #[test]
    fn git_hooks_filtered() {
        assert!(is_git_internal_path(&PathBuf::from(
            "/repo/.git/hooks/pre-commit"
        )));
    }

    #[test]
    fn bare_git_dir_filtered() {
        assert!(is_git_internal_path(&PathBuf::from("/repo/.git")));
    }

    #[test]
    fn commit_editmsg_filtered() {
        assert!(is_git_internal_path(&PathBuf::from(
            "/repo/.git/COMMIT_EDITMSG"
        )));
    }

    #[test]
    fn git_metadata_paths_are_identified() {
        assert!(is_git_path(&PathBuf::from("/repo/.git/refs/heads/main")));
        assert!(!is_git_path(&PathBuf::from("/repo/src/main.rs")));
    }

    #[test]
    fn ignored_build_paths_do_not_trigger_refreshes() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repository = git2::Repository::open(&root).unwrap();
        assert!(path_is_ignored(
            Some(&repository),
            &root,
            &root.join("target/debug/changes")
        ));
    }

    #[test]
    fn tracked_files_are_not_filtered_by_ignore_rules() {
        let root = temporary_path("watcher-ignore-test");
        let repository = git2::Repository::init(&root).unwrap();
        fs::write(root.join(".gitignore"), "forced.txt\n").unwrap();
        fs::write(root.join("forced.txt"), "tracked\n").unwrap();
        assert!(
            repository
                .status_should_ignore(Path::new("forced.txt"))
                .unwrap()
        );

        let mut index = repository.index().unwrap();
        index.add_path(Path::new("forced.txt")).unwrap();
        index.write().unwrap();

        assert!(!path_is_ignored(
            Some(&repository),
            &root,
            &root.join("forced.txt")
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ignored_directories_with_tracked_files_are_not_filtered() {
        let root = temporary_path("watcher-directory-test");
        let repository = git2::Repository::init(&root).unwrap();
        fs::create_dir_all(root.join("ignored")).unwrap();
        fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        fs::write(root.join("ignored/forced.txt"), "tracked\n").unwrap();

        let mut index = repository.index().unwrap();
        index.add_path(Path::new("ignored/forced.txt")).unwrap();
        index.write().unwrap();

        assert!(!path_is_ignored(
            Some(&repository),
            &root,
            &root.join("ignored")
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removing_a_deleted_repository_is_non_fatal() {
        let root = temporary_path("watcher-remove-test");
        fs::create_dir_all(&root).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let mut watcher = RepoWatcher::new(vec![root.clone()], tx).unwrap();

        fs::remove_dir_all(&root).unwrap();
        watcher.remove(&root);

        assert!(
            watcher
                .repo_paths
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }
}
