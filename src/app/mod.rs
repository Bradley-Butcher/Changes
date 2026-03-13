pub mod keys;
pub mod mouse;

use crate::diff::{DiffLine, FileDiff, LineKind};
use crate::git::{self, DiffMode, RepoInfo};
use crate::ui::LayoutHints;
use crate::viewport::{DiffLayout, ViewKind, ViewportState};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// Tunable constants
const FLASH_DURATION: Duration = Duration::from_millis(300);
pub(crate) const SCROLL_SPEED: usize = 3;
pub(crate) const PAGE_SCROLL: usize = 20;
pub(crate) const DOUBLE_CLICK_MS: u64 = 400;

pub struct RepoState {
    pub id: u64,
    pub info: RepoInfo,
    pub mode: DiffMode,
    pub files: Vec<FileDiff>,
    pub base_branch: Option<String>,
    pub branch_name: Option<String>,
    pub unified_layout: Option<DiffLayout>,
    pub sbs_layout: Option<DiffLayout>,
    pub unified_viewport: ViewportState,
    pub sbs_viewport: ViewportState,
}

pub struct FlashState {
    pub until: Instant,
    pub file_idx: usize,
    pub hunk_idx: usize,
}

pub struct FilePickerState {
    pub query: String,
    pub selected: usize,
}

pub struct RepoAdderState {
    pub query: String,
    pub error: Option<String>,
    pub results: Vec<(String, PathBuf)>,
    pub cursor: usize,
    pub checked: std::collections::HashSet<usize>,
}

pub struct App {
    pub repos: Vec<RepoState>,
    next_repo_id: u64,
    pub active_tab: usize,
    pub focused_file: Option<usize>,
    pub side_by_side: bool,
    pub last_error: Option<String>,
    pub flash: Option<FlashState>,
    pub show_help: bool,
    pub file_picker: Option<FilePickerState>,
    pub repo_adder: Option<RepoAdderState>,
    pub layout: LayoutHints,
    pub last_click: Option<(u16, u16, Instant)>,
}

impl App {
    pub fn new(repo_infos: Vec<RepoInfo>) -> Self {
        let repos: Vec<RepoState> = repo_infos
            .into_iter()
            .enumerate()
            .map(|(i, info)| RepoState {
                id: i as u64,
                info,
                mode: DiffMode::Unstaged,
                files: Vec::new(),
                base_branch: None,
                branch_name: None,
                unified_layout: None,
                sbs_layout: None,
                unified_viewport: ViewportState::default(),
                sbs_viewport: ViewportState::default(),
            })
            .collect();
        let next_repo_id = repos.len() as u64;
        Self {
            repos,
            next_repo_id,
            active_tab: 0,
            focused_file: None,
            side_by_side: false,
            last_error: None,
            flash: None,
            show_help: false,
            file_picker: None,
            repo_adder: None,
            layout: LayoutHints::default(),
            last_click: None,
        }
    }

    pub fn current_mode(&self) -> DiffMode {
        self.repos[self.active_tab].mode
    }

    pub fn set_mode(&mut self, mode: DiffMode, diff_tx: &mpsc::UnboundedSender<DiffResult>) {
        self.repos[self.active_tab].mode = mode;
        self.refresh_repo_async(self.active_tab, diff_tx);
        self.jump_active_viewport_top();
    }

    pub fn toggle_collapsed(&mut self, file_idx: usize) {
        if let Some(files) = self.current_files_mut()
            && let Some(file) = files.get_mut(file_idx)
        {
            file.collapsed = !file.collapsed;
        }
        self.invalidate_layouts(self.active_tab);
        self.prepare_active_layout();
        self.clamp_active_viewport();
    }

    pub fn set_all_collapsed(&mut self, collapsed: bool) {
        if let Some(files) = self.current_files_mut() {
            for file in files.iter_mut() {
                file.collapsed = collapsed;
            }
        }
        self.invalidate_layouts(self.active_tab);
        self.prepare_active_layout();
        self.clamp_active_viewport();
    }

