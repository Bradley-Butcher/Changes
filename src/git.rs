use crate::diff::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};
use anyhow::{Context, Result};
use git2::{Delta, DiffOptions, Oid, Repository};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;

const GRAPHITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const GRAPHITE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
const MAX_UNTRACKED_FILE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_UNTRACKED_LINES: usize = 100_000;

/// What a repo tab is comparing. Two of these matter day to day: `Local` answers "what
/// has the agent done that isn't committed", `Branch` answers "what does this unit of
/// work look like against its base". The rest are reachable from the compare picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffMode {
    /// Everything uncommitted: HEAD → working tree, untracked files included.
    Local,
    /// HEAD → index.
    Staged,
    /// Index → working tree.
    Unstaged,
    /// Fork point with `base` → working tree, or → HEAD when `commits_only`.
    Branch { base: Base, commits_only: bool },
    /// One step of the timeline: the tree of commit `from` (the empty tree when None) to
    /// `to`. `kind` only affects the label: a single commit, or everything since the base.
    Range {
        from: Option<String>,
        to: RangeEnd,
        kind: RangeKind,
    },
}

/// The new side of a `DiffMode::Range`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeEnd {
    Commit(String),
    Workdir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeKind {
    /// What one step of the timeline changed.
    Step,
    /// Everything from the base up to the cursor.
    Since,
}

/// Which branch a `DiffMode::Branch` compares against, before it is resolved to a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// The stack parent when Graphite knows one, otherwise trunk.
    Parent,
    /// The repository's main line: origin's default branch or a common name for it.
    Trunk,
    /// The current branch's remote tracking branch.
    Upstream,
    /// A ref the user typed.
    Ref(String),
    /// The empty tree: everything the repository has ever contained. The reference point
    /// for a fresh repo with no remote and no other branch, and for "show me all of it".
    Root,
}

/// The name `Base::Root` resolves to, shown in labels and the picker.
pub const ROOT_BASE_NAME: &str = "repository start";

/// The branches a repo could be compared against, detected once per refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BaseCandidates {
    /// Graphite's parent of the current branch, when `gt` knows one.
    pub parent: Option<String>,
    pub trunk: Option<String>,
    /// Remote tracking branch of the current branch, e.g. `origin/feature`.
    pub upstream: Option<String>,
    pub branch: Option<String>,
}

impl BaseCandidates {
    /// The branch name `base` stands for here, or None when nothing fits. Comparing a
    /// branch with itself is never useful, so a base that names the current branch (being
    /// on main with base main) falls through to its upstream, which shows unpushed work.
    pub fn resolve(&self, base: &Base) -> Option<String> {
        let name = match base {
            Base::Parent => match self.parent.clone().or_else(|| self.trunk.clone()) {
                Some(name) => name,
                // Nothing to diverge from at all: a fresh repository. Everything since
                // the first commit is the only sensible branch view.
                None => return Some(ROOT_BASE_NAME.to_string()),
            },
            Base::Trunk => self.trunk.clone()?,
            Base::Upstream => return self.upstream.clone(),
            Base::Ref(name) => name.clone(),
            Base::Root => return Some(ROOT_BASE_NAME.to_string()),
        };
        if self.branch.as_deref() == Some(name.as_str()) {
            // On trunk itself: unpushed work if there is an upstream, otherwise the whole
            // history, which is what a repo that has never been pushed can show.
            return self
                .upstream
                .clone()
                .or_else(|| (*base == Base::Parent).then(|| ROOT_BASE_NAME.to_string()));
        }
        Some(name)
    }
}

/// Where the "new" side of a diff lives; decides how file contents are read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewSide {
    Workdir,
    Index,
    Head,
    /// A specific commit's tree, for timeline steps.
    Commit(String),
}

impl DiffMode {
    pub fn label(&self, bases: &BaseCandidates) -> Cow<'static, str> {
        match self {
            DiffMode::Local => Cow::Borrowed("Local"),
            DiffMode::Staged => Cow::Borrowed("Staged"),
            DiffMode::Unstaged => Cow::Borrowed("Unstaged"),
            DiffMode::Branch { base, commits_only } => match bases.resolve(base) {
                Some(name) if last_n_commits(&name).is_some() => {
                    let n = last_n_commits(&name).unwrap_or(1);
                    Cow::Owned(if *commits_only {
                        format!("last {n} commit{}", if n == 1 { "" } else { "s" })
                    } else {
                        format!("last {n} + local")
                    })
                }
                Some(name) if name == ROOT_BASE_NAME && *commits_only => {
                    Cow::Borrowed("All commits")
                }
                Some(name) if name == ROOT_BASE_NAME => Cow::Borrowed("Everything"),
                Some(name) if *commits_only => Cow::Owned(format!("vs {name}, commits only")),
                Some(name) => Cow::Owned(format!("vs {name}")),
                None => Cow::Borrowed("Branch"),
            },
            DiffMode::Range { kind, .. } => match kind {
                RangeKind::Step => Cow::Borrowed("Step"),
                RangeKind::Since => Cow::Borrowed("Since"),
            },
        }
    }

    pub fn new_side(&self) -> NewSide {
        match self {
            DiffMode::Local | DiffMode::Unstaged => NewSide::Workdir,
            DiffMode::Staged => NewSide::Index,
            DiffMode::Branch { commits_only, .. } => {
                if *commits_only {
                    NewSide::Head
                } else {
                    NewSide::Workdir
                }
            }
            DiffMode::Range { to, .. } => match to {
                RangeEnd::Commit(id) => NewSide::Commit(id.clone()),
                RangeEnd::Workdir => NewSide::Workdir,
            },
        }
    }

    pub fn is_branch(&self) -> bool {
        matches!(self, DiffMode::Branch { .. })
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

/// The two snapshots a diff compares, resolved once so worker threads can rebuild an
/// identical `Diff` without repeating branch lookups.
#[derive(Debug, Clone, Copy)]
enum DiffSpec {
    IndexToWorkdir,
    TreeToIndex {
        head_tree: Option<Oid>,
    },
    /// A commit's tree against the files on disk, the index consulted for renames and
    /// staged additions. `None` is the empty tree of an unborn HEAD.
    TreeToWorkdir {
        old_tree: Option<Oid>,
    },
    TreeToTree {
        old_tree: Oid,
        new_tree: Oid,
    },
}

impl DiffSpec {
    fn new_side_is_workdir(self) -> bool {
        matches!(
            self,
            DiffSpec::IndexToWorkdir | DiffSpec::TreeToWorkdir { .. }
        )
    }
}

/// Diffs with at least this many changed files are patched on several threads.
const PARALLEL_MIN_DELTAS: usize = 8;
/// libgit2 serialises parts of patch generation internally; beyond this many workers the
/// extra threads only contend for its locks.
const MAX_PATCH_THREADS: usize = 8;

fn diff_options(spec: DiffSpec) -> DiffOptions {
    let mut opts = DiffOptions::new();
    opts.include_untracked(spec.new_side_is_workdir());
    opts.recurse_untracked_dirs(true);
    opts.context_lines(3);
    opts
}

fn build_diff(repo: &Repository, spec: DiffSpec) -> Result<git2::Diff<'_>> {
    let mut opts = diff_options(spec);
    let diff = match spec {
        DiffSpec::IndexToWorkdir => repo.diff_index_to_workdir(None, Some(&mut opts))?,
        DiffSpec::TreeToIndex { head_tree } => {
            let tree = head_tree.map(|id| repo.find_tree(id)).transpose()?;
            repo.diff_tree_to_index(tree.as_ref(), None, Some(&mut opts))?
        }
        DiffSpec::TreeToWorkdir { old_tree } => {
            let tree = old_tree.map(|id| repo.find_tree(id)).transpose()?;
            repo.diff_tree_to_workdir_with_index(tree.as_ref(), Some(&mut opts))?
        }
        DiffSpec::TreeToTree { old_tree, new_tree } => {
            let old = repo.find_tree(old_tree)?;
            let new = repo.find_tree(new_tree)?;
            repo.diff_tree_to_tree(Some(&old), Some(&new), Some(&mut opts))?
        }
    };
    Ok(diff)
}

