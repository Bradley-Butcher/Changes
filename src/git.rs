use crate::diff::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};
use anyhow::{Context, Result};
use git2::{Delta, DiffOptions, Repository};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;

const GRAPHITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const GRAPHITE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    Unstaged,
    Staged,
    Branch,
}

impl DiffMode {
    pub fn label(&self, base_branch: Option<&str>) -> Cow<'static, str> {
        match self {
            DiffMode::Unstaged => Cow::Borrowed("Modified"),
            DiffMode::Staged => Cow::Borrowed("Staged"),
            DiffMode::Branch => {
                let base = base_branch.unwrap_or("main");
                Cow::Owned(format!("vs {}", base))
            }
        }
    }

    pub fn next(&self) -> Self {
        match self {
            DiffMode::Unstaged => DiffMode::Staged,
            DiffMode::Staged => DiffMode::Branch,
            DiffMode::Branch => DiffMode::Unstaged,
        }
    }
}

pub struct RepoInfo {
    pub name: String,
    pub path: PathBuf,
}

pub fn discover_repos(root: &Path) -> Result<Vec<RepoInfo>> {
    // Check if root itself is a git repo
    if root.join(".git").exists() {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string());
        return Ok(vec![RepoInfo {
            name,
            path: root.to_path_buf(),
        }]);
    }

    // Scan immediate children
    let mut repos = Vec::new();
    let entries = std::fs::read_dir(root)
        .with_context(|| format!("Failed to read directory: {}", root.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.join(".git").exists() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "repo".to_string());
            repos.push(RepoInfo { name, path });
        }
    }

    repos.sort_by(|a, b| a.name.cmp(&b.name));

    if repos.is_empty() {
        anyhow::bail!("No git repositories found in {}", root.display());
    }

    Ok(repos)
}

pub fn current_branch(repo_path: &Path) -> Option<String> {
    let repo = Repository::open(repo_path).ok()?;
    let head = repo.head().ok()?;
    head.shorthand().map(|s| s.to_string())
}

pub fn compute_diff(
    repo_path: &Path,
    mode: DiffMode,
    base_branch: Option<&str>,
) -> Result<Vec<FileDiff>> {
    let repo = Repository::open(repo_path)
        .with_context(|| format!("Failed to open repo: {}", repo_path.display()))?;

    let mut diff_opts = DiffOptions::new();
    diff_opts.include_untracked(mode == DiffMode::Unstaged);
    diff_opts.recurse_untracked_dirs(true);
    diff_opts.context_lines(3);

    let diff = match mode {
        DiffMode::Unstaged => repo.diff_index_to_workdir(None, Some(&mut diff_opts))?,
        DiffMode::Staged => {
            let head = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            repo.diff_tree_to_index(head.as_ref(), None, Some(&mut diff_opts))?
        }
        DiffMode::Branch => {
            let branch = base_branch
                .map(|s| s.to_string())
                .unwrap_or_else(|| find_base_branch(repo_path));
            compute_branch_diff(&repo, &branch, &mut diff_opts)?
        }
    };

    let mut files: Vec<FileDiff> = Vec::new();
    let mut current_file: Option<FileDiff> = None;
    let mut current_hunk: Option<Hunk> = None;
    let mut current_hunk_header: String = String::new();

    diff.print(
        git2::DiffFormat::Patch,
        |delta: git2::DiffDelta<'_>,
         hunk_opt: Option<git2::DiffHunk<'_>>,
         line: git2::DiffLine<'_>| {
            let file_path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p: &Path| p.to_string_lossy().to_string())
                .unwrap_or_default();

            let status = match delta.status() {
                Delta::Added => FileStatus::Added,
                Delta::Deleted => FileStatus::Deleted,
                Delta::Renamed => FileStatus::Renamed,
                Delta::Untracked => FileStatus::Untracked,
                _ => FileStatus::Modified,
            };

            // Check if this is a new file
            let need_new_file = match &current_file {
                Some(f) => f.path != file_path,
                None => true,
            };

            if need_new_file {
                // Save current hunk to current file
                if let Some(hunk) = current_hunk.take()
                    && let Some(ref mut file) = current_file
                {
                    file.hunks.push(hunk);
                }
                // Save current file
                if let Some(file) = current_file.take() {
                    files.push(file);
                }
                current_file = Some(FileDiff {
                    path: file_path.clone(),
                    old_path: delta
                        .old_file()
                        .path()
                        .map(|p: &Path| p.to_string_lossy().to_string()),
                    status,
                    hunks: Vec::new(),
                    additions: 0,
                    deletions: 0,
                    collapsed: false,
                    total_new_lines: 0,
                    sbs_cache: None,
                });
                current_hunk_header.clear();
            }

            // Handle hunk header — git2 passes hunk_opt on every line in the hunk,
            // so only create a new Hunk when the header actually changes.
            if let Some(hunk_info) = hunk_opt {
                let header = String::from_utf8_lossy(hunk_info.header())
                    .trim()
                    .to_string();
                if header != current_hunk_header {
                    // New hunk — save the previous one
                    if let Some(hunk) = current_hunk.take()
                        && let Some(ref mut file) = current_file
                    {
                        file.hunks.push(hunk);
                    }
                    current_hunk_header = header.clone();
                    current_hunk = Some(Hunk {
                        header,
                        lines: Vec::new(),
                    });
                }
            }

            let content = String::from_utf8_lossy(line.content())
                .trim_end()
                .to_string();

            let (kind, old_lineno, new_lineno) = match line.origin() {
                '+' | '>' => {
                    if let Some(ref mut file) = current_file {
                        file.additions += 1;
                    }
                    (LineKind::Addition, None, line.new_lineno())
                }
                '-' | '<' => {
                    if let Some(ref mut file) = current_file {
                        file.deletions += 1;
                    }
                    (LineKind::Deletion, line.old_lineno(), None)
                }
                ' ' => (LineKind::Context, line.old_lineno(), line.new_lineno()),
                _ => return true,
            };

            let diff_line = DiffLine {
                kind,
                content,
                old_lineno,
                new_lineno,
            };

            if let Some(ref mut hunk) = current_hunk {
                hunk.lines.push(diff_line);
            } else {
                // Lines before any hunk header (shouldn't happen often with git2)
                let hunk = Hunk {
                    header: String::new(),
                    lines: vec![diff_line],
                };
                current_hunk = Some(hunk);
            }

            true
        },
    )?;

    // Flush remaining
    if let Some(hunk) = current_hunk.take()
        && let Some(ref mut file) = current_file
    {
        file.hunks.push(hunk);
    }
    if let Some(file) = current_file.take() {
        files.push(file);
    }

    // Handle untracked files in unstaged mode - read their content as all-additions
    if mode == DiffMode::Unstaged {
        for file in &mut files {
            if file.status == FileStatus::Untracked
                && file.hunks.is_empty()
                && let Ok(content) = std::fs::read_to_string(repo_path.join(&file.path))
            {
                let lines: Vec<DiffLine> = content
                    .lines()
                    .enumerate()
                    .map(|(i, line)| DiffLine {
                        kind: LineKind::Addition,
                        content: line.to_string(),
                        old_lineno: None,
                        new_lineno: Some(i as u32 + 1),
                    })
                    .collect();
                file.additions = lines.len();
                file.hunks.push(Hunk {
                    header: format!("@@ -0,0 +1,{} @@ (new file)", lines.len()),
                    lines,
                });
            }
        }
    }

    // Compute total_new_lines for expand indicators
    for file in &mut files {
        if file.status == FileStatus::Deleted {
            continue;
        }
        let path = repo_path.join(&file.path);
        if let Ok(content) = std::fs::read_to_string(&path) {
            file.total_new_lines = content.lines().count();
        }
    }

    Ok(files)
}

