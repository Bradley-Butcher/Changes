use crate::diff::{DiffLine, FileDiff};
use crate::git::{self, DiffMode, RepoInfo};
use crate::highlight::Highlighter;
use crate::ui;
use crate::watcher::{self, WatchEvent};
use anyhow::Result;
use arboard::Clipboard;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
    MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub struct App {
    pub repos: Vec<RepoInfo>,
    pub active_tab: usize,
    pub diff_modes: Vec<DiffMode>,
    pub file_diffs: Vec<Vec<FileDiff>>,
    pub scroll_offset: usize,
    pub focused_file: Option<usize>,
    pub side_by_side: bool,
    pub last_error: Option<String>,
    pub flash_until: Option<Instant>,
    pub flash_file_idx: Option<usize>,
    pub flash_hunk_idx: Option<usize>,
}

impl App {
    pub fn new(repos: Vec<RepoInfo>) -> Self {
        let n = repos.len();
        Self {
            repos,
            active_tab: 0,
            diff_modes: vec![DiffMode::Unstaged; n],
            file_diffs: vec![Vec::new(); n],
            scroll_offset: 0,
            focused_file: None,
            side_by_side: false,
            last_error: None,
            flash_until: None,
            flash_file_idx: None,
            flash_hunk_idx: None,
        }
    }

    pub fn current_mode(&self) -> DiffMode {
        self.diff_modes[self.active_tab]
    }

    pub fn current_files(&self) -> Option<&Vec<FileDiff>> {
        self.file_diffs.get(self.active_tab)
    }

    pub fn current_files_mut(&mut self) -> Option<&mut Vec<FileDiff>> {
        self.file_diffs.get_mut(self.active_tab)
    }

    pub fn refresh_repo(&mut self, idx: usize) {
        let mode = self.diff_modes[idx];
        match git::compute_diff(&self.repos[idx].path, mode) {
            Ok(files) => {
                // Preserve collapse state
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
                }

                self.file_diffs[idx] = new_files;
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("{}: {}", self.repos[idx].name, e));
            }
        }
    }

    pub fn refresh_all(&mut self) {
        for i in 0..self.repos.len() {
            self.refresh_repo(i);
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

    pub fn is_line_flashing(&self, _line: &DiffLine) -> bool {
        if let Some(until) = self.flash_until {
            Instant::now() < until
        } else {
            false
        }
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

        let hunk = file.hunks.get(target_hunk)?;

        // Collect changed lines in the hunk
        let changed_lines: Vec<&DiffLine> = hunk
            .lines
            .iter()
            .filter(|l| {
                l.kind == crate::diff::LineKind::Addition
                    || l.kind == crate::diff::LineKind::Deletion
            })
            .collect();

        if changed_lines.is_empty() {
            return None;
        }

        let first_lineno = changed_lines
            .first()
            .and_then(|l| l.new_lineno.or(l.old_lineno))
            .unwrap_or(0);
        let last_lineno = changed_lines
            .last()
            .and_then(|l| l.new_lineno.or(l.old_lineno))
            .unwrap_or(0);

        let mut result = format!("// {}:{}-{}\n", file.path, first_lineno, last_lineno);
        for line in &changed_lines {
            let prefix = match line.kind {
                crate::diff::LineKind::Addition => "+",
                crate::diff::LineKind::Deletion => "-",
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

pub async fn run(path: PathBuf) -> Result<()> {
    let repos = git::discover_repos(&path)?;

    let mut app = App::new(repos);
    app.refresh_all();

    // Set up file watcher
    let (watch_tx, mut watch_rx) = mpsc::unbounded_channel::<WatchEvent>();
    let repo_paths: Vec<PathBuf> = app.repos.iter().map(|r| r.path.clone()).collect();
    let mut debouncer = watcher::start_watching(&repo_paths, watch_tx)?;
    watcher::watch_paths(&mut debouncer, &repo_paths)?;

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let highlighter = Highlighter::new();

    let result = run_loop(&mut terminal, &mut app, &mut watch_rx, &highlighter).await;

    // Restore terminal
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    watch_rx: &mut mpsc::UnboundedReceiver<WatchEvent>,
    highlighter: &Highlighter,
) -> Result<()> {
    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();

    loop {
        // Draw
        terminal.draw(|f| ui::draw(f, app, highlighter))?;

        // Handle events
        let timeout = tick_rate.saturating_sub(last_tick.elapsed());

        tokio::select! {
            // Terminal events
            _ = async {
                loop {
                    if event::poll(timeout).unwrap_or(false) {
                        if let Ok(ev) = event::read() {
                            match ev {
                                Event::Key(key) => {
                                    if handle_key(app, key) {
                                        return;
                                    }
                                }
                                Event::Mouse(mouse) => {
                                    handle_mouse(app, mouse);
                                }
                                Event::Resize(_, _) => {}
                                _ => {}
                            }
                        }
                        break;
                    } else {
                        break;
                    }
                }
            } => {}
            // Watch events
            Some(event) = watch_rx.recv() => {
                app.refresh_repo(event.repo_index);
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }

        // Clear flash if expired
        if let Some(until) = app.flash_until {
            if Instant::now() >= until {
                app.flash_until = None;
                app.flash_file_idx = None;
                app.flash_hunk_idx = None;
            }
        }
    }
}

fn handle_key(app: &mut App, key: event::KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return true,

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
            app.refresh_repo(app.active_tab);
            app.scroll_offset = 0;
        }
        KeyCode::Char('s') => {
            app.diff_modes[app.active_tab] = DiffMode::Staged;
            app.refresh_repo(app.active_tab);
            app.scroll_offset = 0;
        }
        KeyCode::Char('b') => {
            app.diff_modes[app.active_tab] = DiffMode::Branch;
            app.refresh_repo(app.active_tab);
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

        _ => {}
    }
    false
}

fn handle_mouse(app: &mut App, mouse: event::MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.scroll_offset = app.scroll_offset.saturating_sub(3);
            app.focused_file = app.focused_file_from_scroll();
        }
        MouseEventKind::ScrollDown => {
            app.scroll_offset = app.scroll_offset.saturating_add(3);
            app.focused_file = app.focused_file_from_scroll();
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Check if clicking on a file header to toggle collapse
            let click_row = mouse.row as usize;
            // Account for tab bar (3 rows) and border (1 row)
            let content_row = click_row.saturating_sub(4) + app.scroll_offset;
            let positions = app.file_header_positions();

            for (i, &pos) in positions.iter().enumerate() {
                if content_row == pos {
                    app.focused_file = Some(i);
                    if let Some(files) = app.current_files_mut() {
                        if let Some(file) = files.get_mut(i) {
                            file.collapsed = !file.collapsed;
                        }
                    }
                    return;
                }
            }

            // Update focused file based on click position
            app.focused_file = app.focused_file_from_scroll();
        }
        MouseEventKind::Down(MouseButton::Middle) => {
            // Double-click copy (middle click as alternative since true double-click
            // detection requires timing logic)
            if let Some(text) = app.copy_hunk_at_focus() {
                if let Ok(mut clipboard) = Clipboard::new() {
                    let _ = clipboard.set_text(text);
                }
            }
        }
        _ => {}
    }
}
