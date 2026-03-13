mod keys;
mod mouse;

use crate::diff::{DiffLine, FileDiff, LineKind};
use crate::git::{self, DiffMode, RepoInfo};
use crate::highlight::Highlighter;
use crate::ui::{self, LayoutHints};
use crate::watcher::{self, WatchEvent};
use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

// Tunable constants
const FLASH_DURATION: Duration = Duration::from_millis(300);
const FLASH_TICK: Duration = Duration::from_millis(50);
const IDLE_TICK: Duration = Duration::from_secs(60);
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
}

pub struct App {
    pub repos: Vec<RepoState>,
    next_repo_id: u64,
    pub active_tab: usize,
    pub scroll_offset: usize,
    pub focused_file: Option<usize>,
    pub side_by_side: bool,
    pub last_error: Option<String>,
    pub flash_until: Option<Instant>,
    pub flash_file_idx: Option<usize>,
    pub flash_hunk_idx: Option<usize>,
    pub show_help: bool,
    pub show_file_picker: bool,
    pub file_picker_query: String,
    pub file_picker_selected: usize,
    pub show_repo_adder: bool,
    pub repo_adder_query: String,
    pub repo_adder_error: Option<String>,
    pub repo_adder_results: Vec<(String, PathBuf)>,
    pub repo_adder_cursor: usize,
    pub repo_adder_checked: std::collections::HashSet<usize>,
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
            })
            .collect();
        let next_repo_id = repos.len() as u64;
        Self {
            repos,
            next_repo_id,
            active_tab: 0,
            scroll_offset: 0,
            focused_file: None,
            side_by_side: false,
            last_error: None,
            flash_until: None,
            flash_file_idx: None,
            flash_hunk_idx: None,
            show_help: false,
            show_file_picker: false,
            file_picker_query: String::new(),
            file_picker_selected: 0,
            show_repo_adder: false,
            repo_adder_query: String::new(),
            repo_adder_error: None,
            repo_adder_results: Vec::new(),
            repo_adder_cursor: 0,
            repo_adder_checked: std::collections::HashSet::new(),
            layout: LayoutHints::default(),
            last_click: None,
        }
    }

    pub fn current_mode(&self) -> DiffMode {
        self.repos[self.active_tab].mode
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

    /// Returns indices of files matching the picker query (case-insensitive fuzzy substring).
    pub fn filtered_file_indices(&self) -> Vec<usize> {
        let files = match self.current_files() {
            Some(f) => f,
            None => return Vec::new(),
        };
        let query = self.file_picker_query.to_lowercase();
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
        let positions = self.file_header_positions();
        if let Some(&pos) = positions.get(file_idx) {
            self.scroll_offset = pos;
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
        if self.side_by_side {
            file.ensure_sbs_cache();
        }
    }

    pub fn refresh_repo_adder_results(&mut self) {
        let input = &self.repo_adder_query;
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
        self.repo_adder_results = results;
        self.repo_adder_cursor = 0;
        self.repo_adder_checked.clear();
        self.repo_adder_error = None;
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
        });
        self.active_tab = idx;
        self.scroll_offset = 0;
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
        self.current_files()
            .map(|files| files.iter().map(|f| self.file_display_lines(f) + 1).sum())
            .unwrap_or(0)
    }

    pub fn file_header_positions(&self) -> Vec<usize> {
        let mut positions = Vec::new();
        let mut pos = 0;
        if let Some(files) = self.current_files() {
            for file in files {
                positions.push(pos);
                pos += self.file_display_lines(file) + 1;
            }
        }
        positions
    }

    /// Returns the display line count for a file, accounting for SBS mode.
    fn file_display_lines(&self, file: &FileDiff) -> usize {
        if self.side_by_side {
            file.total_sbs_display_lines()
        } else {
            file.total_display_lines()
        }
    }

    pub fn focused_file_from_scroll(&self) -> Option<usize> {
        let positions = self.file_header_positions();
        let mut result = None;
        for (i, &pos) in positions.iter().enumerate() {
            if pos <= self.scroll_offset {
                result = Some(i);
            } else {
                break;
            }
        }
        result
    }

    pub fn is_hunk_flashing(&self, file_idx: usize, hunk_idx: usize) -> bool {
        if let Some(until) = self.flash_until {
            Instant::now() < until
                && self.flash_file_idx == Some(file_idx)
                && self.flash_hunk_idx == Some(hunk_idx)
        } else {
            false
        }
    }

    pub fn file_and_hunk_at_row(&self, content_row: usize) -> Option<(usize, usize)> {
        let positions = self.file_header_positions();
        let files = self.current_files()?;

        let mut file_idx = None;
        for (i, &pos) in positions.iter().enumerate() {
            if content_row >= pos {
                file_idx = Some(i);
            } else {
                break;
            }
        }
        let file_idx = file_idx?;
        let file = files.get(file_idx)?;
        if file.collapsed || file.hunks.is_empty() {
            return None;
        }

        let file_start = positions[file_idx];
        let row_in_file = content_row.saturating_sub(file_start + 1);

        let mut line_count = 0;
        for (i, hunk) in file.hunks.iter().enumerate() {
            let hunk_lines = if self.side_by_side {
                file.sbs_cache
                    .as_ref()
                    .and_then(|c| c.get(i))
                    .map(|h| h.len())
                    .unwrap_or(hunk.lines.len())
            } else {
                hunk.lines.len()
            };
            let hunk_size = 1 + hunk_lines;
            if row_in_file < line_count + hunk_size {
                return Some((file_idx, i));
            }
            line_count += hunk_size;
        }

        Some((file_idx, file.hunks.len().saturating_sub(1)))
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

        let file_start = self.file_header_positions().get(file_idx).copied()?;
        let relative_scroll = self.scroll_offset.saturating_sub(file_start + 1);
        let mut line_count = 0;
        let mut target_hunk = 0;

        for (i, hunk) in file.hunks.iter().enumerate() {
            let hunk_lines = hunk.lines.len() + 1;
            if line_count + hunk_lines > relative_scroll {
                target_hunk = i;
                break;
            }
            line_count += hunk_lines;
            target_hunk = i;
        }

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

        self.flash_until = Some(Instant::now() + FLASH_DURATION);
        self.flash_file_idx = Some(file_idx);
        self.flash_hunk_idx = Some(target_hunk);

        Some(result)
    }
}

