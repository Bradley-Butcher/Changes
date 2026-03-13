use crate::diff::{self, DiffLine, FileDiff, LineKind};
use crate::git::{self, DiffMode, RepoInfo};
use crate::highlight::Highlighter;
use crate::ui;
use crate::watcher::{self, WatchEvent};
use anyhow::Result;
use arboard::Clipboard;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub struct App {
    pub repos: Vec<RepoInfo>,
    pub active_tab: usize,
    pub diff_modes: Vec<DiffMode>,
    pub file_diffs: Vec<Vec<FileDiff>>,
    pub base_branches: Vec<Option<String>>,
    pub branch_names: Vec<Option<String>>,
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
    pub repo_adder_results: Vec<(String, PathBuf)>, // (display name, canonical path)
    pub repo_adder_cursor: usize,
    pub repo_adder_checked: std::collections::HashSet<usize>,
    pub tab_positions: Vec<(u16, u16)>, // (start_col, end_col) for each tab
    pub last_click: Option<(u16, u16, Instant)>, // (row, col, time) for double-click detection
    pub mode_badge_pos: (u16, u16),  // (start_col, end_col) on status bar row
    pub view_badge_pos: (u16, u16),  // (start_col, end_col) on status bar row
    pub status_bar_row: u16,
}

impl App {
    pub fn new(repos: Vec<RepoInfo>) -> Self {
        let n = repos.len();
        Self {
            repos,
            active_tab: 0,
            diff_modes: vec![DiffMode::Unstaged; n],
            file_diffs: vec![Vec::new(); n],
            base_branches: vec![None; n],
            branch_names: vec![None; n],
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
            tab_positions: Vec::new(),
            last_click: None,
            mode_badge_pos: (0, 0),
            view_badge_pos: (0, 0),
            status_bar_row: 0,
        }
    }

    pub fn current_mode(&self) -> DiffMode {
        self.diff_modes[self.active_tab]
    }

