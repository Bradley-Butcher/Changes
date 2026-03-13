use crate::app::App;
use crate::diff::{self, FileStatus, LineKind};
use crate::highlight::Highlighter;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use ratatui::Frame;

const BG_ADD: Color = Color::Rgb(30, 60, 30);
const BG_DEL: Color = Color::Rgb(60, 30, 30);
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

pub fn draw(frame: &mut Frame, app: &mut App, highlighter: &Highlighter) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tab bar
            Constraint::Min(1),   // diff area
            Constraint::Length(1), // status bar
        ])
        .split(frame.area());

    draw_tab_bar(frame, app, chunks[0]);
    draw_diff_area(frame, app, highlighter, chunks[1]);
    draw_status_bar(frame, app, chunks[2]);

    if app.show_repo_adder {
        draw_repo_adder(frame, app);
    } else if app.show_file_picker {
        draw_file_picker(frame, app);
    } else if app.show_help {
        draw_help_overlay(frame);
    }
}

fn draw_tab_bar(frame: &mut Frame, app: &mut App, area: Rect) {
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
    app.tab_positions = positions;

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
            Style::default().fg(Color::Rgb(100, 180, 255)).add_modifier(Modifier::BOLD),
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

    if files.is_empty() {
        draw_empty_state(frame, area);
        return;
    }

    if app.side_by_side {
        draw_side_by_side(frame, app, highlighter, files, area);
    } else {
        draw_unified(frame, app, highlighter, files, area);
    }
}

