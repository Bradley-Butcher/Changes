use super::{
    App, CommentBrowserState, CommentInputState, FilePickerState, FlashState, MarkdownPreviewState,
    PAGE_SCROLL, RepoAdderState,
};
use crate::git::DiffMode;
use crate::screen::ViewportSize;
use arboard::Clipboard;
use crossterm::event::{self, KeyCode, KeyModifiers};
use std::path::PathBuf;

const MAX_MARKDOWN_PREVIEW_BYTES: u64 = 5 * 1024 * 1024;

fn read_markdown_preview(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;

    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_MARKDOWN_PREVIEW_BYTES {
        return Err("Markdown preview is limited to 5 MiB".to_string());
    }

    let mut content = String::new();
    file.take(MAX_MARKDOWN_PREVIEW_BYTES + 1)
        .read_to_string(&mut content)
        .map_err(|error| error.to_string())?;
    if content.len() as u64 > MAX_MARKDOWN_PREVIEW_BYTES {
        return Err("Markdown preview is limited to 5 MiB".to_string());
    }
    Ok(content)
}

pub fn handle_key(app: &mut App, key: event::KeyEvent, viewport: ViewportSize) -> bool {
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
            app.set_mode(DiffMode::Unstaged);
        }
        KeyCode::Char('s') => {
            app.set_mode(DiffMode::Staged);
        }
        KeyCode::Char('b') => {
            app.set_mode(DiffMode::Branch);
        }

        // View toggle
        KeyCode::Char('v') => {
            app.toggle_view(viewport);
        }

        // Scrolling
        KeyCode::Char('j') | KeyCode::Down => {
            app.scroll_active_viewport(1, viewport);
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.scroll_active_viewport(-1, viewport);
        }
        KeyCode::Char('J') => {
            app.prepare_active_layout(viewport);
            if let Some(next) = app
                .current_layout()
                .and_then(|layout| layout.next_file_header_row(app.current_scroll_offset()))
            {
                app.jump_active_viewport_to(next, viewport);
            }
        }
        KeyCode::Char('K') => {
            app.prepare_active_layout(viewport);
            if let Some(prev) = app
                .current_layout()
                .and_then(|layout| layout.prev_file_header_row(app.current_scroll_offset()))
            {
                app.jump_active_viewport_to(prev, viewport);
            }
        }
        KeyCode::Char('g') => {
            app.jump_active_viewport_top();
            app.focused_file = Some(0);
        }
        KeyCode::Char('G') => {
            app.jump_active_viewport_bottom(viewport);
        }
        KeyCode::PageDown => {
            app.scroll_active_viewport(PAGE_SCROLL as isize, viewport);
        }
        KeyCode::PageUp => {
            app.scroll_active_viewport(-(PAGE_SCROLL as isize), viewport);
        }

        // Collapse
        KeyCode::Enter => {
            if let Some(idx) = app.focused_file {
                app.toggle_collapsed(idx, viewport);
            }
        }
        KeyCode::Char('c') => {
            app.set_all_collapsed(true, viewport);
        }
        KeyCode::Char('e') => {
            app.set_all_collapsed(false, viewport);
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
                    .map(str::to_owned)
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
                let now = std::time::Instant::now() + std::time::Duration::from_millis(300);
                let comments = app.comments();
                let flashes: Vec<FlashState> = comments
                    .iter()
                    .map(|&(file_idx, hunk_idx, _)| FlashState {
                        until: now,
                        file_idx,
                        hunk_idx,
                    })
                    .collect();
                let count = comments.len();
                app.flash.extend(flashes);
                app.status_message = Some((
                    format!("Copied {} note{}", count, if count == 1 { "" } else { "s" }),
                    std::time::Instant::now() + std::time::Duration::from_millis(300),
                ));
            }
        }

        // Markdown preview
        KeyCode::Char('p') => {
            if let Some(file_idx) = app.focused_file
                && let Some(repo) = app.repos.get(app.active_tab)
                && let Some(file) = repo.files.get(file_idx)
                && file.path.ends_with(".md")
            {
                let preview_path = file.path.clone();
                let full_path = repo.info.path.join(&preview_path);
                match read_markdown_preview(&full_path) {
                    Ok(content) => {
                        *app.markdown_render_cache.borrow_mut() = None;
                        app.markdown_preview = Some(MarkdownPreviewState {
                            content,
                            path: preview_path,
                            scroll: 0,
                        });
                    }
                    Err(error) => {
                        app.status_message = Some((
                            error,
                            std::time::Instant::now() + std::time::Duration::from_secs(2),
                        ));
                    }
                }
            }
        }

        // Comments browser
        KeyCode::Char('C') => {
            let count = app.comments().len();
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

pub fn handle_file_picker_key(app: &mut App, key: event::KeyEvent, viewport: ViewportSize) {
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
                    app.toggle_collapsed(file_idx, viewport);
                }
                app.jump_to_file(file_idx, viewport);
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

pub fn handle_markdown_preview_key(app: &mut App, key: event::KeyEvent) {
    if app.markdown_preview.is_none() {
        return;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') => {
            app.markdown_preview = None;
            *app.markdown_render_cache.borrow_mut() = None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(ref mut preview) = app.markdown_preview {
                preview.scroll = preview.scroll.saturating_add(1);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(ref mut preview) = app.markdown_preview {
                preview.scroll = preview.scroll.saturating_sub(1);
            }
        }
        KeyCode::PageDown => {
            if let Some(ref mut preview) = app.markdown_preview {
                preview.scroll = preview.scroll.saturating_add(20);
            }
        }
        KeyCode::PageUp => {
            if let Some(ref mut preview) = app.markdown_preview {
                preview.scroll = preview.scroll.saturating_sub(20);
            }
        }
        KeyCode::Char('g') => {
            if let Some(ref mut preview) = app.markdown_preview {
                preview.scroll = 0;
            }
        }
        KeyCode::Char('G') => {
            if let Some(ref mut preview) = app.markdown_preview {
                preview.scroll = usize::MAX; // clamped at render time
            }
        }
        _ => {}
    }
}

fn previous_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    text[..index]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index += 1;
    }
    text[index..]
        .chars()
        .next()
        .map(|character| index + character.len_utf8())
        .unwrap_or(index)
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
                && input.cursor_pos > 0
            {
                let mut current = input.cursor_pos.min(input.text.len());
                while !input.text.is_char_boundary(current) {
                    current -= 1;
                }
                let previous = previous_char_boundary(&input.text, current);
                input.text.drain(previous..current);
                input.cursor_pos = previous;
            }
        }
        KeyCode::Left => {
            if let Some(ref mut input) = app.comment_input {
                input.cursor_pos = previous_char_boundary(&input.text, input.cursor_pos);
            }
        }
        KeyCode::Right => {
            if let Some(ref mut input) = app.comment_input {
                input.cursor_pos = next_char_boundary(&input.text, input.cursor_pos);
            }
        }
        KeyCode::Char(c) => {
            if let Some(ref mut input) = app.comment_input {
                input.text.insert(input.cursor_pos, c);
                input.cursor_pos += c.len_utf8();
            }
        }
        _ => {}
    }
}

