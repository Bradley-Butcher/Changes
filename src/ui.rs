use crate::app::App;
use crate::diff::{FileStatus, LineKind};
use crate::highlight::Highlighter;
use crate::viewport::RowRef;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};

const BG_ADD: Color = Color::Rgb(30, 60, 30);
const BG_DEL: Color = Color::Rgb(60, 30, 30);
const BG_ADD_EMPH: Color = Color::Rgb(40, 90, 40);
const BG_DEL_EMPH: Color = Color::Rgb(90, 40, 40);
const FG_ADD: Color = Color::Rgb(100, 220, 100);
const FG_DEL: Color = Color::Rgb(220, 100, 100);
const FG_HUNK: Color = Color::Rgb(130, 170, 220);
const FG_MUTED: Color = Color::Rgb(120, 120, 120);
const BG_HEADER: Color = Color::Rgb(35, 40, 55);
const BG_FLASH: Color = Color::Rgb(80, 80, 40);
const FG_EXPAND: Color = Color::Rgb(80, 130, 180);
const FG_STATUS_M: Color = Color::Rgb(220, 180, 60);
const FG_STATUS_A: Color = Color::Rgb(100, 220, 100);
const FG_STATUS_D: Color = Color::Rgb(220, 100, 100);
const FG_STATUS_R: Color = Color::Rgb(130, 170, 220);
const FG_PATH_DIR: Color = Color::Rgb(140, 140, 160);
const FG_PATH_FILE: Color = Color::Rgb(240, 240, 250);
const FG_COMMENT: Color = Color::Rgb(220, 180, 60);

/// Layout positions computed during rendering, needed for mouse hit-testing.
/// Kept separate from App so `draw()` doesn't require `&mut App`.
#[derive(Default)]
pub struct LayoutHints {
    pub tab_positions: Vec<(u16, u16)>,
    pub mode_badge_pos: (u16, u16),
    pub view_badge_pos: (u16, u16),
    pub status_bar_row: u16,
    pub content_y: u16,
    pub content_height: u16,
    pub content_width: u16,
}

pub fn draw(frame: &mut Frame, app: &App, highlighter: &Highlighter, hints: &mut LayoutHints) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tab bar
            Constraint::Min(1),    // diff area
            Constraint::Length(1), // status bar
        ])
        .split(frame.area());

    // Compute content area top for mouse hit-testing
    let diff_inner = Block::default().borders(Borders::ALL).inner(chunks[1]);
    hints.content_y = diff_inner.y;
    hints.content_height = diff_inner.height;
    hints.content_width = diff_inner.width;

    draw_tab_bar(frame, app, hints, chunks[0]);
    draw_diff_area(frame, app, highlighter, chunks[1]);
    draw_status_bar(frame, app, hints, chunks[2]);

    if app.markdown_preview.is_some() {
        draw_markdown_preview(frame, app);
    } else if app.comment_input.is_some() {
        draw_comment_input(frame, app);
    } else if app.comment_browser.is_some() {
        draw_comment_browser(frame, app);
    } else if app.repo_adder.is_some() {
        draw_repo_adder(frame, app);
    } else if app.file_picker.is_some() {
        draw_file_picker(frame, app);
    } else if app.show_help {
        draw_help_overlay(frame);
    }
}

fn draw_tab_bar(frame: &mut Frame, app: &App, hints: &mut LayoutHints, area: Rect) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build the tab line manually for full control
    let mut spans: Vec<Span> = Vec::new();
    let mut positions: Vec<(u16, u16)> = Vec::new();
    let mut col = inner.x;

    for (i, repo) in app.repos.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" | ", Style::default().fg(FG_MUTED)));
            col += 3;
        }

        let start = col;
        let label = format!(" {} ", repo.info.name);
        let width = label.len() as u16;

        if i == app.active_tab {
            spans.push(Span::styled(
                label,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(label, Style::default().fg(Color::White)));
        }

        col += width;
        positions.push((start, col));
    }

    // Widen click targets: first tab starts at 0, last extends to edge,
    // others split the divider space with neighbors
    for i in 0..positions.len() {
        if i == 0 {
            positions[i].0 = area.x;
        }
        if i == positions.len() - 1 {
            positions[i].1 = area.x + area.width;
        }
    }
    hints.tab_positions = positions;

    let tab_line = Paragraph::new(Line::from(spans));
    frame.render_widget(tab_line, inner);
}

fn draw_empty_state(frame: &mut Frame, area: Rect) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let logo_lines = [
        r#"██████╗  ██╗  ██╗  █████╗  ███╗   ██╗  ██████╗  ███████╗ ███████╗"#,
        r#"██╔════╝  ██║  ██║ ██╔══██╗ ████╗  ██║ ██╔════╝  ██╔════╝ ██╔════╝"#,
        r#"██║       ███████║ ███████║ ██╔██╗ ██║ ██║  ███╗ █████╗   ███████╗"#,
        r#"██║       ██╔══██║ ██╔══██║ ██║╚██╗██║ ██║   ██║ ██╔══╝   ╚════██║"#,
        r#"╚██████╗  ██║  ██║ ██║  ██║ ██║ ╚████║ ╚██████╔╝ ███████╗ ███████║"#,
        r#" ╚═════╝  ╚═╝  ╚═╝ ╚═╝  ╚═╝ ╚═╝  ╚═══╝  ╚═════╝  ╚══════╝ ╚══════╝"#,
    ];

    let quote1 = r#"> git status"#;
    let quote2 = r#"> I see no changes ... working tree clean"#;

    // Total content height: logo (6) + blank + quote1 + quote2 = 9
    let content_height = 9u16;
    let top_pad = inner.height.saturating_sub(content_height) / 2;

    let mut lines: Vec<Line> = Vec::new();
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }
    for logo_line in &logo_lines {
        lines.push(Line::from(Span::styled(
            *logo_line,
            Style::default()
                .fg(Color::Rgb(100, 180, 255))
                .add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        quote1,
        Style::default().fg(FG_MUTED),
    )));
    lines.push(Line::from(Span::styled(
        quote2,
        Style::default().fg(FG_MUTED).add_modifier(Modifier::ITALIC),
    )));

    let para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(para, inner);
}

