//! Recording the working tree as it changes, so the timeline can replay edits in the
//! order they happened rather than the order git stores them.
//!
//! Each settled state becomes a git tree, wrapped in a commit that records which HEAD it
//! sat on, referenced from `refs/changes/snapshots/<branch>/<millis>`. Git deduplicates
//! by content, the refs never appear in branches or `git log`, and `gc` keeps what they
//! reach. Only what happened while `changes` was running is recorded; older commits keep
//! plain file order, and the strip shows the difference honestly.

use anyhow::{Context, Result};
use git2::{Oid, Repository, Signature, TreeBuilder};
use std::collections::BTreeMap;
use std::path::Path;

/// Snapshots kept per branch; older ones are pruned as new ones arrive.
pub const MAX_SNAPSHOTS_PER_BRANCH: usize = 400;

const REF_ROOT: &str = "refs/changes/snapshots";

/// One recorded working-tree state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The snapshot commit; a real commit object, so it can be diffed like any other.
    pub id: Oid,
    pub tree: Oid,
    /// HEAD when the snapshot was taken: the commit the edits were made on top of.
    pub head: Oid,
    /// Seconds since the epoch.
    pub time: i64,
    /// Milliseconds since the epoch, from the ref name; orders snapshots within a second.
    pub sequence: u128,
}

fn branch_ref_prefix(branch: &str) -> String {
    format!("{REF_ROOT}/{branch}")
}

/// Record the working tree if it differs from the last snapshot on this branch. Returns
/// the new snapshot's id, or None when nothing changed since the last one.
pub fn record(repo_path: &Path, branch: &str) -> Result<Option<Oid>> {
    let repo = Repository::open(repo_path)
        .with_context(|| format!("Failed to open repo: {}", repo_path.display()))?;
    let head = repo.head()?.peel_to_commit()?;
    let tree = working_tree(&repo, &head.tree()?)?;

    let existing = list(&repo, branch)?;
    if existing.last().is_some_and(|last| last.tree == tree) {
        return Ok(None);
    }

    let signature = Signature::now("changes", "changes@localhost")?;
    let message = format!("changes snapshot\n\nhead: {}\n", head.id());
    let tree_obj = repo.find_tree(tree)?;
    let id = repo.commit(None, &signature, &signature, &message, &tree_obj, &[])?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    repo.reference(
        &format!("{}/{millis:013}", branch_ref_prefix(branch)),
        id,
        true,
        "changes snapshot",
    )?;
    prune(&repo, branch, existing.len() + 1)?;
    Ok(Some(id))
}

/// Snapshots on `branch`, oldest first.
pub fn list(repo: &Repository, branch: &str) -> Result<Vec<Snapshot>> {
    let mut snapshots = Vec::new();
    let glob = format!("{}/*", branch_ref_prefix(branch));
    for reference in repo.references_glob(&glob)? {
        let reference = reference?;
        let Some(commit) = reference.peel_to_commit().ok() else {
            continue;
        };
        let head = commit
            .message()
            .and_then(|m| m.lines().find_map(|l| l.strip_prefix("head: ")))
            .and_then(|s| Oid::from_str(s.trim()).ok());
        let Some(head) = head else {
            continue;
        };
        let sequence = reference
            .name()
            .and_then(|name| name.rsplit('/').next())
            .and_then(|millis| millis.parse::<u128>().ok())
            .unwrap_or(0);
        snapshots.push(Snapshot {
            id: commit.id(),
            tree: commit.tree_id(),
            head,
            time: commit.time().seconds(),
            sequence,
        });
    }
    snapshots.sort_by_key(|s| s.sequence);
    Ok(snapshots)
}

/// Delete the oldest refs beyond the cap. `count` is the number of snapshots now.
fn prune(repo: &Repository, branch: &str, count: usize) -> Result<()> {
    if count <= MAX_SNAPSHOTS_PER_BRANCH {
        return Ok(());
    }
    let glob = format!("{}/*", branch_ref_prefix(branch));
    let mut names: Vec<String> = repo
        .references_glob(&glob)?
        .filter_map(|r| r.ok().and_then(|r| r.name().map(str::to_string)))
        .collect();
    names.sort();
    let excess = names.len().saturating_sub(MAX_SNAPSHOTS_PER_BRANCH);
    for name in names.into_iter().take(excess) {
        if let Ok(mut reference) = repo.find_reference(&name) {
            let _ = reference.delete();
        }
    }
    Ok(())
}