/// Diff `repo_path` in `mode`. `bases` is what the app has detected so far; `None` means
/// detection hasn't finished and branch modes detect synchronously instead.
pub fn compute_diff(
    repo_path: &Path,
    mode: &DiffMode,
    bases: Option<&BaseCandidates>,
) -> Result<Vec<FileDiff>> {
    let repo = Repository::open(repo_path)
        .with_context(|| format!("Failed to open repo: {}", repo_path.display()))?;
    let head_tree = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_tree().ok())
        .map(|tree| tree.id());

    let spec = match mode {
        DiffMode::Local => DiffSpec::TreeToWorkdir {
            old_tree: head_tree,
        },
        DiffMode::Unstaged => DiffSpec::IndexToWorkdir,
        DiffMode::Staged => DiffSpec::TreeToIndex { head_tree },
        DiffMode::Branch { base, commits_only } => {
            let detected = match bases {
                Some(bases) => Cow::Borrowed(bases),
                None => Cow::Owned(detect_bases(repo_path)),
            };
            let Some(base_ref) = detected.resolve(base) else {
                return Ok(Vec::new());
            };
            branch_diff_spec(&repo, &base_ref, *commits_only)?
        }
        DiffMode::Range { from, to, .. } => {
            let old_tree = match from {
                Some(id) => Some(commit_tree(&repo, id)?),
                None => None,
            };
            match to {
                RangeEnd::Workdir => DiffSpec::TreeToWorkdir { old_tree },
                RangeEnd::Commit(id) => DiffSpec::TreeToTree {
                    old_tree: match old_tree {
                        Some(tree) => tree,
                        None => repo.treebuilder(None)?.write()?,
                    },
                    new_tree: commit_tree(&repo, id)?,
                },
            }
        }
    };
    let diff = build_diff(&repo, spec)?;
    let new_side = mode.new_side();

    // Pre-populate untracked files from deltas — patches skip them because there's no
    // patch content for untracked files.
    let mut files: Vec<FileDiff> = Vec::new();
    if new_side == NewSide::Workdir {
        for delta in diff.deltas() {
            if delta.status() == Delta::Untracked {
                let file_path = delta
                    .new_file()
                    .path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                // An untracked node_modules or build directory (no .gitignore yet) is
                // tens of thousands of files that nobody reviews; reading them would
                // stall every refresh.
                if crate::symbols::is_vendored_path(&file_path) {
                    continue;
                }
                files.push(FileDiff {
                    path: file_path,
                    old_path: None,
                    status: FileStatus::Untracked,
                    hunks: Vec::new(),
                    additions: 0,
                    deletions: 0,
                    collapsed: false,
                    total_new_lines: 0,
                    sbs_cache: None,
                });
            }
        }
    }

    files.extend(collect_files(repo_path, &diff, spec)?);

    // Untracked files have no patch; read their content as all-additions.
    if new_side == NewSide::Workdir {
        for file in &mut files {
            if file.status == FileStatus::Untracked && file.hunks.is_empty() {
                match read_untracked_lines(&repo_path.join(&file.path)) {
                    Some(lines) => {
                        file.additions = lines.len();
                        file.total_new_lines = lines.len();
                        file.hunks.push(Hunk {
                            header: format!("@@ -0,0 +1,{} @@ (new file)", lines.len()),
                            lines,
                        });
                    }
                    None => file.hunks.push(Hunk {
                        header: "@@ content omitted @@".to_string(),
                        lines: vec![DiffLine {
                            kind: LineKind::Context,
                            content: "[content omitted: file is binary, unreadable, or too large]"
                                .to_string(),
                            old_lineno: None,
                            new_lineno: None,
                        }],
                    }),
                }
            }
        }
    }

    // Compute total_new_lines for expand indicators. The "new" side of the diff is the
    // working tree, the index, or the HEAD commit depending on the mode.
    for file in &mut files {
        if matches!(file.status, FileStatus::Deleted | FileStatus::Untracked) {
            continue;
        }
        match &new_side {
            NewSide::Workdir => {
                let path = repo_path.join(&file.path);
                // Some platforms allow directories to be opened as files. Skip all
                // non-regular paths before the streaming line count.
                if !path.is_file() {
                    continue;
                }
                if let Ok(line_count) = count_lines(&path) {
                    file.total_new_lines = line_count;
                }
            }
            NewSide::Index | NewSide::Head | NewSide::Commit(_) => {
                if let Ok(blob) = new_side_blob(&repo, Path::new(&file.path), new_side.clone()) {
                    file.total_new_lines = count_blob_lines(&blob);
                }
            }
        }
    }

    Ok(files)
}

/// Patch every non-untracked delta into a `FileDiff`, in delta order. Large diffs are
/// split across threads; anything unexpected falls back to the single-threaded path.
fn collect_files(repo_path: &Path, diff: &git2::Diff<'_>, spec: DiffSpec) -> Result<Vec<FileDiff>> {
    let expected: Vec<(usize, String)> = diff
        .deltas()
        .enumerate()
        .filter(|(_, delta)| delta.status() != Delta::Untracked)
        .map(|(idx, delta)| (idx, delta_path(&delta)))
        .collect();
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_PATCH_THREADS);
    if expected.len() < PARALLEL_MIN_DELTAS || threads < 2 {
        return collect_files_serial(diff, &expected);
    }
    match collect_files_parallel(repo_path, spec, &expected, threads) {
        Ok(files) => Ok(files),
        // The working tree moved under us, or a blob was unreadable: the serial path
        // reads everything from one consistent `Diff`.
        Err(_) => collect_files_serial(diff, &expected),
    }
}

/// Single-threaded per-delta patching over one `Diff`. Deliberately not `Diff::print`:
/// libgit2 leaks several megabytes per call of that on large working-tree diffs, which
/// added up to hundreds of megabytes over a session of live refreshes.
fn collect_files_serial(
    diff: &git2::Diff<'_>,
    expected: &[(usize, String)],
) -> Result<Vec<FileDiff>> {
    expected
        .iter()
        .filter_map(|(idx, path)| file_from_patch(diff, *idx, path).transpose())
        .collect()
}

fn collect_files_parallel(
    repo_path: &Path,
    spec: DiffSpec,
    expected: &[(usize, String)],
    threads: usize,
) -> Result<Vec<FileDiff>> {
    let chunk_size = expected.len().div_ceil(threads).max(1);
    let groups: Vec<Result<Vec<FileDiff>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = expected
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || -> Result<Vec<FileDiff>> {
                    let repo = Repository::open(repo_path)?;
                    let diff = build_diff(&repo, spec)?;
                    chunk
                        .iter()
                        .filter_map(|(idx, path)| file_from_patch(&diff, *idx, path).transpose())
                        .collect()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("diff worker panicked"))
            .collect()
    });
    let mut files = Vec::with_capacity(expected.len());
    for group in groups {
        files.extend(group?);
    }
    Ok(files)
}