    pub fn active_view_kind(&self) -> ViewKind {
        if self.side_by_side {
            ViewKind::SideBySide
        } else {
            ViewKind::Unified
        }
    }

    fn viewport_height(&self) -> usize {
        self.layout.content_height.max(1) as usize
    }

    pub fn current_files(&self) -> Option<&Vec<FileDiff>> {
        self.repos.get(self.active_tab).map(|r| &r.files)
    }

    pub fn current_files_mut(&mut self) -> Option<&mut Vec<FileDiff>> {
        self.repos.get_mut(self.active_tab).map(|r| &mut r.files)
    }

    pub fn find_repo(&self, id: u64) -> Option<usize> {
        self.repos.iter().position(|r| r.id == id)
    }

    pub fn current_scroll_offset(&self) -> usize {
        self.repos
            .get(self.active_tab)
            .map(|repo| match self.active_view_kind() {
                ViewKind::Unified => repo.unified_viewport.scroll_offset(),
                ViewKind::SideBySide => repo.sbs_viewport.scroll_offset(),
            })
            .unwrap_or(0)
    }

    pub fn prepare_active_layout(&mut self) {
        let idx = self.active_tab;
        let view = self.active_view_kind();
        self.ensure_layout(idx, view);
    }

    pub fn current_layout(&self) -> Option<&DiffLayout> {
        self.repos
            .get(self.active_tab)
            .and_then(|repo| match self.active_view_kind() {
                ViewKind::Unified => repo.unified_layout.as_ref(),
                ViewKind::SideBySide => repo.sbs_layout.as_ref(),
            })
    }

    fn current_viewport(&self) -> Option<&ViewportState> {
        self.repos
            .get(self.active_tab)
            .map(|repo| match self.active_view_kind() {
                ViewKind::Unified => &repo.unified_viewport,
                ViewKind::SideBySide => &repo.sbs_viewport,
            })
    }

    pub fn visible_row_range(&self, total_lines: usize, viewport_height: usize) -> Range<usize> {
        self.current_viewport()
            .map(|viewport| viewport.visible_range(total_lines, viewport_height))
            .unwrap_or(0..0)
    }

    pub fn warm_row_range(&self, total_lines: usize, viewport_height: usize) -> Range<usize> {
        self.current_viewport()
            .map(|viewport| viewport.warm_range(total_lines, viewport_height))
            .unwrap_or(0..0)
    }

    fn ensure_layout(&mut self, idx: usize, view: ViewKind) {
        let height = self.viewport_height();
        let repo = match self.repos.get_mut(idx) {
            Some(repo) => repo,
            None => return,
        };
        let layout_slot = match view {
            ViewKind::Unified => &mut repo.unified_layout,
            ViewKind::SideBySide => &mut repo.sbs_layout,
        };
        if layout_slot.is_some() {
            return;
        }
        if view == ViewKind::SideBySide {
            for file in &mut repo.files {
                file.ensure_sbs_cache();
            }
        }
        *layout_slot = Some(DiffLayout::build(&repo.files, view));
        let total = layout_slot
            .as_ref()
            .map(DiffLayout::total_lines)
            .unwrap_or(0);
        match view {
            ViewKind::Unified => repo.unified_viewport.clamp_scroll(total, height),
            ViewKind::SideBySide => repo.sbs_viewport.clamp_scroll(total, height),
        }
    }

    fn invalidate_layouts(&mut self, idx: usize) {
        if let Some(repo) = self.repos.get_mut(idx) {
            repo.unified_layout = None;
            repo.sbs_layout = None;
        }
    }

