use anyhow::Result;
use git2::Repository;
use notify::RecursiveMode;
use notify_debouncer_mini::{DebouncedEventKind, new_debouncer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::mpsc;

/// Delay between the last file event and a diff refresh. Long enough to coalesce an
/// agent's burst of writes, short enough that the screen feels live.
const DEBOUNCE_DURATION: Duration = Duration::from_millis(60);

#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub repo_path: PathBuf,
    pub base_refresh_needed: bool,
}

/// A git directory that lives outside the worktree it belongs to. A linked worktree keeps
/// HEAD and its index under `main/.git/worktrees/<name>` and shares refs in `main/.git`, so
/// staging or switching branches there never touches the watched worktree directory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalGitDir {
    /// Directory handed to the OS watcher.
    watch: PathBuf,
    /// Prefix stripped from event paths before applying the git metadata allowlist.
    strip: PathBuf,
    /// The repository tab the events belong to.
    repo_path: PathBuf,
}

#[derive(Default)]
struct WatchedRepos {
    repo_paths: Vec<PathBuf>,
    external_git_dirs: Vec<ExternalGitDir>,
}

pub struct RepoWatcher {
    debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    watched: Arc<RwLock<WatchedRepos>>,
}

impl RepoWatcher {
    pub fn new(repo_paths: Vec<PathBuf>, tx: mpsc::Sender<WatchEvent>) -> Result<Self> {
        let watched = Arc::new(RwLock::new(WatchedRepos::default()));
        let debouncer = start_watching(watched.clone(), tx)?;
        let mut watcher = Self { debouncer, watched };
        for path in repo_paths {
            watcher.add(&path)?;
        }
        Ok(watcher)
    }

    pub fn add(&mut self, path: &Path) -> Result<()> {
        self.debouncer
            .watcher()
            .watch(path, RecursiveMode::Recursive)?;
        let externals = external_git_dirs(path);
        for external in &externals {
            // A missing refs directory is not fatal; the worktree itself is still watched.
            let _ = self
                .debouncer
                .watcher()
                .watch(&external.watch, RecursiveMode::Recursive);
        }
        let mut watched = self.watched.write().unwrap_or_else(|e| e.into_inner());
        watched.repo_paths.push(path.to_path_buf());
        watched.external_git_dirs.extend(externals);
        Ok(())
    }

    pub fn remove(&mut self, path: &Path) {
        // The OS can remove a watch first when its directory disappears.
        // Cleanup must not make closing a repository tab fatal.
        let _ = self.debouncer.watcher().unwatch(path);
        let mut watched = self.watched.write().unwrap_or_else(|e| e.into_inner());
        watched.repo_paths.retain(|repo_path| repo_path != path);
        let (removed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut watched.external_git_dirs)
            .into_iter()
            .partition(|external| external.repo_path == path);
        watched.external_git_dirs = kept;
        for external in removed {
            // Another tab (the main repository) may still need this directory watched.
            let still_used = watched
                .external_git_dirs
                .iter()
                .any(|other| other.watch == external.watch);
            if !still_used {
                let _ = self.debouncer.watcher().unwatch(&external.watch);
            }
        }
    }

    #[cfg(test)]
    fn repo_paths(&self) -> Vec<PathBuf> {
        self.watched
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .repo_paths
            .clone()
    }
}

/// Git directories for `repo_path` that are not inside it. Empty for an ordinary checkout,
/// whose `.git` directory is already covered by the recursive worktree watch.
fn external_git_dirs(repo_path: &Path) -> Vec<ExternalGitDir> {
    let Ok(repository) = Repository::open(repo_path) else {
        return Vec::new();
    };
    // git2 reports canonical paths; compare against the canonical worktree path so a
    // symlinked checkout (e.g. macOS /var → /private/var) is not mistaken for external.
    let canonical_repo = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());
    let mut externals = Vec::new();
    let git_dir = repository.path().to_path_buf();
    if !git_dir.starts_with(&canonical_repo) {
        externals.push(ExternalGitDir {
            watch: git_dir.clone(),
            strip: git_dir.clone(),
            repo_path: repo_path.to_path_buf(),
        });
    }
    let common_dir = repository.commondir().to_path_buf();
    if common_dir != git_dir && !common_dir.starts_with(&canonical_repo) {
        // Only refs are shared state worth watching here; objects/ and logs/ are noise.
        externals.push(ExternalGitDir {
            watch: common_dir.join("refs"),
            strip: common_dir,
            repo_path: repo_path.to_path_buf(),
        });
    }
    externals
}

/// The external git directory an event path falls under, preferring the most specific one.
fn find_external_git_dir<'a>(
    externals: &'a [ExternalGitDir],
    event_path: &Path,
) -> Option<&'a ExternalGitDir> {
    externals
        .iter()
        .filter(|external| event_path.starts_with(&external.strip))
        .max_by_key(|external| external.strip.components().count())
}

