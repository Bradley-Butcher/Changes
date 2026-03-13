use super::{
    App, CommentBrowserState, CommentInputState, DiffResult, FilePickerState, FlashState,
    PAGE_SCROLL, RepoAdderState,
};
use crate::git::DiffMode;
use arboard::Clipboard;
use crossterm::event::{self, KeyCode, KeyModifiers};
use std::path::PathBuf;
use tokio::sync::mpsc;

pub fn handle_key(
    app: &mut App,
    key: event::KeyEvent,
    diff_tx: &mpsc::UnboundedSender<DiffResult>,
) -> bool {
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
            app.jump_active_viewport_top();
            app.focused_file = None;
        }
        KeyCode::Char(c) if ('1'..='9').contains(&c) => {
            let idx = (c as usize) - ('1' as usize);
            if idx < app.repos.len() {
                app.active_tab = idx;
                app.jump_active_viewport_top();
                app.focused_file = None;
            }
        }

        // Mode switching
        KeyCode::Char('m') => {
            app.set_mode(DiffMode::Unstaged, diff_tx);
        }
        KeyCode::Char('s') => {
            app.set_mode(DiffMode::Staged, diff_tx);
        }
        KeyCode::Char('b') => {
            app.set_mode(DiffMode::Branch, diff_tx);
        }

        // View toggle
        KeyCode::Char('v') => {
            app.side_by_side = !app.side_by_side;
            app.prepare_active_layout();
            app.clamp_active_viewport();
        }

        // Scrolling
        KeyCode::Char('j') | KeyCode::Down => {
            app.scroll_active_viewport(1);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.scroll_active_viewport(-1);
        }
        KeyCode::Char('J') => {
            app.prepare_active_layout();
            if let Some(next) = app
                .current_layout()
                .and_then(|layout| layout.next_file_header_row(app.current_scroll_offset()))
            {
                app.jump_active_viewport_to(next);
            }
        }
        KeyCode::Char('K') => {
            app.prepare_active_layout();
            if let Some(prev) = app
                .current_layout()
                .and_then(|layout| layout.prev_file_header_row(app.current_scroll_offset()))
            {
                app.jump_active_viewport_to(prev);
            }
        }
        KeyCode::Char('g') => {
            app.jump_active_viewport_top();
            app.focused_file = Some(0);
        }
        KeyCode::Char('G') => {
            app.jump_active_viewport_bottom();
        }
        KeyCode::PageDown => {
            app.scroll_active_viewport(PAGE_SCROLL as isize);
        }
        KeyCode::PageUp => {
            app.scroll_active_viewport(-(PAGE_SCROLL as isize));
        }

        // Collapse
        KeyCode::Enter => {
            if let Some(idx) = app.focused_file {
                app.toggle_collapsed(idx);
            }
        }
        KeyCode::Char('c') => {
            app.set_all_collapsed(true);
        }
        KeyCode::Char('e') => {
            app.set_all_collapsed(false);
        }

        // Copy
        KeyCode::Char('y') => {
            if let Some(text) = app.copy_hunk_at_focus()
                && let Ok(mut clipboard) = Clipboard::new()
            {
                let _ = clipboard.set_text(text);
            }
        }

        // File picker
        KeyCode::Char('f') => {
            app.file_picker = Some(FilePickerState {
                query: String::new(),
                selected: 0,
            });
        }

        // Add repo
        KeyCode::Char('a') => {
            app.repo_adder = Some(RepoAdderState {
                query: String::new(),
                error: None,
                results: Vec::new(),
                cursor: 0,
                checked: std::collections::HashSet::new(),
            });
            app.refresh_repo_adder_results();
        }

        // Comment on focused hunk
        KeyCode::Char('n') => {
            if let Some((file_idx, hunk_idx)) = app.focused_hunk() {
                let existing_text = app
                    .find_comment(file_idx, hunk_idx)
                    .map(|c| c.text.clone())
                    .unwrap_or_default();
                let cursor_pos = existing_text.len();
                app.comment_input = Some(CommentInputState {
                    file_idx,
                    hunk_idx,
                    text: existing_text,
                    cursor_pos,
                    anchor_row: app.current_scroll_offset(),
                });
            }
        }

        // Remove comment from focused hunk
        KeyCode::Char('N') => {
            if let Some((file_idx, hunk_idx)) = app.focused_hunk() {
                app.remove_comment(file_idx, hunk_idx);
            }
        }

        // Clear all comments
        KeyCode::Char('D') => {
            app.clear_comments();
        }

        // Copy all comments
        KeyCode::Char('Y') => {
            if let Some(text) = app.format_comments_markdown(None) {
                if let Ok(mut clipboard) = Clipboard::new() {
                    let _ = clipboard.set_text(text);
                }
                // Flash all commented hunks
                let now = std::time::Instant::now()
                    + std::time::Duration::from_millis(300);
                if let Some(repo) = app.repos.get(app.active_tab) {
                    let flashes: Vec<FlashState> = repo
                        .comments
                        .iter()
                        .map(|c| FlashState {
                            until: now,
                            file_idx: c.file_idx,
                            hunk_idx: c.hunk_idx,
                        })
                        .collect();
                    app.flash.extend(flashes);
                }
                let count = app
                    .repos
                    .get(app.active_tab)
                    .map(|r| r.comments.len())
                    .unwrap_or(0);
                app.status_message = Some((
                    format!(
                        "Copied {} note{}",
                        count,
                        if count == 1 { "" } else { "s" }
                    ),
                    std::time::Instant::now() + std::time::Duration::from_millis(300),
                ));
            }
        }

        // Comments browser
        KeyCode::Char('C') => {
            let count = app
                .repos
                .get(app.active_tab)
                .map(|r| r.comments.len())
                .unwrap_or(0);
            if count > 0 {
                app.comment_browser = Some(CommentBrowserState {
                    query: String::new(),
                    selected: 0,
                    checked: (0..count).collect(),
                });
            }
        }

        _ => {}
    }
    false
}