fn draw_unified(
    frame: &mut Frame,
    app: &App,
    highlighter: &Highlighter,
    files: &[crate::diff::FileDiff],
    area: Rect,
) {
    let inner_area = Block::default().borders(Borders::ALL).inner(area);
    let block = Block::default().borders(Borders::ALL);
    frame.render_widget(block, area);

    let visible_height = inner_area.height as usize;
    let scroll = app.scroll_offset;
    let visible_end = scroll + visible_height;

    // Compute total lines upfront for stable scrollbar sizing.
    let total_lines: usize = files.iter().map(|f| f.total_display_lines() + 1).sum();

    // Build only the visible lines — skip everything above the viewport,
    // stop once we've filled the viewport, and only syntax-highlight visible lines.
    let mut lines: Vec<Line> = Vec::new();
    let mut row: usize = 0;

    'outer: for (file_idx, file) in files.iter().enumerate() {
        let file_lines = file.total_display_lines() + 1; // +1 blank separator

        // Skip files entirely above the viewport
        if row + file_lines <= scroll {
            row += file_lines;
            continue;
        }

        // Stop if we're past the viewport
        if row >= visible_end {
            break;
        }

        // File header — full-width centered banner
        if row >= scroll && row < visible_end {
            let is_focused = app.focused_file == Some(file_idx);
            lines.push(build_file_header(file, is_focused, inner_area.width));
        }
        row += 1;

        if file.collapsed {
            if row >= scroll && row < visible_end {
                lines.push(Line::from(""));
            }
            row += 1;
            continue;
        }

        let lno_w = lineno_width(file);

        for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
            // Hunk header: gap indicator + function context (only when gap exists)
            if row >= scroll && row < visible_end {
                let gap = if hunk_idx > 0 {
                    diff::gap_between_hunks(&file.hunks[hunk_idx - 1], hunk)
                } else {
                    // Gap before first hunk
                    hunk.first_new_lineno().unwrap_or(1) as usize - 1
                };

                if gap > 0 {
                    let mut spans = vec![Span::styled(
                        format_expand_indicator(gap, lno_w),
                        Style::default().fg(FG_EXPAND),
                    )];
                    if let Some(ctx) = hunk_context(&hunk.header) {
                        spans.push(Span::styled(format!(" {}", ctx), Style::default().fg(FG_HUNK)));
                    }
                    lines.push(Line::from(spans));
                } else if hunk_idx > 0 {
                    let gutter = " ".repeat(lno_w * 2 + 1) + " │";
                    lines.push(Line::from(Span::styled(gutter, Style::default().fg(FG_MUTED))));
                } else {
                    lines.push(Line::from(""));
                }
            }
            row += 1;
            if row >= visible_end {
                break 'outer;
            }

            let flashing = app.is_hunk_flashing(file_idx, hunk_idx);

            for line in &hunk.lines {
                if row >= scroll && row < visible_end {
                    lines.push(build_unified_line(line, &file.path, highlighter, flashing, lno_w));
                }
                row += 1;
                if row >= visible_end {
                    break 'outer;
                }
            }
        }

        // Trailing row — show bottom gap if lines exist after last hunk
        if row >= scroll && row < visible_end {
            let bottom_gap = if !file.collapsed && !file.hunks.is_empty() && file.total_new_lines > 0 {
                let last_new = file.hunks.last().and_then(|h| h.last_new_lineno()).unwrap_or(0) as usize;
                file.total_new_lines.saturating_sub(last_new)
            } else {
                0
            };
            if bottom_gap > 0 {
                lines.push(Line::from(Span::styled(
                    format_expand_indicator(bottom_gap, lno_w),
                    Style::default().fg(FG_EXPAND),
                )));
            } else {
                lines.push(Line::from(""));
            }
        }
        row += 1;
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner_area);

    if total_lines > visible_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines.saturating_sub(visible_height))
            .position(app.scroll_offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn draw_side_by_side(
    frame: &mut Frame,
    app: &App,
    highlighter: &Highlighter,
    files: &[crate::diff::FileDiff],
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
    let scroll = app.scroll_offset;
    let visible_end = scroll + visible_height;

    // Stable total for scrollbar
    let total_lines: usize = files.iter().map(|f| f.total_sbs_display_lines() + 1).sum();

    let mut left_lines: Vec<Line> = Vec::new();
    let mut right_lines: Vec<Line> = Vec::new();
    let mut row: usize = 0;
    let empty_sbs: Vec<Vec<diff::SideBySideLine>> = Vec::new();

    'outer: for (file_idx, file) in files.iter().enumerate() {
        let file_lines = file.total_sbs_display_lines() + 1;

        // Skip files entirely above viewport
        if row + file_lines <= scroll {
            row += file_lines;
            continue;
        }
        // Stop past viewport
        if row >= visible_end {
            break;
        }

        // File header — full-width centered banner (spans both halves)
        if row >= scroll && row < visible_end {
            let is_focused = app.focused_file == Some(file_idx);
            let header = build_file_header(file, is_focused, inner_area.width);
            // Left half gets the header, right half gets matching background
            left_lines.push(header);
            let bg = BG_HEADER;
            right_lines.push(Line::from(Span::styled(
                " ".repeat(inner_area.width as usize),
                Style::default().bg(bg),
            )));
        }
        row += 1;

        if !file.collapsed {
            let sbs_hunks = file.sbs_cache.as_ref().unwrap_or(&empty_sbs);

            for (hunk_idx, sbs_lines) in sbs_hunks.iter().enumerate() {
                // Hunk header: gap indicator + function context (only when gap exists)
                if row >= scroll && row < visible_end {
                    let gap = if hunk_idx > 0 && hunk_idx < file.hunks.len() {
                        diff::gap_between_hunks(&file.hunks[hunk_idx - 1], &file.hunks[hunk_idx])
                    } else if hunk_idx == 0 && hunk_idx < file.hunks.len() {
                        file.hunks[0].first_new_lineno().unwrap_or(1) as usize - 1
                    } else {
                        0
                    };

                    let hunk_line = if gap > 0 {
                        let mut spans = vec![Span::styled(
                            format!("  ↕ {}", gap),
                            Style::default().fg(FG_EXPAND),
                        )];
                        let ctx = if hunk_idx < file.hunks.len() {
                            hunk_context(&file.hunks[hunk_idx].header)
                        } else {
                            None
                        };
                        if let Some(ctx) = ctx {
                            spans.push(Span::styled(format!("  {}", ctx), Style::default().fg(FG_HUNK)));
                        }
                        Line::from(spans)
                    } else if hunk_idx > 0 {
                        Line::from(Span::styled("  ─", Style::default().fg(FG_MUTED)))
                    } else {
                        Line::from("")
                    };
                    left_lines.push(hunk_line.clone());
                    right_lines.push(hunk_line);
                }
                row += 1;
                if row >= visible_end { break 'outer; }

                for sbs in sbs_lines {
                    if row >= scroll && row < visible_end {
                        left_lines.push(build_sbs_line(&sbs.left, &file.path, highlighter, app));
                        right_lines.push(build_sbs_line(&sbs.right, &file.path, highlighter, app));
                    }
                    row += 1;
                    if row >= visible_end { break 'outer; }
                }
            }
        }

        // Trailing row — show bottom gap if lines exist after last hunk
        if row >= scroll && row < visible_end {
            let bottom_gap = if !file.collapsed && !file.hunks.is_empty() && file.total_new_lines > 0 {
                let last_new = file.hunks.last().and_then(|h| h.last_new_lineno()).unwrap_or(0) as usize;
                file.total_new_lines.saturating_sub(last_new)
            } else {
                0
            };
            if bottom_gap > 0 {
                let expand_line = Line::from(Span::styled(
                    format!("  ↕ {}", bottom_gap),
                    Style::default().fg(FG_EXPAND),
                ));
                left_lines.push(expand_line.clone());
                right_lines.push(expand_line);
            } else {
                left_lines.push(Line::from(""));
                right_lines.push(Line::from(""));
            }
        }
        row += 1;
    }

    let left_para = Paragraph::new(left_lines);
    let right_para = Paragraph::new(right_lines);

    frame.render_widget(left_para, halves[0]);
    frame.render_widget(right_para, halves[1]);

    if total_lines > visible_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines.saturating_sub(visible_height))
            .position(app.scroll_offset);
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
) -> Line<'a> {
    let prefix = match line.kind {
        LineKind::Context => "  ",
        LineKind::Addition => "+ ",
        LineKind::Deletion => "- ",
        LineKind::HunkHeader => "  ",
        LineKind::FileHeader => "  ",
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

    let mut spans = vec![
        Span::styled(lineno, Style::default().fg(FG_MUTED)),
        Span::styled(prefix.to_string(), prefix_style),
    ];

    let mut highlighted = highlighter.highlight_line_content(&line.content, file_path, bg);
    spans.append(&mut highlighted.spans);

    Line::from(spans)
}

fn build_sbs_line<'a>(
    line_opt: &Option<crate::diff::DiffLine>,
    file_path: &str,
    highlighter: &Highlighter,
    _app: &App,
) -> Line<'a> {
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

            let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
            let mut highlighted =
                highlighter.highlight_line_content(&line.content, file_path, bg);
            spans.append(&mut highlighted.spans);
            Line::from(spans)
        }
        None => Line::from(Span::styled("~", Style::default().fg(FG_MUTED))),
    }
}