    pub fn current_files(&self) -> Option<&Vec<FileDiff>> {
        self.file_diffs.get(self.active_tab)
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
                // Fuzzy: all query chars must appear in order
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

    /// Jump scroll to put the given file index at the top of the viewport.
    pub fn jump_to_file(&mut self, file_idx: usize) {
        let positions = self.file_header_positions();
        if let Some(&pos) = positions.get(file_idx) {
            self.scroll_offset = pos;
            self.focused_file = Some(file_idx);
        }
    }

    /// Expand context around a gap.
    /// gap_idx 0 = before first hunk, 1..n = between hunks, n = after last hunk.
    pub fn expand_gap(&mut self, file_idx: usize, gap_idx: usize) {
        let repo_path = self.repos[self.active_tab].path.clone();
        let files = match self.file_diffs.get_mut(self.active_tab) {
            Some(f) => f,
            None => return,
        };
        let file = match files.get_mut(file_idx) {
            Some(f) => f,
            None => return,
        };
        if file.hunks.is_empty() {
            return;
        }

        let file_path = repo_path.join(&file.path);
        let content = match std::fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(_) => return,
        };
        let file_lines: Vec<&str> = content.lines().collect();

        const EXPAND_AMOUNT: usize = 20;

        if gap_idx == 0 {
            // Expand above first hunk — prepend context lines
            let first_new = file.hunks[0].first_new_lineno().unwrap_or(1) as usize;
            let first_old = file.hunks[0].first_old_lineno().unwrap_or(1) as usize;
            if first_new <= 1 {
                return;
            }
            let old_offset = first_old as i64 - first_new as i64;
            let gap_end = first_new - 1;
            let gap_start = gap_end.saturating_sub(EXPAND_AMOUNT - 1).max(1);

            let mut new_lines = Vec::new();
            for i in gap_start..=gap_end {
                let line_content = file_lines.get(i - 1).map(|s| s.to_string()).unwrap_or_default();
                new_lines.push(DiffLine {
                    kind: LineKind::Context,
                    content: line_content,
                    old_lineno: Some((i as i64 + old_offset) as u32),
                    new_lineno: Some(i as u32),
                });
            }
            let mut existing = std::mem::take(&mut file.hunks[0].lines);
            new_lines.append(&mut existing);
            file.hunks[0].lines = new_lines;
        } else if gap_idx < file.hunks.len() {
            // Expand between hunks — append to previous hunk
            let prev_idx = gap_idx - 1;
            let prev_last_new = file.hunks[prev_idx].last_new_lineno().unwrap_or(0) as usize;
            let prev_last_old = file.hunks[prev_idx].last_old_lineno().unwrap_or(0) as usize;
            let next_first_new = file.hunks[gap_idx].first_new_lineno().unwrap_or(0) as usize;

            if prev_last_new >= next_first_new.saturating_sub(1) {
                return;
            }

            let gap_start = prev_last_new + 1;
            let gap_end = (next_first_new - 1).min(gap_start + EXPAND_AMOUNT - 1);
            let old_offset = prev_last_old as i64 - prev_last_new as i64;

            let prev_hunk = &mut file.hunks[prev_idx];
            for i in gap_start..=gap_end {
                let line_content = file_lines.get(i - 1).map(|s| s.to_string()).unwrap_or_default();
                prev_hunk.lines.push(DiffLine {
                    kind: LineKind::Context,
                    content: line_content,
                    old_lineno: Some((i as i64 + old_offset) as u32),
                    new_lineno: Some(i as u32),
                });
            }
        } else {
            // Expand below last hunk — append to last hunk
            let last_idx = file.hunks.len() - 1;
            let last_new = file.hunks[last_idx].last_new_lineno().unwrap_or(0) as usize;
            let last_old = file.hunks[last_idx].last_old_lineno().unwrap_or(0) as usize;

            if last_new >= file_lines.len() {
                return;
            }

            let gap_start = last_new + 1;
            let gap_end = file_lines.len().min(gap_start + EXPAND_AMOUNT - 1);
            let old_offset = last_old as i64 - last_new as i64;

            let last_hunk = &mut file.hunks[last_idx];
            for i in gap_start..=gap_end {
                let line_content = file_lines.get(i - 1).map(|s| s.to_string()).unwrap_or_default();
                last_hunk.lines.push(DiffLine {
                    kind: LineKind::Context,
                    content: line_content,
                    old_lineno: Some((i as i64 + old_offset) as u32),
                    new_lineno: Some(i as u32),
                });
            }
        }

        // Invalidate SBS cache
        file.sbs_cache = None;
        file.ensure_sbs_cache();
    }

    /// Scan the directory in repo_adder_query for git repos and populate results.
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
            self.repos.iter().map(|r| r.path.clone()).collect();

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