    pub fn clamp_active_viewport(&mut self) {
        let idx = self.active_tab;
        let view = self.active_view_kind();
        self.ensure_layout(idx, view);
        let total = self
            .repos
            .get(idx)
            .and_then(|repo| match view {
                ViewKind::Unified => repo.unified_layout.as_ref(),
                ViewKind::SideBySide => repo.sbs_layout.as_ref(),
            })
            .map(DiffLayout::total_lines)
            .unwrap_or(0);
        let height = self.viewport_height();
        if let Some(repo) = self.repos.get_mut(idx) {
            match view {
                ViewKind::Unified => repo.unified_viewport.clamp_scroll(total, height),
                ViewKind::SideBySide => repo.sbs_viewport.clamp_scroll(total, height),
            }
        }
    }

    pub fn scroll_active_viewport(&mut self, delta: isize) {
        let idx = self.active_tab;
        let view = self.active_view_kind();
        self.ensure_layout(idx, view);
        let total = self
            .repos
            .get(idx)
            .and_then(|repo| match view {
                ViewKind::Unified => repo.unified_layout.as_ref(),
                ViewKind::SideBySide => repo.sbs_layout.as_ref(),
            })
            .map(DiffLayout::total_lines)
            .unwrap_or(0);
        let height = self.viewport_height();
        if let Some(repo) = self.repos.get_mut(idx) {
            match view {
                ViewKind::Unified => repo.unified_viewport.scroll_by(delta, total, height),
                ViewKind::SideBySide => repo.sbs_viewport.scroll_by(delta, total, height),
            }
        }
        self.focused_file = self.focused_file_from_scroll();
    }

    pub fn jump_active_viewport_to(&mut self, row: usize) {
        let idx = self.active_tab;
        let view = self.active_view_kind();
        self.ensure_layout(idx, view);
        let total = self
            .repos
            .get(idx)
            .and_then(|repo| match view {
                ViewKind::Unified => repo.unified_layout.as_ref(),
                ViewKind::SideBySide => repo.sbs_layout.as_ref(),
            })
            .map(DiffLayout::total_lines)
            .unwrap_or(0);
        let height = self.viewport_height();
        if let Some(repo) = self.repos.get_mut(idx) {
            match view {
                ViewKind::Unified => repo.unified_viewport.jump_to(row, total, height),
                ViewKind::SideBySide => repo.sbs_viewport.jump_to(row, total, height),
            }
        }
        self.focused_file = self.focused_file_from_scroll();
    }

    pub fn jump_active_viewport_top(&mut self) {
        let view = self.active_view_kind();
        if let Some(repo) = self.repos.get_mut(self.active_tab) {
            match view {
                ViewKind::Unified => repo.unified_viewport.jump_to_top(),
                ViewKind::SideBySide => repo.sbs_viewport.jump_to_top(),
            }
        }
        self.focused_file = self.focused_file_from_scroll();
    }

    pub fn jump_active_viewport_bottom(&mut self) {
        let idx = self.active_tab;
        let view = self.active_view_kind();
        self.ensure_layout(idx, view);
        let total = self
            .repos
            .get(idx)
            .and_then(|repo| match view {
                ViewKind::Unified => repo.unified_layout.as_ref(),
                ViewKind::SideBySide => repo.sbs_layout.as_ref(),
            })
            .map(DiffLayout::total_lines)
            .unwrap_or(0);
        let height = self.viewport_height();
        if let Some(repo) = self.repos.get_mut(idx) {
            match view {
                ViewKind::Unified => repo.unified_viewport.jump_to_bottom(total, height),
                ViewKind::SideBySide => repo.sbs_viewport.jump_to_bottom(total, height),
            }
        }
        self.focused_file = self.focused_file_from_scroll();
    }