/// Build a full-width centered file header banner.
fn build_file_header<'a>(
    file: &crate::diff::FileDiff,
    is_focused: bool,
    width: u16,
) -> Line<'a> {
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

    let adds = format!("+{}", file.additions);
    let dels = format!("-{}", file.deletions);

    let bg = BG_HEADER;
    let total_width = width as usize;
    let underline = if is_focused { Modifier::UNDERLINED } else { Modifier::empty() };

    // Left-aligned: small indent then content, fill remaining with bg
    let mut spans = vec![
        Span::styled(" ", Style::default().bg(bg)),
        Span::styled(format!("{} ", collapse), Style::default().fg(FG_MUTED).bg(bg)),
        Span::styled(format!("{}", status_char), Style::default().fg(status_fg).bg(bg).add_modifier(Modifier::BOLD)),
        Span::styled("  ", Style::default().bg(bg)),
    ];

    if !dir.is_empty() {
        spans.push(Span::styled(dir.to_string(), Style::default().fg(FG_PATH_DIR).bg(bg).add_modifier(underline)));
    }
    spans.push(Span::styled(
        filename.to_string(),
        Style::default().fg(FG_PATH_FILE).bg(bg).add_modifier(Modifier::BOLD | underline),
    ));
    spans.push(Span::styled("  ", Style::default().bg(bg)));
    let used = 1 + collapse.len() + 1 + status_char.len() + 2 + dir.len() + filename.len()
        + 2 + adds.len() + 2 + dels.len();
    spans.push(Span::styled(adds, Style::default().fg(FG_ADD).bg(bg)));
    spans.push(Span::styled("  ", Style::default().bg(bg)));
    spans.push(Span::styled(dels, Style::default().fg(FG_DEL).bg(bg)));
    let right_pad = total_width.saturating_sub(used);
    spans.push(Span::styled(" ".repeat(right_pad), Style::default().bg(bg)));

    Line::from(spans)
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
    let max_lineno = file.hunks.iter().flat_map(|h| {
        h.lines.iter().filter_map(|l| l.new_lineno.or(l.old_lineno))
    }).max().unwrap_or(0);
    let digits = if max_lineno == 0 { 1 } else { (max_lineno as f64).log10() as usize + 1 };
    digits.max(4)
}

