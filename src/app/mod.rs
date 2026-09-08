pub mod keys;
pub mod mouse;

use crate::diff::{DiffLine, FileDiff, LineKind};
use crate::git::{self, Base, BaseCandidates, DiffMode, RepoInfo};
use crate::outline::{self, OutlineRow};
use crate::symbols::SymbolIndex;
use crate::ui::LayoutHints;
use crate::viewport::{DiffLayout, RowRef, ViewKind, ViewportState};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// Tunable constants
const FLASH_DURATION: Duration = Duration::from_millis(300);
/// How long a status-bar message stays readable.
pub(crate) const STATUS_DURATION: Duration = Duration::from_secs(2);
pub(crate) const SCROLL_SPEED: usize = 3;
pub(crate) const DOUBLE_CLICK_MS: u64 = 400;
/// Columns of mouse jitter tolerated between the two clicks of a double-click.
pub(crate) const DOUBLE_CLICK_SLOP: u16 = 2;

pub struct HunkComment {
    pub file_idx: usize,
    pub hunk_idx: usize,
    pub text: String,
}

pub struct CommentInputState {
    pub file_idx: usize,
    pub hunk_idx: usize,
    pub text: String,
    pub cursor_pos: usize,
    /// Layout row for positioning the floating input
    pub anchor_row: usize,
}

pub struct CommentBrowserState {
    pub query: String,
    pub selected: usize,
    pub checked: std::collections::HashSet<usize>,
}

#[derive(Default)]
struct RefreshGate {
    in_flight: bool,
    pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GapExpansionKey {
    file_path: String,
    gap_idx: usize,
}

impl RefreshGate {
    fn request(&mut self) -> bool {
        if self.in_flight {
            self.pending = true;
            false
        } else {
            self.in_flight = true;
            true
        }
    }

    fn complete(&mut self) -> bool {
        self.in_flight = false;
        std::mem::take(&mut self.pending)
    }
}

pub struct RepoState {
    pub id: u64,
    pub info: RepoInfo,
    pub mode: DiffMode,
    pub files: Vec<FileDiff>,
    /// Branches this repo can be compared against; None until detection has run once.
    pub bases: Option<BaseCandidates>,
    /// Whether branch comparisons leave uncommitted work out. Remembered across mode
    /// switches so `b` comes back to the same view the picker last set up.
    pub commits_only: bool,
    pub unified_layout: Option<DiffLayout>,
    pub sbs_layout: Option<DiffLayout>,
    pub unified_viewport: ViewportState,
    pub sbs_viewport: ViewportState,
    pub comments: Vec<HunkComment>,
    /// False until the first diff computation for this repo has finished (or failed).
    pub loaded: bool,
    /// Call-site index of the working tree, built in the background; None until ready.
    pub symbols: Option<Arc<SymbolIndex>>,
    index_in_flight: bool,
    /// Paths to re-index once the in-flight job finishes; `Some(empty)` means everything.
    index_pending: Option<Vec<String>>,
    diff_generation: u64,
    pending_gap_expansions: std::collections::HashSet<GapExpansionKey>,
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

/// The `B` popup: everything the active repo could be compared against.
pub struct ComparePickerState {
    pub selected: usize,
    /// A ref being typed for the custom row.
    pub query: String,
}

/// One line of the compare picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareRow {
    /// A branch comparison, with the name `base` resolves to and a short reason to pick it.
    Base {
        base: Base,
        name: String,
        detail: &'static str,
    },
    Staged,
    Unstaged,
    /// The checkbox that leaves uncommitted work out of branch comparisons.
    CommitsOnly,
    /// Free-text ref entry.
    CustomRef,
}

pub struct RepoAdderState {
    pub query: String,
    pub error: Option<String>,
    pub results: Vec<(String, PathBuf)>,
    pub cursor: usize,
    pub checked: std::collections::HashSet<usize>,
}

pub struct MarkdownPreviewState {
    pub content: String,
    pub path: String,
    pub scroll: usize,
}

/// The change-shape view: a file tree with the declarations each hunk touches.
pub struct OutlineState {
    pub rows: Vec<OutlineRow>,
    /// Index into `rows`; always a selectable row when any exist.
    pub selected: usize,
    pub scroll: usize,
    /// Symbols whose callers and callees are shown, keyed by (file path, identifier).
    pub expanded: std::collections::HashSet<(String, String)>,
}

/// Diffs with at least this many files open in the outline first, so the shape of the
/// change is visible before any single hunk.
pub const OUTLINE_AUTO_OPEN_FILES: usize = 15;

/// Whole-file additions or deletions longer than this start collapsed.
pub const LARGE_WHOLE_FILE_LINES: usize = 200;

pub struct App {
    pub repos: Vec<RepoState>,
    next_repo_id: u64,
    diff_refreshes: std::collections::HashMap<u64, RefreshGate>,
    base_refreshes: std::collections::HashMap<u64, RefreshGate>,
    pub active_tab: usize,
    pub focused_file: Option<usize>,
    /// Hunk explicitly selected with `]`/`[` or a click. Only honoured while it is on screen;
    /// otherwise focus falls back to the first hunk in view. Cleared on every diff refresh.
    hunk_cursor: Option<(usize, usize)>,
    pub side_by_side: bool,
    pub last_error: Option<String>,
    pub flash: Vec<FlashState>,
    pub status_message: Option<(String, Instant)>,
    pub show_help: bool,
    pub file_picker: Option<FilePickerState>,
    pub compare_picker: Option<ComparePickerState>,
    pub repo_adder: Option<RepoAdderState>,
    pub comment_input: Option<CommentInputState>,
    pub comment_browser: Option<CommentBrowserState>,
    pub markdown_preview: Option<MarkdownPreviewState>,
    pub outline: Option<OutlineState>,
    pub(crate) markdown_render_cache:
        std::cell::RefCell<Option<(u16, Vec<ratatui::text::Line<'static>>)>>,
    pub layout: LayoutHints,
    pub last_click: Option<(u16, u16, Instant)>,
    diff_worker: Option<DiffWorker>,
    index_worker: Option<IndexWorker>,
}

impl App {
    pub fn new(repo_infos: Vec<RepoInfo>) -> Self {
        let repos: Vec<RepoState> = repo_infos
            .into_iter()
            .enumerate()
            .map(|(i, info)| RepoState {
                id: i as u64,
                info,
                mode: DiffMode::Local,
                files: Vec::new(),
                bases: None,
                commits_only: false,
                unified_layout: None,
                sbs_layout: None,
                unified_viewport: ViewportState::default(),
                sbs_viewport: ViewportState::default(),
                comments: Vec::new(),
                loaded: false,
                symbols: None,
                index_in_flight: false,
                index_pending: None,
                diff_generation: 0,
                pending_gap_expansions: std::collections::HashSet::new(),
            })
            .collect();
        let next_repo_id = repos.len() as u64;
        let diff_refreshes = repos
            .iter()
            .map(|repo| (repo.id, RefreshGate::default()))
            .collect();
        let base_refreshes = repos
            .iter()
            .map(|repo| (repo.id, RefreshGate::default()))
            .collect();
        Self {
            repos,
            next_repo_id,
            diff_refreshes,
            base_refreshes,
            active_tab: 0,
            focused_file: None,
            hunk_cursor: None,
            side_by_side: false,
            last_error: None,
            flash: Vec::new(),
            status_message: None,
            show_help: false,
            file_picker: None,
            compare_picker: None,
            repo_adder: None,
            comment_input: None,
            comment_browser: None,
            markdown_preview: None,
            outline: None,
            markdown_render_cache: std::cell::RefCell::new(None),
            layout: LayoutHints::default(),
            last_click: None,
            diff_worker: None,
            index_worker: None,
        }
    }

    pub fn current_mode(&self) -> &DiffMode {
        &self.repos[self.active_tab].mode
    }

    /// Detected bases for the active repo, empty until detection has run.
    pub fn current_bases(&self) -> BaseCandidates {
        self.repos[self.active_tab]
            .bases
            .clone()
            .unwrap_or_default()
    }