    /// Returns indices of files matching the picker query (case-insensitive fuzzy substring).
    pub fn filtered_file_indices(&self) -> Vec<usize> {
        let files = match self.current_files() {
            Some(f) => f,
            None => return Vec::new(),
        };
        let query = self
            .file_picker
            .as_ref()
            .map(|fp| fp.query.to_lowercase())
            .unwrap_or_default();
        if query.is_empty() {
            return (0..files.len()).collect();
        }
        files
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                let path = f.path.to_lowercase();
                let mut chars = query.chars();
                let mut current = chars.next();
                for c in path.chars() {
                    if let Some(q) = current {
                        if c == q {
                            current = chars.next();
                        }
                    } else {
                        break;
                    }
                }
                current.is_none()
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn jump_to_file(&mut self, file_idx: usize) {
        self.prepare_active_layout();
        if let Some(pos) = self
            .current_layout()
            .and_then(|layout| layout.file_header_row(file_idx))
        {
            self.jump_active_viewport_to(pos);
            self.focused_file = Some(file_idx);
        }
    }

    pub fn expand_gap(&mut self, file_idx: usize, gap_idx: usize) {
        let repo_path = self.repos[self.active_tab].info.path.clone();
        let file = match self
            .repos
            .get_mut(self.active_tab)
            .and_then(|r| r.files.get_mut(file_idx))
        {
            Some(f) => f,
            None => return,
        };
        if file.hunks.is_empty() {
            return;
        }

        const EXPAND_AMOUNT: usize = 20;

        // Compute the line range we need (1-based) without reading the file yet
        let (gap_start, gap_end, old_offset) = if gap_idx == 0 {
            let first_new = file.hunks[0].first_new_lineno().unwrap_or(1) as usize;
            let first_old = file.hunks[0].first_old_lineno().unwrap_or(1) as usize;
            if first_new <= 1 {
                return;
            }
            let end = first_new - 1;
            let start = end.saturating_sub(EXPAND_AMOUNT - 1).max(1);
            (start, end, first_old as i64 - first_new as i64)
        } else if gap_idx < file.hunks.len() {
            let prev_idx = gap_idx - 1;
            let prev_last_new = file.hunks[prev_idx].last_new_lineno().unwrap_or(0) as usize;
            let prev_last_old = file.hunks[prev_idx].last_old_lineno().unwrap_or(0) as usize;
            let next_first_new = file.hunks[gap_idx].first_new_lineno().unwrap_or(0) as usize;
            if prev_last_new >= next_first_new.saturating_sub(1) {
                return;
            }
            let start = prev_last_new + 1;
            let end = (next_first_new - 1).min(start + EXPAND_AMOUNT - 1);
            (start, end, prev_last_old as i64 - prev_last_new as i64)
        } else {
            let last_idx = file.hunks.len() - 1;
            let last_new = file.hunks[last_idx].last_new_lineno().unwrap_or(0) as usize;
            let last_old = file.hunks[last_idx].last_old_lineno().unwrap_or(0) as usize;
            if last_new >= file.total_new_lines {
                return;
            }
            let start = last_new + 1;
            let end = file.total_new_lines.min(start + EXPAND_AMOUNT - 1);
            (start, end, last_old as i64 - last_new as i64)
        };

        // Read only the lines we need via BufRead (skip + take)
        let file_path = repo_path.join(&file.path);
        let f = match std::fs::File::open(&file_path) {
            Ok(f) => f,
            Err(_) => return,
        };
        use std::io::BufRead;
        let context_lines: Vec<DiffLine> = std::io::BufReader::new(f)
            .lines()
            .skip(gap_start - 1)
            .take(gap_end - gap_start + 1)
            .enumerate()
            .map(|(offset, line)| {
                let lineno = gap_start + offset;
                DiffLine {
                    kind: LineKind::Context,
                    content: line.unwrap_or_default(),
                    old_lineno: Some((lineno as i64 + old_offset) as u32),
                    new_lineno: Some(lineno as u32),
                }
            })
            .collect();

        // Apply to the appropriate hunk
        if gap_idx == 0 {
            let mut existing = std::mem::take(&mut file.hunks[0].lines);
            let mut new_lines = context_lines;
            new_lines.append(&mut existing);
            file.hunks[0].lines = new_lines;
        } else if gap_idx < file.hunks.len() {
            file.hunks[gap_idx - 1].lines.extend(context_lines);
        } else {
            let last_idx = file.hunks.len() - 1;
            file.hunks[last_idx].lines.extend(context_lines);
        }

        // Invalidate SBS cache — will be recomputed on demand before next draw
        file.sbs_cache = None;
        self.invalidate_layouts(self.active_tab);
        self.prepare_active_layout();
        self.clamp_active_viewport();
    }

    pub fn refresh_repo_adder_results(&mut self) {
        let adder = match self.repo_adder.as_mut() {
            Some(a) => a,
            None => return,
        };
        let input = &adder.query;
        let (dir_part, filter) = if input.is_empty() {
            (".", "")
        } else if input.ends_with('/') {
            (input.as_str(), "")
        } else if let Some(pos) = input.rfind('/') {
            (&input[..=pos], &input[pos + 1..])
        } else {
            (".", input.as_str())
        };

        let base = std::env::current_dir().unwrap_or_default();
        let resolved = if Path::new(dir_part).is_absolute() {
            PathBuf::from(dir_part)
        } else {
            base.join(dir_part)
        };

        let existing: std::collections::HashSet<PathBuf> =
            self.repos.iter().map(|r| r.info.path.clone()).collect();

        let filter_lower = filter.to_lowercase();
        let mut results: Vec<(String, PathBuf)> = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&resolved) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() || !path.join(".git").exists() {
                    continue;
                }
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if !filter_lower.is_empty() && !name.to_lowercase().contains(&filter_lower) {
                    continue;
                }
                if let Ok(canonical) = path.canonicalize() {
                    if existing.contains(&canonical) {
                        continue;
                    }
                    results.push((name, canonical));
                }
            }
        }

        results.sort_by(|a, b| a.0.cmp(&b.0));
        adder.results = results;
        adder.cursor = 0;
        adder.checked.clear();
        adder.error = None;
    }

    pub fn add_repo(&mut self, input: &str) -> Result<usize, String> {
        let path = Path::new(input);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        let canonical = resolved
            .canonicalize()
            .map_err(|_| format!("Path not found: {}", input))?;

        if !canonical.join(".git").exists() {
            return Err(format!("Not a git repo: {}", canonical.display()));
        }

        if self.repos.iter().any(|r| r.info.path == canonical) {
            return Err("Repo already added".to_string());
        }

        let name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string());

        let id = self.next_repo_id;
        self.next_repo_id += 1;
        let idx = self.repos.len();
        self.repos.push(RepoState {
            id,
            info: git::RepoInfo {
                name,
                path: canonical,
            },
            mode: DiffMode::Unstaged,
            files: Vec::new(),
            base_branch: None,
            branch_name: None,
            unified_layout: None,
            sbs_layout: None,
            unified_viewport: ViewportState::default(),
            sbs_viewport: ViewportState::default(),
        });
        self.active_tab = idx;
        self.focused_file = None;

        Ok(idx)
    }

    /// Ensure side-by-side caches exist for the active tab's files.
    /// Called before each draw; no-op when caches already exist or unified mode is active.
    pub fn ensure_sbs_caches(&mut self) {
        if !self.side_by_side {
            return;
        }
        if let Some(files) = self.current_files_mut() {
            for file in files {
                file.ensure_sbs_cache();
            }
        }
        self.ensure_layout(self.active_tab, ViewKind::SideBySide);
    }

    pub fn apply_diff_result(&mut self, idx: usize, result: anyhow::Result<Vec<FileDiff>>) {
        match result {
            Ok(files) => {
                let old_collapsed: std::collections::HashMap<String, bool> = self.repos[idx]
                    .files
                    .iter()
                    .map(|f| (f.path.clone(), f.collapsed))
                    .collect();

                let mut new_files = files;
                for file in &mut new_files {
                    if let Some(&collapsed) = old_collapsed.get(&file.path) {
                        file.collapsed = collapsed;
                    }
                    if self.side_by_side {
                        file.ensure_sbs_cache();
                    }
                }

                self.repos[idx].files = new_files;
                self.invalidate_layouts(idx);
                self.ensure_layout(idx, ViewKind::Unified);
                if self.side_by_side && idx == self.active_tab {
                    self.ensure_layout(idx, ViewKind::SideBySide);
                }
                let height = self.viewport_height();
                if let Some(repo) = self.repos.get_mut(idx) {
                    let unified_total = repo
                        .unified_layout
                        .as_ref()
                        .map(DiffLayout::total_lines)
                        .unwrap_or(0);
                    repo.unified_viewport.clamp_scroll(unified_total, height);
                    let sbs_total = repo
                        .sbs_layout
                        .as_ref()
                        .map(DiffLayout::total_lines)
                        .unwrap_or(0);
                    repo.sbs_viewport.clamp_scroll(sbs_total, height);
                }
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("{}: {}", self.repos[idx].info.name, e));
            }
        }
    }

    pub fn refresh_repo_async(&self, idx: usize, diff_tx: &mpsc::UnboundedSender<DiffResult>) {
        let repo = &self.repos[idx];
        let id = repo.id;
        let path = repo.info.path.clone();
        let mode = repo.mode;
        let base = repo.base_branch.clone();
        let tx = diff_tx.clone();
        std::thread::spawn(move || {
            let result = git::compute_diff(&path, mode, base.as_deref());
            let _ = tx.send(DiffResult {
                repo_id: id,
                mode,
                result,
            });
        });
    }

    pub fn refresh_repo_sync(&mut self, idx: usize) {
        let repo = &self.repos[idx];
        let mode = repo.mode;
        let base = repo.base_branch.clone();
        let path = repo.info.path.clone();
        let result = git::compute_diff(&path, mode, base.as_deref());
        self.apply_diff_result(idx, result);
    }

    pub fn refresh_all_sync(&mut self) {
        for i in 0..self.repos.len() {
            self.refresh_repo_sync(i);
        }
    }

    pub fn total_display_lines(&self) -> usize {
        self.current_layout()
            .map(DiffLayout::total_lines)
            .unwrap_or(0)
    }

    pub fn file_header_positions(&self) -> Vec<usize> {
        let mut positions = Vec::new();
        if let Some(layout) = self.current_layout() {
            for file_idx in 0..self.current_files().map(|files| files.len()).unwrap_or(0) {
                if let Some(row) = layout.file_header_row(file_idx) {
                    positions.push(row);
                }
            }
        }
        positions
    }

    pub fn focused_file_from_scroll(&self) -> Option<usize> {
        self.current_layout()
            .and_then(|layout| layout.focused_file_at_scroll(self.current_scroll_offset()))
    }

    pub fn is_hunk_flashing(&self, file_idx: usize, hunk_idx: usize) -> bool {
        if let Some(ref flash) = self.flash {
            Instant::now() < flash.until
                && flash.file_idx == file_idx
                && flash.hunk_idx == hunk_idx
        } else {
            false
        }
    }

    pub fn file_and_hunk_at_row(&self, content_row: usize) -> Option<(usize, usize)> {
        self.current_layout()
            .and_then(|layout| layout.hunk_at_row(content_row))
    }

    pub fn copy_hunk_at_row(&mut self, content_row: usize) -> Option<String> {
        let (file_idx, target_hunk) = self.file_and_hunk_at_row(content_row)?;
        self.copy_hunk(file_idx, target_hunk)
    }

    pub fn copy_hunk_at_focus(&mut self) -> Option<String> {
        let file_idx = self.focused_file?;
        let files = self.current_files()?;
        let file = files.get(file_idx)?;

        if file.hunks.is_empty() {
            return None;
        }

        let target_hunk = self
            .current_layout()
            .and_then(|layout| layout.hunk_at_or_after_row(self.current_scroll_offset(), file_idx))
            .map(|(_, hunk_idx)| hunk_idx)
            .unwrap_or(0);

        self.copy_hunk(file_idx, target_hunk)
    }

    fn copy_hunk(&mut self, file_idx: usize, target_hunk: usize) -> Option<String> {
        let files = self.current_files()?;
        let file = files.get(file_idx)?;
        let hunk = file.hunks.get(target_hunk)?;

        if hunk.lines.is_empty() {
            return None;
        }

        let first_lineno = hunk
            .lines
            .first()
            .and_then(|l| l.new_lineno.or(l.old_lineno))
            .unwrap_or(0);
        let last_lineno = hunk
            .lines
            .last()
            .and_then(|l| l.new_lineno.or(l.old_lineno))
            .unwrap_or(0);

        let mut result = format!("// {}:{}-{}\n", file.path, first_lineno, last_lineno);
        for line in &hunk.lines {
            let prefix = match line.kind {
                LineKind::Addition => "+",
                LineKind::Deletion => "-",
                LineKind::Context => " ",
            };
            result.push_str(&format!("{} {}\n", prefix, line.content));
        }

        self.flash = Some(FlashState {
            until: Instant::now() + FLASH_DURATION,
            file_idx,
            hunk_idx: target_hunk,
        });

        Some(result)
    }
}