fn format_lineno(line: &crate::diff::DiffLine, width: usize) -> String {
    use std::fmt::Write;
    let mut buf = String::with_capacity(width * 2 + 4);
    match line.old_lineno {
        Some(n) => { let _ = write!(buf, "{:>w$}", n, w = width); }
        None => { for _ in 0..width { buf.push(' '); } }
    }
    buf.push(' ');
    match line.new_lineno {
        Some(n) => { let _ = write!(buf, "{:>w$}", n, w = width); }
        None => { for _ in 0..width { buf.push(' '); } }
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

fn draw_status_bar(frame: &mut Frame, app: &mut App, area: Rect) {
    let total_files: usize = app.repos.iter().map(|r| r.files.len()).sum();
    let base = app.repos.get(app.active_tab).and_then(|r| r.base_branch.as_deref());
    let mode = app.current_mode().label(base);
    let view = if app.side_by_side { "side-by-side" } else { "unified" };
    let repo_count = app.repos.len();

    let branch_name = app.repos.get(app.active_tab)
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

    let right = format!("a:add  f:find  ?:help  q:quit ");

    let mut spans: Vec<Span> = vec![
        Span::styled(left, Style::default().fg(Color::White)),
    ];

    if let Some(ref err) = app.last_error {
        spans.push(Span::styled(format!(" │ {}", err), Style::default().fg(Color::Red)));
    }

    // Mode and view as highlighted badges — track positions for click handling
    spans.push(Span::raw("  "));
    let col_before_mode: u16 = area.x + spans.iter().map(|s| s.content.len() as u16).sum::<u16>();
    let mode_text = format!(" {} ", mode);
    let mode_width = mode_text.len() as u16;
    spans.push(Span::styled(
        mode_text,
        Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
    ));
    app.mode_badge_pos = (col_before_mode, col_before_mode + mode_width);

    spans.push(Span::raw(" "));
    let col_before_view: u16 = area.x + spans.iter().map(|s| s.content.len() as u16).sum::<u16>();
    let view_text = format!(" {} ", view);
    let view_width = view_text.len() as u16;
    spans.push(Span::styled(
        view_text,
        Style::default().fg(Color::Black).bg(Color::Magenta).add_modifier(Modifier::BOLD),
    ));
    app.view_badge_pos = (col_before_view, col_before_view + view_width);
    app.status_bar_row = area.y;

    // Right-align help/quit hints by padding
    let used_width: usize = spans.iter().map(|s| s.content.len()).sum();
    let padding = (area.width as usize).saturating_sub(used_width + right.len());
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled(right, Style::default().fg(FG_MUTED)));

    let status = Paragraph::new(Line::from(spans))
        .style(Style::default().bg(Color::Rgb(40, 40, 50)));

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

    // Input line with cursor
    let input_text = format!(" > {}_", app.file_picker_query);
    let input_line = Paragraph::new(Line::from(Span::styled(
        input_text,
        Style::default().fg(Color::Yellow),
    )))
    .style(Style::default().bg(Color::Rgb(30, 30, 40)));
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(input_line, input_area);

    // File list
    let list_area = Rect::new(inner.x, inner.y + 1, inner.width, inner.height.saturating_sub(1));
    let filtered = app.filtered_file_indices();
    let files = match app.current_files() {
        Some(f) => f,
        None => return,
    };

    let mut lines: Vec<Line> = Vec::new();
    let visible_count = list_area.height as usize;

    // Scroll the list to keep selected item visible
    let list_scroll = if app.file_picker_selected >= visible_count {
        app.file_picker_selected - visible_count + 1
    } else {
        0
    };

    for (display_idx, &file_idx) in filtered.iter().enumerate().skip(list_scroll).take(visible_count) {
        let file = &files[file_idx];
        let is_selected = display_idx == app.file_picker_selected;

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

    if inner.height < 2 {
        return;
    }

    // Input line
    let input_text = format!(" > {}_", app.repo_adder_query);
    let input_line = Paragraph::new(Line::from(Span::styled(
        input_text,
        Style::default().fg(Color::Yellow),
    )))
    .style(Style::default().bg(Color::Rgb(30, 30, 40)));
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(input_line, input_area);

    // Error or results list
    let list_area = Rect::new(inner.x, inner.y + 1, inner.width, inner.height.saturating_sub(1));

    if let Some(ref err) = app.repo_adder_error {
        let err_line = Paragraph::new(Line::from(Span::styled(
            format!(" {}", err),
            Style::default().fg(Color::Red),
        )))
        .style(Style::default().bg(Color::Rgb(30, 30, 40)));
        frame.render_widget(err_line, list_area);
        return;
    }

    let visible_count = list_area.height as usize;
    let list_scroll = if app.repo_adder_cursor >= visible_count {
        app.repo_adder_cursor - visible_count + 1
    } else {
        0
    };

    let mut lines: Vec<Line> = Vec::new();
    for (display_idx, (name, _path)) in app
        .repo_adder_results
        .iter()
        .enumerate()
        .skip(list_scroll)
        .take(visible_count)
    {
        let is_cursor = display_idx == app.repo_adder_cursor;
        let is_checked = app.repo_adder_checked.contains(&display_idx);
        let check = if is_checked { "[x]" } else { "[ ]" };
        let text = format!(" {} {}", check, name);

        let style = if is_cursor {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(100, 180, 255))
                .add_modifier(Modifier::BOLD)
        } else if is_checked {
            Style::default()
                .fg(Color::Green)
                .bg(Color::Rgb(30, 30, 40))
        } else {
            Style::default()
                .fg(Color::White)
                .bg(Color::Rgb(30, 30, 40))
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    if app.repo_adder_results.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No git repos found",
            Style::default().fg(FG_MUTED).bg(Color::Rgb(30, 30, 40)),
        )));
    }

    let list = Paragraph::new(lines);
    frame.render_widget(list, list_area);
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
        Line::from(Span::styled("Keybindings", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(vec![Span::styled("Navigation", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))]),
        Line::from("  j/k  ↑/↓       Scroll up/down"),
        Line::from("  J/K            Jump to prev/next file"),
        Line::from("  g/G            Top / bottom"),
        Line::from("  PgUp/PgDn      Scroll by page"),
        Line::from("  Mouse scroll   Scroll"),
        Line::from(""),
        Line::from(vec![Span::styled("Tabs", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))]),
        Line::from("  1-9            Switch to tab N"),
        Line::from("  Tab/Shift+Tab  Cycle tabs"),
        Line::from("  Click tab      Switch to tab"),
        Line::from(""),
        Line::from(vec![Span::styled("Modes & Views", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))]),
        Line::from("  m/s/b          Modified/Staged/Branch diff"),
        Line::from("  v              Toggle unified/side-by-side"),
        Line::from(""),
        Line::from(vec![Span::styled("Actions", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))]),
        Line::from("  a              Add repo to watch"),
        Line::from("  x              Remove current repo tab"),
        Line::from("  f              Find file (fuzzy picker)"),
        Line::from("  Enter/Click    Toggle collapse file"),
        Line::from("  c/e            Collapse/Expand all"),
        Line::from("  y              Copy hunk to clipboard"),
        Line::from("  q/Esc          Quit"),
        Line::from(""),
        Line::from(Span::styled("Press ? or Esc to close", Style::default().fg(FG_MUTED))),
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
