use super::{
    App, CommentBrowserState, CompareRow, DiffResult, FilePickerState, FlashState,
    MarkdownPreviewState, RepoAdderState,
};
use crate::git::{Base, DiffMode};
use crossterm::event::{self, KeyCode, KeyEventKind, KeyModifiers};
use std::path::PathBuf;
use tokio::sync::mpsc;

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

pub fn handle_key(
    app: &mut App,
    key: event::KeyEvent,
    diff_tx: &mpsc::UnboundedSender<DiffResult>,
) -> bool {
    handle_key_with_set_mode(app, key, |app, mode| app.set_mode(mode, diff_tx))
}

pub(crate) fn handle_key_bounded(
    app: &mut App,
    key: event::KeyEvent,
    diff_tx: &mpsc::Sender<DiffResult>,
) -> bool {
    handle_key_with_set_mode(app, key, |app, mode| {
        app.set_mode_bounded(mode, diff_tx);
    })
}

fn handle_key_with_set_mode(
    app: &mut App,
    key: event::KeyEvent,
    mut set_mode: impl FnMut(&mut App, DiffMode),
) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('c') if ctrl => return true,
        // Esc only ever closes something; quitting is `q` so a stray Esc can't end a review.
        KeyCode::Esc => {
            app.show_help = false;
        }

        // Timeline: the diff is base → cursor; these move the cursor.
        KeyCode::Char('>') | KeyCode::Char('.') => {
            if let Some(mode) = app.timeline_move(1) {
                set_mode(app, mode);
            } else {
                app.set_status("Already at now");
            }
        }
        KeyCode::Char('<') | KeyCode::Char(',') => {
            if let Some(mode) = app.timeline_move(-1) {
                set_mode(app, mode);
            } else if app.repos[app.active_tab].steps.len() < 2 {
                app.set_status("Nothing between the base and now to step through");
            } else {
                app.set_status("Already at the first step after the base");
            }
        }
        KeyCode::Char('}') => {
            let last = app.repos[app.active_tab].steps.len().saturating_sub(1);
            if let Some(mode) = app.timeline_jump(last) {
                set_mode(app, mode);
            }
        }
        KeyCode::Char('{') => {
            if let Some(mode) = app.timeline_jump(0) {
                set_mode(app, mode);
            }
        }
        KeyCode::Char('s') => {
            if let Some(mode) = app.timeline_toggle_step_only() {
                set_mode(app, mode);
            } else {
                app.set_status("Nothing between the base and now to step through");
            }
        }
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
        }

        // Vim-style paging (must precede the plain-letter arms below)
        KeyCode::Char('d') if ctrl => {
            app.scroll_active_viewport(app.half_page_size() as isize);
        }
        KeyCode::Char('u') if ctrl => {
            app.scroll_active_viewport(-(app.half_page_size() as isize));
        }
        KeyCode::Char('f') if ctrl => {
            app.scroll_active_viewport(app.page_size() as isize);
        }
        KeyCode::Char('b') if ctrl => {
            app.scroll_active_viewport(-(app.page_size() as isize));
        }

        // Tab switching
        KeyCode::Tab => app.next_tab(),
        KeyCode::BackTab => app.prev_tab(),
        KeyCode::Char(c) if ('1'..='9').contains(&c) => {
            let idx = (c as usize) - ('1' as usize);
            if idx < app.repos.len() {
                app.switch_tab(idx);
            } else {
                app.set_status(format!("No tab {c}"));
            }
        }

        // Mode switching
        KeyCode::Char('m') => {
            set_mode(app, DiffMode::Local);
        }
        KeyCode::Char('b') => {
            let mode = app.branch_mode();
            set_mode(app, mode);
        }
        KeyCode::Char('B') => {
            app.open_compare_picker();
        }

        // View toggle
        KeyCode::Char('v') => {
            app.toggle_view();
        }
        KeyCode::Char('o') => app.show_overview(false),
        KeyCode::Char('t') => app.show_overview(true),

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
        KeyCode::Char(']') => {
            if !app.select_next_hunk() {
                app.set_status("Already at the last hunk");
            }
        }
        KeyCode::Char('[') => {
            if !app.select_prev_hunk() {
                app.set_status("Already at the first hunk");
            }
        }
        KeyCode::Char('g') | KeyCode::Home => {
            app.jump_active_viewport_top();
        }
        KeyCode::Char('G') | KeyCode::End => {
            app.jump_active_viewport_bottom();
        }
        KeyCode::PageDown => {
            app.scroll_active_viewport(app.page_size() as isize);
        }
        KeyCode::PageUp => {
            app.scroll_active_viewport(-(app.page_size() as isize));
        }

        // Collapse
        KeyCode::Enter => {
            if let Some(idx) = app.focused_file.or_else(|| app.focused_file_from_scroll()) {
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
            app.copy_hunk_with_feedback(None);
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
                app.open_comment_input(file_idx, hunk_idx, None);
            } else {
                app.set_status("No hunk in view to annotate");
            }
        }

        // Remove comment from focused hunk
        KeyCode::Char('N') => {
            if let Some((file_idx, hunk_idx)) = app.focused_hunk() {
                if app.find_comment(file_idx, hunk_idx).is_some() {
                    app.remove_comment(file_idx, hunk_idx);
                    app.set_status("Removed note");
                } else {
                    app.set_status("No note on this hunk");
                }
            }
        }

        // Clear all comments
        KeyCode::Char('D') => {
            app.clear_comments();
        }

        // Copy all comments
        KeyCode::Char('Y') => {
            if let Some(text) = app.format_comments_markdown(None) {
                // Flash all commented hunks
                let now = std::time::Instant::now() + std::time::Duration::from_millis(300);
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
                let label = format!(
                    "{} note{} as markdown",
                    count,
                    if count == 1 { "" } else { "s" }
                );
                app.copy_to_clipboard(text, &label);
            } else {
                app.set_status("No notes to copy — press n on a hunk to add one");
            }
        }

        // Peek at the file as it stands: rendered for markdown, the code itself otherwise.
        KeyCode::Char('p') => {
            let focused = app.focused_file.or_else(|| app.focused_file_from_scroll());
            let Some(file_idx) = focused else {
                return false;
            };
            let target = app
                .repos
                .get(app.active_tab)
                .and_then(|repo| repo.files.get(file_idx).map(|file| (repo, file)));
            let Some((repo, file)) = target else {
                return false;
            };
            if !file.path.ends_with(".md") {
                if key.kind == KeyEventKind::Repeat {
                    app.peek_repeat();
                } else {
                    app.peek_press();
                }
                return false;
            }
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
                Err(error) => app.set_status(error),
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
            } else {
                app.set_status("No notes yet — press n on a hunk to add one");
            }
        }

        _ => {}
    }
    false
}