pub fn handle_comment_browser_key(app: &mut App, key: event::KeyEvent, viewport: ViewportSize) {
    if app.comment_browser.is_none() {
        return;
    }

    let comment_count = app.comments().len();

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
            if let Some((file_idx, _, _)) = app.comments().get(selected).copied() {
                app.comment_browser = None;
                // Uncollapse if needed
                if app
                    .current_files()
                    .and_then(|f| f.get(file_idx))
                    .is_some_and(|f| f.collapsed)
                {
                    app.toggle_collapsed(file_idx, viewport);
                }
                app.jump_to_file(file_idx, viewport);
            }
        }
        KeyCode::Char('d') => {
            // Delete selected comment
            let selected = app.comment_browser.as_ref().unwrap().selected;
            if app.remove_comment_at(selected) {
                let new_count = app.comments().len();
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

#[cfg(test)]
mod tests {
    use super::{
        MAX_MARKDOWN_PREVIEW_BYTES, next_char_boundary, previous_char_boundary,
        read_markdown_preview,
    };

    #[test]
    fn cursor_navigation_uses_utf8_boundaries() {
        let text = "aé界";
        assert_eq!(next_char_boundary(text, 1), 3);
        assert_eq!(next_char_boundary(text, 3), 6);
        assert_eq!(previous_char_boundary(text, 6), 3);
        assert_eq!(previous_char_boundary(text, 3), 1);
    }

    #[test]
    fn markdown_preview_has_a_size_limit() {
        let path =
            std::env::temp_dir().join(format!("changes-markdown-preview-{}", std::process::id()));
        std::fs::write(&path, "# small").unwrap();
        assert_eq!(read_markdown_preview(&path).unwrap(), "# small");

        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_MARKDOWN_PREVIEW_BYTES + 1)
            .unwrap();
        assert!(read_markdown_preview(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