fn draw_diff_area(frame: &mut Frame, app: &App, highlighter: &Highlighter, area: Rect) {
    let files = match app.current_files() {
        Some(f) => f,
        None => {
            draw_empty_state(frame, area);
            return;
        }
    };
    let layout = match app.current_layout() {
        Some(layout) => layout,
        None => {
            draw_empty_state(frame, area);
            return;
        }
    };

    if files.is_empty() {
        draw_empty_state(frame, area);
        return;
    }

    if app.side_by_side {
        draw_side_by_side(frame, app, highlighter, files, layout, area);
    } else {
        draw_unified(frame, app, highlighter, files, layout, area);
    }
}

fn draw_unified(
    frame: &mut Frame,
    app: &App,
    highlighter: &Highlighter,
    files: &[crate::diff::FileDiff],
    layout: &crate::viewport::DiffLayout,
    area: Rect,
) {
    let inner_area = Block::default().borders(Borders::ALL).inner(area);
    let block = Block::default().borders(Borders::ALL);
    frame.render_widget(block, area);

    let visible_height = inner_area.height as usize;
    let total_lines = layout.total_lines();
    let visible_rows = app.visible_row_range(total_lines, visible_height);
    let mut lines: Vec<Line> = Vec::new();

    for row in visible_rows {
        let Some(row_ref) = layout.row(row) else {
            continue;
        };
        match row_ref {
            RowRef::FileHeader { file_idx } => {
                if let Some(file) = files.get(file_idx) {
                    let is_focused = app.focused_file == Some(file_idx);
                    lines.push(build_file_header(file, is_focused, inner_area.width));
                }
            }
            RowRef::HunkHeader {
                file_idx,
                hunk_idx,
                gap_before,
            } => {
                let Some(file) = files.get(file_idx) else {
                    continue;
                };
                let Some(hunk) = file.hunks.get(hunk_idx) else {
                    continue;
                };
                let lno_w = lineno_width(file);
                let has_comment = layout.hunk_has_comment(file_idx, hunk_idx);
                if gap_before > 0 {
                    let mut spans = vec![Span::styled(
                        format_expand_indicator(gap_before, lno_w),
                        Style::default().fg(FG_EXPAND),
                    )];
                    if let Some(ctx) = hunk_context(&hunk.header) {
                        spans.push(Span::styled(
                            format!(" {}", ctx),
                            Style::default().fg(FG_HUNK),
                        ));
                    }
                    if has_comment {
                        spans.push(Span::styled(" [!]", Style::default().fg(FG_COMMENT)));
                    }
                    lines.push(Line::from(spans));
                } else if hunk_idx > 0 {
                    let mut gutter = " ".repeat(lno_w * 2 + 1) + " │";
                    if has_comment {
                        gutter.push_str(" [!]");
                    }
                    lines.push(Line::from(Span::styled(
                        gutter,
                        if has_comment {
                            Style::default().fg(FG_COMMENT)
                        } else {
                            Style::default().fg(FG_MUTED)
                        },
                    )));
                } else if has_comment {
                    lines.push(Line::from(Span::styled(
                        " [!]",
                        Style::default().fg(FG_COMMENT),
                    )));
                } else {
                    lines.push(Line::from(""));
                }
            }
            RowRef::Comment {
                file_idx,
                hunk_idx,
                wrap_idx,
            } => {
                if let Some(text) = layout.comment_line_text(file_idx, hunk_idx, wrap_idx) {
                    let Some(file) = files.get(file_idx) else {
                        continue;
                    };
                    let lno_w = lineno_width(file);
                    let gutter = " ".repeat(lno_w * 2 + 1) + " ┃";
                    lines.push(Line::from(vec![
                        Span::styled(gutter, Style::default().fg(FG_COMMENT)),
                        Span::styled(format!(" {}", text), Style::default().fg(FG_COMMENT)),
                    ]));
                }
            }
            RowRef::UnifiedLine {
                file_idx,
                hunk_idx,
                line_idx,
            } => {
                let Some(file) = files.get(file_idx) else {
                    continue;
                };
                let Some(hunk) = file.hunks.get(hunk_idx) else {
                    continue;
                };
                let Some(line) = hunk.lines.get(line_idx) else {
                    continue;
                };
                let flashing = app.is_hunk_flashing(file_idx, hunk_idx);
                lines.extend(build_unified_line(
                    line,
                    &file.path,
                    highlighter,
                    flashing,
                    lineno_width(file),
                    inner_area.width as usize,
                ));
            }
            RowRef::GapTail { gap_after, .. } if gap_after > 0 => {
                let Some(file_idx) = layout.row_file_idx(row) else {
                    continue;
                };
                let Some(file) = files.get(file_idx) else {
                    continue;
                };
                lines.push(Line::from(Span::styled(
                    format_expand_indicator(gap_after, lineno_width(file)),
                    Style::default().fg(FG_EXPAND),
                )));
            }
            RowRef::Blank { .. } | RowRef::GapTail { .. } => lines.push(Line::from("")),
            RowRef::SideBySideLine { .. } => {}
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner_area);

    if total_lines > visible_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines.saturating_sub(visible_height))
            .position(app.current_scroll_offset());
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn draw_side_by_side(
    frame: &mut Frame,
    app: &App,
    highlighter: &Highlighter,
    files: &[crate::diff::FileDiff],
    layout: &crate::viewport::DiffLayout,
    area: Rect,
) {
    let block = Block::default().borders(Borders::ALL);
    let inner_area = block.inner(area);
    frame.render_widget(block, area);

    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner_area);

    let visible_height = inner_area.height as usize;
    let total_lines = layout.total_lines();
    let visible_rows = app.visible_row_range(total_lines, visible_height);

    let mut left_lines: Vec<Line> = Vec::new();
    let mut right_lines: Vec<Line> = Vec::new();
    for row in visible_rows {
        let Some(row_ref) = layout.row(row) else {
            continue;
        };
        match row_ref {
            RowRef::FileHeader { file_idx } => {
                let Some(file) = files.get(file_idx) else {
                    continue;
                };
                let is_focused = app.focused_file == Some(file_idx);
                let header = build_file_header(file, is_focused, inner_area.width);
                left_lines.push(header);
                right_lines.push(Line::from(Span::styled(
                    " ".repeat(inner_area.width as usize),
                    Style::default().bg(BG_HEADER),
                )));
            }
            RowRef::HunkHeader {
                file_idx,
                hunk_idx,
                gap_before,
            } => {
                let Some(file) = files.get(file_idx) else {
                    continue;
                };
                let ctx = file
                    .hunks
                    .get(hunk_idx)
                    .and_then(|hunk| hunk_context(&hunk.header));
                let has_comment = layout.hunk_has_comment(file_idx, hunk_idx);
                let hunk_line = if gap_before > 0 {
                    let mut spans = vec![Span::styled(
                        format!("  ↕ {}", gap_before),
                        Style::default().fg(FG_EXPAND),
                    )];
                    if let Some(ctx) = ctx {
                        spans.push(Span::styled(
                            format!("  {}", ctx),
                            Style::default().fg(FG_HUNK),
                        ));
                    }
                    if has_comment {
                        spans.push(Span::styled(" [!]", Style::default().fg(FG_COMMENT)));
                    }
                    Line::from(spans)
                } else if hunk_idx > 0 {
                    if has_comment {
                        Line::from(Span::styled("  ─ [!]", Style::default().fg(FG_COMMENT)))
                    } else {
                        Line::from(Span::styled("  ─", Style::default().fg(FG_MUTED)))
                    }
                } else if has_comment {
                    Line::from(Span::styled(" [!]", Style::default().fg(FG_COMMENT)))
                } else {
                    Line::from("")
                };
                left_lines.push(hunk_line.clone());
                right_lines.push(hunk_line);
            }
            RowRef::SideBySideLine {
                file_idx,
                hunk_idx,
                line_idx,
            } => {
                let Some(file) = files.get(file_idx) else {
                    continue;
                };
                let Some(sbs_line) = file
                    .sbs_cache
                    .as_ref()
                    .and_then(|cache| cache.get(hunk_idx))
                    .and_then(|rows| rows.get(line_idx))
                else {
                    continue;
                };
                let pane_w = halves[0].width as usize;
                let mut left = build_sbs_line(
                    &sbs_line.left,
                    &sbs_line.left_changed,
                    &file.path,
                    highlighter,
                    pane_w,
                );
                let mut right = build_sbs_line(
                    &sbs_line.right,
                    &sbs_line.right_changed,
                    &file.path,
                    highlighter,
                    pane_w,
                );
                // Pad shorter side so both panes stay aligned
                while left.len() < right.len() {
                    left.push(Line::from(""));
                }
                while right.len() < left.len() {
                    right.push(Line::from(""));
                }
                left_lines.extend(left);
                right_lines.extend(right);
            }
            RowRef::GapTail { gap_after, .. } if gap_after > 0 => {
                let expand_line = Line::from(Span::styled(
                    format!("  ↕ {}", gap_after),
                    Style::default().fg(FG_EXPAND),
                ));
                left_lines.push(expand_line.clone());
                right_lines.push(expand_line);
            }
            RowRef::Comment {
                file_idx,
                hunk_idx,
                wrap_idx,
            } => {
                if let Some(text) = layout.comment_line_text(file_idx, hunk_idx, wrap_idx) {
                    let comment_line = Line::from(vec![
                        Span::styled(" ┃", Style::default().fg(FG_COMMENT)),
                        Span::styled(format!(" {}", text), Style::default().fg(FG_COMMENT)),
                    ]);
                    left_lines.push(comment_line);
                    right_lines.push(Line::from(""));
                }
            }
            RowRef::Blank { .. } | RowRef::GapTail { .. } => {
                left_lines.push(Line::from(""));
                right_lines.push(Line::from(""));
            }
            RowRef::UnifiedLine { .. } => {}
        }
    }

    let left_para = Paragraph::new(left_lines);
    let right_para = Paragraph::new(right_lines);

    frame.render_widget(left_para, halves[0]);
    frame.render_widget(right_para, halves[1]);

    if total_lines > visible_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines.saturating_sub(visible_height))
            .position(app.current_scroll_offset());
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn build_unified_line<'a>(
    line: &crate::diff::DiffLine,
    file_path: &str,
    highlighter: &Highlighter,
    is_flashing: bool,
    lno_width: usize,
    content_width: usize,
) -> Vec<Line<'a>> {
    let prefix = match line.kind {
        LineKind::Context => "  ",
        LineKind::Addition => "+ ",
        LineKind::Deletion => "- ",
    };

    let lineno = format_lineno(line, lno_width);

    let bg = if is_flashing {
        Some(BG_FLASH)
    } else {
        match line.kind {
            LineKind::Addition => Some(BG_ADD),
            LineKind::Deletion => Some(BG_DEL),
            _ => None,
        }
    };

    let prefix_style = match line.kind {
        LineKind::Addition => Style::default().fg(FG_ADD).bg(bg.unwrap_or_default()),
        LineKind::Deletion => Style::default().fg(FG_DEL).bg(bg.unwrap_or_default()),
        _ => Style::default().fg(FG_MUTED),
    };

    // Gutter width: line numbers + prefix
    let gutter_width = lno_width * 2 + 2 + prefix.len(); // "NNNN NNNN │+ "
    let available = content_width.saturating_sub(gutter_width);

    if available == 0 || line.content.len() <= available {
        let mut spans = vec![
            Span::styled(lineno, Style::default().fg(FG_MUTED)),
            Span::styled(prefix.to_string(), prefix_style),
        ];
        let mut highlighted = highlighter.highlight_line_content(&line.content, file_path, bg);
        spans.append(&mut highlighted.spans);
        return vec![Line::from(spans)];
    }

    // Split content into chunks that fit
    let content = &line.content;
    let padding = " ".repeat(gutter_width);
    let mut result = Vec::new();
    let mut pos = 0;

    while pos < content.len() {
        let end = (pos + available).min(content.len());
        // Try to break at a word boundary
        let chunk_end = if end < content.len() {
            content[pos..end]
                .rfind(' ')
                .map(|i| pos + i + 1)
                .unwrap_or(end)
        } else {
            end
        };
        let chunk = &content[pos..chunk_end];

        if pos == 0 {
            // First line: full gutter
            let mut spans = vec![
                Span::styled(lineno.clone(), Style::default().fg(FG_MUTED)),
                Span::styled(prefix.to_string(), prefix_style),
            ];
            let mut highlighted = highlighter.highlight_line_content(chunk, file_path, bg);
            spans.append(&mut highlighted.spans);
            result.push(Line::from(spans));
        } else {
            // Continuation: padding where gutter would be
            let mut spans = vec![Span::styled(
                padding.clone(),
                Style::default().fg(FG_MUTED),
            )];
            let mut highlighted = highlighter.highlight_line_content(chunk, file_path, bg);
            spans.append(&mut highlighted.spans);
            result.push(Line::from(spans));
        }

        pos = chunk_end;
    }

    result
}