/// Keys for the outline view. Returns false when the key is not an outline key, so the
/// caller can pass it to the normal handler (mode switching, tabs, quitting still work).
pub fn handle_outline_key(app: &mut App, key: event::KeyEvent) -> bool {
    if app.outline.is_none() {
        return false;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.close_outline(),
        KeyCode::Char('o') => app.show_overview(false),
        KeyCode::Char('j') | KeyCode::Down => app.outline_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.outline_move(-1),
        KeyCode::Char('d') if ctrl => app.outline_move(app.half_page_size() as isize),
        KeyCode::Char('u') if ctrl => app.outline_move(-(app.half_page_size() as isize)),
        KeyCode::PageDown => app.outline_move(app.page_size() as isize),
        KeyCode::PageUp => app.outline_move(-(app.page_size() as isize)),
        KeyCode::Char('g') | KeyCode::Home => app.outline_jump_to_end(false),
        KeyCode::Char('G') | KeyCode::End => app.outline_jump_to_end(true),
        KeyCode::Enter => app.outline_jump(),
        KeyCode::Char('t') => app.show_overview(true),
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => app.outline_set_expanded(true),
        KeyCode::Left | KeyCode::Char('h') => app.outline_set_expanded(false),
        KeyCode::Char('y') => match app.outline_markdown() {
            Some(text) => app.copy_to_clipboard(text, "change outline as markdown"),
            None => app.set_status("Nothing to copy"),
        },
        _ => return false,
    }
    true
}