    /// Whether the active repo's `DiffMode::Branch` has a base to compare against.
    pub fn branch_base_resolved(&self) -> bool {
        match self.current_mode() {
            DiffMode::Branch { base, .. } => self.current_bases().resolve(base).is_some(),
            _ => true,
        }
    }

    /// The `b` view for the active repo: everything since the fork point with the stack
    /// parent (or trunk), uncommitted work included unless the picker turned it off.
    pub fn branch_mode(&self) -> DiffMode {
        DiffMode::Branch {
            base: Base::Parent,
            commits_only: self.repos[self.active_tab].commits_only,
        }
    }

    /// Rows for the compare picker, in display order. Bases that resolve to the same
    /// branch appear once (on main, trunk and upstream are both `origin/main`).
    pub fn compare_rows(&self) -> Vec<CompareRow> {
        let bases = self.current_bases();
        let mut rows: Vec<CompareRow> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut push_base = |base: Base, detail: &'static str| {
            let Some(name) = bases.resolve(&base) else {
                return;
            };
            if seen.contains(&name) {
                return;
            }
            seen.push(name.clone());
            rows.push(CompareRow::Base { base, name, detail });
        };
        if bases.parent.is_some() {
            push_base(Base::Parent, "stack parent");
            push_base(Base::Trunk, "trunk, whole stack");
        } else {
            push_base(Base::Trunk, "trunk");
        }
        push_base(Base::Upstream, "upstream, unpushed");
        rows.push(CompareRow::Staged);
        rows.push(CompareRow::Unstaged);
        rows.push(CompareRow::CommitsOnly);
        rows.push(CompareRow::CustomRef);
        rows
    }

    /// Open the `B` popup with the cursor on whatever is showing now. A typed ref that is
    /// active comes back into the custom row so it can be edited rather than retyped.
    pub fn open_compare_picker(&mut self) {
        let rows = self.compare_rows();
        let current = self.current_mode().clone();
        let query = match &current {
            DiffMode::Branch {
                base: Base::Ref(name),
                ..
            } => name.clone(),
            _ => String::new(),
        };
        let selected = rows
            .iter()
            .position(|row| match (row, &current) {
                (CompareRow::Base { base, .. }, DiffMode::Branch { base: current, .. }) => {
                    base == current
                }
                (CompareRow::CustomRef, _) => !query.is_empty(),
                (CompareRow::Staged, DiffMode::Staged) => true,
                (CompareRow::Unstaged, DiffMode::Unstaged) => true,
                _ => false,
            })
            .unwrap_or(0);
        self.compare_picker = Some(ComparePickerState { selected, query });
    }

    /// True while a popup owns the keyboard; mouse clicks on the diff are ignored then.
    pub fn modal_open(&self) -> bool {
        self.comment_input.is_some()
            || self.comment_browser.is_some()
            || self.file_picker.is_some()
            || self.compare_picker.is_some()
            || self.repo_adder.is_some()
            || self.markdown_preview.is_some()
            || self.show_help
    }

    /// Show a transient message in the status bar.
    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status_message = Some((message.into(), Instant::now() + STATUS_DURATION));
    }

    /// Rows scrolled by PageUp/PageDown: one screen minus a line of overlap.
    pub fn page_size(&self) -> usize {
        self.viewport_height().saturating_sub(1).max(1)
    }

    /// Rows scrolled by Ctrl+D/Ctrl+U.
    pub fn half_page_size(&self) -> usize {
        (self.viewport_height() / 2).max(1)
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx >= self.repos.len() {
            return;
        }
        self.active_tab = idx;
        self.hunk_cursor = None;
        self.prepare_active_layout();
        self.clamp_active_viewport();
        self.focused_file = self.focused_file_from_scroll();
        if self.outline.is_some() {
            self.rebuild_outline();
        }
    }

    // -- Outline (change shape) view --

    pub fn open_outline(&mut self) {
        let expanded = self
            .outline
            .take()
            .map(|state| state.expanded)
            .unwrap_or_default();
        let rows = self
            .repos
            .get(self.active_tab)
            .map(|repo| outline::build_outline(&repo.files, repo.symbols.as_deref(), &expanded))
            .unwrap_or_default();
        let selected = rows.iter().position(OutlineRow::is_selectable).unwrap_or(0);
        self.outline = Some(OutlineState {
            rows,
            selected,
            scroll: 0,
            expanded,
        });
    }

    /// Show or hide the callers and callees of the symbol the cursor is on.
    pub fn outline_set_expanded(&mut self, expand: bool) {
        let Some(state) = &self.outline else {
            return;
        };
        let Some(key) = state
            .rows
            .get(state.selected)
            .and_then(OutlineRow::symbol_key)
            .map(|(path, ident)| (path.to_string(), ident.to_string()))
        else {
            return;
        };
        let Some(state) = &mut self.outline else {
            return;
        };
        let changed = if expand {
            state.expanded.insert(key.clone())
        } else {
            state.expanded.remove(&key)
        };
        if !changed {
            return;
        }
        self.rebuild_outline();
        // Land on the symbol row itself, which exists in either state.
        if let Some(state) = &mut self.outline
            && let Some(index) = state.rows.iter().position(|row| {
                matches!(row, OutlineRow::Symbol { .. })
                    && row.symbol_key() == Some((key.0.as_str(), key.1.as_str()))
            })
        {
            state.selected = index;
        }
        self.keep_outline_selection_visible();
    }

    pub fn close_outline(&mut self) {
        self.outline = None;
    }

    pub fn toggle_outline(&mut self) {
        if self.outline.is_some() {
            self.close_outline();
        } else {
            self.open_outline();
        }
    }

    /// Recompute rows after the diff changed, keeping the cursor on the same file.
    fn rebuild_outline(&mut self) {
        let Some(state) = &self.outline else {
            return;
        };
        let previous_target = state.rows.get(state.selected).and_then(OutlineRow::target);
        let previous_key = state
            .rows
            .get(state.selected)
            .and_then(OutlineRow::symbol_key)
            .map(|(path, ident)| (path.to_string(), ident.to_string()));
        let previous_path = previous_target.and_then(|(file_idx, _)| {
            self.current_files()
                .and_then(|files| files.get(file_idx))
                .map(|file| file.path.clone())
        });
        let previous_scroll = state.scroll;
        self.open_outline();
        let files = &self.repos[self.active_tab].files;
        if let Some(state) = &mut self.outline {
            let same_symbol = previous_key.as_ref().and_then(|key| {
                state.rows.iter().position(|row| {
                    matches!(row, OutlineRow::Symbol { .. })
                        && row.symbol_key() == Some((key.0.as_str(), key.1.as_str()))
                })
            });
            let same_file = previous_path.as_ref().and_then(|path| {
                state.rows.iter().position(|row| {
                    matches!(row, OutlineRow::File { file_idx, .. }
                        if files.get(*file_idx).is_some_and(|f| &f.path == path))
                })
            });
            if let Some(index) = same_symbol.or(same_file) {
                state.selected = index;
            }
            state.scroll = previous_scroll;
        }
        self.keep_outline_selection_visible();
    }