// -- Event loop and terminal management --

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

enum AppEvent {
    Terminal(Event),
    FileChange(WatchEvent),
    DiffDone(DiffResult),
    BaseBranch(BaseBranchResult),
    Tick,
}

/// Bundles the event channels used by the run loop.
struct Channels {
    watch_rx: mpsc::UnboundedReceiver<WatchEvent>,
    diff_rx: mpsc::UnboundedReceiver<DiffResult>,
    diff_tx: mpsc::UnboundedSender<DiffResult>,
    base_rx: mpsc::UnboundedReceiver<BaseBranchResult>,
    base_tx: mpsc::UnboundedSender<BaseBranchResult>,
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Show);
}

pub async fn run(path: PathBuf) -> Result<()> {
    let repo_infos = git::discover_repos(&path)?;

    let mut app = App::new(repo_infos);
    app.refresh_all_sync();

    // Set up file watcher with shared repo paths
    let (watch_tx, watch_rx) = mpsc::unbounded_channel::<WatchEvent>();
    let repo_paths: Vec<PathBuf> = app.repos.iter().map(|r| r.info.path.clone()).collect();
    let shared_paths = Arc::new(RwLock::new(repo_paths));
    let mut debouncer = watcher::start_watching(shared_paths.clone(), watch_tx)?;
    for p in shared_paths
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
    {
        watcher::watch_repo(&mut debouncer, p)?;
    }

    // Panic hook to restore terminal on crash
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let highlighter = Highlighter::new();

    let (diff_tx, diff_rx) = mpsc::unbounded_channel::<DiffResult>();
    let (base_tx, base_rx) = mpsc::unbounded_channel::<BaseBranchResult>();

    // Resolve base branches + branch names in background at startup
    for repo in app.repos.iter() {
        let id = repo.id;
        let path = repo.info.path.clone();
        let tx = base_tx.clone();
        std::thread::spawn(move || {
            let branch = git::find_base_branch(&path);
            let branch_name = git::current_branch(&path);
            let _ = tx.send(BaseBranchResult {
                repo_id: id,
                branch,
                branch_name,
            });
        });
    }

    let mut channels = Channels {
        watch_rx,
        diff_rx,
        diff_tx,
        base_rx,
        base_tx,
    };

    let result = run_loop(
        &mut terminal,
        &mut app,
        &mut channels,
        &highlighter,
        &mut debouncer,
        &shared_paths,
    )
    .await;

    restore_terminal();

    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    ch: &mut Channels,
    highlighter: &Highlighter,
    debouncer: &mut notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    shared_paths: &Arc<RwLock<Vec<PathBuf>>>,
) -> Result<()> {
    let (term_tx, mut term_rx) = mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(ev) => {
                    if term_tx.send(ev).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });

    let mut needs_redraw = true;

    loop {
        if needs_redraw {
            app.ensure_sbs_caches();
            let mut hints = LayoutHints::default();
            terminal.draw(|f| ui::draw(f, app, highlighter, &mut hints))?;
            app.layout = hints;
            needs_redraw = false;
        }

        let tick_dur = if app.flash_until.is_some() {
            FLASH_TICK
        } else {
            IDLE_TICK
        };
        let tick_sleep = tokio::time::sleep(tick_dur);
        tokio::pin!(tick_sleep);

        let event = tokio::select! {
            Some(ev) = term_rx.recv() => AppEvent::Terminal(ev),
            Some(ev) = ch.watch_rx.recv() => AppEvent::FileChange(ev),
            Some(ev) = ch.diff_rx.recv() => AppEvent::DiffDone(ev),
            Some(ev) = ch.base_rx.recv() => AppEvent::BaseBranch(ev),
            () = &mut tick_sleep => AppEvent::Tick,
        };

        match event {
            AppEvent::Terminal(Event::Key(key)) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.show_file_picker {
                    keys::handle_file_picker_key(app, key);
                    needs_redraw = true;
                    continue;
                }
                if app.show_repo_adder {
                    let added = keys::handle_repo_adder_key(app, key);
                    for new_idx in added {
                        let repo = &app.repos[new_idx];
                        let id = repo.id;
                        let path = repo.info.path.clone();
                        shared_paths
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(path.clone());
                        let _ = watcher::watch_repo(debouncer, &path);
                        app.refresh_repo_async(new_idx, &ch.diff_tx);
                        let btx = ch.base_tx.clone();
                        std::thread::spawn(move || {
                            let branch = git::find_base_branch(&path);
                            let branch_name = git::current_branch(&path);
                            let _ = btx.send(BaseBranchResult {
                                repo_id: id,
                                branch,
                                branch_name,
                            });
                        });
                    }
                    needs_redraw = true;
                    continue;
                }
                // Remove current tab
                if key.code == KeyCode::Char('x') && app.repos.len() > 1 {
                    let idx = app.active_tab;
                    let path = app.repos[idx].info.path.clone();

                    app.repos.remove(idx);

                    if app.active_tab >= app.repos.len() {
                        app.active_tab = app.repos.len() - 1;
                    }
                    app.scroll_offset = 0;
                    app.focused_file = None;

                    let _ = debouncer.watcher().unwatch(&path);
                    if let Ok(mut paths) = shared_paths.write() {
                        paths.retain(|p| *p != path);
                    }

                    needs_redraw = true;
                    continue;
                }
                if keys::handle_key(app, key, &ch.diff_tx) {
                    return Ok(());
                }
                needs_redraw = true;
            }
            AppEvent::Terminal(Event::Mouse(m)) => {
                if mouse::handle_mouse(app, m, &ch.diff_tx) {
                    needs_redraw = true;
                }
            }
            AppEvent::Terminal(Event::Resize(_, _)) => {
                needs_redraw = true;
            }
            AppEvent::Terminal(_) => {}
            AppEvent::FileChange(event) => {
                if let Some(idx) = app
                    .repos
                    .iter()
                    .position(|r| r.info.path == event.repo_path)
                {
                    app.refresh_repo_async(idx, &ch.diff_tx);
                    let id = app.repos[idx].id;
                    let path = event.repo_path;
                    let btx = ch.base_tx.clone();
                    std::thread::spawn(move || {
                        let branch_name = git::current_branch(&path);
                        let base = git::find_base_branch(&path);
                        let _ = btx.send(BaseBranchResult {
                            repo_id: id,
                            branch: base,
                            branch_name,
                        });
                    });
                }
            }
            AppEvent::DiffDone(result) => {
                if let Some(idx) = app.find_repo(result.repo_id)
                    && result.mode == app.repos[idx].mode
                {
                    app.apply_diff_result(idx, result.result);
                    needs_redraw = true;
                }
            }
            AppEvent::BaseBranch(result) => {
                if let Some(idx) = app.find_repo(result.repo_id) {
                    let base_changed = app.repos[idx].base_branch != result.branch;
                    app.repos[idx].base_branch = result.branch;
                    app.repos[idx].branch_name = result.branch_name;
                    needs_redraw = true;
                    if base_changed && app.repos[idx].mode == DiffMode::Branch {
                        app.refresh_repo_async(idx, &ch.diff_tx);
                    }
                }
            }
            AppEvent::Tick => {
                if let Some(until) = app.flash_until
                    && Instant::now() >= until
                {
                    app.flash_until = None;
                    app.flash_file_idx = None;
                    app.flash_hunk_idx = None;
                    needs_redraw = true;
                }
            }
        }
    }
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

    #[test]
    fn fuzzy_match_exact_match() {
        let mut app = test_app_with_files(&["src/main.rs"]);
        app.file_picker_query = "src/main.rs".to_string();
        assert_eq!(app.filtered_file_indices(), vec![0]);
    }

    #[test]
    fn fuzzy_match_subsequence() {
        let mut app = test_app_with_files(&["src/main.rs"]);
        app.file_picker_query = "smr".to_string();
        assert_eq!(app.filtered_file_indices(), vec![0]);
    }

    #[test]
    fn fuzzy_match_no_match() {
        let mut app = test_app_with_files(&["src/main.rs"]);
        app.file_picker_query = "xyz".to_string();
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
        app.file_picker_query = "main".to_string();
        assert_eq!(app.filtered_file_indices(), vec![0]);
    }

    #[test]
    fn fuzzy_match_query_longer_than_path() {
        let mut app = test_app_with_files(&["ab"]);
        app.file_picker_query = "abc".to_string();
        assert!(app.filtered_file_indices().is_empty());
    }
}
