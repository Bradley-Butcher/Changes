use super::{App, DiffResult};
use crate::git::DiffMode;
use arboard::Clipboard;
use crossterm::event::{self, KeyCode, KeyModifiers};
use std::path::PathBuf;
use tokio::sync::mpsc;

pub fn handle_key(app: &mut App, key: event::KeyEvent, diff_tx: &mpsc::UnboundedSender<DiffResult>) -> bool {
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
        KeyCode::Char('m') => {
            app.repos[app.active_tab].mode = DiffMode::Unstaged;
            app.refresh_repo_async(app.active_tab, diff_tx);
            app.scroll_offset = 0;
        }
        KeyCode::Char('s') => {
            app.repos[app.active_tab].mode = DiffMode::Staged;
            app.refresh_repo_async(app.active_tab, diff_tx);
            app.scroll_offset = 0;
        }
        KeyCode::Char('b') => {
            app.repos[app.active_tab].mode = DiffMode::Branch;
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
            let positions = app.file_header_positions();
            if let Some(next) = positions.iter().find(|&&p| p > app.scroll_offset) {
                app.scroll_offset = *next;
                app.focused_file = app.focused_file_from_scroll();
            }
        }
        KeyCode::Char('K') => {
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

pub fn handle_file_picker_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.show_file_picker = false;
        }
        KeyCode::Enter => {
            let filtered = app.filtered_file_indices();
            if let Some(&file_idx) = filtered.get(app.file_picker_selected) {
                app.jump_to_file(file_idx);
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
pub fn handle_repo_adder_key(app: &mut App, key: event::KeyEvent) -> Vec<usize> {
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