    /// Validate a path and add it as a new repo tab.
    /// Returns the new repo index, or an error message.
    pub fn add_repo(&mut self, input: &str) -> Result<usize, String> {
        let path = Path::new(input);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| e.to_string())?
                .join(path)
        };
        let canonical = resolved.canonicalize().map_err(|_| format!("Path not found: {}", input))?;

        if !canonical.join(".git").exists() {
            return Err(format!("Not a git repo: {}", canonical.display()));
        }

        // Check for duplicates
        if self.repos.iter().any(|r| r.path == canonical) {
            return Err("Repo already added".to_string());
        }

        let name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string());

        let idx = self.repos.len();
        self.repos.push(git::RepoInfo { name, path: canonical });
        self.diff_modes.push(DiffMode::Unstaged);
        self.file_diffs.push(Vec::new());
        self.base_branches.push(None);
        self.branch_names.push(None);
        self.active_tab = idx;
        self.scroll_offset = 0;
        self.focused_file = None;

        Ok(idx)
    }

    pub fn current_files_mut(&mut self) -> Option<&mut Vec<FileDiff>> {
        self.file_diffs.get_mut(self.active_tab)
    }

    pub fn apply_diff_result(&mut self, idx: usize, result: Result<Vec<FileDiff>, String>) {
        match result {
            Ok(files) => {
                let old_collapsed: std::collections::HashMap<String, bool> = self
                    .file_diffs
                    .get(idx)
                    .map(|files| {
                        files
                            .iter()
                            .map(|f| (f.path.clone(), f.collapsed))
                            .collect()
                    })
                    .unwrap_or_default();

                let mut new_files = files;
                for file in &mut new_files {
                    if let Some(&collapsed) = old_collapsed.get(&file.path) {
                        file.collapsed = collapsed;
                    }
                    file.ensure_sbs_cache();
                }

                self.file_diffs[idx] = new_files;
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("{}: {}", self.repos[idx].name, e));
            }
        }
    }

    /// Spawn a background diff computation. Results arrive via diff_tx.
    pub fn refresh_repo_async(&self, idx: usize, diff_tx: &mpsc::UnboundedSender<DiffResult>) {
        let path = self.repos[idx].path.clone();
        let mode = self.diff_modes[idx];
        let base = self.base_branches[idx].clone();
        let tx = diff_tx.clone();
        std::thread::spawn(move || {
            let result = git::compute_diff(&path, mode, base.as_deref())
                .map_err(|e| e.to_string());
            let _ = tx.send(DiffResult { repo_index: idx, mode, result });
        });
    }

    /// Synchronous refresh for initial load (before the event loop starts).
    pub fn refresh_repo_sync(&mut self, idx: usize) {
        let mode = self.diff_modes[idx];
        let base = self.base_branches[idx].clone();
        let result = git::compute_diff(&self.repos[idx].path, mode, base.as_deref())
            .map_err(|e| e.to_string());
        self.apply_diff_result(idx, result);
    }

    pub fn refresh_all_sync(&mut self) {
        for i in 0..self.repos.len() {
            self.refresh_repo_sync(i);
        }
    }

    pub fn total_display_lines(&self) -> usize {
        self.current_files()
            .map(|files| files.iter().map(|f| f.total_display_lines() + 1).sum()) // +1 for blank separator
            .unwrap_or(0)
    }

    pub fn file_header_positions(&self) -> Vec<usize> {
        let mut positions = Vec::new();
        let mut pos = 0;
        if let Some(files) = self.current_files() {
            for file in files {
                positions.push(pos);
                pos += file.total_display_lines() + 1; // +1 blank separator
            }
        }
        positions
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

    /// Find which file and hunk a given content row falls in.
    /// content_row is the absolute row in the scrollable content (scroll_offset + screen row).
    pub fn file_and_hunk_at_row(&self, content_row: usize) -> Option<(usize, usize)> {
        let positions = self.file_header_positions();
        let files = self.current_files()?;

        // Find which file this row is in
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
        let row_in_file = content_row.saturating_sub(file_start + 1); // skip file header

        // Walk hunks to find which one
        let mut line_count = 0;
        for (i, hunk) in file.hunks.iter().enumerate() {
            let hunk_size = 1 + hunk.lines.len(); // hunk header + lines
            if row_in_file < line_count + hunk_size {
                return Some((file_idx, i));
            }
            line_count += hunk_size;
        }

        // Past the last hunk — return last hunk
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

        // Find hunk closest to scroll position
        let file_start = self.file_header_positions().get(file_idx).copied()?;
        let relative_scroll = self.scroll_offset.saturating_sub(file_start + 1); // skip header
        let mut line_count = 0;
        let mut target_hunk = 0;

        for (i, hunk) in file.hunks.iter().enumerate() {
            let hunk_lines = hunk.lines.len() + 1; // +1 for hunk header
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
                crate::diff::LineKind::Addition => "+",
                crate::diff::LineKind::Deletion => "-",
                crate::diff::LineKind::Context => " ",
                _ => " ",
            };
            result.push_str(&format!("{} {}\n", prefix, line.content));
        }

        // Set flash
        self.flash_until = Some(Instant::now() + Duration::from_millis(300));
        self.flash_file_idx = Some(file_idx);
        self.flash_hunk_idx = Some(target_hunk);

        Some(result)
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Show);
}