pub fn find_base_branch(repo_path: &Path) -> String {
    // Try Graphite first (with timeout so it can't hang the UI)
    if let Ok(mut child) = Command::new("gt")
        .arg("parent")
        .current_dir(repo_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
    {
        // Poll with a 2-second deadline
        let deadline = std::time::Instant::now() + GRAPHITE_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success()
                        && let Some(mut stdout) = child.stdout.take()
                    {
                        let mut buf = String::new();
                        if std::io::Read::read_to_string(&mut stdout, &mut buf).is_ok() {
                            let parent = buf.trim().to_string();
                            if !parent.is_empty() {
                                return parent;
                            }
                        }
                    }
                    break;
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        break;
                    }
                    std::thread::sleep(GRAPHITE_POLL_INTERVAL);
                }
                Err(_) => break,
            }
        }
    }

    // Fall back to main, then master
    let repo = match Repository::open(repo_path) {
        Ok(r) => r,
        Err(_) => return "main".to_string(),
    };

    if repo.find_branch("main", git2::BranchType::Local).is_ok() {
        "main".to_string()
    } else if repo.find_branch("master", git2::BranchType::Local).is_ok() {
        "master".to_string()
    } else {
        "main".to_string()
    }
}

fn compute_branch_diff<'a>(
    repo: &'a Repository,
    base_branch: &str,
    opts: &mut DiffOptions,
) -> Result<git2::Diff<'a>> {
    let head = repo.head()?.peel_to_commit()?;
    let head_branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(|s| s.to_string()));

    // If we're on the same branch as the base (e.g. on master, base=master),
    // try diffing against the remote tracking branch to show unpushed commits.
    let is_same_branch = head_branch.as_deref() == Some(base_branch);

    let base_commit = if is_same_branch {
        // Try remote tracking branch (e.g. origin/master)
        let remote_name = format!("origin/{}", base_branch);
        match repo.find_branch(&remote_name, git2::BranchType::Remote) {
            Ok(remote_ref) => remote_ref.get().peel_to_commit()?,
            Err(_) => {
                // No remote — nothing meaningful to diff against
                let head_tree = head.tree()?;
                let diff =
                    repo.diff_tree_to_tree(Some(&head_tree), Some(&head_tree), Some(opts))?;
                return Ok(diff);
            }
        }
    } else {
        let base_ref = repo
            .find_branch(base_branch, git2::BranchType::Local)
            .with_context(|| format!("Branch '{}' not found", base_branch))?;
        base_ref.get().peel_to_commit()?
    };

    let merge_base = repo.merge_base(base_commit.id(), head.id())?;
    let merge_base_commit = repo.find_commit(merge_base)?;
    let merge_base_tree = merge_base_commit.tree()?;

    let head_tree = head.tree()?;

    let diff = repo.diff_tree_to_tree(Some(&merge_base_tree), Some(&head_tree), Some(opts))?;
    Ok(diff)
}