fn build_sbs_line<'a>(
    line_opt: &Option<crate::diff::DiffLine>,
    changed_ranges: &Option<crate::diff::ChangedRanges>,
    file_path: &str,
    highlighter: &Highlighter,
    pane_width: usize,
) -> Vec<Line<'a>> {
    match line_opt {
        Some(line) => {
            let bg = match line.kind {
                LineKind::Addition => Some(BG_ADD),
                LineKind::Deletion => Some(BG_DEL),
                _ => None,
            };

            let prefix = match line.kind {
                LineKind::Addition => "+ ",
                LineKind::Deletion => "- ",
                _ => "  ",
            };

            let prefix_style = match line.kind {
                LineKind::Addition => Style::default().fg(FG_ADD),
                LineKind::Deletion => Style::default().fg(FG_DEL),
                _ => Style::default().fg(FG_MUTED),
            };

            let gutter_width = prefix.len();
            let available = pane_width.saturating_sub(gutter_width);

            if available == 0 || line.content.len() <= available {
                let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
                let mut highlighted =
                    highlighter.highlight_line_content(&line.content, file_path, bg);
                if let Some(ranges) = changed_ranges {
                    let emph_bg = match line.kind {
                        LineKind::Addition => BG_ADD_EMPH,
                        LineKind::Deletion => BG_DEL_EMPH,
                        _ => return vec![Line::from(spans)],
                    };
                    apply_inline_emphasis(&mut highlighted.spans, ranges, emph_bg);
                }
                spans.append(&mut highlighted.spans);
                return vec![Line::from(spans)];
            }

            // Wrap long content
            let content = &line.content;
            let padding = " ".repeat(gutter_width);
            let mut result = Vec::new();
            let mut pos = 0;

            while pos < content.len() {
                let end = (pos + available).min(content.len());
                let chunk_end = if end < content.len() {
                    content[pos..end]
                        .rfind(' ')
                        .map(|i| pos + i + 1)
                        .unwrap_or(end)
                } else {
                    end
                };
                let chunk = &content[pos..chunk_end];

                if pos == 0 {
                    let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
                    let mut highlighted =
                        highlighter.highlight_line_content(chunk, file_path, bg);
                    if let Some(ranges) = changed_ranges {
                        let emph_bg = match line.kind {
                            LineKind::Addition => BG_ADD_EMPH,
                            LineKind::Deletion => BG_DEL_EMPH,
                            _ => {
                                spans.append(&mut highlighted.spans);
                                result.push(Line::from(spans));
                                pos = chunk_end;
                                continue;
                            }
                        };
                        apply_inline_emphasis(&mut highlighted.spans, ranges, emph_bg);
                    }
                    spans.append(&mut highlighted.spans);
                    result.push(Line::from(spans));
                } else {
                    let mut spans =
                        vec![Span::styled(padding.clone(), Style::default().fg(FG_MUTED))];
                    let mut highlighted =
                        highlighter.highlight_line_content(chunk, file_path, bg);
                    spans.append(&mut highlighted.spans);
                    result.push(Line::from(spans));
                }

                pos = chunk_end;
            }

            result
        }
        None => vec![Line::from(Span::styled("~", Style::default().fg(FG_MUTED)))],
    }
}