fn delta_path(delta: &git2::DiffDelta<'_>) -> String {
    delta
        .new_file()
        .path()
        .or_else(|| delta.old_file().path())
        .map(|p: &Path| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn file_status(status: Delta) -> FileStatus {
    match status {
        Delta::Added => FileStatus::Added,
        Delta::Deleted => FileStatus::Deleted,
        Delta::Renamed => FileStatus::Renamed,
        Delta::Untracked => FileStatus::Untracked,
        _ => FileStatus::Modified,
    }
}

/// Map a libgit2 line origin to our kind. `>` / `<` are the "no newline at end of file"
/// markers, which git shows on the added / deleted side respectively.
fn line_kind(origin: char) -> Option<LineKind> {
    match origin {
        '+' | '>' => Some(LineKind::Addition),
        '-' | '<' => Some(LineKind::Deletion),
        ' ' => Some(LineKind::Context),
        _ => None,
    }
}

fn line_content(line: &git2::DiffLine<'_>) -> String {
    String::from_utf8_lossy(line.content())
        .trim_end_matches(&['\r', '\n'][..])
        .to_string()
}

/// Build one delta's `FileDiff` from a lazily computed patch.
/// `None` for a text file libgit2 lists as modified although nothing in it changed. A
/// tree-to-workdir diff that consults the index reports every file whose index entry
/// differs from the tree, even when the file on disk matches the tree byte for byte.
fn file_from_patch(
    diff: &git2::Diff<'_>,
    idx: usize,
    expected_path: &str,
) -> Result<Option<FileDiff>> {
    let delta = diff
        .get_delta(idx)
        .context("delta disappeared while diffing")?;
    let path = delta_path(&delta);
    if path != expected_path {
        anyhow::bail!("working tree changed while diffing");
    }
    let mut file = FileDiff {
        path,
        old_path: delta
            .old_file()
            .path()
            .map(|p: &Path| p.to_string_lossy().to_string()),
        status: file_status(delta.status()),
        hunks: Vec::new(),
        additions: 0,
        deletions: 0,
        collapsed: false,
        total_new_lines: 0,
        sbs_cache: None,
    };
    let Some(patch) = git2::Patch::from_diff(diff, idx)? else {
        return Ok(Some(file));
    };
    if patch.num_hunks() == 0
        && file.status == FileStatus::Modified
        && !patch.delta().flags().is_binary()
        && delta.old_file().mode() == delta.new_file().mode()
    {
        return Ok(None);
    }
    for hunk_idx in 0..patch.num_hunks() {
        let (hunk, line_count) = patch.hunk(hunk_idx)?;
        let header = String::from_utf8_lossy(hunk.header()).trim().to_string();
        let mut lines = Vec::with_capacity(line_count);
        for line_idx in 0..line_count {
            let line = patch.line_in_hunk(hunk_idx, line_idx)?;
            let Some(kind) = line_kind(line.origin()) else {
                continue;
            };
            let (old_lineno, new_lineno) = match kind {
                LineKind::Addition => {
                    file.additions += 1;
                    (None, line.new_lineno())
                }
                LineKind::Deletion => {
                    file.deletions += 1;
                    (line.old_lineno(), None)
                }
                LineKind::Context => (line.old_lineno(), line.new_lineno()),
            };
            lines.push(DiffLine {
                kind,
                content: line_content(&line),
                old_lineno,
                new_lineno,
            });
        }
        file.hunks.push(Hunk { header, lines });
    }
    Ok(Some(file))
}

/// Reference implementation: the original walk over libgit2's print callback. Kept only
/// so tests can prove the patch-based paths produce identical output.
#[cfg(test)]
fn collect_files_via_print(diff: &git2::Diff<'_>) -> Result<Vec<FileDiff>> {
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
                .trim_end_matches(&['\r', '\n'][..])
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
    Ok(files)
}

/// Content of `rel_path` from the index or the HEAD commit. Working-tree sides are read
/// from disk instead.
pub fn new_side_blob(repo: &Repository, rel_path: &Path, side: NewSide) -> Result<Vec<u8>> {
    let oid = match side {
        NewSide::Workdir => anyhow::bail!("working-tree diffs read files from disk"),
        NewSide::Index => {
            repo.index()?
                .get_path(rel_path, 0)
                .with_context(|| format!("{} is not in the index", rel_path.display()))?
                .id
        }
        NewSide::Head => repo
            .head()?
            .peel_to_tree()?
            .get_path(rel_path)
            .with_context(|| format!("{} is not in HEAD", rel_path.display()))?
            .id(),
        NewSide::Commit(id) => repo
            .find_tree(commit_tree(repo, &id)?)?
            .get_path(rel_path)
            .with_context(|| format!("{} is not in {id}", rel_path.display()))?
            .id(),
    };
    Ok(repo.find_blob(oid)?.content().to_vec())
}

/// The tree of the commit `id` names.
fn commit_tree(repo: &Repository, id: &str) -> Result<Oid> {
    Ok(repo
        .revparse_single(id)
        .with_context(|| format!("'{id}' is not a commit"))?
        .peel_to_commit()?
        .tree_id())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Commit,
    /// A recorded working-tree state between two commits.
    Snapshot,
    Workdir,
}

/// One node of the timeline: a commit between the base and HEAD, a snapshot recorded
/// while `changes` was running, or the working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineStep {
    pub kind: StepKind,
    /// Full commit id (snapshots are commit objects too); None for the working tree.
    pub id: Option<String>,
    pub short: String,
    pub subject: String,
    /// "2h ago", "3d ago".
    pub when: String,
    /// The step before this one: the previous commit or snapshot, or None when it is
    /// the first commit ever.
    pub parent: Option<String>,
}

/// Commits are listed newest-first by git; the timeline keeps at most this many of the
/// most recent, oldest on the left.
pub const MAX_TIMELINE_STEPS: usize = 500;
/// How far back plain history goes when the branch has nothing unmerged to walk.
pub const HISTORY_TIMELINE_STEPS: usize = 100;

/// The commits from the fork point with `base` (exclusive) to HEAD, oldest first, followed
/// by the working tree. `base` is a branch, tag, commit, or `ROOT_BASE_NAME` for the whole
/// history; None means HEAD's own history as far back as it goes.
pub fn timeline_steps(
    repo_path: &Path,
    base: Option<&str>,
    limit: usize,
    branch: Option<&str>,
) -> Result<Vec<TimelineStep>> {
    let repo = Repository::open(repo_path)
        .with_context(|| format!("Failed to open repo: {}", repo_path.display()))?;
    let head = repo.head()?.peel_to_commit()?;
    let base = base
        .map(|name| normalize_base_ref(&repo, name))
        .transpose()?;
    let stop = match base.as_deref() {
        Some(name) if name != ROOT_BASE_NAME => {
            let base_commit = base_commit(&repo, name)?;
            Some(repo.merge_base(base_commit.id(), head.id())?)
        }
        _ => None,
    };

    let mut walk = repo.revwalk()?;
    walk.simplify_first_parent()?;
    walk.push(head.id())?;
    if let Some(stop) = stop {
        walk.hide(stop)?;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut commits: Vec<(git2::Commit<'_>, Oid)> = Vec::new(); // commit, tree
    for oid in walk.take(limit.min(MAX_TIMELINE_STEPS)) {
        let commit = repo.find_commit(oid?)?;
        let tree = commit.tree_id();
        commits.push((commit, tree));
    }
    commits.reverse();

    // Snapshots recorded while `changes` ran, grouped by the commit they were taken on.
    let snapshots = branch
        .map(|branch| crate::snapshots::list(&repo, branch))
        .transpose()?
        .unwrap_or_default();
    let workdir_tree = crate::snapshots::working_tree(&repo, &head.tree()?).ok();

    let mut steps: Vec<TimelineStep> = Vec::new();
    // Steps chain: each diffs from the step before it, so a commit made after recorded
    // edits shows only what changed since the last edit, not the whole commit again.
    let mut chain_prev: Option<String> = None;

    // Edits recorded on the base commit itself lead up to the first commit after it
    // (or straight to the working tree when the base is HEAD).
    if let Some(stop) = stop {
        let base_tree = repo.find_commit(stop)?.tree_id();
        let next_tree = commits.first().map(|(_, tree)| *tree).or(workdir_tree);
        chain_prev = push_snapshot_steps(
            &mut steps,
            &snapshots,
            stop,
            base_tree,
            stop.to_string(),
            next_tree,
            now,
        );
    }
    for (index, (commit, tree)) in commits.iter().enumerate() {
        let id = commit.id().to_string();
        steps.push(TimelineStep {
            kind: StepKind::Commit,
            short: id[..7.min(id.len())].to_string(),
            id: Some(id.clone()),
            subject: commit.summary().unwrap_or("").to_string(),
            when: relative_time(now - commit.time().seconds()),
            parent: chain_prev
                .clone()
                .or_else(|| commit.parent_id(0).ok().map(|p| p.to_string())),
        });
        let next_tree = commits
            .get(index + 1)
            .map(|(_, tree)| *tree)
            .or(workdir_tree);
        let last_edit = push_snapshot_steps(
            &mut steps,
            &snapshots,
            commit.id(),
            *tree,
            id.clone(),
            next_tree,
            now,
        );
        chain_prev = Some(last_edit.unwrap_or(id));
    }
    let last_id = steps.last().and_then(|s| s.id.clone());
    steps.push(TimelineStep {
        kind: StepKind::Workdir,
        id: None,
        short: "now".to_string(),
        subject: "working tree".to_string(),
        when: String::new(),
        parent: last_id.or_else(|| Some(head.id().to_string())),
    });
    Ok(steps)
}

/// Append the edits recorded while HEAD was `on`, chained from `previous_id`. A snapshot
/// identical to its predecessor adds nothing; one identical to `next_tree` (the next
/// commit, or the working tree) is already a node. Returns the last id pushed.
fn push_snapshot_steps(
    steps: &mut Vec<TimelineStep>,
    snapshots: &[crate::snapshots::Snapshot],
    on: Oid,
    mut previous_tree: Oid,
    mut previous_id: String,
    next_tree: Option<Oid>,
    now: i64,
) -> Option<String> {
    let mut pushed = None;
    for snapshot in snapshots.iter().filter(|s| s.head == on) {
        if snapshot.tree == previous_tree || Some(snapshot.tree) == next_tree {
            continue;
        }
        let snapshot_id = snapshot.id.to_string();
        steps.push(TimelineStep {
            kind: StepKind::Snapshot,
            short: String::new(),
            id: Some(snapshot_id.clone()),
            subject: "recorded edit".to_string(),
            when: relative_time(now - snapshot.time),
            parent: Some(previous_id),
        });
        previous_tree = snapshot.tree;
        previous_id = snapshot_id.clone();
        pushed = Some(snapshot_id);
    }
    pushed
}

/// The timeline for a user-chosen comparison: the nodes between its base and its "now".
/// Local: recorded edits on top of HEAD, then the working tree. Branch: commits since
/// the fork point with recorded edits between them, then the working tree (or just the
/// commits when `commits_only`). Staged and unstaged have no intermediate states.
pub fn timeline_for_mode(
    repo_path: &Path,
    mode: &DiffMode,
    bases: &BaseCandidates,
) -> Result<Vec<TimelineStep>> {
    let branch = bases.branch.as_deref();
    match mode {
        DiffMode::Local => {
            let repo = Repository::open(repo_path)?;
            let head = repo.head()?.peel_to_commit()?.id().to_string();
            // Only snapshots on top of HEAD; the commit itself is the base, not a node.
            let mut steps = timeline_steps(repo_path, None, 1, branch)?;
            steps.retain(|step| step.kind != StepKind::Commit);
            // The first node's parent is the base, HEAD, whatever kind it is.
            if let Some(first) = steps.first_mut() {
                first.parent = Some(head);
            }
            Ok(steps)
        }
        DiffMode::Branch { base, commits_only } => {
            let Some(base_ref) = bases.resolve(base) else {
                return Ok(Vec::new());
            };
            let mut steps = timeline_steps(repo_path, Some(&base_ref), MAX_TIMELINE_STEPS, branch)?;
            if *commits_only {
                // The chain must stay contiguous: re-parent commits onto commits.
                let mut previous: Option<String> = None;
                steps.retain(|step| step.kind == StepKind::Commit);
                for step in &mut steps {
                    if let Some(prev) = &previous {
                        step.parent = Some(prev.clone());
                    }
                    previous = step.id.clone();
                }
            }
            Ok(steps)
        }
        DiffMode::Staged | DiffMode::Unstaged | DiffMode::Range { .. } => Ok(Vec::new()),
    }
}

/// New-side line numbers `to` changed relative to `from`, per file: the added lines,
/// and for deletions the line that now sits where the deleted block was. Used to mark
/// which hunks of the accumulated diff belong to the step under the cursor.
pub fn step_changed_lines(
    repo_path: &Path,
    from: Option<&str>,
    to: &RangeEnd,
) -> Result<std::collections::HashMap<String, Vec<u32>>> {
    let mode = DiffMode::Range {
        from: from.map(str::to_string),
        to: to.clone(),
        kind: RangeKind::Step,
    };
    let files = compute_diff(repo_path, &mode, None)?;
    let mut lines = std::collections::HashMap::new();
    for file in files {
        let mut touched: Vec<u32> = Vec::new();
        for hunk in &file.hunks {
            let mut after_deletion = false;
            for line in &hunk.lines {
                match line.kind {
                    LineKind::Addition => {
                        touched.extend(line.new_lineno);
                        after_deletion = false;
                    }
                    LineKind::Deletion => after_deletion = true,
                    LineKind::Context => {
                        if after_deletion {
                            touched.extend(line.new_lineno);
                            after_deletion = false;
                        }
                    }
                }
            }
            if after_deletion {
                // Deletion at the very end of the hunk: mark the last new line seen.
                touched.extend(hunk.last_new_lineno());
            }
        }
        if !touched.is_empty() {
            lines.insert(file.path, touched);
        }
    }
    Ok(lines)
}

fn relative_time(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        "just now".to_string()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3600)
    } else if seconds < 86_400 * 30 {
        format!("{}d ago", seconds / 86_400)
    } else if seconds < 86_400 * 365 {
        format!("{}mo ago", seconds / (86_400 * 30))
    } else {
        format!("{}y ago", seconds / (86_400 * 365))
    }
}

/// Lines `start..=end` (1-based) of `rel_path` on the new side of a diff in `mode`.
pub fn read_new_side_lines(
    repo_path: &Path,
    rel_path: &Path,
    mode: &DiffMode,
    start: usize,
    end: usize,
) -> Result<Vec<String>> {
    use std::io::BufRead;
    let count = end.saturating_sub(start) + 1;
    let skip = start.saturating_sub(1);
    match mode.new_side() {
        NewSide::Workdir => {
            let file = std::fs::File::open(repo_path.join(rel_path))?;
            Ok(std::io::BufReader::new(file)
                .lines()
                .skip(skip)
                .take(count)
                .map(|line| line.unwrap_or_default())
                .collect())
        }
        side @ (NewSide::Index | NewSide::Head | NewSide::Commit(_)) => {
            let repo = Repository::open(repo_path)?;
            let blob = new_side_blob(&repo, rel_path, side)?;
            Ok(String::from_utf8_lossy(&blob)
                .lines()
                .skip(skip)
                .take(count)
                .map(str::to_string)
                .collect())
        }
    }
}

fn count_blob_lines(bytes: &[u8]) -> usize {
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count();
    if bytes.is_empty() || bytes.ends_with(b"\n") {
        newlines
    } else {
        newlines + 1
    }
}

/// Everything the current branch could be compared against. Runs `gt parent` (bounded
/// by a timeout), so it belongs off the UI thread.
pub fn detect_bases(repo_path: &Path) -> BaseCandidates {
    let parent = graphite_parent(repo_path);
    let Ok(repo) = Repository::open(repo_path) else {
        return BaseCandidates {
            parent,
            ..BaseCandidates::default()
        };
    };
    let branch = repo
        .head()
        .ok()
        .and_then(|head| head.shorthand().map(str::to_string));
    let trunk = remote_default_branch(&repo).or_else(|| find_common_base_branch(&repo));
    let upstream = branch.as_deref().and_then(|name| upstream_of(&repo, name));
    BaseCandidates {
        parent,
        trunk,
        upstream,
        branch,
    }
}

/// The parent Graphite records for the current branch, if `gt` is installed and answers
/// within the timeout.
fn graphite_parent(repo_path: &Path) -> Option<String> {
    let mut child = Command::new("gt")
        .arg("parent")
        .current_dir(repo_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + GRAPHITE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut stdout = child.stdout.take()?;
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut stdout, &mut buf).ok()?;
                let parent = buf.trim();
                return (!parent.is_empty()).then(|| parent.to_string());
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(GRAPHITE_POLL_INTERVAL);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn upstream_of(repo: &Repository, branch: &str) -> Option<String> {
    let local = repo.find_branch(branch, git2::BranchType::Local).ok()?;
    let upstream = local.upstream().ok()?;
    upstream.name().ok().flatten().map(str::to_string)
}

fn read_untracked_lines(path: &Path) -> Option<Vec<DiffLine>> {
    use std::io::{BufRead, Read};

    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_UNTRACKED_FILE_BYTES {
        return None;
    }

    let mut reader = std::io::BufReader::new(file).take(MAX_UNTRACKED_FILE_BYTES + 1);
    let mut lines = Vec::new();
    for (index, line) in reader.by_ref().lines().enumerate() {
        if index >= MAX_UNTRACKED_LINES {
            return None;
        }
        lines.push(DiffLine {
            kind: LineKind::Addition,
            content: line.ok()?,
            old_lineno: None,
            new_lineno: Some(index as u32 + 1),
        });
    }
    (reader.limit() > 0).then_some(lines)
}

fn count_lines(path: &Path) -> std::io::Result<usize> {
    use std::io::BufRead;

    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut count = 0;
    let mut has_bytes = false;
    let mut ends_with_newline = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        has_bytes = true;
        ends_with_newline = buffer.last() == Some(&b'\n');
        count += buffer.iter().filter(|&&byte| byte == b'\n').count();
        let length = buffer.len();
        reader.consume(length);
    }
    Ok(count + usize::from(has_bytes && !ends_with_newline))
}

fn remote_default_branch(repo: &Repository) -> Option<String> {
    let reference = repo.find_reference("refs/remotes/origin/HEAD").ok()?;
    let target = reference.symbolic_target()?;
    target
        .strip_prefix("refs/remotes/origin/")
        .map(std::string::ToString::to_string)
}

fn find_common_base_branch(repo: &Repository) -> Option<String> {
    const COMMON_BASE_BRANCHES: &[&str] = &["main", "master", "develop", "trunk", "dev"];

    COMMON_BASE_BRANCHES
        .iter()
        .find(|branch| branch_exists(repo, branch))
        .map(|branch| (*branch).to_string())
}

fn branch_exists(repo: &Repository, branch: &str) -> bool {
    repo.find_branch(branch, git2::BranchType::Local).is_ok()
        || repo
            .find_branch(&format!("origin/{}", branch), git2::BranchType::Remote)
            .is_ok()
}

/// Fork point of `base_ref` and HEAD → HEAD, or → the working tree unless `commits_only`.
fn branch_diff_spec(repo: &Repository, base_ref: &str, commits_only: bool) -> Result<DiffSpec> {
    let head = repo.head()?.peel_to_commit()?;
    let base_ref = &normalize_base_ref(repo, base_ref)?;
    if base_ref == ROOT_BASE_NAME {
        // The empty tree: every file the repository holds is an addition.
        return Ok(if commits_only {
            DiffSpec::TreeToTree {
                old_tree: repo.treebuilder(None)?.write()?,
                new_tree: head.tree()?.id(),
            }
        } else {
            DiffSpec::TreeToWorkdir { old_tree: None }
        });
    }
    let base_commit = base_commit(repo, base_ref)?;
    let merge_base = repo.merge_base(base_commit.id(), head.id())?;
    let merge_base_tree = repo.find_commit(merge_base)?.tree()?.id();

    if commits_only {
        Ok(DiffSpec::TreeToTree {
            old_tree: merge_base_tree,
            new_tree: head.tree()?.id(),
        })
    } else {
        Ok(DiffSpec::TreeToWorkdir {
            old_tree: Some(merge_base_tree),
        })
    }
}

/// `-2`, `~2`, `2` and `HEAD~2` all mean "the last two commits". Returns how many.
pub fn last_n_commits(base_ref: &str) -> Option<usize> {
    let trimmed = base_ref.trim();
    let digits = trimmed
        .strip_prefix("HEAD~")
        .or_else(|| trimmed.strip_prefix("@~"))
        .or_else(|| trimmed.strip_prefix('-'))
        .or_else(|| trimmed.strip_prefix('~'))
        .unwrap_or(trimmed);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|n| *n > 0)
}