    /// Move the outline cursor by `delta` selectable rows.
    pub fn outline_move(&mut self, delta: isize) {
        let Some(state) = &mut self.outline else {
            return;
        };
        let selectable: Vec<usize> = state
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is_selectable())
            .map(|(index, _)| index)
            .collect();
        if selectable.is_empty() {
            return;
        }
        let position = selectable
            .iter()
            .position(|&index| index >= state.selected)
            .unwrap_or(selectable.len() - 1);
        let next = position
            .saturating_add_signed(delta)
            .min(selectable.len() - 1);
        state.selected = selectable[next];
        self.keep_outline_selection_visible();
    }

    /// Scroll the outline just enough to keep the selection on screen, with a small
    /// margin so the next row is already visible when moving.
    fn keep_outline_selection_visible(&mut self) {
        let height = self.viewport_height();
        let Some(state) = &mut self.outline else {
            return;
        };
        let max_scroll = state.rows.len().saturating_sub(height);
        let margin = 2.min(height / 3);
        if state.selected < state.scroll + margin {
            state.scroll = state.selected.saturating_sub(margin);
        } else if state.selected + margin >= state.scroll + height {
            state.scroll = (state.selected + margin + 1).saturating_sub(height);
        }
        state.scroll = state.scroll.min(max_scroll);
    }

    pub fn outline_jump_to_end(&mut self, end: bool) {
        let Some(state) = &mut self.outline else {
            return;
        };
        let target = if end {
            state.rows.iter().rposition(OutlineRow::is_selectable)
        } else {
            state.rows.iter().position(OutlineRow::is_selectable)
        };
        if let Some(index) = target {
            state.selected = index;
        }
        self.keep_outline_selection_visible();
    }

    /// Select the row under a content-area click, if it is selectable.
    pub fn outline_select_row(&mut self, row: usize) -> bool {
        let Some(state) = &mut self.outline else {
            return false;
        };
        if state.rows.get(row).is_some_and(OutlineRow::is_selectable) {
            state.selected = row;
            self.keep_outline_selection_visible();
            true
        } else {
            false
        }
    }

    /// Leave the outline and scroll the diff to the selected file or hunk. On a symbol's
    /// summary row this expands or collapses its call tree instead.
    pub fn outline_jump(&mut self) {
        let selected_row = self
            .outline
            .as_ref()
            .and_then(|state| state.rows.get(state.selected).cloned());
        match &selected_row {
            Some(OutlineRow::Summary { expanded, .. }) => {
                self.outline_set_expanded(!expanded);
                return;
            }
            Some(OutlineRow::Call {
                file_idx: None,
                location,
                ..
            }) => {
                self.set_status(format!("{location} is outside this diff"));
                return;
            }
            _ => {}
        }
        let Some(target) = selected_row.as_ref().and_then(OutlineRow::target) else {
            return;
        };
        self.outline = None;
        let (file_idx, hunk_idx) = target;
        if self
            .current_files()
            .and_then(|files| files.get(file_idx))
            .is_some_and(|file| file.collapsed)
        {
            self.toggle_collapsed(file_idx);
        }
        match hunk_idx {
            Some(hunk_idx) => {
                self.prepare_active_layout();
                let row = self
                    .current_layout()
                    .and_then(|layout| layout.hunk_row_range(file_idx, hunk_idx))
                    .map(|rows| rows.start.saturating_sub(1));
                if let Some(row) = row {
                    self.jump_active_viewport_to(row);
                }
                self.hunk_cursor = Some((file_idx, hunk_idx));
                self.focused_file = Some(file_idx);
            }
            None => self.jump_to_file(file_idx),
        }
    }

    pub fn outline_markdown(&self) -> Option<String> {
        let state = self.outline.as_ref()?;
        (!state.rows.is_empty()).then(|| outline::outline_markdown(&state.rows))
    }

    pub fn next_tab(&mut self) {
        if !self.repos.is_empty() {
            self.switch_tab((self.active_tab + 1) % self.repos.len());
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.repos.is_empty() {
            self.switch_tab((self.active_tab + self.repos.len() - 1) % self.repos.len());
        }
    }

    pub fn set_mode(&mut self, mode: DiffMode, diff_tx: &mpsc::UnboundedSender<DiffResult>) {
        self.remember_mode(&mode);
        self.repos[self.active_tab].mode = mode;
        self.refresh_repo_async(self.active_tab, diff_tx);
        self.jump_active_viewport_top();
    }

    pub(crate) fn set_mode_bounded(&mut self, mode: DiffMode, diff_tx: &mpsc::Sender<DiffResult>) {
        self.remember_mode(&mode);
        self.repos[self.active_tab].mode = mode;
        self.refresh_repo_async_bounded(self.active_tab, diff_tx);
        self.jump_active_viewport_top();
    }

    fn remember_mode(&mut self, mode: &DiffMode) {
        if let DiffMode::Branch { commits_only, .. } = mode {
            self.repos[self.active_tab].commits_only = *commits_only;
        }
    }

    pub fn toggle_view(&mut self) {
        self.side_by_side = !self.side_by_side;
        if !self.side_by_side {
            for repo in &mut self.repos {
                repo.sbs_layout = None;
                for file in &mut repo.files {
                    file.sbs_cache = None;
                }
            }
        }
        self.prepare_active_layout();
        self.clamp_active_viewport();
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
        let width = self.layout.content_width.max(1) as usize;
        let width = if width <= 1 { 80 } else { width };
        let repo = match self.repos.get_mut(idx) {
            Some(repo) => repo,
            None => return,
        };
        let layout_slot = match view {
            ViewKind::Unified => &mut repo.unified_layout,
            ViewKind::SideBySide => &mut repo.sbs_layout,
        };
        if layout_slot
            .as_ref()
            .is_some_and(|layout| layout.content_width() == width)
        {
            return;
        }
        if view == ViewKind::SideBySide {
            crate::diff::ensure_sbs_caches(&mut repo.files);
        }
        *layout_slot = Some(DiffLayout::build(&repo.files, view, &repo.comments, width));
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

    /// Returns raw comment indices matching the browser query.
    pub fn filtered_comment_indices(&self) -> Vec<usize> {
        let Some(repo) = self.repos.get(self.active_tab) else {
            return Vec::new();
        };
        let query = self
            .comment_browser
            .as_ref()
            .map(|browser| browser.query.to_lowercase())
            .unwrap_or_default();

        repo.comments
            .iter()
            .enumerate()
            .filter(|(_, comment)| {
                if query.is_empty() {
                    return true;
                }
                let file_path = repo
                    .files
                    .get(comment.file_idx)
                    .map(|file| file.path.as_str())
                    .unwrap_or("");
                format!("{} {}", file_path, comment.text)
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(index, _)| index)
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

    /// Start an async gap expansion. Returns parameters for the background file read,
    /// or None if the gap is already closed or the indices are invalid.
    pub fn start_expand_gap(
        &mut self,
        file_idx: usize,
        gap_idx: usize,
    ) -> Option<GapExpandRequest> {
        let repo = self.repos.get(self.active_tab)?;
        let file = repo.files.get(file_idx)?;
        if file.hunks.is_empty() {
            return None;
        }
        let expansion_key = GapExpansionKey {
            file_path: file.path.clone(),
            gap_idx,
        };
        if repo.pending_gap_expansions.contains(&expansion_key) {
            return None;
        }

        const EXPAND_AMOUNT: usize = 20;

        let (gap_start, gap_end, old_offset) = if gap_idx == 0 {
            let first_new = file.hunks[0].first_new_lineno().unwrap_or(1) as usize;
            let first_old = file.hunks[0].first_old_lineno().unwrap_or(1) as usize;
            if first_new <= 1 {
                return None;
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
                return None;
            }
            let start = prev_last_new + 1;
            let end = (next_first_new - 1).min(start + EXPAND_AMOUNT - 1);
            (start, end, prev_last_old as i64 - prev_last_new as i64)
        } else {
            let last_idx = file.hunks.len() - 1;
            let last_new = file.hunks[last_idx].last_new_lineno().unwrap_or(0) as usize;
            let last_old = file.hunks[last_idx].last_old_lineno().unwrap_or(0) as usize;
            if last_new >= file.total_new_lines {
                return None;
            }
            let start = last_new + 1;
            let end = file.total_new_lines.min(start + EXPAND_AMOUNT - 1);
            (start, end, last_old as i64 - last_new as i64)
        };

        let request = GapExpandRequest {
            repo_id: repo.id,
            diff_generation: repo.diff_generation,
            file_idx,
            gap_idx,
            repo_path: repo.info.path.clone(),
            mode: repo.mode.clone(),
            diff_file_path: file.path.clone(),
            gap_start,
            gap_end,
            old_offset,
        };
        self.repos
            .get_mut(self.active_tab)?
            .pending_gap_expansions
            .insert(expansion_key);
        Some(request)
    }

    /// Apply the result of a background gap expansion.
    pub fn apply_gap_expand(&mut self, result: GapExpandResult) {
        let idx = match self.find_repo(result.repo_id) {
            Some(i) => i,
            None => return,
        };
        let Some(repo) = self.repos.get_mut(idx) else {
            return;
        };
        if repo.diff_generation != result.diff_generation {
            return;
        }
        let expansion_key = GapExpansionKey {
            file_path: result.diff_file_path.clone(),
            gap_idx: result.gap_idx,
        };
        if !repo.pending_gap_expansions.remove(&expansion_key) {
            return;
        }
        let Some(file) = repo.files.get_mut(result.file_idx) else {
            return;
        };
        if file.path != result.diff_file_path || file.hunks.is_empty() {
            return;
        }

        let context_lines = result.lines;
        let gap_idx = result.gap_idx;

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

        file.sbs_cache = None;
        self.invalidate_layouts(idx);
        if idx == self.active_tab {
            self.prepare_active_layout();
            self.clamp_active_viewport();
        }
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
            mode: DiffMode::Local,
            files: Vec::new(),
            bases: None,
            commits_only: false,
            unified_layout: None,
            sbs_layout: None,
            unified_viewport: ViewportState::default(),
            sbs_viewport: ViewportState::default(),
            comments: Vec::new(),
            loaded: false,
            symbols: None,
            index_in_flight: false,
            index_pending: None,
            diff_generation: 0,
            pending_gap_expansions: std::collections::HashSet::new(),
        });
        self.diff_refreshes.insert(id, RefreshGate::default());
        self.base_refreshes.insert(id, RefreshGate::default());
        self.switch_tab(idx);

        Ok(idx)
    }

    pub(crate) fn remove_repo_at(&mut self, idx: usize) -> Option<PathBuf> {
        let repo = self.repos.get(idx)?;
        let id = repo.id;
        let path = repo.info.path.clone();
        self.repos.remove(idx);
        self.diff_refreshes.remove(&id);
        self.base_refreshes.remove(&id);
        if self.repos.is_empty() {
            self.active_tab = 0;
            self.focused_file = None;
            self.hunk_cursor = None;
        } else {
            self.switch_tab(self.active_tab.min(self.repos.len() - 1));
        }
        Some(path)
    }

    /// Preserve the original public cache-preparation entry point.
    pub fn ensure_sbs_caches(&mut self) {
        if !self.side_by_side {
            return;
        }
        if let Some(files) = self.current_files_mut() {
            crate::diff::ensure_sbs_caches(files);
        }
        self.ensure_layout(self.active_tab, ViewKind::SideBySide);
    }

    pub fn apply_diff_result(&mut self, idx: usize, result: anyhow::Result<Vec<FileDiff>>) {
        let was_loaded = std::mem::replace(&mut self.repos[idx].loaded, true);
        match result {
            Ok(files) => {
                self.repos[idx].diff_generation = self.repos[idx].diff_generation.wrapping_add(1);
                self.repos[idx].pending_gap_expansions.clear();
                let old_collapsed: std::collections::HashMap<String, bool> = self.repos[idx]
                    .files
                    .iter()
                    .map(|f| (f.path.clone(), f.collapsed))
                    .collect();

                let first_load = !was_loaded;
                let mut changed_paths: Vec<String> = self.repos[idx]
                    .files
                    .iter()
                    .chain(files.iter())
                    .map(|file| file.path.clone())
                    .collect();
                changed_paths.sort();
                changed_paths.dedup();
                let mut new_files = files;
                for file in &mut new_files {
                    if let Some(&collapsed) = old_collapsed.get(&file.path) {
                        file.collapsed = collapsed;
                    } else if file.is_whole_file_change()
                        && file.total_display_lines() > LARGE_WHOLE_FILE_LINES
                    {
                        // A 400-line new file is best read as a file, not a diff; start
                        // folded so the shape of the change stays visible.
                        file.collapsed = true;
                    }
                }
                if self.side_by_side {
                    crate::diff::ensure_sbs_caches(&mut new_files);
                }

                self.repos[idx].files = new_files;
                let cleared_notes = self.repos[idx].comments.len();
                self.repos[idx].comments.clear();
                if idx == self.active_tab {
                    let had_draft = self.comment_input.is_some();
                    self.comment_input = None;
                    self.comment_browser = None;
                    self.hunk_cursor = None;
                    if had_draft {
                        self.set_status("Diff changed — unsaved note discarded");
                    } else if cleared_notes > 0 {
                        self.set_status(format!(
                            "Diff changed — {} note{} cleared",
                            cleared_notes,
                            if cleared_notes == 1 { "" } else { "s" }
                        ));
                    }
                }
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
                self.request_index(idx, Some(changed_paths));
                if idx == self.active_tab {
                    self.focused_file = self.focused_file_from_scroll();
                    let file_count = self.repos[idx].files.len();
                    if self.outline.is_some() {
                        self.rebuild_outline();
                    } else if first_load && file_count >= OUTLINE_AUTO_OPEN_FILES {
                        self.open_outline();
                        self.set_status(format!(
                            "{file_count} files changed — showing the outline; Enter opens a file, o shows the full diff"
                        ));
                    }
                }
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("{}: {}", self.repos[idx].info.name, e));
            }
        }
    }

    pub(crate) fn refresh_repo_async_bounded(
        &mut self,
        idx: usize,
        diff_tx: &mpsc::Sender<DiffResult>,
    ) {
        let repo = &self.repos[idx];
        let id = repo.id;
        if !self.diff_refreshes.entry(id).or_default().request() {
            return;
        }
        let job = DiffJob {
            repo_id: id,
            path: repo.info.path.clone(),
            mode: repo.mode.clone(),
            bases: repo.bases.clone(),
        };
        if let Some(worker) = &self.diff_worker
            && worker.submit(job.clone())
        {
            return;
        }
        // No worker (tests) or the worker is gone: fall back to a one-off thread.
        let tx = diff_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.blocking_send(job.run());
        });
    }

    /// Route diff computations through one long-lived thread instead of a thread per
    /// refresh. Besides skipping spawn cost, this keeps the (large, short-lived) diff
    /// allocations in a single malloc magazine, so freed memory is reused rather than
    /// left dirty across many per-thread arenas.
    pub fn attach_diff_worker(&mut self, results: mpsc::Sender<DiffResult>) {
        self.diff_worker = Some(DiffWorker::spawn(results));
    }

    pub fn attach_index_worker(&mut self, results: mpsc::Sender<IndexResult>) {
        self.index_worker = Some(IndexWorker::spawn(results));
    }

    /// Ask for the symbol index of a repo to be (re)built in the background. `changed`
    /// limits the work to those paths when an index already exists; `None` rebuilds all.
    /// Requests that arrive while a job runs are merged and run once it finishes.
    pub fn request_index(&mut self, idx: usize, changed: Option<Vec<String>>) {
        let Some(worker) = &self.index_worker else {
            return; // tests and headless use
        };
        let repo = &mut self.repos[idx];
        let changed = match (&repo.symbols, changed) {
            (Some(_), Some(paths)) => Some(paths),
            _ => None,
        };
        if repo.index_in_flight {
            repo.index_pending = match (repo.index_pending.take(), changed) {
                (Some(mut pending), Some(paths)) if !pending.is_empty() => {
                    pending.extend(paths);
                    pending.sort();
                    pending.dedup();
                    Some(pending)
                }
                (None, Some(paths)) => Some(paths),
                _ => Some(Vec::new()), // full rebuild wins
            };
            return;
        }
        let job = IndexJob {
            repo_id: repo.id,
            root: repo.info.path.clone(),
            base: repo.symbols.clone(),
            changed,
        };
        if worker.submit(job) {
            repo.index_in_flight = true;
        }
    }

    /// Store a finished index; returns true when the active tab's outline needs redrawing.
    pub fn apply_index_result(&mut self, result: IndexResult) -> bool {
        let Some(idx) = self.find_repo(result.repo_id) else {
            return false;
        };
        {
            let repo = &mut self.repos[idx];
            repo.symbols = Some(result.index);
            repo.index_in_flight = false;
        }
        if let Some(pending) = self.repos[idx].index_pending.take() {
            let changed = (!pending.is_empty()).then_some(pending);
            self.request_index(idx, changed);
        }
        if idx == self.active_tab && self.outline.is_some() {
            self.rebuild_outline();
            return true;
        }
        false
    }

    pub fn refresh_repo_async(&self, idx: usize, diff_tx: &mpsc::UnboundedSender<DiffResult>) {
        let repo = &self.repos[idx];
        let id = repo.id;
        let path = repo.info.path.clone();
        let mode = repo.mode.clone();
        let bases = repo.bases.clone();
        let tx = diff_tx.clone();
        std::thread::spawn(move || {
            let result = git::compute_diff(&path, &mode, bases.as_ref());
            let _ = tx.send(DiffResult {
                repo_id: id,
                mode,
                result,
            });
        });
    }

    pub(crate) fn apply_diff_refresh_result(
        &mut self,
        result: DiffResult,
        diff_tx: &mpsc::Sender<DiffResult>,
    ) -> bool {
        let Some(idx) = self.find_repo(result.repo_id) else {
            return false;
        };

        let pending = self
            .diff_refreshes
            .entry(result.repo_id)
            .or_default()
            .complete();
        let applied = !pending && result.mode == self.repos[idx].mode;
        if applied {
            self.apply_diff_result(idx, result.result);
        }

        if pending {
            self.refresh_repo_async_bounded(idx, diff_tx);
        }
        applied
    }

    pub(crate) fn refresh_base_async(
        &mut self,
        idx: usize,
        base_tx: &mpsc::Sender<BaseBranchResult>,
    ) {
        let repo = &self.repos[idx];
        let id = repo.id;
        if !self.base_refreshes.entry(id).or_default().request() {
            return;
        }
        let path = repo.info.path.clone();
        let tx = base_tx.clone();
        std::thread::spawn(move || {
            let bases = git::detect_bases(&path);
            let _ = tx.blocking_send(BaseBranchResult { repo_id: id, bases });
        });
    }

    pub(crate) fn apply_base_refresh_result(
        &mut self,
        result: BaseBranchResult,
        base_tx: &mpsc::Sender<BaseBranchResult>,
        diff_tx: &mpsc::Sender<DiffResult>,
    ) -> bool {
        let Some(idx) = self.find_repo(result.repo_id) else {
            return false;
        };

        let pending = self
            .base_refreshes
            .entry(result.repo_id)
            .or_default()
            .complete();
        if !pending {
            let changed = self.repos[idx].bases.as_ref() != Some(&result.bases);
            self.repos[idx].bases = Some(result.bases);
            if changed && self.repos[idx].mode.is_branch() {
                self.refresh_repo_async_bounded(idx, diff_tx);
            }
        } else {
            self.refresh_base_async(idx, base_tx);
        }
        !pending
    }

    pub fn total_display_lines(&self) -> usize {
        self.current_layout()
            .map(DiffLayout::total_lines)
            .unwrap_or(0)
    }

    pub fn file_header_positions(&self) -> Vec<usize> {
        let mut positions = Vec::new();
        if let Some(layout) = self.current_layout() {
            for file_idx in 0..self.current_files().map(Vec::len).unwrap_or(0) {
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
        let now = Instant::now();
        self.flash
            .iter()
            .any(|f| now < f.until && f.file_idx == file_idx && f.hunk_idx == hunk_idx)
    }

    pub fn file_and_hunk_at_row(&self, content_row: usize) -> Option<(usize, usize)> {
        self.current_layout()
            .and_then(|layout| layout.hunk_at_row(content_row))
    }

    /// Put text on the clipboard and report the outcome in the status bar.
    pub fn copy_to_clipboard(&mut self, text: String, description: &str) {
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text)) {
            Ok(()) => self.set_status(format!("Copied {description}")),
            Err(error) => self.set_status(format!("Clipboard unavailable: {error}")),
        }
    }

    /// Copy the focused hunk (or the hunk at `content_row` if given) with feedback.
    pub fn copy_hunk_with_feedback(&mut self, content_row: Option<usize>) {
        let target = match content_row {
            Some(row) => self.file_and_hunk_at_row(row),
            None => self.focused_hunk(),
        };
        let Some((file_idx, hunk_idx)) = target else {
            self.set_status("No hunk to copy");
            return;
        };
        let Some(text) = self.copy_hunk(file_idx, hunk_idx) else {
            self.set_status("No hunk to copy");
            return;
        };
        let label = text.lines().next().unwrap_or("").trim_start_matches("// ");
        let label = format!("hunk {label}");
        self.copy_to_clipboard(text, &label);
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

        // Collect any comments attached to this hunk
        let hunk_comments: Vec<&HunkComment> = self
            .repos
            .get(self.active_tab)
            .map(|r| {
                r.comments
                    .iter()
                    .filter(|c| c.file_idx == file_idx && c.hunk_idx == target_hunk)
                    .collect()
            })
            .unwrap_or_default();

        let mut result = format!("// {}:{}-{}\n", file.path, first_lineno, last_lineno);

        if !hunk_comments.is_empty() {
            for c in &hunk_comments {
                for comment_line in c.text.lines() {
                    result.push_str(&format!("// > {}\n", comment_line));
                }
            }
        }

        for line in &hunk.lines {
            let prefix = match line.kind {
                LineKind::Addition => "+",
                LineKind::Deletion => "-",
                LineKind::Context => " ",
            };
            result.push_str(&format!("{} {}\n", prefix, line.content));
        }

        self.flash.push(FlashState {
            until: Instant::now() + FLASH_DURATION,
            file_idx,
            hunk_idx: target_hunk,
        });

        Some(result)
    }

    // -- Comment methods --

    /// Open the floating note editor for a hunk, pre-filled with any existing note.
    /// `anchor_row` is the layout row the editor floats next to; defaults to the hunk header.
    pub fn open_comment_input(
        &mut self,
        file_idx: usize,
        hunk_idx: usize,
        anchor_row: Option<usize>,
    ) {
        let anchor_row = anchor_row
            .or_else(|| {
                self.current_layout()
                    .and_then(|layout| layout.hunk_row_range(file_idx, hunk_idx))
                    .map(|rows| rows.start)
            })
            .unwrap_or_else(|| self.current_scroll_offset());
        let existing_text = self
            .find_comment(file_idx, hunk_idx)
            .map(|c| c.text.clone())
            .unwrap_or_default();
        let cursor_pos = existing_text.len();
        self.hunk_cursor = Some((file_idx, hunk_idx));
        self.focused_file = Some(file_idx);
        self.comment_input = Some(CommentInputState {
            file_idx,
            hunk_idx,
            text: existing_text,
            cursor_pos,
            anchor_row,
        });
    }

    pub fn find_comment(&self, file_idx: usize, hunk_idx: usize) -> Option<&HunkComment> {
        self.repos
            .get(self.active_tab)?
            .comments
            .iter()
            .find(|c| c.file_idx == file_idx && c.hunk_idx == hunk_idx)
    }

    pub fn add_or_update_comment(&mut self, file_idx: usize, hunk_idx: usize, text: String) {
        if let Some(repo) = self.repos.get_mut(self.active_tab) {
            if let Some(existing) = repo
                .comments
                .iter_mut()
                .find(|c| c.file_idx == file_idx && c.hunk_idx == hunk_idx)
            {
                existing.text = text;
            } else {
                repo.comments.push(HunkComment {
                    file_idx,
                    hunk_idx,
                    text,
                });
            }
        }
        self.invalidate_layouts(self.active_tab);
    }

    pub fn remove_comment(&mut self, file_idx: usize, hunk_idx: usize) {
        if let Some(repo) = self.repos.get_mut(self.active_tab) {
            repo.comments
                .retain(|c| !(c.file_idx == file_idx && c.hunk_idx == hunk_idx));
        }
        self.invalidate_layouts(self.active_tab);
    }

    pub fn clear_comments(&mut self) {
        let count = self
            .repos
            .get(self.active_tab)
            .map(|r| r.comments.len())
            .unwrap_or(0);
        if let Some(repo) = self.repos.get_mut(self.active_tab) {
            repo.comments.clear();
        }
        self.invalidate_layouts(self.active_tab);
        if count > 0 {
            self.set_status(format!(
                "Cleared {} note{}",
                count,
                if count == 1 { "" } else { "s" }
            ));
        } else {
            self.set_status("No notes to clear");
        }
    }

    pub fn format_comments_markdown(&self, indices: Option<&[usize]>) -> Option<String> {
        let repo = self.repos.get(self.active_tab)?;
        let files = &repo.files;
        let comments = &repo.comments;

        if comments.is_empty() {
            return None;
        }

        // Collect the comments to include, sorted by file then hunk position
        let mut selected: Vec<&HunkComment> = match indices {
            Some(idxs) => idxs.iter().filter_map(|&i| comments.get(i)).collect(),
            None => comments.iter().collect(),
        };

        if selected.is_empty() {
            return None;
        }

        selected.sort_by(|a, b| {
            let file_cmp = files
                .get(a.file_idx)
                .map(|f| f.path.as_str())
                .cmp(&files.get(b.file_idx).map(|f| f.path.as_str()));
            file_cmp.then(a.hunk_idx.cmp(&b.hunk_idx))
        });

        let mut result = String::from("## Review comments\n");

        for comment in &selected {
            let Some(file) = files.get(comment.file_idx) else {
                continue;
            };
            let Some(hunk) = file.hunks.get(comment.hunk_idx) else {
                continue;
            };

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

            result.push_str(&format!(
                "\n### {}:{}-{}\n",
                file.path, first_lineno, last_lineno
            ));

            for line in comment.text.lines() {
                result.push_str(&format!("> {}\n", line));
            }

            result.push_str("\n```diff\n");
            for line in &hunk.lines {
                let prefix = match line.kind {
                    LineKind::Addition => "+",
                    LineKind::Deletion => "-",
                    LineKind::Context => " ",
                };
                result.push_str(&format!("{} {}\n", prefix, line.content));
            }
            result.push_str("```\n");
        }

        Some(result)
    }

    /// The hunk that `y`, `n`, and `N` act on. This is the explicitly selected hunk while it is
    /// on screen, otherwise the first hunk visible in the viewport.
    pub fn focused_hunk(&self) -> Option<(usize, usize)> {
        let layout = self.current_layout()?;
        let visible = self.visible_row_range(layout.total_lines(), self.viewport_height());
        if let Some((file_idx, hunk_idx)) = self.hunk_cursor
            && let Some(rows) = layout.hunk_row_range(file_idx, hunk_idx)
            && rows.start < visible.end
            && rows.end > visible.start
        {
            return Some((file_idx, hunk_idx));
        }
        layout.first_hunk_in_rows(visible)
    }

    pub fn is_hunk_focused(&self, file_idx: usize, hunk_idx: usize) -> bool {
        self.focused_hunk() == Some((file_idx, hunk_idx))
    }

    /// Select a hunk and scroll just enough to bring its first rows into view.
    pub fn select_hunk(&mut self, file_idx: usize, hunk_idx: usize) {
        self.prepare_active_layout();
        let Some(rows) = self
            .current_layout()
            .and_then(|layout| layout.hunk_row_range(file_idx, hunk_idx))
        else {
            return;
        };
        let total = self.total_display_lines();
        let height = self.viewport_height();
        let visible = self.visible_row_range(total, height);
        // Leave one row above the hunk header for the pinned file header, otherwise the
        // header (and its function context) would be hidden behind it.
        let header_visible_from = visible.start + usize::from(visible.start > 0);
        if rows.start < header_visible_from || rows.start >= visible.end {
            self.jump_active_viewport_to(rows.start.saturating_sub(1));
        }
        self.hunk_cursor = Some((file_idx, hunk_idx));
        self.focused_file = Some(file_idx);
    }

    /// Move the hunk cursor forward to the next hunk in display order.
    pub fn select_next_hunk(&mut self) -> bool {
        self.prepare_active_layout();
        let Some(layout) = self.current_layout() else {
            return false;
        };
        let anchor = self
            .focused_hunk()
            .and_then(|(file_idx, hunk_idx)| layout.hunk_row_range(file_idx, hunk_idx))
            .map(|rows| rows.start)
            .unwrap_or(self.current_scroll_offset());
        let Some((file_idx, hunk_idx)) = layout.next_hunk_after_row(anchor) else {
            return false;
        };
        self.select_hunk(file_idx, hunk_idx);
        true
    }

    /// Move the hunk cursor back to the previous hunk in display order.
    pub fn select_prev_hunk(&mut self) -> bool {
        self.prepare_active_layout();
        let Some(layout) = self.current_layout() else {
            return false;
        };
        let anchor = self
            .focused_hunk()
            .and_then(|(file_idx, hunk_idx)| layout.hunk_row_range(file_idx, hunk_idx))
            .map(|rows| rows.start)
            .unwrap_or(self.current_scroll_offset());
        let Some((file_idx, hunk_idx)) = layout.prev_hunk_before_row(anchor) else {
            return false;
        };
        self.select_hunk(file_idx, hunk_idx);
        true
    }

    /// The file whose header should be pinned to the top of the diff area because the
    /// viewport has scrolled past it. `None` when the first visible row is already a header.
    pub fn sticky_header_file(&self) -> Option<usize> {
        let layout = self.current_layout()?;
        let first_row = self
            .visible_row_range(layout.total_lines(), self.viewport_height())
            .next()?;
        match layout.row(first_row)? {
            RowRef::FileHeader { .. } => None,
            row => Some(row.file_idx()),
        }
    }
}