pub fn handle_file_picker_key(app: &mut App, key: event::KeyEvent) {
    if app.file_picker.is_none() {
        return;
    }
    match key.code {
        KeyCode::Esc => {
            app.file_picker = None;
        }
        KeyCode::Enter => {
            let selected = app.file_picker.as_ref().unwrap().selected;
            let filtered = app.filtered_file_indices();
            if let Some(&file_idx) = filtered.get(selected) {
                app.file_picker = None;
                // Ensure the file is uncollapsed before jumping
                if app
                    .current_files()
                    .and_then(|f| f.get(file_idx))
                    .is_some_and(|f| f.collapsed)
                {
                    app.toggle_collapsed(file_idx);
                }
                app.jump_to_file(file_idx);
            } else {
                app.file_picker = None;
            }
        }
        KeyCode::Up => {
            if let Some(ref mut picker) = app.file_picker {
                picker.selected = picker.selected.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            let max = app.filtered_file_indices().len().saturating_sub(1);
            if let Some(ref mut picker) = app.file_picker {
                picker.selected = (picker.selected + 1).min(max);
            }
        }
        KeyCode::Backspace => {
            if let Some(ref mut picker) = app.file_picker {
                picker.query.pop();
                picker.selected = 0;
            }
        }
        KeyCode::Char(c) => {
            if let Some(ref mut picker) = app.file_picker {
                picker.query.push(c);
                picker.selected = 0;
            }
        }
        _ => {}
    }
}

/// Returns indices of newly added repos (empty if none).
pub fn handle_repo_adder_key(app: &mut App, key: event::KeyEvent) -> Vec<usize> {
    if app.repo_adder.is_none() {
        return Vec::new();
    }
    match key.code {
        KeyCode::Esc => {
            app.repo_adder = None;
        }
        KeyCode::Up => {
            if let Some(ref mut adder) = app.repo_adder {
                adder.cursor = adder.cursor.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(ref mut adder) = app.repo_adder {
                let max = adder.results.len().saturating_sub(1);
                adder.cursor = (adder.cursor + 1).min(max);
            }
        }
        KeyCode::Char(' ') => {
            if let Some(ref mut adder) = app.repo_adder {
                let idx = adder.cursor;
                if idx < adder.results.len() {
                    if adder.checked.contains(&idx) {
                        adder.checked.remove(&idx);
                    } else {
                        adder.checked.insert(idx);
                    }
                }
            }
        }
        KeyCode::Enter => {
            let adder = app.repo_adder.as_ref().unwrap();
            let to_add: Vec<PathBuf> = if adder.checked.is_empty() {
                if let Some((_, path)) = adder.results.get(adder.cursor) {
                    vec![path.clone()]
                } else {
                    return Vec::new();
                }
            } else {
                let mut indices: Vec<usize> = adder.checked.iter().copied().collect();
                indices.sort();
                indices
                    .iter()
                    .filter_map(|&i| adder.results.get(i).map(|(_, p)| p.clone()))
                    .collect()
            };

            let mut added = Vec::new();
            for path in to_add {
                let path_str = path.to_string_lossy().to_string();
                match app.add_repo(&path_str) {
                    Ok(idx) => added.push(idx),
                    Err(e) => {
                        if let Some(ref mut adder) = app.repo_adder {
                            adder.error = Some(e);
                        }
                    }
                }
            }
            if !added.is_empty() {
                app.repo_adder = None;
            }
            return added;
        }
        KeyCode::Backspace => {
            if let Some(ref mut adder) = app.repo_adder {
                adder.query.pop();
            }
            app.refresh_repo_adder_results();
        }
        KeyCode::Char(c) => {
            if let Some(ref mut adder) = app.repo_adder {
                adder.query.push(c);
            }
            app.refresh_repo_adder_results();
        }
        _ => {}
    }
    Vec::new()
}

pub fn handle_comment_input_key(app: &mut App, key: event::KeyEvent) {
    if app.comment_input.is_none() {
        return;
    }
    match key.code {
        KeyCode::Esc => {
            app.comment_input = None;
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Ctrl+D: save and close
            let input = app.comment_input.take().unwrap();
            let text = input.text.trim().to_string();
            if text.is_empty() {
                app.remove_comment(input.file_idx, input.hunk_idx);
            } else {
                app.add_or_update_comment(input.file_idx, input.hunk_idx, text);
            }
        }
        KeyCode::Enter => {
            if let Some(ref mut input) = app.comment_input {
                input.text.insert(input.cursor_pos, '\n');
                input.cursor_pos += 1;
            }
        }
        KeyCode::Backspace => {
            if let Some(ref mut input) = app.comment_input
                && input.cursor_pos > 0 {
                    input.cursor_pos -= 1;
                    input.text.remove(input.cursor_pos);
                }
        }
        KeyCode::Left => {
            if let Some(ref mut input) = app.comment_input {
                input.cursor_pos = input.cursor_pos.saturating_sub(1);
            }
        }
        KeyCode::Right => {
            if let Some(ref mut input) = app.comment_input {
                input.cursor_pos = input.cursor_pos.min(input.text.len()).min(input.text.len());
                if input.cursor_pos < input.text.len() {
                    input.cursor_pos += 1;
                }
            }
        }
        KeyCode::Char(c) => {
            if let Some(ref mut input) = app.comment_input {
                input.text.insert(input.cursor_pos, c);
                input.cursor_pos += 1;
            }
        }
        _ => {}
    }
}

pub fn handle_comment_browser_key(app: &mut App, key: event::KeyEvent) {
    if app.comment_browser.is_none() {
        return;
    }

    let comment_count = app
        .repos
        .get(app.active_tab)
        .map(|r| r.comments.len())
        .unwrap_or(0);

    match key.code {
        KeyCode::Esc => {
            app.comment_browser = None;
        }
        KeyCode::Up => {
            if let Some(ref mut browser) = app.comment_browser {
                browser.selected = browser.selected.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            if let Some(ref mut browser) = app.comment_browser {
                let max = comment_count.saturating_sub(1);
                browser.selected = (browser.selected + 1).min(max);
            }
        }
        KeyCode::Char(' ') => {
            if let Some(ref mut browser) = app.comment_browser {
                let idx = browser.selected;
                if browser.checked.contains(&idx) {
                    browser.checked.remove(&idx);
                } else {
                    browser.checked.insert(idx);
                }
            }
        }
        KeyCode::Enter => {
            // Jump to the selected comment's hunk
            let selected = app.comment_browser.as_ref().unwrap().selected;
            if let Some(comment) = app
                .repos
                .get(app.active_tab)
                .and_then(|r| r.comments.get(selected))
            {
                let file_idx = comment.file_idx;
                let _hunk_idx = comment.hunk_idx;
                app.comment_browser = None;
                // Uncollapse if needed
                if app
                    .current_files()
                    .and_then(|f| f.get(file_idx))
                    .is_some_and(|f| f.collapsed)
                {
                    app.toggle_collapsed(file_idx);
                }
                app.jump_to_file(file_idx);
            }
        }
        KeyCode::Char('d') => {
            // Delete selected comment
            let selected = app.comment_browser.as_ref().unwrap().selected;
            let can_delete = app
                .repos
                .get(app.active_tab)
                .is_some_and(|r| selected < r.comments.len());
            if can_delete {
                app.repos[app.active_tab].comments.remove(selected);
                app.invalidate_layouts(app.active_tab);
                let new_count = app.repos[app.active_tab].comments.len();
                if new_count == 0 {
                    app.comment_browser = None;
                    return;
                }
                if let Some(ref mut browser) = app.comment_browser {
                    browser.checked.remove(&selected);
                    let new_checked: std::collections::HashSet<usize> = browser
                        .checked
                        .iter()
                        .map(|&i| if i > selected { i - 1 } else { i })
                        .collect();
                    browser.checked = new_checked;
                    browser.selected = browser.selected.min(new_count - 1);
                }
            }
        }
        KeyCode::Char('y') => {
            // Copy checked comments
            let checked: Vec<usize> = app
                .comment_browser
                .as_ref()
                .unwrap()
                .checked
                .iter()
                .copied()
                .collect();
            if let Some(text) = app.format_comments_markdown(Some(&checked)) {
                if let Ok(mut clipboard) = Clipboard::new() {
                    let _ = clipboard.set_text(text);
                }
                let count = checked.len();
                app.status_message = Some((
                    format!("Copied {} note{}", count, if count == 1 { "" } else { "s" }),
                    std::time::Instant::now() + std::time::Duration::from_millis(300),
                ));
            }
            app.comment_browser = None;
        }
        KeyCode::Backspace => {
            if let Some(ref mut browser) = app.comment_browser {
                browser.query.pop();
                browser.selected = 0;
            }
        }
        KeyCode::Char(c) => {
            if let Some(ref mut browser) = app.comment_browser {
                browser.query.push(c);
                browser.selected = 0;
            }
        }
        _ => {}
    }
}