/// Turn a "last N commits" ref into the commit N first-parent steps behind HEAD, or the
/// repository start when the history is shorter than that. Other refs pass through.
fn normalize_base_ref(repo: &Repository, base_ref: &str) -> Result<String> {
    let Some(n) = last_n_commits(base_ref) else {
        return Ok(base_ref.to_string());
    };
    let mut commit = repo.head()?.peel_to_commit()?;
    for _ in 0..n {
        match commit.parent(0) {
            Ok(parent) => commit = parent,
            Err(_) => return Ok(ROOT_BASE_NAME.to_string()),
        }
    }
    Ok(commit.id().to_string())
}

/// The commit `base_ref` names. Prefers `origin/<base_ref>`: it's almost always at or
/// ahead of the rebase point, so merge-base finds the fork point, whereas the local
/// branch often lags after a rebase onto origin. Falls back to whatever git can parse,
/// so typed refs like `origin/main`, tags and SHAs work too.
fn base_commit<'repo>(repo: &'repo Repository, base_ref: &str) -> Result<git2::Commit<'repo>> {
    let remote_name = format!("origin/{base_ref}");
    if let Ok(remote_ref) = repo.find_branch(&remote_name, git2::BranchType::Remote) {
        return Ok(remote_ref.get().peel_to_commit()?);
    }
    if let Ok(local_ref) = repo.find_branch(base_ref, git2::BranchType::Local) {
        return Ok(local_ref.get().peel_to_commit()?);
    }
    repo.revparse_single(base_ref)
        .and_then(|object| object.peel_to_commit())
        .with_context(|| format!("'{base_ref}' is not a branch, tag or commit"))
}