pub async fn run(path: PathBuf) -> Result<()> {
    let repos = git::discover_repos(&path)?;

    let mut app = App::new(repos);
    app.refresh_all_sync();

    // Set up file watcher with shared repo paths
    let (watch_tx, mut watch_rx) = mpsc::unbounded_channel::<WatchEvent>();
    let repo_paths: Vec<PathBuf> = app.repos.iter().map(|r| r.path.clone()).collect();
    let shared_paths = Arc::new(RwLock::new(repo_paths));
    let mut debouncer = watcher::start_watching(shared_paths.clone(), watch_tx)?;
    for p in shared_paths.read().unwrap().iter() {
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
    for (i, repo) in app.repos.iter().enumerate() {
        let path = repo.path.clone();
        let tx = base_tx.clone();
        std::thread::spawn(move || {
            let branch = git::find_base_branch(&path);
            let branch_name = git::current_branch(&path);
            let _ = tx.send(BaseBranchResult { repo_index: i, branch, branch_name });
        });
    }

    let result = run_loop(
        &mut terminal, &mut app, &mut watch_rx, diff_rx, &diff_tx,
        base_rx, &base_tx, &highlighter, &mut debouncer, &shared_paths,
    ).await;

    // Restore terminal
    restore_terminal();

    result
}

pub struct DiffResult {
    pub repo_index: usize,
    pub mode: DiffMode,
    pub result: Result<Vec<FileDiff>, String>,
}

pub struct BaseBranchResult {
    pub repo_index: usize,
    pub branch: String,
    pub branch_name: Option<String>,
}

enum AppEvent {
    Terminal(Event),
    FileChange(WatchEvent),
    DiffDone(DiffResult),
    BaseBranch(BaseBranchResult),
    Tick,
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    watch_rx: &mut mpsc::UnboundedReceiver<WatchEvent>,
    mut diff_rx: mpsc::UnboundedReceiver<DiffResult>,
    diff_tx: &mpsc::UnboundedSender<DiffResult>,
    mut base_rx: mpsc::UnboundedReceiver<BaseBranchResult>,
    base_tx: &mpsc::UnboundedSender<BaseBranchResult>,
    highlighter: &Highlighter,
    debouncer: &mut notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    shared_paths: &Arc<RwLock<Vec<PathBuf>>>,
) -> Result<()> {
    // Bridge crossterm events to a channel from a dedicated thread.
    // event::read() blocks until an event is available — simple and reliable.
    let (term_tx, mut term_rx) = mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(ev) => {
                    if term_tx.send(ev).is_err() {
                        return; // receiver dropped, app is shutting down
                    }
                }
                Err(_) => return,
            }
        }
    });

    // Only redraw when state actually changes. Flash expiry is the only
    // time-based redraw; use a long tick so we're idle otherwise.
    let mut needs_redraw = true;

    loop {
        if needs_redraw {
            terminal.draw(|f| ui::draw(f, app, highlighter))?;
            needs_redraw = false;
        }

        // Pick a tick duration: short if a flash is active (need to clear it),
        // long otherwise (just a keepalive, almost never fires).
        let tick_dur = if app.flash_until.is_some() {
            Duration::from_millis(50)
        } else {
            Duration::from_secs(60)
        };
        let tick_sleep = tokio::time::sleep(tick_dur);
        tokio::pin!(tick_sleep);

        // Wait for next event
        let event = tokio::select! {
            Some(ev) = term_rx.recv() => AppEvent::Terminal(ev),
            Some(ev) = watch_rx.recv() => AppEvent::FileChange(ev),
            Some(ev) = diff_rx.recv() => AppEvent::DiffDone(ev),
            Some(ev) = base_rx.recv() => AppEvent::BaseBranch(ev),
            () = &mut tick_sleep => AppEvent::Tick,
        };

        match event {
            AppEvent::Terminal(Event::Key(key)) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.show_file_picker {
                    handle_file_picker_key(app, key);
                    needs_redraw = true;
                    continue;
                }
                if app.show_repo_adder {
                    let added = handle_repo_adder_key(app, key);
                    for new_idx in added {
                        let path = app.repos[new_idx].path.clone();
                        shared_paths.write().unwrap().push(path.clone());
                        let _ = watcher::watch_repo(debouncer, &path);
                        app.refresh_repo_async(new_idx, diff_tx);
                        let btx = base_tx.clone();
                        std::thread::spawn(move || {
                            let branch = git::find_base_branch(&path);
                            let branch_name = git::current_branch(&path);
                            let _ = btx.send(BaseBranchResult { repo_index: new_idx, branch, branch_name });
                        });
                    }
                    needs_redraw = true;
                    continue;
                }
                // Remove current tab
                if key.code == KeyCode::Char('x') && app.repos.len() > 1 {
                    let idx = app.active_tab;
                    let path = app.repos[idx].path.clone();

                    app.repos.remove(idx);
                    app.diff_modes.remove(idx);
                    app.file_diffs.remove(idx);
                    app.base_branches.remove(idx);
                    app.branch_names.remove(idx);

                    if app.active_tab >= app.repos.len() {
                        app.active_tab = app.repos.len() - 1;
                    }
                    app.scroll_offset = 0;
                    app.focused_file = None;

                    // Stop watching and remove from shared paths
                    let _ = debouncer.watcher().unwatch(&path);
                    if let Ok(mut paths) = shared_paths.write() {
                        paths.retain(|p| *p != path);
                    }

                    needs_redraw = true;
                    continue;
                }
                if handle_key(app, key, diff_tx) {
                    return Ok(());
                }
                needs_redraw = true;
            }
            AppEvent::Terminal(Event::Mouse(mouse)) => {
                if handle_mouse(app, mouse, diff_tx) {
                    needs_redraw = true;
                }
            }
            AppEvent::Terminal(Event::Resize(_, _)) => {
                needs_redraw = true;
            }
            AppEvent::Terminal(_) => {}
            AppEvent::FileChange(event) => {
                app.refresh_repo_async(event.repo_index, diff_tx);
                // Re-resolve branch names + base branch in background
                let path = app.repos[event.repo_index].path.clone();
                let idx = event.repo_index;
                let btx = base_tx.clone();
                std::thread::spawn(move || {
                    let branch_name = git::current_branch(&path);
                    let base = git::find_base_branch(&path);
                    // Send both via base_tx — branch name piggybacks
                    let _ = btx.send(BaseBranchResult { repo_index: idx, branch: base, branch_name });
                });
            }
            AppEvent::DiffDone(result) => {
                if result.mode == app.diff_modes[result.repo_index] {
                    app.apply_diff_result(result.repo_index, result.result);
                    needs_redraw = true;
                }
            }
            AppEvent::BaseBranch(result) => {
                let base_changed = app.base_branches[result.repo_index].as_deref() != Some(&result.branch);
                app.base_branches[result.repo_index] = Some(result.branch);
                app.branch_names[result.repo_index] = result.branch_name;
                needs_redraw = true;
                if base_changed && app.diff_modes[result.repo_index] == DiffMode::Branch {
                    app.refresh_repo_async(result.repo_index, diff_tx);
                }
            }
            AppEvent::Tick => {
                if let Some(until) = app.flash_until {
                    if Instant::now() >= until {
                        app.flash_until = None;
                        app.flash_file_idx = None;
                        app.flash_hunk_idx = None;
                        needs_redraw = true;
                    }
                }
            }
        }
    }
}