/// Build a full-width centered file header banner.
fn build_file_header<'a>(file: &crate::diff::FileDiff, is_focused: bool, width: u16) -> Line<'a> {
    let collapse = if file.collapsed { "▶" } else { "▼" };

    let (status_char, status_fg) = match file.status {
        FileStatus::Modified => ("M", FG_STATUS_M),
        FileStatus::Added => ("A", FG_STATUS_A),
        FileStatus::Deleted => ("D", FG_STATUS_D),
        FileStatus::Renamed => ("R", FG_STATUS_R),
        FileStatus::Untracked => ("?", FG_MUTED),
    };

    // Split path into directory + filename
    let (dir, filename) = match file.path.rfind('/') {
        Some(pos) => (&file.path[..=pos], &file.path[pos + 1..]),
        None => ("", file.path.as_str()),
    };

    // For renames where old_path differs, show "old_path → new_path"
    let is_rename = file.status == FileStatus::Renamed
        && file.old_path.as_deref().is_some_and(|old| old != file.path);
    let old_path_str = if is_rename {
        file.old_path.as_deref().unwrap_or("")
    } else {
        ""
    };

    let adds = format!("+{}", file.additions);
    let dels = format!("-{}", file.deletions);

    let bg = BG_HEADER;
    let total_width = width as usize;
    let underline = if is_focused {
        Modifier::UNDERLINED
    } else {
        Modifier::empty()
    };

    // Left-aligned: small indent then content, fill remaining with bg
    let mut spans = vec![
        Span::styled(" ", Style::default().bg(bg)),
        Span::styled(
            format!("{} ", collapse),
            Style::default().fg(FG_MUTED).bg(bg),
        ),
        Span::styled(
            status_char.to_string(),
            Style::default()
                .fg(status_fg)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default().bg(bg)),
    ];

    // Track path display width for padding calculation
    let path_display_len = if is_rename {
        // Show: old_path → dir/filename
        let arrow = " → ";
        spans.push(Span::styled(
            old_path_str.to_string(),
            Style::default()
                .fg(FG_PATH_DIR)
                .bg(bg)
                .add_modifier(underline),
        ));
        spans.push(Span::styled(
            arrow.to_string(),
            Style::default().fg(FG_MUTED).bg(bg),
        ));
        if !dir.is_empty() {
            spans.push(Span::styled(
                dir.to_string(),
                Style::default()
                    .fg(FG_PATH_DIR)
                    .bg(bg)
                    .add_modifier(underline),
            ));
        }
        spans.push(Span::styled(
            filename.to_string(),
            Style::default()
                .fg(FG_PATH_FILE)
                .bg(bg)
                .add_modifier(Modifier::BOLD | underline),
        ));
        old_path_str.len() + arrow.len() + dir.len() + filename.len()
    } else {
        if !dir.is_empty() {
            spans.push(Span::styled(
                dir.to_string(),
                Style::default()
                    .fg(FG_PATH_DIR)
                    .bg(bg)
                    .add_modifier(underline),
            ));
        }
        spans.push(Span::styled(
            filename.to_string(),
            Style::default()
                .fg(FG_PATH_FILE)
                .bg(bg)
                .add_modifier(Modifier::BOLD | underline),
        ));
        dir.len() + filename.len()
    };
    spans.push(Span::styled("  ", Style::default().bg(bg)));
    let used = 1
        + collapse.len()
        + 1
        + status_char.len()
        + 2
        + path_display_len
        + 2
        + adds.len()
        + 2
        + dels.len();
    spans.push(Span::styled(adds, Style::default().fg(FG_ADD).bg(bg)));
    spans.push(Span::styled("  ", Style::default().bg(bg)));
    spans.push(Span::styled(dels, Style::default().fg(FG_DEL).bg(bg)));
    let right_pad = total_width.saturating_sub(used);
    spans.push(Span::styled(" ".repeat(right_pad), Style::default().bg(bg)));

    Line::from(spans)
}