#[cfg(test)]
mod tests {
    use super::{
        Base, BaseCandidates, DiffMode, DiffSpec, MAX_UNTRACKED_FILE_BYTES, MAX_UNTRACKED_LINES,
        PARALLEL_MIN_DELTAS, ROOT_BASE_NAME, RangeEnd, RangeKind, build_diff,
        collect_files_parallel, collect_files_serial, collect_files_via_print, compute_diff,
        count_lines, delta_path, last_n_commits, read_untracked_lines,
    };
    use crate::diff::{FileDiff, FileStatus, LineKind};
    use git2::Delta;
    use std::path::Path;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "changes-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// Stage every path and commit the index; returns the new commit.
    fn commit_all(repo: &git2::Repository, message: &str) -> git2::Oid {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .unwrap()
    }

    fn statuses(files: &[FileDiff]) -> Vec<(String, FileStatus)> {
        let mut out: Vec<(String, FileStatus)> = files
            .iter()
            .map(|file| (file.path.clone(), file.status))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    #[test]
    fn local_mode_merges_staged_unstaged_and_untracked_against_head() {
        let root = temp_path("local-mode");
        std::fs::create_dir(&root).unwrap();
        let repo = git2::Repository::init(&root).unwrap();
        std::fs::write(root.join("staged.txt"), "one\n").unwrap();
        std::fs::write(root.join("unstaged.txt"), "one\n").unwrap();
        std::fs::write(root.join("both.txt"), "one\ntwo\n").unwrap();
        commit_all(&repo, "initial");

        std::fs::write(root.join("staged.txt"), "one\nstaged\n").unwrap();
        std::fs::write(root.join("both.txt"), "one\nstaged\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("staged.txt")).unwrap();
        index.add_path(Path::new("both.txt")).unwrap();
        index.write().unwrap();
        drop(index);
        std::fs::write(root.join("unstaged.txt"), "one\nunstaged\n").unwrap();
        std::fs::write(root.join("both.txt"), "one\nstaged\nunstaged\n").unwrap();
        std::fs::write(root.join("untracked.txt"), "new\n").unwrap();
        drop(repo);

        let local = compute_diff(&root, &DiffMode::Local, None).unwrap();
        let staged = compute_diff(&root, &DiffMode::Staged, None).unwrap();
        let unstaged = compute_diff(&root, &DiffMode::Unstaged, None).unwrap();
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(
            statuses(&local),
            [
                ("both.txt".to_string(), FileStatus::Modified),
                ("staged.txt".to_string(), FileStatus::Modified),
                ("unstaged.txt".to_string(), FileStatus::Modified),
                ("untracked.txt".to_string(), FileStatus::Untracked),
            ]
        );
        // The file edited in both places is one net diff against HEAD, not two hunks.
        let both = local.iter().find(|f| f.path == "both.txt").unwrap();
        assert_eq!((both.additions, both.deletions), (2, 1));
        assert_eq!(both.total_new_lines, 3);
        let untracked = local.iter().find(|f| f.path == "untracked.txt").unwrap();
        assert_eq!(untracked.additions, 1);

        assert_eq!(
            statuses(&staged),
            [
                ("both.txt".to_string(), FileStatus::Modified),
                ("staged.txt".to_string(), FileStatus::Modified),
            ]
        );
        assert_eq!(
            statuses(&unstaged),
            [
                ("both.txt".to_string(), FileStatus::Modified),
                ("unstaged.txt".to_string(), FileStatus::Modified),
                ("untracked.txt".to_string(), FileStatus::Untracked),
            ]
        );
    }

    #[test]
    fn branch_mode_includes_uncommitted_work_unless_commits_only() {
        let root = temp_path("branch-mode");
        std::fs::create_dir(&root).unwrap();
        let repo = git2::Repository::init(&root).unwrap();
        std::fs::write(root.join("committed.txt"), "one\n").unwrap();
        std::fs::write(root.join("dirty.txt"), "one\n").unwrap();
        let base = commit_all(&repo, "initial");
        repo.branch("main", &repo.find_commit(base).unwrap(), true)
            .unwrap();
        repo.branch("feature", &repo.find_commit(base).unwrap(), true)
            .unwrap();
        repo.set_head("refs/heads/feature").unwrap();

        std::fs::write(root.join("committed.txt"), "one\ntwo\n").unwrap();
        commit_all(&repo, "feature work");
        std::fs::write(root.join("dirty.txt"), "one\nedited\n").unwrap();
        std::fs::write(root.join("new.txt"), "brand new\n").unwrap();
        drop(repo);

        let bases = BaseCandidates {
            parent: None,
            trunk: Some("main".to_string()),
            upstream: None,
            branch: Some("feature".to_string()),
        };
        let with_local = DiffMode::Branch {
            base: Base::Parent,
            commits_only: false,
        };
        let commits_only = DiffMode::Branch {
            base: Base::Parent,
            commits_only: true,
        };
        let typed = DiffMode::Branch {
            base: Base::Ref("main".to_string()),
            commits_only: true,
        };
        let everything = compute_diff(&root, &with_local, Some(&bases)).unwrap();
        let committed = compute_diff(&root, &commits_only, Some(&bases)).unwrap();
        let via_ref = compute_diff(&root, &typed, Some(&bases)).unwrap();
        let missing = compute_diff(
            &root,
            &DiffMode::Branch {
                base: Base::Ref("nope".to_string()),
                commits_only: true,
            },
            Some(&bases),
        );
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(
            statuses(&everything),
            [
                ("committed.txt".to_string(), FileStatus::Modified),
                ("dirty.txt".to_string(), FileStatus::Modified),
                ("new.txt".to_string(), FileStatus::Untracked),
            ]
        );
        assert_eq!(
            statuses(&committed),
            [("committed.txt".to_string(), FileStatus::Modified)]
        );
        assert_eq!(statuses(&via_ref), statuses(&committed));
        assert!(missing.is_err(), "an unknown ref must surface as an error");
    }

    #[test]
    fn base_resolution_prefers_parent_and_avoids_comparing_a_branch_with_itself() {
        let stacked = BaseCandidates {
            parent: Some("pr2".to_string()),
            trunk: Some("main".to_string()),
            upstream: Some("origin/pr3".to_string()),
            branch: Some("pr3".to_string()),
        };
        assert_eq!(stacked.resolve(&Base::Parent).as_deref(), Some("pr2"));
        assert_eq!(stacked.resolve(&Base::Trunk).as_deref(), Some("main"));
        assert_eq!(
            stacked.resolve(&Base::Upstream).as_deref(),
            Some("origin/pr3")
        );
        assert_eq!(
            stacked.resolve(&Base::Ref("v1".to_string())).as_deref(),
            Some("v1")
        );

        let on_main = BaseCandidates {
            parent: None,
            trunk: Some("main".to_string()),
            upstream: Some("origin/main".to_string()),
            branch: Some("main".to_string()),
        };
        assert_eq!(
            on_main.resolve(&Base::Parent).as_deref(),
            Some("origin/main")
        );
        assert_eq!(
            on_main.resolve(&Base::Trunk).as_deref(),
            Some("origin/main")
        );
        assert_eq!(
            on_main.resolve(&Base::Ref("main".to_string())).as_deref(),
            Some("origin/main")
        );

        // On trunk with no upstream (a repo that has never been pushed), the branch view
        // falls back to the whole history rather than having nothing to show.
        let unpushed_main = BaseCandidates {
            trunk: Some("main".to_string()),
            branch: Some("main".to_string()),
            ..BaseCandidates::default()
        };
        assert_eq!(
            unpushed_main.resolve(&Base::Parent).as_deref(),
            Some(ROOT_BASE_NAME)
        );
        assert_eq!(unpushed_main.resolve(&Base::Trunk), None);
        assert_eq!(
            DiffMode::Branch {
                base: Base::Parent,
                commits_only: false
            }
            .label(&unpushed_main),
            "Everything"
        );
        // A brand-new repository has no trunk detected at all: same fallback.
        let fresh = BaseCandidates {
            branch: Some("main".to_string()),
            ..BaseCandidates::default()
        };
        assert_eq!(
            fresh.resolve(&Base::Parent).as_deref(),
            Some(ROOT_BASE_NAME)
        );
        assert_eq!(
            DiffMode::Branch {
                base: Base::Trunk,
                commits_only: true
            }
            .label(&stacked),
            "vs main, commits only"
        );
    }

    #[test]
    fn a_fresh_repository_shows_its_whole_history_in_branch_mode() {
        let root = temp_path("fresh-repo");
        std::fs::create_dir_all(&root).unwrap();
        let repo = git2::Repository::init(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        commit_all(&repo, "first");
        std::fs::write(root.join("b.txt"), "two\n").unwrap();
        commit_all(&repo, "second");
        std::fs::write(root.join("c.txt"), "three\n").unwrap();
        drop(repo);

        let branch = DiffMode::Branch {
            base: Base::Parent,
            commits_only: false,
        };
        let mut paths: Vec<String> = compute_diff(&root, &branch, None)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            ["a.txt", "b.txt", "c.txt"],
            "both commits plus the working tree"
        );

        let commits_only = DiffMode::Branch {
            base: Base::Root,
            commits_only: true,
        };
        let mut paths: Vec<String> = compute_diff(&root, &commits_only, None)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        paths.sort();
        assert_eq!(paths, ["a.txt", "b.txt"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn timeline_lists_commits_oldest_first_then_the_working_tree() {
        let root = temp_path("timeline");
        std::fs::create_dir_all(&root).unwrap();
        let repo = git2::Repository::init(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        let first = commit_all(&repo, "first");
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        let second = commit_all(&repo, "second");
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        drop(repo);

        let steps = super::timeline_steps(&root, Some(ROOT_BASE_NAME), 50, None).unwrap();
        let subjects: Vec<&str> = steps.iter().map(|s| s.subject.as_str()).collect();
        assert_eq!(subjects, ["first", "second", "working tree"]);
        assert_eq!(steps[0].parent, None, "the first commit has no parent");
        assert_eq!(steps[1].parent.as_deref(), Some(first.to_string().as_str()));
        assert_eq!(steps[2].id, None);
        assert_eq!(
            steps[2].parent.as_deref(),
            Some(second.to_string().as_str())
        );

        // A step is what that commit changed; since is everything up to it.
        let step = DiffMode::Range {
            from: steps[1].parent.clone(),
            to: super::RangeEnd::Commit(second.to_string()),
            kind: super::RangeKind::Step,
        };
        let files = compute_diff(&root, &step, None).unwrap();
        assert_eq!(files[0].additions, 1);
        let since = DiffMode::Range {
            from: None,
            to: super::RangeEnd::Workdir,
            kind: super::RangeKind::Since,
        };
        let files = compute_diff(&root, &since, None).unwrap();
        assert_eq!(files[0].additions, 3);

        // Limited history stops early and reports the parent it stopped at.
        let recent = super::timeline_steps(&root, None, 1, None).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].subject, "second");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn snapshots_become_ticks_between_the_commits_they_were_taken_on() {
        use super::StepKind;
        let root = temp_path("timeline-snapshots");
        std::fs::create_dir_all(&root).unwrap();
        let repo = git2::Repository::init(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        commit_all(&repo, "first");
        // Two recorded edits on top of the first commit, then a commit of the second.
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        crate::snapshots::record(&root, "main").unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        crate::snapshots::record(&root, "main").unwrap().unwrap();
        commit_all(&repo, "second");
        // A further uncommitted edit, recorded too.
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        crate::snapshots::record(&root, "main").unwrap().unwrap();
        drop(repo);

        let steps = super::timeline_steps(&root, Some(ROOT_BASE_NAME), 50, Some("main")).unwrap();
        let kinds: Vec<StepKind> = steps.iter().map(|s| s.kind).collect();
        // first, edit (one snapshot equals the second commit's tree and is dropped),
        // second, (snapshot equal to the working tree is dropped), working tree
        assert_eq!(
            kinds,
            [
                StepKind::Commit,
                StepKind::Snapshot,
                StepKind::Commit,
                StepKind::Workdir
            ]
        );
        // Each step's parent is the step before it, so step diffs are one edit each:
        // the second commit diffs from the recorded edit, not from the first commit.
        assert_eq!(steps[1].parent, steps[0].id);
        assert_eq!(steps[2].parent, steps[1].id);
        assert_eq!(steps[3].parent, steps[2].id);
        let edit = DiffMode::Range {
            from: steps[1].parent.clone(),
            to: super::RangeEnd::Commit(steps[1].id.clone().unwrap()),
            kind: super::RangeKind::Step,
        };
        let files = compute_diff(&root, &edit, None).unwrap();
        assert_eq!(files[0].additions, 1);

        // Without a branch name there are no ticks: plain commit behaviour.
        let plain = super::timeline_steps(&root, Some(ROOT_BASE_NAME), 50, None).unwrap();
        assert_eq!(plain.len(), 3);

        // Base at HEAD, as on a pushed trunk: no commits, but the edits recorded on top
        // of HEAD are still steps. One more edit so a snapshot differs from the tree.
        std::fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        let at_head = super::timeline_steps(&root, Some("HEAD"), 50, Some("main")).unwrap();
        let kinds: Vec<StepKind> = at_head.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [StepKind::Snapshot, StepKind::Workdir]);
        assert_eq!(at_head[1].parent, at_head[0].id);

        // Base one commit back: the edits recorded on the base commit lead up to the
        // commit after it, and that commit diffs from the last of them.
        let from_first = super::timeline_steps(&root, Some("-1"), 50, Some("main")).unwrap();
        let kinds: Vec<StepKind> = from_first.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                StepKind::Snapshot,
                StepKind::Commit,
                StepKind::Snapshot,
                StepKind::Workdir
            ]
        );
        assert_eq!(from_first[1].parent, from_first[0].id);
        let first_edit = DiffMode::Range {
            from: from_first[0].parent.clone(),
            to: super::RangeEnd::Commit(from_first[0].id.clone().unwrap()),
            kind: super::RangeKind::Step,
        };
        let files = compute_diff(&root, &first_edit, None).unwrap();
        assert_eq!(files[0].additions, 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn describe(files: &[FileDiff]) -> Vec<String> {
        files
            .iter()
            .map(|file| {
                let hunks: Vec<String> = file
                    .hunks
                    .iter()
                    .map(|hunk| {
                        let lines: Vec<String> = hunk
                            .lines
                            .iter()
                            .map(|line| {
                                format!(
                                    "{:?}|{:?}|{:?}|{}",
                                    line.kind, line.old_lineno, line.new_lineno, line.content
                                )
                            })
                            .collect();
                        format!("{}\n{}", hunk.header, lines.join("\n"))
                    })
                    .collect();
                format!(
                    "{} {:?} {:?} +{} -{}\n{}",
                    file.path,
                    file.old_path,
                    file.status,
                    file.additions,
                    file.deletions,
                    hunks.join("\n--\n")
                )
            })
            .collect()
    }

    #[test]
    fn parallel_patching_matches_the_serial_walk() {
        let repo_path = temp_path("parallel-vs-serial");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        for f in 0..20 {
            let body: String = (0..50).map(|l| format!("file {f} line {l}\n")).collect();
            std::fs::write(repo_path.join(format!("f{f}.txt")), body).unwrap();
        }
        std::fs::write(repo_path.join("binary.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
        std::fs::write(repo_path.join("noeol.txt"), "alpha\nbeta").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        drop(tree);

        // Modify most files, delete one, dirty the binary, and add a trailing newline.
        for f in 0..18 {
            let path = repo_path.join(format!("f{f}.txt"));
            let text = std::fs::read_to_string(&path).unwrap();
            let edited = text.replace(&format!("line {}\n", f + 3), "EDITED\n");
            std::fs::write(&path, edited).unwrap();
        }
        std::fs::remove_file(repo_path.join("f19.txt")).unwrap();
        std::fs::write(
            repo_path.join("binary.bin"),
            [0u8, 159, 146, 150, 9, 9, 9, 9],
        )
        .unwrap();
        std::fs::write(repo_path.join("noeol.txt"), "alpha\nbeta\n").unwrap();
        std::fs::write(repo_path.join("untracked.txt"), "new\n").unwrap();

        let spec = DiffSpec::IndexToWorkdir;
        let diff = build_diff(&repo, spec).unwrap();
        let expected: Vec<(usize, String)> = diff
            .deltas()
            .enumerate()
            .filter(|(_, delta)| delta.status() != Delta::Untracked)
            .map(|(idx, delta)| (idx, delta_path(&delta)))
            .collect();
        assert!(expected.len() >= PARALLEL_MIN_DELTAS);

        let reference = collect_files_via_print(&diff).unwrap();
        let serial = collect_files_serial(&diff, &expected).unwrap();
        let parallel = collect_files_parallel(&repo_path, spec, &expected, 4).unwrap();
        drop(diff);
        drop(index);
        drop(repo);
        std::fs::remove_dir_all(&repo_path).unwrap();

        assert_eq!(describe(&serial), describe(&reference));
        assert_eq!(describe(&parallel), describe(&reference));
        assert!(serial.iter().any(|f| f.status == FileStatus::Deleted));
        assert!(
            serial
                .iter()
                .any(|f| f.path == "binary.bin" && f.hunks.is_empty())
        );
        let noeol = serial.iter().find(|f| f.path == "noeol.txt").unwrap();
        // "-beta", "+beta", and the "\ No newline at end of file" marker libgit2 reports
        // with the added-side origin.
        assert_eq!((noeol.deletions, noeol.additions), (1, 2));
    }

    #[test]
    fn compute_diff_preserves_trailing_horizontal_whitespace() {
        let repo_path = temp_path("trailing-whitespace");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        let file_path = repo_path.join("example.txt");
        std::fs::write(&file_path, "first\nsecond\n").unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("example.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        drop(index);
        drop(repo);

        std::fs::write(&file_path, "first  \nsecond\t\t\n").unwrap();
        let files = compute_diff(&repo_path, &DiffMode::Unstaged, None).unwrap();
        let added_lines: Vec<&str> = files[0]
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == LineKind::Addition)
            .map(|line| line.content.as_str())
            .collect();
        std::fs::remove_dir_all(repo_path).unwrap();

        assert_eq!(added_lines, ["first  ", "second\t\t"]);
    }

    #[test]
    fn relative_refs_mean_the_last_n_commits_capped_at_the_first() {
        assert_eq!(last_n_commits("-2"), Some(2));
        assert_eq!(last_n_commits("~3"), Some(3));
        assert_eq!(last_n_commits("HEAD~1"), Some(1));
        assert_eq!(last_n_commits("1"), Some(1));
        assert_eq!(last_n_commits("-0"), None);
        assert_eq!(last_n_commits("main"), None);
        assert_eq!(last_n_commits("-1a"), None);

        let repo_path = temp_path("relative-refs");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        std::fs::write(repo_path.join("a.txt"), "one\n").unwrap();
        commit_all(&repo, "first");
        std::fs::write(repo_path.join("b.txt"), "two\n").unwrap();
        commit_all(&repo, "second");
        std::fs::write(repo_path.join("c.txt"), "three\n").unwrap();
        commit_all(&repo, "third");
        drop(repo);

        let paths = |base: &str| -> Vec<String> {
            let mode = DiffMode::Branch {
                base: Base::Ref(base.to_string()),
                commits_only: true,
            };
            let mut paths: Vec<String> = compute_diff(&repo_path, &mode, None)
                .unwrap()
                .into_iter()
                .map(|file| file.path)
                .collect();
            paths.sort();
            paths
        };
        let last_one = paths("-1");
        let last_two = paths("-2");
        let past_the_start = paths("-10");
        std::fs::remove_dir_all(&repo_path).unwrap();

        assert_eq!(last_one, ["c.txt"]);
        assert_eq!(last_two, ["b.txt", "c.txt"]);
        assert_eq!(past_the_start, ["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn range_to_workdir_omits_files_that_match_the_from_tree() {
        let repo_path = temp_path("range-phantoms");
        std::fs::create_dir(&repo_path).unwrap();
        let repo = git2::Repository::init(&repo_path).unwrap();
        std::fs::write(repo_path.join("a.txt"), "a\n").unwrap();
        std::fs::write(repo_path.join("b.txt"), "b\n").unwrap();
        commit_all(&repo, "first");
        drop(repo);

        // Both files change, a snapshot records that state, then only b moves on. The
        // index still holds the committed versions, so a diff that consults it lists
        // a.txt too; the file matches the snapshot and must not show up.
        std::fs::write(repo_path.join("a.txt"), "a edited\n").unwrap();
        std::fs::write(repo_path.join("b.txt"), "b edited\n").unwrap();
        let snapshot = crate::snapshots::record(&repo_path, "master")
            .unwrap()
            .expect("snapshot recorded");
        std::fs::write(repo_path.join("b.txt"), "b edited twice\n").unwrap();

        let mode = DiffMode::Range {
            from: Some(snapshot.to_string()),
            to: RangeEnd::Workdir,
            kind: RangeKind::Step,
        };
        let files = compute_diff(&repo_path, &mode, None).unwrap();
        std::fs::remove_dir_all(&repo_path).unwrap();

        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["b.txt"]);
        assert_eq!(files[0].additions, 1);
    }

    #[test]
    fn streaming_line_count_matches_string_lines() {
        let path = temp_path("line-count");
        for content in ["", "one", "one\n", "one\ntwo", "one\ntwo\n"] {
            std::fs::write(&path, content).unwrap();
            assert_eq!(count_lines(&path).unwrap(), content.lines().count());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn untracked_reader_rejects_excessive_line_counts() {
        let path = temp_path("line-limit");
        std::fs::write(&path, "\n".repeat(MAX_UNTRACKED_LINES + 1)).unwrap();
        assert!(read_untracked_lines(&path).is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn untracked_reader_rejects_excessive_file_sizes() {
        let path = temp_path("size-limit");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_UNTRACKED_FILE_BYTES + 1)
            .unwrap();
        assert!(read_untracked_lines(&path).is_none());
        std::fs::remove_file(path).unwrap();
    }
}