fn handle_key(app: &mut App, key: event::KeyEvent, diff_tx: &mpsc::UnboundedSender<DiffResult>) -> bool {
    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Esc => {
            if app.show_help {
                app.show_help = false;
            } else {
                return true;
            }
        }
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
        }

        // Tab switching
        KeyCode::Tab => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                if app.active_tab == 0 {
                    app.active_tab = app.repos.len() - 1;
                } else {
                    app.active_tab -= 1;
                }
            } else {
                app.active_tab = (app.active_tab + 1) % app.repos.len();
            }
            app.scroll_offset = 0;
            app.focused_file = None;
        }
        KeyCode::Char(c) if c >= '1' && c <= '9' => {
            let idx = (c as usize) - ('1' as usize);
            if idx < app.repos.len() {
                app.active_tab = idx;
                app.scroll_offset = 0;
                app.focused_file = None;
            }
        }

        // Mode switching
        KeyCode::Char('u') => {
            app.diff_modes[app.active_tab] = DiffMode::Unstaged;
            app.refresh_repo_async(app.active_tab, diff_tx);
            app.scroll_offset = 0;
        }
        KeyCode::Char('s') => {
            app.diff_modes[app.active_tab] = DiffMode::Staged;
            app.refresh_repo_async(app.active_tab, diff_tx);
            app.scroll_offset = 0;
        }
        KeyCode::Char('b') => {
            app.diff_modes[app.active_tab] = DiffMode::Branch;
            app.refresh_repo_async(app.active_tab, diff_tx);
            app.scroll_offset = 0;
        }

        // View toggle
        KeyCode::Char('v') => {
            app.side_by_side = !app.side_by_side;
        }

        // Scrolling
        KeyCode::Char('j') | KeyCode::Down => {
            app.scroll_offset = app.scroll_offset.saturating_add(1);
            app.focused_file = app.focused_file_from_scroll();
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.scroll_offset = app.scroll_offset.saturating_sub(1);
            app.focused_file = app.focused_file_from_scroll();
        }
        KeyCode::Char('J') => {
            // Jump to next file
            let positions = app.file_header_positions();
            if let Some(next) = positions.iter().find(|&&p| p > app.scroll_offset) {
                app.scroll_offset = *next;
                app.focused_file = app.focused_file_from_scroll();
            }
        }
        KeyCode::Char('K') => {
            // Jump to previous file
            let positions = app.file_header_positions();
            if let Some(prev) = positions.iter().rev().find(|&&p| p < app.scroll_offset) {
                app.scroll_offset = *prev;
                app.focused_file = app.focused_file_from_scroll();
            }
        }
        KeyCode::Char('g') => {
            app.scroll_offset = 0;
            app.focused_file = Some(0);
        }
        KeyCode::Char('G') => {
            let total = app.total_display_lines();
            app.scroll_offset = total.saturating_sub(1);
            app.focused_file = app.focused_file_from_scroll();
        }
        KeyCode::PageDown => {
            app.scroll_offset = app.scroll_offset.saturating_add(20);
            app.focused_file = app.focused_file_from_scroll();
        }
        KeyCode::PageUp => {
            app.scroll_offset = app.scroll_offset.saturating_sub(20);
            app.focused_file = app.focused_file_from_scroll();
        }

        // Collapse
        KeyCode::Enter => {
            if let Some(idx) = app.focused_file {
                if let Some(files) = app.current_files_mut() {
                    if let Some(file) = files.get_mut(idx) {
                        file.collapsed = !file.collapsed;
                    }
                }
            }
        }
        KeyCode::Char('c') => {
            if let Some(files) = app.current_files_mut() {
                for file in files.iter_mut() {
                    file.collapsed = true;
                }
            }
        }
        KeyCode::Char('e') => {
            if let Some(files) = app.current_files_mut() {
                for file in files.iter_mut() {
                    file.collapsed = false;
                }
            }
        }

        // Copy
        KeyCode::Char('y') => {
            if let Some(text) = app.copy_hunk_at_focus() {
                if let Ok(mut clipboard) = Clipboard::new() {
                    let _ = clipboard.set_text(text);
                }
            }
        }

        // File picker
        KeyCode::Char('f') => {
            app.show_file_picker = true;
            app.file_picker_query.clear();
            app.file_picker_selected = 0;
        }

        // Add repo
        KeyCode::Char('a') => {
            app.show_repo_adder = true;
            app.repo_adder_query.clear();
            app.repo_adder_error = None;
            app.repo_adder_checked.clear();
            app.refresh_repo_adder_results();
        }

        _ => {}
    }
    false
}