/// Overlay emphasized background on syntax-highlighted spans at the given byte ranges.
/// Splits spans at range boundaries so only the changed characters get the brighter bg.
fn apply_inline_emphasis(spans: &mut Vec<Span<'_>>, ranges: &[(usize, usize)], emph_bg: Color) {
    let mut new_spans: Vec<Span<'_>> = Vec::new();
    let mut byte_offset = 0usize;

    for span in spans.drain(..) {
        let span_start = byte_offset;
        let span_len = span.content.len();
        let span_end = span_start + span_len;
        let base_style = span.style;

        let mut pos = 0; // position within this span's content

        for &(range_start, range_end) in ranges {
            // Skip ranges that don't overlap this span
            if range_end <= span_start || range_start >= span_end {
                continue;
            }

            // Clamp range to this span
            let local_start = range_start.saturating_sub(span_start).min(span_len);
            let local_end = range_end.saturating_sub(span_start).min(span_len);

            // Emit normal portion before this range
            if pos < local_start {
                new_spans.push(Span::styled(
                    span.content[pos..local_start].to_string(),
                    base_style,
                ));
            }

            // Emit emphasized portion
            if local_start < local_end {
                new_spans.push(Span::styled(
                    span.content[local_start..local_end].to_string(),
                    base_style.bg(emph_bg),
                ));
            }

            pos = local_end;
        }

        // Emit remaining normal portion
        if pos < span_len {
            new_spans.push(Span::styled(span.content[pos..].to_string(), base_style));
        }

        byte_offset = span_end;
    }

    *spans = new_spans;
}

/// Extract the function context from a hunk header like `@@ -10,5 +10,7 @@ fn foo()`.
/// Returns the function name if present, otherwise None.
fn hunk_context(header: &str) -> Option<&str> {
    // Find the closing "@@" (skip the opening one)
    let rest = header.strip_prefix("@@")?;
    let end = rest.find("@@")?;
    let after = rest[end + 2..].trim();
    if after.is_empty() { None } else { Some(after) }
}

/// Compute the digit width needed for a file's line numbers (minimum 4).
fn lineno_width(file: &crate::diff::FileDiff) -> usize {
    let max_lineno = file
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter().filter_map(|l| l.new_lineno.or(l.old_lineno)))
        .max()
        .unwrap_or(0);
    let digits = if max_lineno == 0 {
        1
    } else {
        max_lineno.ilog10() as usize + 1
    };
    digits.max(4)
}

fn format_lineno(line: &crate::diff::DiffLine, width: usize) -> String {
    use std::fmt::Write;
    let mut buf = String::with_capacity(width * 2 + 4);
    match line.old_lineno {
        Some(n) => {
            let _ = write!(buf, "{:>w$}", n, w = width);
        }
        None => {
            for _ in 0..width {
                buf.push(' ');
            }
        }
    }
    buf.push(' ');
    match line.new_lineno {
        Some(n) => {
            let _ = write!(buf, "{:>w$}", n, w = width);
        }
        None => {
            for _ in 0..width {
                buf.push(' ');
            }
        }
    }
    buf.push_str(" │");
    buf
}

/// Expand indicator that fits exactly in the gutter: `    ↕ NN │`
fn format_expand_indicator(gap: usize, width: usize) -> String {
    // Gutter layout: [width] [1 space] [width] [ │]
    // Total before │ = width*2 + 1, then " │"
    let gap_str = format!("{}", gap);
    let gutter_content = width * 2 + 1; // chars before " │"
    let used = 1 + 1 + gap_str.len(); // "↕" + " " + digits
    let pad = gutter_content.saturating_sub(used);
    format!("{}↕ {} │", " ".repeat(pad), gap_str)
}

