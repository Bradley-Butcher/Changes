use crate::app::{App, BaseBranchResult, DiffResult, GapExpandResult};
use crate::git;
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
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::app::keys;
use crate::app::mouse;

const FLASH_TICK: Duration = Duration::from_millis(50);
const IDLE_TICK: Duration = Duration::from_secs(60);
const EVENT_CHANNEL_CAPACITY: usize = 64;
const DIFF_CHANNEL_CAPACITY: usize = 8;

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
    watch_rx: mpsc::Receiver<WatchEvent>,
    diff_rx: mpsc::Receiver<DiffResult>,
    diff_tx: mpsc::Sender<DiffResult>,
    base_rx: mpsc::Receiver<BaseBranchResult>,
    base_tx: mpsc::Sender<BaseBranchResult>,
    gap_rx: mpsc::Receiver<GapExpandResult>,
    gap_tx: mpsc::Sender<GapExpandResult>,
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

    // Set up file watcher with shared repo paths
    let (watch_tx, watch_rx) = mpsc::channel::<WatchEvent>(EVENT_CHANNEL_CAPACITY);
    let repo_paths: Vec<PathBuf> = app.repos.iter().map(|r| r.info.path.clone()).collect();
    let mut repo_watcher = watcher::RepoWatcher::new(repo_paths, watch_tx)?;

    // Panic hook to restore terminal on crash
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    // Set up terminal. The guard is armed as soon as raw mode is on so that a failure in
    // any later setup step still restores the terminal.
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let highlighter = Highlighter::new();

    let (diff_tx, diff_rx) = mpsc::channel::<DiffResult>(DIFF_CHANNEL_CAPACITY);
    app.attach_diff_worker(diff_tx.clone());
    let (base_tx, base_rx) = mpsc::channel::<BaseBranchResult>(EVENT_CHANNEL_CAPACITY);
    let (gap_tx, gap_rx) = mpsc::channel::<GapExpandResult>(EVENT_CHANNEL_CAPACITY);

    // Load every repo in the background so the first frame appears immediately, with the
    // active tab first so it fills in before the others.
    let active = app.active_tab;
    let load_order: Vec<usize> = std::iter::once(active)
        .chain((0..app.repos.len()).filter(|&idx| idx != active))
        .collect();
    for idx in load_order {
        app.refresh_repo_async_bounded(idx, &diff_tx);
        app.refresh_base_async(idx, &base_tx);
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
        &mut repo_watcher,
    )
    .await
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    ch: &mut Channels,
    highlighter: &Highlighter,
    repo_watcher: &mut watcher::RepoWatcher,
) -> Result<()> {
    let (term_tx, mut term_rx) = mpsc::channel::<Event>(EVENT_CHANNEL_CAPACITY);
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(ev) => {
                    if term_tx.blocking_send(ev).is_err() {
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
            let content_area = ui::diff_inner_area(terminal.size()?.into());
            app.layout.content_y = content_area.y;
            app.layout.content_height = content_area.height;
            app.layout.content_width = content_area.width;
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

        let first = tokio::select! {
            Some(ev) = term_rx.recv() => AppEvent::Terminal(ev),
            Some(ev) = ch.watch_rx.recv() => AppEvent::FileChange(ev),
            Some(ev) = ch.diff_rx.recv() => AppEvent::DiffDone(ev),
            Some(ev) = ch.base_rx.recv() => AppEvent::BaseBranch(ev),
            Some(ev) = ch.gap_rx.recv() => AppEvent::GapExpanded(ev),
            () = &mut tick_sleep => AppEvent::Tick,
        };

        // Drain whatever else is already queued so a burst of scroll events or a stack of
        // finished diffs costs one frame, not one frame each. The terminal is the slowest
        // link, so never send it a frame nobody will see.
        let mut batch = vec![first];
        while batch.len() < MAX_EVENTS_PER_FRAME {
            let next = if let Ok(ev) = term_rx.try_recv() {
                AppEvent::Terminal(ev)
            } else if let Ok(ev) = ch.diff_rx.try_recv() {
                AppEvent::DiffDone(ev)
            } else if let Ok(ev) = ch.gap_rx.try_recv() {
                AppEvent::GapExpanded(ev)
            } else if let Ok(ev) = ch.base_rx.try_recv() {
                AppEvent::BaseBranch(ev)
            } else if let Ok(ev) = ch.watch_rx.try_recv() {
                AppEvent::FileChange(ev)
            } else {
                break;
            };
            batch.push(next);
        }

        for event in batch {
            if handle_event(event, app, ch, repo_watcher, &mut needs_redraw)? {
                return Ok(());
            }
        }
    }
}

/// Upper bound on events folded into one frame, so a flood cannot starve drawing.
const MAX_EVENTS_PER_FRAME: usize = 256;

/// Apply one event to the app. Returns `Ok(true)` when the user asked to quit.
fn handle_event(
    event: AppEvent,
    app: &mut App,
    ch: &mut Channels,
    repo_watcher: &mut watcher::RepoWatcher,
    needs_redraw: &mut bool,
) -> Result<bool> {
    match event {
        AppEvent::Terminal(Event::Key(key)) => {
            if key.kind != KeyEventKind::Press {
                return Ok(false);
            }
            if app.markdown_preview.is_some() {
                keys::handle_markdown_preview_key(app, key);
                *needs_redraw = true;
                return Ok(false);
            }
            if app.comment_input.is_some() {
                keys::handle_comment_input_key(app, key);
                *needs_redraw = true;
                return Ok(false);
            }
            if app.comment_browser.is_some() {
                keys::handle_comment_browser_key(app, key);
                *needs_redraw = true;
                return Ok(false);
            }
            if app.file_picker.is_some() {
                keys::handle_file_picker_key(app, key);
                *needs_redraw = true;
                return Ok(false);
            }
            if app.repo_adder.is_some() {
                let added = keys::handle_repo_adder_key(app, key);
                for new_idx in added.into_iter().rev() {
                    let path = app.repos[new_idx].info.path.clone();
                    if let Err(error) = repo_watcher.add(&path) {
                        app.remove_repo_at(new_idx);
                        app.set_status(format!("Cannot watch {}: {error}", path.display()));
                        continue;
                    }
                    app.refresh_repo_async_bounded(new_idx, &ch.diff_tx);
                    app.refresh_base_async(new_idx, &ch.base_tx);
                }
                *needs_redraw = true;
                return Ok(false);
            }
            if keys::handle_outline_key(app, key) {
                *needs_redraw = true;
                return Ok(false);
            }
            // Remove current tab
            if key.code == KeyCode::Char('x') {
                if app.repos.len() > 1 {
                    let idx = app.active_tab;
                    let name = app.repos[idx].info.name.clone();
                    if let Some(path) = app.remove_repo_at(idx) {
                        repo_watcher.remove(&path);
                    }
                    app.set_status(format!("Stopped watching {name}"));
                } else {
                    app.set_status("Can't remove the last repo — press q to quit");
                }
                *needs_redraw = true;
                return Ok(false);
            }
            if keys::handle_key_bounded(app, key, &ch.diff_tx) {
                return Ok(true);
            }
            *needs_redraw = true;
        }
        AppEvent::Terminal(Event::Mouse(m)) => {
            if mouse::handle_mouse_bounded(app, m, &ch.diff_tx, &ch.gap_tx) {
                *needs_redraw = true;
            }
        }
        AppEvent::Terminal(Event::Resize(_, _)) => {
            *needs_redraw = true;
        }
        AppEvent::Terminal(_) => {}
        AppEvent::FileChange(event) => {
            if let Some(idx) = app
                .repos
                .iter()
                .position(|r| r.info.path == event.repo_path)
            {
                app.refresh_repo_async_bounded(idx, &ch.diff_tx);
                if event.base_refresh_needed {
                    app.refresh_base_async(idx, &ch.base_tx);
                }
            }
        }
        AppEvent::DiffDone(result) => {
            if app.apply_diff_refresh_result(result, &ch.diff_tx) {
                *needs_redraw = true;
            }
        }
        AppEvent::BaseBranch(result) => {
            if app.apply_base_refresh_result(result, &ch.base_tx, &ch.diff_tx) {
                *needs_redraw = true;
            }
        }
        AppEvent::GapExpanded(result) => {
            app.apply_gap_expand(result);
            *needs_redraw = true;
        }
        AppEvent::Tick => {
            let now = Instant::now();
            let before = app.flash.len();
            app.flash.retain(|f| now < f.until);
            if app.flash.len() != before {
                *needs_redraw = true;
            }
            if let Some((_, until)) = app.status_message
                && now >= until
            {
                app.status_message = None;
                *needs_redraw = true;
            }
        }
    }
    Ok(false)
}