/// The tree id of the working tree right now: HEAD's tree with every changed path
/// updated, so unchanged files cost nothing. Untracked files count; ignored and vendored
/// ones do not.
pub fn working_tree(repo: &Repository, head_tree: &git2::Tree<'_>) -> Result<Oid> {
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .exclude_submodules(true)
        .renames_head_to_index(false)
        .renames_index_to_workdir(false);
    let statuses = repo.statuses(Some(&mut options))?;
    let workdir = repo.workdir().context("bare repository")?;

    let mut updates: BTreeMap<String, Option<(Oid, i32)>> = BTreeMap::new();
    for entry in statuses.iter() {
        let Some(path) = entry.path() else {
            continue;
        };
        if crate::symbols::is_vendored_path(path) {
            continue;
        }
        let full = workdir.join(path);
        let metadata = std::fs::symlink_metadata(&full).ok();
        let value = match metadata {
            Some(meta) if meta.is_file() => {
                let mode = if is_executable(&meta) {
                    0o100755
                } else {
                    0o100644
                };
                Some((repo.blob_path(&full)?, mode))
            }
            Some(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&full)?;
                let blob = repo.blob(target.to_string_lossy().as_bytes())?;
                Some((blob, 0o120000))
            }
            _ => None, // deleted, or a directory placeholder
        };
        updates.insert(path.to_string(), value);
    }
    if updates.is_empty() {
        return Ok(head_tree.id());
    }
    let updates: Vec<(Vec<&str>, Option<(Oid, i32)>)> = updates
        .iter()
        .map(|(path, value)| (path.split('/').collect(), *value))
        .collect();
    update_tree(repo, Some(head_tree), &updates)
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Write `base` with `updates` applied, recursing into directories. An update of `None`
/// removes the path; an empty directory disappears with its last file.
fn update_tree(
    repo: &Repository,
    base: Option<&git2::Tree<'_>>,
    updates: &[(Vec<&str>, Option<(Oid, i32)>)],
) -> Result<Oid> {
    let mut builder: TreeBuilder = repo.treebuilder(base)?;
    // Group updates by their first path component.
    let mut groups: BTreeMap<&str, Vec<(Vec<&str>, Option<(Oid, i32)>)>> = BTreeMap::new();
    for (parts, value) in updates {
        let Some((first, rest)) = parts.split_first() else {
            continue;
        };
        groups
            .entry(first)
            .or_default()
            .push((rest.to_vec(), *value));
    }
    for (name, group) in groups {
        let leaf = group.iter().find(|(rest, _)| rest.is_empty());
        if let Some((_, value)) = leaf {
            match value {
                Some((blob, mode)) => {
                    builder.insert(name, *blob, *mode)?;
                }
                None => {
                    if builder.get(name)?.is_some() {
                        builder.remove(name)?;
                    }
                }
            }
            continue;
        }
        let existing = base
            .and_then(|tree| tree.get_name(name))
            .and_then(|entry| entry.to_object(repo).ok())
            .and_then(|object| object.into_tree().ok());
        let sub = update_tree(repo, existing.as_ref(), &group)?;
        if repo.find_tree(sub)?.is_empty() {
            if builder.get(name)?.is_some() {
                builder.remove(name)?;
            }
        } else {
            builder.insert(name, sub, 0o040000)?;
        }
    }
    Ok(builder.write()?)
}

#[cfg(test)]
mod tests {
    use super::{list, record, working_tree};
    use std::path::{Path, PathBuf};

    fn temp_repo(label: &str) -> (PathBuf, git2::Repository) {
        let root = std::env::temp_dir().join(format!(
            "changes-snap-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let repo = git2::Repository::init(&root).unwrap();
        (root, repo)
    }

    fn commit_all(repo: &git2::Repository, message: &str) -> git2::Oid {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
            .unwrap()
    }

    #[test]
    fn working_tree_applies_edits_additions_and_deletions_to_head() {
        let (root, repo) = temp_repo("tree");
        std::fs::write(root.join("src/a.rs"), "a\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "b\n").unwrap();
        std::fs::write(root.join("README"), "r\n").unwrap();
        commit_all(&repo, "base");
        let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
        assert_eq!(working_tree(&repo, &head_tree).unwrap(), head_tree.id());

        std::fs::write(root.join("src/a.rs"), "a2\n").unwrap();
        std::fs::remove_file(root.join("src/b.rs")).unwrap();
        std::fs::create_dir_all(root.join("new/deep")).unwrap();
        std::fs::write(root.join("new/deep/c.rs"), "c\n").unwrap();
        let tree = repo
            .find_tree(working_tree(&repo, &head_tree).unwrap())
            .unwrap();
        let entry = |p: &str| tree.get_path(Path::new(p)).ok().map(|e| e.id());
        assert_eq!(entry("src/a.rs"), Some(repo.blob(b"a2\n").unwrap()));
        assert_eq!(entry("src/b.rs"), None);
        assert_eq!(entry("new/deep/c.rs"), Some(repo.blob(b"c\n").unwrap()));
        assert_eq!(
            entry("README"),
            head_tree.get_path(Path::new("README")).ok().map(|e| e.id())
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn record_skips_unchanged_states_and_remembers_head() {
        let (root, repo) = temp_repo("record");
        std::fs::write(root.join("src/a.rs"), "a\n").unwrap();
        let head = commit_all(&repo, "base");

        std::fs::write(root.join("src/a.rs"), "a2\n").unwrap();
        let first = record(&root, "main").unwrap();
        assert!(first.is_some());
        assert!(record(&root, "main").unwrap().is_none(), "same state twice");

        std::fs::write(root.join("src/a.rs"), "a3\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = record(&root, "main").unwrap().unwrap();

        let snapshots = list(&repo, "main").unwrap();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[1].id, second);
        assert!(snapshots.iter().all(|s| s.head == head));
        assert_ne!(snapshots[0].tree, snapshots[1].tree);
        // Nothing leaked into the branch.
        assert_eq!(repo.head().unwrap().target(), Some(head));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