fn draw_status_bar(frame: &mut Frame, app: &App, hints: &mut LayoutHints, area: Rect) {
    let total_files: usize = app.repos.iter().map(|r| r.files.len()).sum();
    let base = app
        .repos
        .get(app.active_tab)
        .and_then(|r| r.base_branch.as_deref());
    let mode = app.current_mode().label(base);
    let view = if app.side_by_side {
        "side-by-side"
    } else {
        "unified"
    };
    let repo_count = app.repos.len();

    let branch_name = app
        .repos
        .get(app.active_tab)
        .and_then(|r| r.branch_name.as_deref())
        .unwrap_or("HEAD");

    let left = format!(
        " {} repo{} │ {} file{} │ {}",
        repo_count,
        if repo_count != 1 { "s" } else { "" },
        total_files,
        if total_files != 1 { "s" } else { "" },
        branch_name,
    );

    let right = "a:add  f:find  ?:help  q:quit ".to_string();

    let mut spans: Vec<Span> = vec![Span::styled(left, Style::default().fg(Color::White))];

    if let Some((ref msg, _)) = app.status_message {
        spans.push(Span::styled(
            format!(" │ {}", msg),
            Style::default().fg(FG_COMMENT),
        ));
    } else if let Some(ref err) = app.last_error {
        spans.push(Span::styled(
            format!(" │ {}", err),
            Style::default().fg(Color::Red),
        ));
    } else if app.current_mode() == crate::git::DiffMode::Branch && base.is_none() {
        spans.push(Span::styled(
            " │ base branch not detected",
            Style::default().fg(Color::Yellow),
        ));
    }

    // Mode and view as highlighted badges — track positions for click handling
    spans.push(Span::raw("  "));
    let col_before_mode: u16 = area.x + spans.iter().map(|s| s.content.len() as u16).sum::<u16>();
    let mode_text = format!(" {} ", mode);
    let mode_width = mode_text.len() as u16;
    spans.push(Span::styled(
        mode_text,
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    hints.mode_badge_pos = (col_before_mode, col_before_mode + mode_width);

    spans.push(Span::raw(" "));
    let col_before_view: u16 = area.x + spans.iter().map(|s| s.content.len() as u16).sum::<u16>();
    let view_text = format!(" {} ", view);
    let view_width = view_text.len() as u16;
    spans.push(Span::styled(
        view_text,
        Style::default()
            .fg(Color::Black)
            .bg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    ));
    hints.view_badge_pos = (col_before_view, col_before_view + view_width);
    hints.status_bar_row = area.y;

    // Right-align help/quit hints by padding
    let used_width: usize = spans.iter().map(|s| s.content.len()).sum();
    let padding = (area.width as usize).saturating_sub(used_width + right.len());
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled(right, Style::default().fg(FG_MUTED)));

    let status =
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(40, 40, 50)));

    frame.render_widget(status, area);
}

fn draw_file_picker(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let max_items = 20u16;
    // 3 lines for border + input + separator, then items
    let height = (max_items + 3).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height.saturating_sub(height) / 3; // Upper third
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Find file (type to filter) ")
        .style(Style::default().bg(Color::Rgb(30, 30, 40)));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height < 2 {
        return;
    }

    let picker = match app.file_picker.as_ref() {
        Some(p) => p,
        None => return,
    };

    // Input line with cursor
    let input_text = format!(" > {}_", picker.query);
    let input_line = Paragraph::new(Line::from(Span::styled(
        input_text,
        Style::default().fg(Color::Yellow),
    )))
    .style(Style::default().bg(Color::Rgb(30, 30, 40)));
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(input_line, input_area);

    // File list
    let list_area = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    let filtered = app.filtered_file_indices();
    let files = match app.current_files() {
        Some(f) => f,
        None => return,
    };

    let mut lines: Vec<Line> = Vec::new();
    let visible_count = list_area.height as usize;

    // Scroll the list to keep selected item visible
    let list_scroll = if picker.selected >= visible_count {
        picker.selected - visible_count + 1
    } else {
        0
    };

    for (display_idx, &file_idx) in filtered
        .iter()
        .enumerate()
        .skip(list_scroll)
        .take(visible_count)
    {
        let file = &files[file_idx];
        let is_selected = display_idx == picker.selected;

        let status_char = match file.status {
            FileStatus::Modified => "M",
            FileStatus::Added => "A",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
            FileStatus::Untracked => "?",
        };

        let text = format!(
            " {} {}  (+{} -{})",
            status_char, file.path, file.additions, file.deletions
        );

        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(100, 180, 255))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White).bg(Color::Rgb(30, 30, 40))
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching files",
            Style::default().fg(FG_MUTED).bg(Color::Rgb(30, 30, 40)),
        )));
    }

    let list = Paragraph::new(lines);
    frame.render_widget(list, list_area);
}

fn draw_repo_adder(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let max_items = 15u16;
    let height = (max_items + 3).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height.saturating_sub(height) / 3;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Add repo (type path, space=select, enter=add) ")
        .style(Style::default().bg(Color::Rgb(30, 30, 40)));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let adder = match app.repo_adder.as_ref() {
        Some(a) => a,
        None => return,
    };

    if inner.height < 2 {
        return;
    }

    // Input line
    let input_text = format!(" > {}_", adder.query);
    let input_line = Paragraph::new(Line::from(Span::styled(
        input_text,
        Style::default().fg(Color::Yellow),
    )))
    .style(Style::default().bg(Color::Rgb(30, 30, 40)));
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(input_line, input_area);

    // Error or results list
    let list_area = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );

    if let Some(ref err) = adder.error {
        let err_line = Paragraph::new(Line::from(Span::styled(
            format!(" {}", err),
            Style::default().fg(Color::Red),
        )))
        .style(Style::default().bg(Color::Rgb(30, 30, 40)));
        frame.render_widget(err_line, list_area);
        return;
    }

    let visible_count = list_area.height as usize;
    let list_scroll = if adder.cursor >= visible_count {
        adder.cursor - visible_count + 1
    } else {
        0
    };

    let mut lines: Vec<Line> = Vec::new();
    for (display_idx, (name, _path)) in adder
        .results
        .iter()
        .enumerate()
        .skip(list_scroll)
        .take(visible_count)
    {
        let is_cursor = display_idx == adder.cursor;
        let is_checked = adder.checked.contains(&display_idx);
        let check = if is_checked { "[x]" } else { "[ ]" };
        let text = format!(" {} {}", check, name);

        let style = if is_cursor {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(100, 180, 255))
                .add_modifier(Modifier::BOLD)
        } else if is_checked {
            Style::default().fg(Color::Green).bg(Color::Rgb(30, 30, 40))
        } else {
            Style::default().fg(Color::White).bg(Color::Rgb(30, 30, 40))
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    if adder.results.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No git repos found",
            Style::default().fg(FG_MUTED).bg(Color::Rgb(30, 30, 40)),
        )));
    }

    let list = Paragraph::new(lines);
    frame.render_widget(list, list_area);
}