fn handle_file_picker_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.show_file_picker = false;
        }
        KeyCode::Enter => {
            let filtered = app.filtered_file_indices();
            if let Some(&file_idx) = filtered.get(app.file_picker_selected) {
                app.jump_to_file(file_idx);
                // Expand the file if collapsed
                if let Some(files) = app.current_files_mut() {
                    if let Some(file) = files.get_mut(file_idx) {
                        file.collapsed = false;
                    }
                }
            }
            app.show_file_picker = false;
        }
        KeyCode::Up => {
            app.file_picker_selected = app.file_picker_selected.saturating_sub(1);
        }
        KeyCode::Down => {
            let max = app.filtered_file_indices().len().saturating_sub(1);
            app.file_picker_selected = (app.file_picker_selected + 1).min(max);
        }
        KeyCode::Backspace => {
            app.file_picker_query.pop();
            app.file_picker_selected = 0;
        }
        KeyCode::Char(c) => {
            app.file_picker_query.push(c);
            app.file_picker_selected = 0;
        }
        _ => {}
    }
}

/// Returns indices of newly added repos (empty if none).
fn handle_repo_adder_key(app: &mut App, key: event::KeyEvent) -> Vec<usize> {
    match key.code {
        KeyCode::Esc => {
            app.show_repo_adder = false;
        }
        KeyCode::Up => {
            app.repo_adder_cursor = app.repo_adder_cursor.saturating_sub(1);
        }
        KeyCode::Down => {
            let max = app.repo_adder_results.len().saturating_sub(1);
            app.repo_adder_cursor = (app.repo_adder_cursor + 1).min(max);
        }
        KeyCode::Char(' ') => {
            // Toggle check on current item
            let idx = app.repo_adder_cursor;
            if idx < app.repo_adder_results.len() {
                if app.repo_adder_checked.contains(&idx) {
                    app.repo_adder_checked.remove(&idx);
                } else {
                    app.repo_adder_checked.insert(idx);
                }
            }
        }
        KeyCode::Enter => {
            // Add checked repos, or just the cursor item if none checked
            let to_add: Vec<PathBuf> = if app.repo_adder_checked.is_empty() {
                if let Some((_, path)) = app.repo_adder_results.get(app.repo_adder_cursor) {
                    vec![path.clone()]
                } else {
                    return Vec::new();
                }
            } else {
                let mut indices: Vec<usize> = app.repo_adder_checked.iter().copied().collect();
                indices.sort();
                indices
                    .iter()
                    .filter_map(|&i| app.repo_adder_results.get(i).map(|(_, p)| p.clone()))
                    .collect()
            };

            let mut added = Vec::new();
            for path in to_add {
                let path_str = path.to_string_lossy().to_string();
                match app.add_repo(&path_str) {
                    Ok(idx) => added.push(idx),
                    Err(e) => {
                        app.repo_adder_error = Some(e);
                    }
                }
            }
            if !added.is_empty() {
                app.show_repo_adder = false;
            }
            return added;
        }
        KeyCode::Backspace => {
            app.repo_adder_query.pop();
            app.refresh_repo_adder_results();
        }
        KeyCode::Char(c) => {
            app.repo_adder_query.push(c);
            app.refresh_repo_adder_results();
        }
        _ => {}
    }
    Vec::new()
}