fn start_watching(
    watched: Arc<RwLock<WatchedRepos>>,
    tx: mpsc::Sender<WatchEvent>,
) -> Result<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>> {
    let debouncer = new_debouncer(
        DEBOUNCE_DURATION,
        move |result: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            let events = match result {
                Ok(events) => events,
                Err(_) => return,
            };

            let watched = watched.read().unwrap_or_else(|e| e.into_inner());
            let mut pending = HashMap::<PathBuf, bool>::new();
            let mut repositories = HashMap::<PathBuf, Option<Repository>>::new();

            for event in events {
                if event.kind != DebouncedEventKind::Any {
                    continue;
                }

                // Metadata of a linked worktree stored outside its directory.
                if let Some(external) =
                    find_external_git_dir(&watched.external_git_dirs, &event.path)
                {
                    let relative = event
                        .path
                        .strip_prefix(&external.strip)
                        .unwrap_or(&event.path);
                    if git_metadata_is_relevant(relative) {
                        pending.insert(external.repo_path.clone(), true);
                    }
                    // Fall through: the same path may also belong to a watched main repo.
                }

                if is_git_internal_path(&event.path) {
                    continue;
                }

                let Some(repo_path) = find_repo_path(&watched.repo_paths, &event.path) else {
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
            drop(watched);

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

/// Whether a path relative to a git directory changes on commit/checkout/stage/rebase:
/// HEAD, index, refs/*, MERGE_HEAD, REBASE_HEAD, CHERRY_PICK_HEAD. Everything else
/// (objects/, logs/, COMMIT_EDITMSG, hooks/, ...) is noise.
fn git_metadata_is_relevant(relative: &Path) -> bool {
    let Some(first) = relative.components().next() else {
        return false; // the git directory itself
    };
    matches!(
        first.as_os_str().to_string_lossy().as_ref(),
        "HEAD" | "index" | "MERGE_HEAD" | "REBASE_HEAD" | "CHERRY_PICK_HEAD" | "refs"
    )
}

/// Returns true for `.git` paths that are noisy and irrelevant to diff state.
/// Allows through key files that change on commit/checkout/stage/rebase:
/// - HEAD, index, refs/*, MERGE_HEAD, REBASE_HEAD, CHERRY_PICK_HEAD
fn is_git_internal_path(path: &Path) -> bool {
    let components: Vec<_> = path.components().collect();
    let Some(pos) = components.iter().position(|c| c.as_os_str() == ".git") else {
        return false; // not inside .git at all
    };
    let relative: PathBuf = components[pos + 1..].iter().collect();
    !git_metadata_is_relevant(&relative)
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
    use super::{
        ExternalGitDir, RepoWatcher, external_git_dirs, find_external_git_dir,
        git_metadata_is_relevant, is_git_internal_path, is_git_path, path_is_ignored,
    };
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

        assert!(watcher.repo_paths().is_empty());
    }

    fn init_repo_with_commit(root: &Path) -> git2::Repository {
        fs::create_dir_all(root).unwrap();
        let repository = git2::Repository::init(root).unwrap();
        fs::write(root.join("tracked.txt"), "tracked\n").unwrap();
        let mut index = repository.index().unwrap();
        index.add_path(Path::new("tracked.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        {
            let tree = repository.find_tree(tree_id).unwrap();
            let signature = git2::Signature::now("Test", "test@example.com").unwrap();
            repository
                .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
                .unwrap();
        }
        repository
    }

    #[test]
    fn ordinary_checkouts_have_no_external_git_dirs() {
        let root = temporary_path("watcher-plain-repo");
        let _repository = init_repo_with_commit(&root);
        assert!(external_git_dirs(&root).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn linked_worktrees_watch_their_git_dir_and_shared_refs() {
        let root = temporary_path("watcher-worktree-main");
        let repository = init_repo_with_commit(&root);
        let worktree_path = temporary_path("watcher-worktree-linked");
        repository.worktree("linked", &worktree_path, None).unwrap();
        let worktree_path = worktree_path.canonicalize().unwrap();

        let externals = external_git_dirs(&worktree_path);
        let git_dir = root
            .canonicalize()
            .unwrap()
            .join(".git")
            .join("worktrees")
            .join("linked");
        let common_dir = root.canonicalize().unwrap().join(".git");
        assert!(
            externals.iter().any(|external| {
                external.strip == git_dir && external.repo_path == worktree_path
            }),
            "expected the worktree git dir in {externals:?}"
        );
        assert!(
            externals
                .iter()
                .any(|external| external.watch == common_dir.join("refs")),
            "expected the shared refs directory in {externals:?}"
        );

        // Staging in the worktree only touches its index, which now maps back to the tab.
        let found = find_external_git_dir(&externals, &git_dir.join("index")).unwrap();
        assert_eq!(found.repo_path, worktree_path);
        assert!(git_metadata_is_relevant(
            git_dir.join("index").strip_prefix(&found.strip).unwrap()
        ));

        fs::remove_dir_all(&worktree_path).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn most_specific_external_git_dir_wins() {
        let externals = vec![
            ExternalGitDir {
                watch: PathBuf::from("/main/.git/refs"),
                strip: PathBuf::from("/main/.git"),
                repo_path: PathBuf::from("/wt"),
            },
            ExternalGitDir {
                watch: PathBuf::from("/main/.git/worktrees/wt"),
                strip: PathBuf::from("/main/.git/worktrees/wt"),
                repo_path: PathBuf::from("/wt"),
            },
        ];
        let found =
            find_external_git_dir(&externals, Path::new("/main/.git/worktrees/wt/index")).unwrap();
        assert_eq!(found.strip, PathBuf::from("/main/.git/worktrees/wt"));
        assert!(git_metadata_is_relevant(Path::new("index")));
        assert!(git_metadata_is_relevant(Path::new("refs/heads/main")));
        assert!(!git_metadata_is_relevant(Path::new("objects/ab/cdef")));
        assert!(!git_metadata_is_relevant(Path::new("")));
    }
}