fn draw_markdown_preview(frame: &mut Frame, app: &App) {
    let preview = match app.markdown_preview.as_ref() {
        Some(p) => p,
        None => return,
    };

    let area = frame.area();
    let width = area.width.saturating_sub(6).min(120);
    let height = area.height.saturating_sub(4);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let title = format!(" {} — j/k scroll, q/esc/p close ", preview.path);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().bg(Color::Rgb(25, 25, 35)));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut skin = termimad::MadSkin::default_dark();
    // Clear backgrounds on paragraph text so it doesn't look highlighted
    skin.paragraph.set_bg(termimad::crossterm::style::Color::Reset);
    skin.bold.set_bg(termimad::crossterm::style::Color::Reset);
    skin.italic.set_bg(termimad::crossterm::style::Color::Reset);
    skin.strikeout.set_bg(termimad::crossterm::style::Color::Reset);
    let fmt_text = skin.text(&preview.content, Some(inner.width as usize));
    let rendered = format!("{fmt_text}");

    // Convert ANSI-styled string to ratatui Text
    let text: ratatui::text::Text = match ansi_to_tui::IntoText::into_text(&rendered) {
        Ok(t) => t,
        Err(_) => ratatui::text::Text::raw(&preview.content),
    };

    let total_lines = text.lines.len();
    let scroll = preview
        .scroll
        .min(total_lines.saturating_sub(inner.height as usize));

    let para = Paragraph::new(text)
        .scroll((scroll as u16, 0))
        .style(Style::default().bg(Color::Rgb(25, 25, 35)));

    frame.render_widget(para, inner);

    // Scrollbar
    if total_lines > inner.height as usize {
        let mut scrollbar_state =
            ScrollbarState::new(total_lines.saturating_sub(inner.height as usize))
                .position(scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, popup_area, &mut scrollbar_state);
    }
}

fn draw_help_overlay(frame: &mut Frame) {
    let area = frame.area();
    let width = 50u16.min(area.width.saturating_sub(4));
    let height = 22u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let help_text = vec![
        Line::from(Span::styled(
            "Keybindings",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Navigation",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  j/k  ↑/↓       Scroll up/down"),
        Line::from("  J/K            Jump to prev/next file"),
        Line::from("  g/G            Top / bottom"),
        Line::from("  PgUp/PgDn      Scroll by page"),
        Line::from("  Mouse scroll   Scroll"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Tabs",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  1-9            Switch to tab N"),
        Line::from("  Tab/Shift+Tab  Cycle tabs"),
        Line::from("  Click tab      Switch to tab"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Modes & Views",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  m/s/b          Modified/Staged/Branch diff"),
        Line::from("  v              Toggle unified/side-by-side"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Actions",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  a              Add repo to watch"),
        Line::from("  x              Remove current repo tab"),
        Line::from("  f              Find file (fuzzy picker)"),
        Line::from("  Enter/Click    Toggle collapse file"),
        Line::from("  c/e            Collapse/Expand all"),
        Line::from("  y              Copy hunk to clipboard"),
        Line::from("  q/Esc          Quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Press ? or Esc to close",
            Style::default().fg(FG_MUTED),
        )),
    ];

    let help = Paragraph::new(help_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Help ")
                .style(Style::default().bg(Color::Rgb(30, 30, 40))),
        )
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::White).bg(Color::Rgb(30, 30, 40)));

    frame.render_widget(help, popup_area);
}

fn draw_comment_input(frame: &mut Frame, app: &App) {
    let input = match app.comment_input.as_ref() {
        Some(i) => i,
        None => return,
    };

    let area = frame.area();

    // Get file name for context label
    let file_label = app
        .current_files()
        .and_then(|f| f.get(input.file_idx))
        .map(|f| {
            let name = f.path.rsplit('/').next().unwrap_or(&f.path);
            let lineno = f
                .hunks
                .get(input.hunk_idx)
                .and_then(|h| h.first_new_lineno())
                .unwrap_or(0);
            format!("{} @{}", name, lineno)
        })
        .unwrap_or_default();

    // Compute box dimensions
    let text_lines: Vec<&str> = input.text.split('\n').collect();
    let line_count = text_lines.len().max(1);
    let box_height = (line_count as u16 + 2).clamp(3, 8); // +2 for borders
    let box_width = (area.width / 2).max(40).min(area.width.saturating_sub(4));

    // Position anchored near the hunk
    let anchor_screen_y = (input.anchor_row as u16)
        .saturating_sub(app.current_scroll_offset() as u16)
        + app.layout.content_y
        + 1;

    let y = if anchor_screen_y + box_height < app.layout.content_y + app.layout.content_height {
        anchor_screen_y
    } else {
        anchor_screen_y.saturating_sub(box_height + 1)
    }
    .clamp(app.layout.content_y, area.height.saturating_sub(box_height));

    let x = (area.width.saturating_sub(box_width)) / 2;
    let popup_area = Rect::new(x, y, box_width, box_height);

    frame.render_widget(Clear, popup_area);

    let title = format!(" note ({}) — Ctrl+D save, Esc cancel ", file_label);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().bg(Color::Rgb(30, 30, 20)).fg(FG_COMMENT));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Render text with cursor
    let mut display_lines: Vec<Line> = Vec::new();
    let mut char_count = 0;
    for (i, text_line) in text_lines.iter().enumerate() {
        let line_start = char_count;
        let line_end = line_start + text_line.len();

        if input.cursor_pos >= line_start && input.cursor_pos <= line_end {
            let cursor_col = input.cursor_pos - line_start;
            let before = &text_line[..cursor_col.min(text_line.len())];
            let after = &text_line[cursor_col.min(text_line.len())..];
            let cursor_char = if after.is_empty() { " " } else { &after[..1] };
            let after_cursor = if after.len() > 1 { &after[1..] } else { "" };
            display_lines.push(Line::from(vec![
                Span::styled(
                    format!(" {}", before),
                    Style::default().fg(Color::White).bg(Color::Rgb(30, 30, 20)),
                ),
                Span::styled(
                    cursor_char.to_string(),
                    Style::default()
                        .fg(Color::Black)
                        .bg(FG_COMMENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    after_cursor.to_string(),
                    Style::default().fg(Color::White).bg(Color::Rgb(30, 30, 20)),
                ),
            ]));
        } else {
            display_lines.push(Line::from(Span::styled(
                format!(" {}", text_line),
                Style::default().fg(Color::White).bg(Color::Rgb(30, 30, 20)),
            )));
        }

        // +1 for the \n between lines
        char_count = line_end + if i < text_lines.len() - 1 { 1 } else { 0 };
    }

    let para = Paragraph::new(display_lines).style(Style::default().bg(Color::Rgb(30, 30, 20)));
    frame.render_widget(para, inner);
}