pub struct DiffResult {
    pub repo_id: u64,
    pub mode: DiffMode,
    pub result: anyhow::Result<Vec<FileDiff>>,
}

pub struct BaseBranchResult {
    pub repo_id: u64,
    pub branch: Option<String>,
    pub branch_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::App;
    use crate::diff::{FileDiff, FileStatus};
    use crate::git::RepoInfo;
    use std::path::PathBuf;

    fn test_app_with_files(paths: &[&str]) -> App {
        let mut app = App::new(vec![RepoInfo {
            name: "repo".to_string(),
            path: PathBuf::from("/repo"),
        }]);
        app.repos[0].files = paths
            .iter()
            .map(|path| FileDiff {
                path: (*path).to_string(),
                old_path: None,
                status: FileStatus::Modified,
                hunks: Vec::new(),
                additions: 0,
                deletions: 0,
                collapsed: false,
                total_new_lines: 0,
                sbs_cache: None,
            })
            .collect();
        app
    }

    fn set_picker_query(app: &mut App, query: &str) {
        app.file_picker = Some(super::FilePickerState {
            query: query.to_string(),
            selected: 0,
        });
    }

    #[test]
    fn fuzzy_match_exact_match() {
        let mut app = test_app_with_files(&["src/main.rs"]);
        set_picker_query(&mut app, "src/main.rs");
        assert_eq!(app.filtered_file_indices(), vec![0]);
    }

    #[test]
    fn fuzzy_match_subsequence() {
        let mut app = test_app_with_files(&["src/main.rs"]);
        set_picker_query(&mut app, "smr");
        assert_eq!(app.filtered_file_indices(), vec![0]);
    }

    #[test]
    fn fuzzy_match_no_match() {
        let mut app = test_app_with_files(&["src/main.rs"]);
        set_picker_query(&mut app, "xyz");
        assert!(app.filtered_file_indices().is_empty());
    }

    #[test]
    fn fuzzy_match_empty_query_matches_all() {
        let app = test_app_with_files(&["anything.rs", "src/main.rs"]);
        assert_eq!(app.filtered_file_indices(), vec![0, 1]);
    }

    #[test]
    fn fuzzy_match_case_insensitive() {
        let mut app = test_app_with_files(&["src/Main.RS"]);
        set_picker_query(&mut app, "main");
        assert_eq!(app.filtered_file_indices(), vec![0]);
    }

    #[test]
    fn fuzzy_match_query_longer_than_path() {
        let mut app = test_app_with_files(&["ab"]);
        set_picker_query(&mut app, "abc");
        assert!(app.filtered_file_indices().is_empty());
    }
}