/// Returns true if the event changed state (needs redraw).
fn handle_mouse(app: &mut App, mouse: event::MouseEvent, diff_tx: &mpsc::UnboundedSender<DiffResult>) -> bool {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.scroll_offset = app.scroll_offset.saturating_sub(3);
            app.focused_file = app.focused_file_from_scroll();
            true
        }
        MouseEventKind::ScrollDown => {
            app.scroll_offset = app.scroll_offset.saturating_add(3);
            app.focused_file = app.focused_file_from_scroll();
            true
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let click_row = mouse.row;
            let click_col = mouse.column;
            let now = Instant::now();

            // Double-click detection (same position within 400ms)
            let is_double_click = if let Some((prev_row, prev_col, prev_time)) = app.last_click {
                prev_row == click_row
                    && prev_col == click_col
                    && now.duration_since(prev_time) < Duration::from_millis(400)
            } else {
                false
            };
            app.last_click = Some((click_row, click_col, now));

            if is_double_click && click_row >= 4 {
                let content_row = (click_row as usize).saturating_sub(4) + app.scroll_offset;
                if let Some(text) = app.copy_hunk_at_row(content_row) {
                    if let Ok(mut clipboard) = Clipboard::new() {
                        let _ = clipboard.set_text(text);
                    }
                }
                app.last_click = None;
                return true;
            }

            // Status bar badges
            if click_row == app.status_bar_row {
                let (ms, me) = app.mode_badge_pos;
                let (vs, ve) = app.view_badge_pos;
                if click_col >= ms && click_col < me {
                    let next = app.diff_modes[app.active_tab].next();
                    app.diff_modes[app.active_tab] = next;
                    app.refresh_repo_async(app.active_tab, diff_tx);
                    app.scroll_offset = 0;
                    return true;
                }
                if click_col >= vs && click_col < ve {
                    app.side_by_side = !app.side_by_side;
                    return true;
                }
                return false;
            }

            // Tab bar (rows 0-2)
            if click_row <= 2 {
                for (i, &(start, end)) in app.tab_positions.iter().enumerate() {
                    if click_col >= start && click_col < end {
                        app.active_tab = i;
                        app.scroll_offset = 0;
                        app.focused_file = None;
                        return true;
                    }
                }
                return false;
            }

            // Content area click
            let content_row = (click_row as usize).saturating_sub(4) + app.scroll_offset;

            // Check for expand row click (gap between hunks)
            if let Some((file_idx, gap_idx)) = find_expand_gap(app, content_row) {
                app.expand_gap(file_idx, gap_idx);
                return true;
            }

            // File header collapse toggle
            let positions = app.file_header_positions();

            for (i, &pos) in positions.iter().enumerate() {
                if content_row == pos {
                    app.focused_file = Some(i);
                    if let Some(files) = app.current_files_mut() {
                        if let Some(file) = files.get_mut(i) {
                            file.collapsed = !file.collapsed;
                        }
                    }
                    return true;
                }
            }

            app.focused_file = app.focused_file_from_scroll();
            true
        }
        MouseEventKind::Down(MouseButton::Middle) => {
            if let Some(text) = app.copy_hunk_at_focus() {
                if let Ok(mut clipboard) = Clipboard::new() {
                    let _ = clipboard.set_text(text);
                }
            }
            true
        }
        // Ignore move/release/drag — no state change, no redraw
        _ => false,
    }
}