fn draw_comment_browser(frame: &mut Frame, app: &App) {
    let browser = match app.comment_browser.as_ref() {
        Some(b) => b,
        None => return,
    };

    let comments = match app.repos.get(app.active_tab) {
        Some(r) => &r.comments,
        None => return,
    };

    let area = frame.area();
    let width = 70u16.min(area.width.saturating_sub(4));
    let max_height = area.height.saturating_sub(6);
    let height = max_height.min(30);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height.saturating_sub(height) / 3;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let title = format!(" Review notes ({}) — type to filter ", comments.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().bg(Color::Rgb(30, 30, 20)).fg(FG_COMMENT));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height < 3 {
        return;
    }

    // Input line
    let input_text = format!(" > {}_", browser.query);
    let input_line = Paragraph::new(Line::from(Span::styled(
        input_text,
        Style::default().fg(FG_COMMENT),
    )))
    .style(Style::default().bg(Color::Rgb(30, 30, 20)));
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(input_line, input_area);

    // Hint bar at bottom
    let hint_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    let hint = Paragraph::new(Line::from(Span::styled(
        " ↵ jump  y copy  ␣ toggle  d del  esc close",
        Style::default().fg(FG_MUTED),
    )))
    .style(Style::default().bg(Color::Rgb(30, 30, 20)));
    frame.render_widget(hint, hint_area);

    // Comment list
    let list_area = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(2),
    );

    let files = match app.current_files() {
        Some(f) => f,
        None => return,
    };

    let mut lines: Vec<Line> = Vec::new();

    if comments.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No review notes yet — right-click a hunk to add one",
            Style::default().fg(FG_MUTED).bg(Color::Rgb(30, 30, 20)),
        )));
    } else {
        // Filter comments by query
        let query_lower = browser.query.to_lowercase();
        let filtered: Vec<usize> = (0..comments.len())
            .filter(|&i| {
                if query_lower.is_empty() {
                    return true;
                }
                let c = &comments[i];
                let file_path = files.get(c.file_idx).map(|f| f.path.as_str()).unwrap_or("");
                let haystack = format!("{} {}", file_path, c.text).to_lowercase();
                haystack.contains(&query_lower)
            })
            .collect();

        for (display_idx, &comment_idx) in filtered.iter().enumerate() {
            let c = &comments[comment_idx];
            let is_selected = display_idx == browser.selected;
            let is_checked = browser.checked.contains(&comment_idx);
            let check = if is_checked { "[x]" } else { "[ ]" };

            let file_name = files
                .get(c.file_idx)
                .map(|f| f.path.as_str())
                .unwrap_or("?");
            let lineno = files
                .get(c.file_idx)
                .and_then(|f| f.hunks.get(c.hunk_idx))
                .and_then(|h| h.first_new_lineno())
                .unwrap_or(0);

            let header_style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(FG_COMMENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(FG_COMMENT).bg(Color::Rgb(30, 30, 20))
            };

            lines.push(Line::from(Span::styled(
                format!(" {} {} @{}", check, file_name, lineno),
                header_style,
            )));

            // Show comment text
            let text_style = if is_selected {
                Style::default().fg(Color::White).bg(Color::Rgb(40, 40, 30))
            } else {
                Style::default().fg(Color::White).bg(Color::Rgb(30, 30, 20))
            };

            for text_line in c.text.lines() {
                lines.push(Line::from(Span::styled(
                    format!("     {}", text_line),
                    text_style,
                )));
            }

            // Blank separator
            lines.push(Line::from(""));
        }
    }

    // Scroll to keep selected visible
    let visible_count = list_area.height as usize;
    let scroll = if lines.len() > visible_count {
        // Find the line index where the selected comment header is
        let mut selected_line = 0;
        let mut comment_count = 0;
        for (i, _) in lines.iter().enumerate() {
            if comment_count == browser.selected {
                selected_line = i;
                break;
            }
            // Count comment headers (lines starting with checkbox)
            if lines.get(i).is_some_and(|l| {
                l.spans
                    .first()
                    .is_some_and(|s| s.content.contains("[x]") || s.content.contains("[ ]"))
            }) {
                comment_count += 1;
            }
        }
        selected_line.saturating_sub(visible_count / 2)
    } else {
        0
    };

    let display_lines: Vec<Line> = lines.into_iter().skip(scroll).take(visible_count).collect();
    let list = Paragraph::new(display_lines);
    frame.render_widget(list, list_area);
}

#[cfg(test)]
mod tests {
    use super::hunk_context;

    #[test]
    fn extracts_function_name() {
        assert_eq!(hunk_context("@@ -10,5 +10,7 @@ fn foo()"), Some("fn foo()"));
    }

    #[test]
    fn no_function_context() {
        assert_eq!(hunk_context("@@ -10,5 +10,7 @@"), None);
    }

    #[test]
    fn whitespace_only_after_returns_none() {
        assert_eq!(hunk_context("@@ -10,5 +10,7 @@   "), None);
    }

    #[test]
    fn impl_block_context() {
        assert_eq!(hunk_context("@@ -1,3 +1,5 @@ impl Foo"), Some("impl Foo"));
    }

    #[test]
    fn non_hunk_header_returns_none() {
        assert_eq!(hunk_context("not a header"), None);
    }
}
