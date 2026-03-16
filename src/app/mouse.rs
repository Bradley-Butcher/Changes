use super::{App, CommentInputState, DOUBLE_CLICK_MS, DiffResult, GapExpandResult, SCROLL_SPEED};
use crate::viewport::RowRef;
use arboard::Clipboard;
use crossterm::event::{self, MouseButton, MouseEventKind};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Returns true if the event changed state (needs redraw).
pub fn handle_mouse(
    app: &mut App,
    mouse: event::MouseEvent,
    diff_tx: &mpsc::UnboundedSender<DiffResult>,
    gap_tx: &mpsc::UnboundedSender<GapExpandResult>,
) -> bool {
    // Scroll wheel in markdown preview
    if app.markdown_preview.is_some() {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if let Some(ref mut preview) = app.markdown_preview {
                    preview.scroll = preview.scroll.saturating_sub(SCROLL_SPEED);
                }
                return true;
            }
            MouseEventKind::ScrollDown => {
                if let Some(ref mut preview) = app.markdown_preview {
                    preview.scroll = preview.scroll.saturating_add(SCROLL_SPEED);
                }
                return true;
            }
            _ => return false,
        }
    }

    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.scroll_active_viewport(-(SCROLL_SPEED as isize));
            true
        }
        MouseEventKind::ScrollDown => {
            app.scroll_active_viewport(SCROLL_SPEED as isize);
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
                    && now.duration_since(prev_time) < Duration::from_millis(DOUBLE_CLICK_MS)
            } else {
                false
            };
            app.last_click = Some((click_row, click_col, now));

            if is_double_click && click_row >= app.layout.content_y {
                let content_row = (click_row as usize)
                    .saturating_sub(app.layout.content_y as usize)
                    + app.current_scroll_offset();
                if let Some(text) = app.copy_hunk_at_row(content_row)
                    && let Ok(mut clipboard) = Clipboard::new()
                {
                    let _ = clipboard.set_text(text);
                }
                app.last_click = None;
                return true;
            }

            // Status bar badges
            if click_row == app.layout.status_bar_row {
                let (ms, me) = app.layout.mode_badge_pos;
                let (vs, ve) = app.layout.view_badge_pos;
                if click_col >= ms && click_col < me {
                    let next = app.current_mode().next();
                    app.set_mode(next, diff_tx);
                    return true;
                }
                if click_col >= vs && click_col < ve {
                    app.side_by_side = !app.side_by_side;
                    app.prepare_active_layout();
                    app.clamp_active_viewport();
                    return true;
                }
                return false;
            }

            // Tab bar (rows 0-2)
            if click_row <= 2 {
                for (i, &(start, end)) in app.layout.tab_positions.iter().enumerate() {
                    if click_col >= start && click_col < end {
                        app.active_tab = i;
                        app.jump_active_viewport_top();
                        app.focused_file = None;
                        return true;
                    }
                }
                return false;
            }

            // Content area click
            let content_row = (click_row as usize).saturating_sub(app.layout.content_y as usize)
                + app.current_scroll_offset();

            // Check for expand row click (gap between hunks) — async
            app.prepare_active_layout();
            if let Some((file_idx, gap_idx)) = app
                .current_layout()
                .and_then(|layout| layout.expand_gap_at_row(content_row))
            {
                if let Some(req) = app.start_expand_gap(file_idx, gap_idx) {
                    let tx = gap_tx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(req.execute());
                    });
                }
                return true;
            }

            // File header collapse toggle
            if let Some(RowRef::FileHeader { file_idx }) = app
                .current_layout()
                .and_then(|layout| layout.row(content_row))
            {
                app.focused_file = Some(file_idx);
                app.toggle_collapsed(file_idx);
                return true;
            }

            app.focused_file = app
                .current_layout()
                .and_then(|layout| layout.row_file_idx(content_row))
                .or_else(|| app.focused_file_from_scroll());
            true
        }
        MouseEventKind::Down(MouseButton::Middle) => {
            if let Some(text) = app.copy_hunk_at_focus()
                && let Ok(mut clipboard) = Clipboard::new()
            {
                let _ = clipboard.set_text(text);
            }
            true
        }
        MouseEventKind::Down(MouseButton::Right) => {
            // Right-click on a hunk to add/edit a comment
            if app.comment_input.is_some()
                || app.file_picker.is_some()
                || app.repo_adder.is_some()
                || app.comment_browser.is_some()
            {
                return false;
            }
            if mouse.row < app.layout.content_y {
                return false;
            }
            let content_row = (mouse.row as usize).saturating_sub(app.layout.content_y as usize)
                + app.current_scroll_offset();

            app.prepare_active_layout();
            let Some((file_idx, hunk_idx)) = app
                .current_layout()
                .and_then(|layout| layout.hunk_at_row(content_row))
            else {
                return false;
            };

            let anchor_row = content_row;

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
                anchor_row,
            });
            true
        }
        // Ignore move/release/drag — no state change, no redraw
        _ => false,
    }
}
