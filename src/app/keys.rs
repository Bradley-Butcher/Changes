use super::{App, DiffResult, FilePickerState, RepoAdderState, PAGE_SCROLL};
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
