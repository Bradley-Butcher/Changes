use super::{App, DOUBLE_CLICK_MS, DOUBLE_CLICK_SLOP, GapExpandResult, SCROLL_SPEED};
use crate::viewport::RowRef;
use crossterm::event::{self, MouseButton, MouseEventKind};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Clone)]
enum GapSender {
    Bounded(mpsc::Sender<GapExpandResult>),
    Unbounded(mpsc::UnboundedSender<GapExpandResult>),
}

impl GapSender {
    fn send(self, result: GapExpandResult) {
        match self {
            Self::Bounded(tx) => {
                let _ = tx.blocking_send(result);
            }
            Self::Unbounded(tx) => {
                let _ = tx.send(result);
            }
        }
    }
}

/// Returns true if the event changed state (needs redraw).
pub fn handle_mouse(
    app: &mut App,
    mouse: event::MouseEvent,
    gap_tx: &mpsc::UnboundedSender<GapExpandResult>,
) -> bool {
    handle_mouse_with_sender(app, mouse, GapSender::Unbounded(gap_tx.clone()))
}

pub(crate) fn handle_mouse_bounded(
    app: &mut App,
    mouse: event::MouseEvent,
    gap_tx: &mpsc::Sender<GapExpandResult>,
) -> bool {
    handle_mouse_with_sender(app, mouse, GapSender::Bounded(gap_tx.clone()))
}

fn handle_mouse_with_sender(app: &mut App, mouse: event::MouseEvent, gap_tx: GapSender) -> bool {
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

    // While a popup is open, clicks must not reach the tabs or the diff underneath it:
    // switching tabs mid-edit would save the note into the wrong repository.
    if app.modal_open() && matches!(mouse.kind, MouseEventKind::Down(_)) {
        return false;
    }

    // The outline view owns scrolling and clicks inside the content area.
    if app.outline.is_some() {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                app.outline_move(-(SCROLL_SPEED as isize));
                return true;
            }
            MouseEventKind::ScrollDown => {
                app.outline_move(SCROLL_SPEED as isize);
                return true;
            }
            MouseEventKind::Down(MouseButton::Left) if mouse.row >= app.layout.content_y => {
                let row = (mouse.row as usize).saturating_sub(app.layout.content_y as usize)
                    + app.outline.as_ref().map_or(0, |state| state.scroll);
                if app.outline_select_row(row) {
                    app.outline_jump();
                }
                return true;
            }
            MouseEventKind::Down(_) if mouse.row >= app.layout.content_y => return false,
            _ => {}
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

            // Double-click detection (same row, nearly the same column, within 400ms)
            let is_double_click = if let Some((prev_row, prev_col, prev_time)) = app.last_click {
                prev_row == click_row
                    && prev_col.abs_diff(click_col) <= DOUBLE_CLICK_SLOP
                    && now.duration_since(prev_time) < Duration::from_millis(DOUBLE_CLICK_MS)
            } else {
                false
            };
            app.last_click = Some((click_row, click_col, now));

            if is_double_click && click_row >= app.layout.content_y {
                let content_row = (click_row as usize)
                    .saturating_sub(app.layout.content_y as usize)
                    + app.current_scroll_offset();
                app.copy_hunk_with_feedback(Some(content_row));
                app.last_click = None;
                return true;
            }

            // Status bar badges
            if click_row == app.layout.status_bar_row {
                let (ms, me) = app.layout.mode_badge_pos;
                let (vs, ve) = app.layout.view_badge_pos;
                if click_col >= ms && click_col < me {
                    app.open_compare_picker();
                    return true;
                }
                if click_col >= vs && click_col < ve {
                    app.toggle_view();
                    return true;
                }
                return false;
            }

            // Tab bar
            if click_row == app.layout.tab_bar_row {
                for (i, &(start, end)) in app.layout.tab_positions.iter().enumerate() {
                    if click_col >= start && click_col < end {
                        app.switch_tab(i);
                        return true;
                    }
                }
                return false;
            }
            if click_row < app.layout.content_y {
                return false;
            }

            // A click on the pinned file header acts on that file, not the row underneath.
            if click_row == app.layout.content_y
                && let Some(file_idx) = app.sticky_header_file()
            {
                app.focused_file = Some(file_idx);
                app.toggle_collapsed(file_idx);
                app.jump_to_file(file_idx);
                return true;
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
                        tx.send(req.execute());
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

            // Clicking inside a hunk selects it for y / n / N.
            if let Some((file_idx, hunk_idx)) = app.file_and_hunk_at_row(content_row) {
                app.select_hunk(file_idx, hunk_idx);
                return true;
            }
            app.focused_file = app
                .current_layout()
                .and_then(|layout| layout.row_file_idx(content_row))
                .or_else(|| app.focused_file_from_scroll());
            true
        }
        MouseEventKind::Down(MouseButton::Middle) => {
            app.copy_hunk_with_feedback(None);
            true
        }
        MouseEventKind::Down(MouseButton::Right) => {
            // Right-click on a hunk to add/edit a comment
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

            app.open_comment_input(file_idx, hunk_idx, Some(content_row));
            true
        }
        // Ignore move/release/drag — no state change, no redraw
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::handle_mouse_bounded;
    use crate::app::{App, CommentInputState};
    use crate::git::RepoInfo;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use std::path::PathBuf;

    #[test]
    fn clicks_are_ignored_while_a_note_is_being_edited() {
        let mut app = App::new(vec![
            RepoInfo {
                name: "one".to_string(),
                path: PathBuf::from("/one"),
            },
            RepoInfo {
                name: "two".to_string(),
                path: PathBuf::from("/two"),
            },
        ]);
        app.layout.tab_bar_row = 0;
        app.layout.tab_positions = vec![(0, 10), (10, 20)];
        app.comment_input = Some(CommentInputState {
            file_idx: 0,
            hunk_idx: 0,
            text: "draft".to_string(),
            cursor_pos: 5,
            anchor_row: 0,
        });

        let (gap_tx, _gap_rx) = tokio::sync::mpsc::channel(1);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 15,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert!(!handle_mouse_bounded(&mut app, click, &gap_tx));
        assert_eq!(
            app.active_tab, 0,
            "a tab click must not switch repos mid-edit"
        );
        assert!(app.comment_input.is_some());
    }
}
