use super::{App, DiffResult};
use crate::diff;
use arboard::Clipboard;
use crossterm::event::{self, MouseButton, MouseEventKind};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Returns true if the event changed state (needs redraw).
pub fn handle_mouse(app: &mut App, mouse: event::MouseEvent, diff_tx: &mpsc::UnboundedSender<DiffResult>) -> bool {
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
                    let next = app.repos[app.active_tab].mode.next();
                    app.repos[app.active_tab].mode = next;
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