pub struct DiffResult {
    pub repo_id: u64,
    pub mode: DiffMode,
    pub result: anyhow::Result<Vec<FileDiff>>,
}

/// One diff computation to run off the UI thread.
#[derive(Clone)]
struct DiffJob {
    repo_id: u64,
    path: PathBuf,
    mode: DiffMode,
    bases: Option<BaseCandidates>,
}

impl DiffJob {
    fn run(self) -> DiffResult {
        let result = git::compute_diff(&self.path, &self.mode, self.bases.as_ref());
        DiffResult {
            repo_id: self.repo_id,
            mode: self.mode,
            result,
        }
    }
}

/// A single long-lived thread that computes diffs in submission order.
struct DiffWorker {
    jobs: std::sync::mpsc::Sender<DiffJob>,
}

impl DiffWorker {
    fn spawn(results: mpsc::Sender<DiffResult>) -> Self {
        let (jobs, inbox) = std::sync::mpsc::channel::<DiffJob>();
        std::thread::Builder::new()
            .name("diff-worker".to_string())
            .spawn(move || {
                for job in inbox {
                    if results.blocking_send(job.run()).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn diff worker thread");
        Self { jobs }
    }

    /// False if the worker thread has exited, so the caller can fall back.
    fn submit(&self, job: DiffJob) -> bool {
        self.jobs.send(job).is_ok()
    }
}

struct IndexJob {
    repo_id: u64,
    root: PathBuf,
    base: Option<Arc<SymbolIndex>>,
    /// Paths to re-parse against `base`; `None` means index the whole repository.
    changed: Option<Vec<String>>,
}

pub struct IndexResult {
    pub repo_id: u64,
    pub index: Arc<SymbolIndex>,
}

impl IndexJob {
    fn run(self) -> IndexResult {
        let index = match (self.base, self.changed) {
            (Some(base), Some(changed)) => base.with_updated_files(&self.root, &changed),
            _ => {
                let paths = git2::Repository::open(&self.root)
                    .map(|repo| crate::symbols::indexable_paths(&repo))
                    .unwrap_or_default();
                SymbolIndex::build(&self.root, &paths)
            }
        };
        IndexResult {
            repo_id: self.repo_id,
            index: Arc::new(index),
        }
    }
}

/// One long-lived thread that builds symbol indexes in submission order.
struct IndexWorker {
    jobs: std::sync::mpsc::Sender<IndexJob>,
}

impl IndexWorker {
    fn spawn(results: mpsc::Sender<IndexResult>) -> Self {
        let (jobs, inbox) = std::sync::mpsc::channel::<IndexJob>();
        std::thread::Builder::new()
            .name("index-worker".to_string())
            .spawn(move || {
                for job in inbox {
                    if results.blocking_send(job.run()).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn index worker thread");
        Self { jobs }
    }

    fn submit(&self, job: IndexJob) -> bool {
        self.jobs.send(job).is_ok()
    }
}

pub struct BaseBranchResult {
    pub repo_id: u64,
    pub bases: BaseCandidates,
}

pub struct GapExpandRequest {
    pub repo_id: u64,
    pub diff_generation: u64,
    pub file_idx: usize,
    pub gap_idx: usize,
    pub repo_path: PathBuf,
    /// Decides where context comes from: working tree, index, or HEAD commit.
    pub mode: DiffMode,
    pub diff_file_path: String,
    pub gap_start: usize,
    pub gap_end: usize,
    pub old_offset: i64,
}

pub struct GapExpandResult {
    pub repo_id: u64,
    pub diff_generation: u64,
    pub file_idx: usize,
    pub gap_idx: usize,
    pub diff_file_path: String,
    pub lines: Vec<DiffLine>,
}

impl GapExpandRequest {
    /// Read the context lines in a background thread. This is the blocking part.
    pub fn execute(self) -> GapExpandResult {
        let lines = git::read_new_side_lines(
            &self.repo_path,
            Path::new(&self.diff_file_path),
            &self.mode,
            self.gap_start,
            self.gap_end,
        )
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(offset, content)| {
            let lineno = self.gap_start + offset;
            DiffLine {
                kind: LineKind::Context,
                content,
                old_lineno: Some((lineno as i64 + self.old_offset) as u32),
                new_lineno: Some(lineno as u32),
            }
        })
        .collect();
        GapExpandResult {
            repo_id: self.repo_id,
            diff_generation: self.diff_generation,
            file_idx: self.file_idx,
            gap_idx: self.gap_idx,
            diff_file_path: self.diff_file_path,
            lines,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{App, DiffResult, RefreshGate};
    use crate::diff::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};
    use crate::git::DiffMode;
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

    fn file_with_hunk(path: &str, first_line: u32) -> FileDiff {
        FileDiff {
            path: path.to_string(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: vec![Hunk {
                header: format!("@@ -{first_line} +{first_line} @@"),
                lines: vec![DiffLine {
                    kind: LineKind::Context,
                    content: "existing".to_string(),
                    old_lineno: Some(first_line),
                    new_lineno: Some(first_line),
                }],
            }],
            additions: 0,
            deletions: 0,
            collapsed: false,
            total_new_lines: first_line as usize,
            sbs_cache: None,
        }
    }

    fn set_picker_query(app: &mut App, query: &str) {
        app.file_picker = Some(super::FilePickerState {
            query: query.to_string(),
            selected: 0,
        });
    }

    #[test]
    fn stale_gap_expansion_does_not_modify_a_newer_diff() {
        let root = std::env::temp_dir().join(format!(
            "changes-stale-gap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create temporary repository");
        std::fs::write(root.join("old.rs"), "first\nsecond\nexisting\n").expect("write old file");

        let mut app = test_app_with_files(&[]);
        app.repos[0].info.path = root.clone();
        app.repos[0].files = vec![file_with_hunk("old.rs", 3)];
        let request = app
            .start_expand_gap(0, 0)
            .expect("old diff should have an expandable leading gap");

        app.apply_diff_result(0, Ok(vec![file_with_hunk("new.rs", 3)]));
        app.apply_gap_expand(request.execute());

        assert_eq!(app.repos[0].files[0].path, "new.rs");
        assert_eq!(app.repos[0].files[0].hunks[0].lines.len(), 1);
        std::fs::remove_dir_all(root).expect("remove temporary repository");
    }

    #[test]
    fn staged_gap_expansion_reads_the_index_not_the_working_tree() {
        let root = std::env::temp_dir().join(format!(
            "changes-staged-gap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create temporary repository");
        let repository = git2::Repository::init(&root).expect("init repository");
        let committed: String = (1..=10).map(|n| format!("line {n}\n")).collect();
        std::fs::write(root.join("file.txt"), &committed).expect("write file");
        let mut index = repository.index().expect("index");
        index
            .add_path(std::path::Path::new("file.txt"))
            .expect("add file");
        index.write().expect("write index");
        let tree_id = index.write_tree().expect("write tree");
        {
            let tree = repository.find_tree(tree_id).expect("tree");
            let signature = git2::Signature::now("Test", "test@example.com").expect("sig");
            repository
                .commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
                .expect("commit");
        }
        // Stage a change to line 10, then dirty line 3 in the working tree only.
        let staged = committed.replace("line 10\n", "STAGED\n");
        std::fs::write(root.join("file.txt"), &staged).expect("write staged");
        index
            .add_path(std::path::Path::new("file.txt"))
            .expect("stage change");
        index.write().expect("write index");
        let unstaged = staged.replace("line 3\n", "UNSTAGED\n");
        std::fs::write(root.join("file.txt"), &unstaged).expect("write unstaged");
        drop(index);
        drop(repository);

        let mut app = test_app_with_files(&[]);
        app.repos[0].info.path = root.clone();
        app.repos[0].mode = DiffMode::Staged;
        let files = crate::git::compute_diff(&root, &DiffMode::Staged, None).expect("diff");
        app.apply_diff_result(0, Ok(files));
        assert_eq!(app.repos[0].files[0].total_new_lines, 10);

        let request = app
            .start_expand_gap(0, 0)
            .expect("staged hunk has hidden lines above it");
        let result = request.execute();
        let contents: Vec<&str> = result.lines.iter().map(|l| l.content.as_str()).collect();
        std::fs::remove_dir_all(root).expect("remove temporary repository");

        assert!(
            contents.contains(&"line 3"),
            "expected index content: {contents:?}"
        );
        assert!(
            !contents.contains(&"UNSTAGED"),
            "working tree leaked: {contents:?}"
        );
    }

    #[test]
    fn duplicate_gap_expansion_request_is_coalesced() {
        let mut app = test_app_with_files(&[]);
        app.repos[0].files = vec![file_with_hunk("old.rs", 3)];

        assert!(app.start_expand_gap(0, 0).is_some());
        assert!(app.start_expand_gap(0, 0).is_none());
    }

    /// A file with `count` single-line hunks spaced far enough apart to leave gaps.
    fn file_with_hunks(path: &str, count: u32) -> FileDiff {
        let mut file = file_with_hunk(path, 10);
        file.hunks = (0..count)
            .map(|i| {
                let line = 10 + i * 30;
                Hunk {
                    header: format!("@@ -{line} +{line} @@"),
                    lines: vec![DiffLine {
                        kind: LineKind::Addition,
                        content: format!("hunk {i}"),
                        old_lineno: None,
                        new_lineno: Some(line),
                    }],
                }
            })
            .collect();
        file.total_new_lines = (10 + count * 30) as usize;
        file
    }

    fn app_with_two_files_of_three_hunks() -> App {
        let mut app = test_app_with_files(&[]);
        app.layout.content_width = 80;
        app.layout.content_height = 6;
        app.apply_diff_result(
            0,
            Ok(vec![file_with_hunks("a.rs", 3), file_with_hunks("b.rs", 3)]),
        );
        app
    }

    #[test]
    fn focus_is_available_before_the_user_scrolls() {
        let app = app_with_two_files_of_three_hunks();
        assert_eq!(app.focused_file, Some(0));
        assert_eq!(app.focused_hunk(), Some((0, 0)));
    }

    #[test]
    fn bracket_navigation_walks_hunks_across_files_and_keeps_them_in_view() {
        let mut app = app_with_two_files_of_three_hunks();

        assert!(app.select_next_hunk());
        assert_eq!(app.focused_hunk(), Some((0, 1)));
        assert!(app.select_next_hunk());
        assert!(app.select_next_hunk());
        assert_eq!(app.focused_hunk(), Some((1, 0)));
        assert_eq!(app.focused_file, Some(1));

        // The selected hunk's rows are always inside the viewport.
        let layout = app.current_layout().unwrap();
        let rows = layout.hunk_row_range(1, 0).unwrap();
        let visible = app.visible_row_range(layout.total_lines(), 6);
        assert!(rows.start >= visible.start && rows.start < visible.end);

        assert!(app.select_prev_hunk());
        assert_eq!(app.focused_hunk(), Some((0, 2)));

        for _ in 0..10 {
            app.select_prev_hunk();
        }
        assert!(!app.select_prev_hunk());
        assert_eq!(app.focused_hunk(), Some((0, 0)));
    }

    #[test]
    fn hunk_cursor_yields_to_the_viewport_once_scrolled_away() {
        let mut app = app_with_two_files_of_three_hunks();
        app.select_hunk(0, 1);
        assert_eq!(app.focused_hunk(), Some((0, 1)));

        app.jump_active_viewport_bottom();
        let focused = app.focused_hunk().expect("a hunk is visible at the bottom");
        assert_ne!(focused, (0, 1));
        assert_eq!(focused.0, 1);
    }

    #[test]
    fn diff_refresh_drops_the_hunk_cursor() {
        let mut app = app_with_two_files_of_three_hunks();
        app.select_hunk(1, 2);
        app.apply_diff_result(0, Ok(vec![file_with_hunks("a.rs", 3)]));
        assert_eq!(app.focused_hunk(), Some((0, 0)));
    }

    #[test]
    fn sticky_header_appears_only_after_scrolling_past_the_file_header() {
        let mut app = app_with_two_files_of_three_hunks();
        assert_eq!(app.sticky_header_file(), None);

        app.scroll_active_viewport(2);
        assert_eq!(app.sticky_header_file(), Some(0));

        let header_row = app.current_layout().unwrap().file_header_row(1).unwrap();
        app.jump_active_viewport_to(header_row);
        assert_eq!(app.sticky_header_file(), None);
    }

    #[test]
    fn switching_tabs_keeps_a_focused_file() {
        let mut app = App::new(vec![
            RepoInfo {
                name: "one".to_string(),
                path: PathBuf::from("/one"),
            },
            RepoInfo {
                name: "two".to_string(),
                path: PathBuf::from("/two"),
            },
        ]);
        app.layout.content_width = 80;
        app.layout.content_height = 6;
        app.apply_diff_result(1, Ok(vec![file_with_hunks("b.rs", 1)]));

        app.next_tab();
        assert_eq!(app.active_tab, 1);
        assert_eq!(app.focused_file, Some(0));
        assert_eq!(app.focused_hunk(), Some((0, 0)));
    }

    #[test]
    fn outline_navigation_skips_directories_and_jumps_to_hunks() {
        let mut app = app_with_two_files_of_three_hunks();
        app.repos[0].files[1].path = "src/b.rs".to_string();
        app.open_outline();
        let state = app.outline.as_ref().unwrap();
        // First row is the `src/` directory, which is not selectable.
        assert!(!state.rows[0].is_selectable());
        assert!(state.rows[state.selected].is_selectable());

        // Move onto the first symbol row of the first file and jump to it.
        app.outline_move(1);
        let target = app.outline.as_ref().unwrap().rows[app.outline.as_ref().unwrap().selected]
            .target()
            .unwrap();
        app.outline_jump();
        assert!(app.outline.is_none());
        assert_eq!(app.focused_hunk(), Some((target.0, target.1.unwrap_or(0))));
    }

    #[test]
    fn big_diffs_open_in_the_outline_on_first_load_only() {
        let mut app = test_app_with_files(&[]);
        app.layout.content_width = 80;
        app.layout.content_height = 20;
        let many: Vec<FileDiff> = (0..super::OUTLINE_AUTO_OPEN_FILES)
            .map(|i| file_with_hunks(&format!("src/f{i}.rs"), 1))
            .collect();
        app.apply_diff_result(0, Ok(many.clone()));
        assert!(
            app.outline.is_some(),
            "first load of a big diff shows the outline"
        );

        app.close_outline();
        app.apply_diff_result(0, Ok(many));
        assert!(
            app.outline.is_none(),
            "later refreshes respect the user's choice"
        );
    }

    #[test]
    fn large_new_files_start_collapsed_and_stay_as_the_user_left_them() {
        let mut app = test_app_with_files(&[]);
        app.layout.content_width = 80;
        app.layout.content_height = 20;
        let mut big = file_with_hunks("new.rs", 1);
        big.status = FileStatus::Untracked;
        big.hunks[0].lines = (0..super::LARGE_WHOLE_FILE_LINES as u32 + 5)
            .map(|n| DiffLine {
                kind: LineKind::Addition,
                content: format!("line {n}"),
                old_lineno: None,
                new_lineno: Some(n + 1),
            })
            .collect();
        let mut small = file_with_hunks("small.rs", 1);
        small.status = FileStatus::Added;
        app.apply_diff_result(0, Ok(vec![big.clone(), small.clone()]));
        assert!(app.repos[0].files[0].collapsed);
        assert!(!app.repos[0].files[1].collapsed);

        app.toggle_collapsed(0);
        app.apply_diff_result(0, Ok(vec![big, small]));
        assert!(
            !app.repos[0].files[0].collapsed,
            "explicit expand survives a refresh"
        );
    }

    #[test]
    fn refresh_gate_coalesces_work() {
        let mut gate = RefreshGate::default();
        assert!(gate.request());
        assert!(!gate.request());
        assert!(gate.complete());
        assert!(gate.request());
    }

    #[test]
    fn pending_refresh_drops_the_old_result() {
        let mut app = test_app_with_files(&["old.rs"]);
        let repo_id = app.repos[0].id;
        let gate = app.diff_refreshes.get_mut(&repo_id).unwrap();
        assert!(gate.request());
        assert!(!gate.request());
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let mut stale = test_app_with_files(&["stale.rs"]);
        let result = DiffResult {
            repo_id: app.repos[0].id,
            mode: app.repos[0].mode.clone(),
            result: Ok(stale.repos.remove(0).files),
        };

        assert!(!app.apply_diff_refresh_result(result, &tx));
        assert_eq!(app.repos[0].files[0].path, "old.rs");
    }

    #[test]
    fn active_layout_rebuilds_when_content_width_changes() {
        let mut app = test_app_with_files(&[]);
        let mut file = file_with_hunk("src/main.rs", 1);
        file.hunks[0].lines[0].content = "word ".repeat(30);
        app.repos[0].files = vec![file];

        app.layout.content_width = 80;
        app.prepare_active_layout();
        let wide_total = app.current_layout().unwrap().total_lines();

        app.layout.content_width = 20;
        app.prepare_active_layout();
        let narrow_total = app.current_layout().unwrap().total_lines();

        assert!(narrow_total > wide_total);
    }

    #[test]
    fn leaving_side_by_side_view_releases_its_cache() {
        let mut app = test_app_with_files(&["src/main.rs"]);

        app.toggle_view();
        assert!(app.repos[0].files[0].sbs_cache.is_some());

        app.toggle_view();
        assert!(app.repos[0].files[0].sbs_cache.is_none());
        assert!(app.repos[0].sbs_layout.is_none());
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
