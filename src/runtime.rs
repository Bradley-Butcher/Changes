use crate::app::{App, BaseBranchResult, DiffResult, GapExpandResult};
use crate::git::{self, DiffMode};
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
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::app::keys;
use crate::app::mouse;

const FLASH_TICK: Duration = Duration::from_millis(50);
const IDLE_TICK: Duration = Duration::from_secs(60);

enum AppEvent {
    Terminal(Event),
    FileChange(WatchEvent),
    DiffDone(DiffResult),
    BaseBranch(BaseBranchResult),
    GapExpanded(GapExpandResult),
    Tick,
}

/// Bundles the event channels used by the run loop.
struct Channels {
    watch_rx: mpsc::UnboundedReceiver<WatchEvent>,
    diff_rx: mpsc::UnboundedReceiver<DiffResult>,
    diff_tx: mpsc::UnboundedSender<DiffResult>,
    base_rx: mpsc::UnboundedReceiver<BaseBranchResult>,
    base_tx: mpsc::UnboundedSender<BaseBranchResult>,
    gap_rx: mpsc::UnboundedReceiver<GapExpandResult>,
    gap_tx: mpsc::UnboundedSender<GapExpandResult>,
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Show);
}

/// RAII guard that restores the terminal on drop.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
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

    // RAII guard ensures terminal is restored even on early return via `?`
    let _guard = TerminalGuard;

    let highlighter = Highlighter::new();

    let (diff_tx, diff_rx) = mpsc::unbounded_channel::<DiffResult>();
    let (base_tx, base_rx) = mpsc::unbounded_channel::<BaseBranchResult>();
    let (gap_tx, gap_rx) = mpsc::unbounded_channel::<GapExpandResult>();

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
        gap_rx,
        gap_tx,
    };

    run_loop(
        &mut terminal,
        &mut app,
        &mut channels,
        &highlighter,
        &mut debouncer,
        &shared_paths,
    )
    .await
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
            app.prepare_active_layout();
            let mut hints = LayoutHints::default();
            terminal.draw(|f| ui::draw(f, app, highlighter, &mut hints))?;
            app.layout = hints;
            app.clamp_active_viewport();
            needs_redraw = false;
        }

        let tick_dur = if !app.flash.is_empty() || app.status_message.is_some() {
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
            Some(ev) = ch.gap_rx.recv() => AppEvent::GapExpanded(ev),
            () = &mut tick_sleep => AppEvent::Tick,
        };

        match event {
            AppEvent::Terminal(Event::Key(key)) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.markdown_preview.is_some() {
                    keys::handle_markdown_preview_key(app, key);
                    needs_redraw = true;
                    continue;
                }
                if app.comment_input.is_some() {
                    keys::handle_comment_input_key(app, key);
                    needs_redraw = true;
                    continue;
                }
                if app.comment_browser.is_some() {
                    keys::handle_comment_browser_key(app, key);
                    needs_redraw = true;
                    continue;
                }
                if app.file_picker.is_some() {
                    keys::handle_file_picker_key(app, key);
                    needs_redraw = true;
                    continue;
                }
                if app.repo_adder.is_some() {
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
                    app.jump_active_viewport_top();
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
                if mouse::handle_mouse(app, m, &ch.diff_tx, &ch.gap_tx) {
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
                    highlighter.clear_highlight_cache();
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
            AppEvent::GapExpanded(result) => {
                app.apply_gap_expand(result);
                needs_redraw = true;
            }
            AppEvent::Tick => {
                let now = Instant::now();
                let before = app.flash.len();
                app.flash.retain(|f| now < f.until);
                if app.flash.len() != before {
                    needs_redraw = true;
                }
                if let Some((_, until)) = app.status_message
                    && now >= until
                {
                    app.status_message = None;
                    needs_redraw = true;
                }
            }
        }
    }
}