/// Keys for the `B` popup. Returns a mode to switch to, which the caller applies; the
/// popup itself stays open only for the commits-only checkbox.
pub fn handle_compare_picker_key(app: &mut App, key: event::KeyEvent) -> Option<DiffMode> {
    let rows = app.compare_rows();
    let picker = app.compare_picker.as_mut()?;
    let last = rows.len().saturating_sub(1);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.compare_picker = None;
            None
        }
        KeyCode::Up | KeyCode::BackTab => {
            picker.selected = picker.selected.saturating_sub(1);
            None
        }
        KeyCode::Down | KeyCode::Tab => {
            picker.selected = (picker.selected + 1).min(last);
            None
        }
        KeyCode::Char('p' | 'k') if ctrl => {
            picker.selected = picker.selected.saturating_sub(1);
            None
        }
        KeyCode::Char('n' | 'j') if ctrl => {
            picker.selected = (picker.selected + 1).min(last);
            None
        }
        KeyCode::Backspace => {
            picker.query.pop();
            None
        }
        KeyCode::Char(' ') if rows.get(picker.selected) != Some(&CompareRow::CustomRef) => {
            toggle_commits_only(app)
        }
        KeyCode::Enter => {
            let row = rows.get(picker.selected)?.clone();
            let commits_only = app.repos[app.active_tab].commits_only;
            let chosen = match row {
                CompareRow::Base { base, .. } => Some(DiffMode::Branch { base, commits_only }),
                CompareRow::Staged => Some(DiffMode::Staged),
                CompareRow::Unstaged => Some(DiffMode::Unstaged),
                CompareRow::CommitsOnly => return toggle_commits_only(app),
                CompareRow::CustomRef => {
                    let name = picker.query.trim().to_string();
                    if name.is_empty() {
                        return None;
                    }
                    Some(DiffMode::Branch {
                        base: Base::Ref(name),
                        commits_only,
                    })
                }
            };
            app.compare_picker = None;
            chosen
        }
        KeyCode::Char(c) => {
            picker.query.push(c);
            picker.selected = last;
            None
        }
        _ => None,
    }
}