/// Walk the content layout to find if `content_row` lands on an expandable gap.
/// Returns (file_idx, gap_idx) where gap_idx is:
///   0 = before first hunk, 1..n = between hunks, n = after last hunk.
fn find_expand_gap(app: &App, content_row: usize) -> Option<(usize, usize)> {
    let files = app.current_files()?;
    let mut pos: usize = 0;

    for (file_idx, file) in files.iter().enumerate() {
        // File header
        pos += 1;

        if file.collapsed {
            pos += 1; // trailing row
            continue;
        }

        for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
            // Hunk header row
            if content_row == pos {
                let gap = if hunk_idx > 0 {
                    diff::gap_between_hunks(&file.hunks[hunk_idx - 1], hunk)
                } else {
                    hunk.first_new_lineno().unwrap_or(1) as usize - 1
                };
                if gap > 0 {
                    return Some((file_idx, hunk_idx));
                }
            }
            pos += 1; // hunk header

            // Hunk content lines
            if app.side_by_side {
                let sbs_len = file
                    .sbs_cache
                    .as_ref()
                    .and_then(|c| c.get(hunk_idx))
                    .map(|h| h.len())
                    .unwrap_or(hunk.lines.len());
                pos += sbs_len;
            } else {
                pos += hunk.lines.len();
            }
        }

        // Trailing row — bottom gap
        if content_row == pos && !file.hunks.is_empty() && file.total_new_lines > 0 {
            let last_new = file.hunks.last().and_then(|h| h.last_new_lineno()).unwrap_or(0) as usize;
            if file.total_new_lines > last_new {
                return Some((file_idx, file.hunks.len()));
            }
        }
        pos += 1;
    }

    None
}