/// Flip the commits-only checkbox. The picker stays open; when a branch comparison is
/// already showing, it refreshes so the change is visible behind the popup.
fn toggle_commits_only(app: &mut App) -> Option<DiffMode> {
    let repo = &mut app.repos[app.active_tab];
    repo.commits_only = !repo.commits_only;
    match &repo.mode {
        DiffMode::Branch { base, .. } => Some(DiffMode::Branch {
            base: base.clone(),
            commits_only: repo.commits_only,
        }),
        _ => None,
    }
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
        KeyCode::Up | KeyCode::BackTab => {
            if let Some(ref mut picker) = app.file_picker {
                picker.selected = picker.selected.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            let max = app.filtered_file_indices().len().saturating_sub(1);
            if let Some(ref mut picker) = app.file_picker {
                picker.selected = (picker.selected + 1).min(max);
            }
        }
        KeyCode::Char('p' | 'k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(ref mut picker) = app.file_picker {
                picker.selected = picker.selected.saturating_sub(1);
            }
        }
        KeyCode::Char('n' | 'j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
        KeyCode::Up | KeyCode::BackTab => {
            if let Some(ref mut adder) = app.repo_adder {
                adder.cursor = adder.cursor.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            if let Some(ref mut adder) = app.repo_adder {
                let max = adder.results.len().saturating_sub(1);
                adder.cursor = (adder.cursor + 1).min(max);
            }
        }
        KeyCode::Char('p' | 'k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(ref mut adder) = app.repo_adder {
                adder.cursor = adder.cursor.saturating_sub(1);
            }
        }
        KeyCode::Char('n' | 'j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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

/// Keys while the peek is up: `p` again (or Esc) closes it, the usual keys scroll,
/// `]` / `[` step between the added lines.
pub fn handle_peek_key(app: &mut App, key: event::KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let half = (app.layout.content_height as isize / 2).max(1);
    match key.code {
        KeyCode::Char('p') if key.kind == KeyEventKind::Repeat => app.peek_repeat(),
        KeyCode::Char('p') => app.peek_press(),
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => app.close_peek(),
        KeyCode::Down | KeyCode::Char('j') => app.peek_scroll_by(1),
        KeyCode::Up | KeyCode::Char('k') => app.peek_scroll_by(-1),
        KeyCode::Char('d') if ctrl => app.peek_scroll_by(half),
        KeyCode::Char('u') if ctrl => app.peek_scroll_by(-half),
        KeyCode::PageDown => app.peek_scroll_by(half * 2),
        KeyCode::PageUp => app.peek_scroll_by(-(half * 2)),
        KeyCode::Char('g') => app.peek_scroll_to(0),
        KeyCode::Char('G') => app.peek_scroll_to_bottom(),
        KeyCode::Char(']') => app.peek_next_change(true),
        KeyCode::Char('[') => app.peek_next_change(false),
        _ => {}
    }
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

/// Byte index of the start of the line containing `index`.
fn line_start(text: &str, index: usize) -> usize {
    let index = index.min(text.len());
    text[..index].rfind('\n').map(|pos| pos + 1).unwrap_or(0)
}

/// Byte index of the end of the line containing `index` (before its newline).
fn line_end(text: &str, index: usize) -> usize {
    let index = index.min(text.len());
    text[index..]
        .find('\n')
        .map(|pos| index + pos)
        .unwrap_or(text.len())
}

/// Column (in chars) of `index` within its line.
fn column_of(text: &str, index: usize) -> usize {
    text[line_start(text, index)..index].chars().count()
}

/// Byte index at `column` chars into the line starting at `start`, clamped to that line.
fn index_at_column(text: &str, start: usize, column: usize) -> usize {
    let end = line_end(text, start);
    text[start..end]
        .char_indices()
        .nth(column)
        .map(|(offset, _)| start + offset)
        .unwrap_or(end)
}

fn cursor_line_up(text: &str, index: usize) -> usize {
    let start = line_start(text, index);
    if start == 0 {
        return 0;
    }
    let column = column_of(text, index);
    let previous_start = line_start(text, start - 1);
    index_at_column(text, previous_start, column)
}

fn cursor_line_down(text: &str, index: usize) -> usize {
    let end = line_end(text, index);
    if end >= text.len() {
        return text.len();
    }
    let column = column_of(text, index);
    index_at_column(text, end + 1, column)
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
                app.set_status("Note removed");
            } else {
                app.add_or_update_comment(input.file_idx, input.hunk_idx, text);
                let count = app
                    .repos
                    .get(app.active_tab)
                    .map(|repo| repo.comments.len())
                    .unwrap_or(0);
                app.set_status(format!(
                    "Note saved ({count} total) — Y copies all notes as markdown"
                ));
            }
        }
        KeyCode::Delete => {
            if let Some(ref mut input) = app.comment_input
                && input.cursor_pos < input.text.len()
            {
                let end = next_char_boundary(&input.text, input.cursor_pos);
                input.text.drain(input.cursor_pos..end);
            }
        }
        KeyCode::Home => {
            if let Some(ref mut input) = app.comment_input {
                input.cursor_pos = line_start(&input.text, input.cursor_pos);
            }
        }
        KeyCode::End => {
            if let Some(ref mut input) = app.comment_input {
                input.cursor_pos = line_end(&input.text, input.cursor_pos);
            }
        }
        KeyCode::Up => {
            if let Some(ref mut input) = app.comment_input {
                input.cursor_pos = cursor_line_up(&input.text, input.cursor_pos);
            }
        }
        KeyCode::Down => {
            if let Some(ref mut input) = app.comment_input {
                input.cursor_pos = cursor_line_down(&input.text, input.cursor_pos);
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

pub fn handle_comment_browser_key(app: &mut App, key: event::KeyEvent) {
    if app.comment_browser.is_none() {
        return;
    }

    let filtered_indices = app.filtered_comment_indices();

    match key.code {
        KeyCode::Esc => {
            app.comment_browser = None;
        }
        KeyCode::Up | KeyCode::BackTab => {
            if let Some(ref mut browser) = app.comment_browser {
                browser.selected = browser.selected.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            if let Some(ref mut browser) = app.comment_browser {
                let max = filtered_indices.len().saturating_sub(1);
                browser.selected = (browser.selected + 1).min(max);
            }
        }
        KeyCode::Char('p' | 'k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(ref mut browser) = app.comment_browser {
                browser.selected = browser.selected.saturating_sub(1);
            }
        }
        KeyCode::Char('n' | 'j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(ref mut browser) = app.comment_browser {
                let max = filtered_indices.len().saturating_sub(1);
                browser.selected = (browser.selected + 1).min(max);
            }
        }
        KeyCode::Char(' ') => {
            let selected = app.comment_browser.as_ref().unwrap().selected;
            if let Some(&comment_idx) = filtered_indices.get(selected)
                && let Some(ref mut browser) = app.comment_browser
            {
                if browser.checked.contains(&comment_idx) {
                    browser.checked.remove(&comment_idx);
                } else {
                    browser.checked.insert(comment_idx);
                }
            }
        }
        KeyCode::Enter => {
            // Jump to the selected comment's hunk
            let selected = app.comment_browser.as_ref().unwrap().selected;
            let file_idx = filtered_indices.get(selected).and_then(|&comment_idx| {
                app.repos
                    .get(app.active_tab)
                    .and_then(|repo| repo.comments.get(comment_idx))
                    .map(|comment| comment.file_idx)
            });
            if let Some(file_idx) = file_idx {
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
            if let Some(&comment_idx) = filtered_indices.get(selected) {
                app.repos[app.active_tab].comments.remove(comment_idx);
                app.invalidate_layouts(app.active_tab);
                let new_count = app.repos[app.active_tab].comments.len();
                if new_count == 0 {
                    app.comment_browser = None;
                    return;
                }
                let filtered_count = app.filtered_comment_indices().len();
                if let Some(ref mut browser) = app.comment_browser {
                    browser.checked.remove(&comment_idx);
                    let new_checked: std::collections::HashSet<usize> = browser
                        .checked
                        .iter()
                        .map(|&index| {
                            if index > comment_idx {
                                index - 1
                            } else {
                                index
                            }
                        })
                        .collect();
                    browser.checked = new_checked;
                    browser.selected = browser.selected.min(filtered_count.saturating_sub(1));
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
                let count = checked.len();
                let label = format!(
                    "{} note{} as markdown",
                    count,
                    if count == 1 { "" } else { "s" }
                );
                app.copy_to_clipboard(text, &label);
                app.comment_browser = None;
            } else {
                app.set_status("No notes checked — press space to check one");
            }
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
        MAX_MARKDOWN_PREVIEW_BYTES, handle_comment_browser_key, handle_compare_picker_key,
        next_char_boundary, previous_char_boundary, read_markdown_preview,
    };
    use crate::app::{App, CommentBrowserState, CompareRow, HunkComment};
    use crate::diff::{FileDiff, FileStatus};
    use crate::git::{Base, BaseCandidates, DiffMode, RepoInfo};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn app_with_filtered_comment_browser() -> App {
        let mut app = App::new(vec![RepoInfo {
            name: "repo".to_string(),
            path: PathBuf::from("/repo"),
        }]);
        app.repos[0].files = ["first.rs", "later.rs"]
            .into_iter()
            .map(|path| FileDiff {
                path: path.to_string(),
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
        app.repos[0].comments = vec![
            HunkComment {
                file_idx: 0,
                hunk_idx: 0,
                text: "first note".to_string(),
            },
            HunkComment {
                file_idx: 1,
                hunk_idx: 0,
                text: "later matching note".to_string(),
            },
        ];
        app.comment_browser = Some(CommentBrowserState {
            query: "later".to_string(),
            selected: 0,
            checked: HashSet::from([0, 1]),
        });
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        handle_comment_browser_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn filtered_comment_browser_actions_target_the_displayed_comment() {
        let mut app = app_with_filtered_comment_browser();

        press(&mut app, KeyCode::Down);
        assert_eq!(app.comment_browser.as_ref().unwrap().selected, 0);

        press(&mut app, KeyCode::Char(' '));
        assert_eq!(
            app.comment_browser.as_ref().unwrap().checked,
            HashSet::from([0])
        );

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focused_file, Some(1));

        app.comment_browser = Some(CommentBrowserState {
            query: "later".to_string(),
            selected: 0,
            checked: HashSet::from([0, 1]),
        });
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.repos[0].comments.len(), 1);
        assert_eq!(app.repos[0].comments[0].text, "first note");
        let browser = app.comment_browser.as_ref().unwrap();
        assert_eq!(browser.selected, 0);
        assert_eq!(browser.checked, HashSet::from([0]));
    }

    fn main_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        super::handle_key_bounded(app, KeyEvent::new(code, modifiers), &tx)
    }

    #[test]
    fn escape_closes_help_but_never_quits() {
        let mut app = app_with_filtered_comment_browser();
        app.comment_browser = None;
        app.show_help = true;
        assert!(!main_key(&mut app, KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_help);
        assert!(!main_key(&mut app, KeyCode::Esc, KeyModifiers::NONE));
    }

    #[test]
    fn q_and_ctrl_c_quit_while_ctrl_d_scrolls() {
        let mut app = app_with_filtered_comment_browser();
        app.comment_browser = None;
        assert!(main_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(main_key(
            &mut app,
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ));
        assert!(!main_key(
            &mut app,
            KeyCode::Char('d'),
            KeyModifiers::CONTROL
        ));
        assert!(
            app.repos[0].comments.len() == 2,
            "Ctrl+D must not clear notes"
        );
    }

    #[test]
    fn note_editor_moves_between_lines_with_home_end_and_arrows() {
        let text = "first line\nsecond";
        assert_eq!(super::line_start(text, 13), 11);
        assert_eq!(super::line_end(text, 2), 10);
        assert_eq!(super::cursor_line_down(text, 3), 14);
        assert_eq!(super::cursor_line_up(text, 14), 3);
        assert_eq!(super::cursor_line_down(text, 8), 17);
        assert_eq!(super::cursor_line_up(text, 4), 0);
    }

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

    fn stacked_app() -> App {
        let mut app = App::new(vec![RepoInfo {
            name: "repo".to_string(),
            path: PathBuf::from("/repo"),
        }]);
        app.repos[0].bases = Some(BaseCandidates {
            parent: Some("pr2".to_string()),
            trunk: Some("main".to_string()),
            upstream: Some("origin/pr3".to_string()),
            branch: Some("pr3".to_string()),
        });
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn compare_rows_list_each_distinct_base_once() {
        let mut app = stacked_app();
        let names: Vec<String> = app
            .compare_rows()
            .into_iter()
            .filter_map(|row| match row {
                CompareRow::Base { name, .. } => Some(name),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["pr2", "main", "origin/pr3"]);

        // On main, trunk and upstream both mean origin/main: one row, not two.
        app.repos[0].bases = Some(BaseCandidates {
            parent: None,
            trunk: Some("main".to_string()),
            upstream: Some("origin/main".to_string()),
            branch: Some("main".to_string()),
        });
        let rows = app.compare_rows();
        assert_eq!(
            rows,
            [
                CompareRow::Base {
                    base: Base::Trunk,
                    name: "origin/main".to_string(),
                    detail: "trunk",
                },
                CompareRow::Staged,
                CompareRow::Unstaged,
                CompareRow::CommitsOnly,
                CompareRow::CustomRef,
            ]
        );
    }

    #[test]
    fn compare_picker_selects_bases_and_remembers_commits_only() {
        let mut app = stacked_app();
        app.open_compare_picker();
        assert_eq!(app.compare_picker.as_ref().unwrap().selected, 0);

        // Down to trunk, Enter: a branch comparison with the working tree included.
        assert_eq!(
            handle_compare_picker_key(&mut app, key(KeyCode::Down)),
            None
        );
        let chosen = handle_compare_picker_key(&mut app, key(KeyCode::Enter));
        assert_eq!(
            chosen,
            Some(DiffMode::Branch {
                base: Base::Trunk,
                commits_only: false,
            })
        );
        assert!(
            app.compare_picker.is_none(),
            "choosing a base closes the popup"
        );

        // Reopen on the current base, tick commits only (stays open), pick parent.
        app.repos[0].mode = chosen.unwrap();
        app.open_compare_picker();
        assert_eq!(app.compare_picker.as_ref().unwrap().selected, 1);
        assert_eq!(
            handle_compare_picker_key(&mut app, key(KeyCode::Char(' '))),
            Some(DiffMode::Branch {
                base: Base::Trunk,
                commits_only: true
            })
        );
        assert!(app.compare_picker.is_some());
        assert!(app.repos[0].commits_only);
        assert_eq!(handle_compare_picker_key(&mut app, key(KeyCode::Up)), None);
        assert_eq!(
            handle_compare_picker_key(&mut app, key(KeyCode::Enter)),
            Some(DiffMode::Branch {
                base: Base::Parent,
                commits_only: true,
            })
        );
        assert_eq!(
            app.branch_mode(),
            DiffMode::Branch {
                base: Base::Parent,
                commits_only: true,
            },
            "b keeps the commits-only choice"
        );
    }

    #[test]
    fn compare_picker_typing_targets_the_custom_ref_row() {
        let mut app = stacked_app();
        app.open_compare_picker();
        for c in "v1.2".chars() {
            assert_eq!(
                handle_compare_picker_key(&mut app, key(KeyCode::Char(c))),
                None
            );
        }
        let rows = app.compare_rows();
        assert_eq!(
            app.compare_picker.as_ref().unwrap().selected,
            rows.len() - 1
        );
        assert_eq!(
            handle_compare_picker_key(&mut app, key(KeyCode::Enter)),
            Some(DiffMode::Branch {
                base: Base::Ref("v1.2".to_string()),
                commits_only: false,
            })
        );

        // An empty custom ref is not a comparison; Enter does nothing.
        app.open_compare_picker();
        app.compare_picker.as_mut().unwrap().selected = rows.len() - 1;
        assert_eq!(
            handle_compare_picker_key(&mut app, key(KeyCode::Enter)),
            None
        );
        assert!(app.compare_picker.is_some());
        assert_eq!(handle_compare_picker_key(&mut app, key(KeyCode::Esc)), None);
        assert!(app.compare_picker.is_none());
    }
}
