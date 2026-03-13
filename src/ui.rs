use crate::app::App;
use crate::diff::{self, FileStatus, LineKind};
use crate::highlight::Highlighter;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Tabs};
use ratatui::Frame;

const BG_ADD: Color = Color::Rgb(30, 60, 30);
const BG_DEL: Color = Color::Rgb(60, 30, 30);
const FG_ADD: Color = Color::Rgb(100, 220, 100);
const FG_DEL: Color = Color::Rgb(220, 100, 100);
const FG_HUNK: Color = Color::Rgb(130, 170, 220);
const FG_MUTED: Color = Color::Rgb(120, 120, 120);
const FG_HEADER: Color = Color::Rgb(220, 200, 100);
const BG_FLASH: Color = Color::Rgb(80, 80, 40);

pub fn draw(frame: &mut Frame, app: &App, highlighter: &Highlighter) {
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
}

fn draw_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = app
        .repos
        .iter()
        .map(|r| Line::from(r.name.clone()))
        .collect();

    let tabs = Tabs::new(titles)
        .select(app.active_tab)
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .divider(Span::raw(" │ "))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" changes — {} ", app.current_mode().label())),
        );

    frame.render_widget(tabs, area);
}

fn draw_diff_area(frame: &mut Frame, app: &App, highlighter: &Highlighter, area: Rect) {
    let files = match app.current_files() {
        Some(f) => f,
        None => {
            let empty = Paragraph::new("No changes")
                .style(Style::default().fg(FG_MUTED))
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(empty, area);
            return;
        }
    };

    if files.is_empty() {
        let empty = Paragraph::new("No changes")
            .style(Style::default().fg(FG_MUTED))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(empty, area);
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

    let mut lines: Vec<Line> = Vec::new();

    for (file_idx, file) in files.iter().enumerate() {
        // File header
        let status_char = match file.status {
            FileStatus::Modified => "M",
            FileStatus::Added => "A",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
            FileStatus::Untracked => "?",
        };

        let collapse_char = if file.collapsed { "▶" } else { "▼" };
        let header_text = format!(
            "{} {} {}  (+{} -{})",
            collapse_char, status_char, file.path, file.additions, file.deletions
        );

        let is_focused = app.focused_file == Some(file_idx);
        let header_style = if is_focused {
            Style::default()
                .fg(FG_HEADER)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default()
                .fg(FG_HEADER)
                .add_modifier(Modifier::BOLD)
        };

        lines.push(Line::from(Span::styled(header_text, header_style)));

        if file.collapsed {
            continue;
        }

        for hunk in &file.hunks {
            // Hunk header
            if !hunk.header.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("  {}", hunk.header),
                    Style::default().fg(FG_HUNK),
                )));
            }

            for line in &hunk.lines {
                let display_line = build_unified_line(line, &file.path, highlighter, app);
                lines.push(display_line);
            }
        }

        // Blank separator between files
        lines.push(Line::from(""));
    }

    let total_lines = lines.len();

    let paragraph = Paragraph::new(lines).scroll((app.scroll_offset as u16, 0));
    frame.render_widget(paragraph, inner_area);

    // Scrollbar
    let visible_height = inner_area.height as usize;
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

    let mut left_lines: Vec<Line> = Vec::new();
    let mut right_lines: Vec<Line> = Vec::new();

    for (file_idx, file) in files.iter().enumerate() {
        let status_char = match file.status {
            FileStatus::Modified => "M",
            FileStatus::Added => "A",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
            FileStatus::Untracked => "?",
        };

        let collapse_char = if file.collapsed { "▶" } else { "▼" };
        let header_text = format!(
            "{} {} {}  (+{} -{})",
            collapse_char, status_char, file.path, file.additions, file.deletions
        );

        let is_focused = app.focused_file == Some(file_idx);
        let header_style = if is_focused {
            Style::default()
                .fg(FG_HEADER)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default()
                .fg(FG_HEADER)
                .add_modifier(Modifier::BOLD)
        };

        left_lines.push(Line::from(Span::styled(header_text.clone(), header_style)));
        right_lines.push(Line::from(Span::styled(header_text, header_style)));

        if file.collapsed {
            continue;
        }

        let sbs_hunks = diff::compute_side_by_side(&file.hunks);

        for (hunk_idx, sbs_lines) in sbs_hunks.iter().enumerate() {
            // Hunk header
            if hunk_idx < file.hunks.len() && !file.hunks[hunk_idx].header.is_empty() {
                let hunk_line = Line::from(Span::styled(
                    format!("  {}", file.hunks[hunk_idx].header),
                    Style::default().fg(FG_HUNK),
                ));
                left_lines.push(hunk_line.clone());
                right_lines.push(hunk_line);
            }

            for sbs in sbs_lines {
                left_lines.push(build_sbs_line(
                    &sbs.left,
                    &file.path,
                    highlighter,
                    app,
                ));
                right_lines.push(build_sbs_line(
                    &sbs.right,
                    &file.path,
                    highlighter,
                    app,
                ));
            }
        }

        left_lines.push(Line::from(""));
        right_lines.push(Line::from(""));
    }

    let left_para = Paragraph::new(left_lines).scroll((app.scroll_offset as u16, 0));
    let right_para = Paragraph::new(right_lines).scroll((app.scroll_offset as u16, 0));

    frame.render_widget(left_para, halves[0]);
    frame.render_widget(right_para, halves[1]);
}

fn build_unified_line<'a>(
    line: &crate::diff::DiffLine,
    file_path: &str,
    highlighter: &Highlighter,
    app: &App,
) -> Line<'a> {
    let is_flashing = app.is_line_flashing(line);
    let prefix = match line.kind {
        LineKind::Context => "  ",
        LineKind::Addition => "+ ",
        LineKind::Deletion => "- ",
        LineKind::HunkHeader => "  ",
        LineKind::FileHeader => "  ",
    };

    let lineno = format_lineno(line);

    let bg = if is_flashing {
        Some(BG_FLASH)
    } else {
        match line.kind {
            LineKind::Addition => Some(BG_ADD),
            LineKind::Deletion => Some(BG_DEL),
            _ => None,
        }
    };

    let mut highlighted = highlighter.highlight_line_content(&line.content, file_path, bg);

    // Prepend line number and prefix
    let prefix_style = match line.kind {
        LineKind::Addition => Style::default().fg(FG_ADD).bg(bg.unwrap_or_default()),
        LineKind::Deletion => Style::default().fg(FG_DEL).bg(bg.unwrap_or_default()),
        _ => Style::default().fg(FG_MUTED),
    };

    let mut spans = vec![
        Span::styled(lineno, Style::default().fg(FG_MUTED)),
        Span::styled(prefix.to_string(), prefix_style),
    ];
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

            let mut highlighted =
                highlighter.highlight_line_content(&line.content, file_path, bg);
            let mut spans = vec![Span::styled(prefix.to_string(), prefix_style)];
            spans.append(&mut highlighted.spans);
            Line::from(spans)
        }
        None => Line::from(Span::styled("~", Style::default().fg(FG_MUTED))),
    }
}

fn format_lineno(line: &crate::diff::DiffLine) -> String {
    let old = line
        .old_lineno
        .map(|n| format!("{:>4}", n))
        .unwrap_or_else(|| "    ".to_string());
    let new = line
        .new_lineno
        .map(|n| format!("{:>4}", n))
        .unwrap_or_else(|| "    ".to_string());
    format!("{} {} │", old, new)
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let total_files: usize = app.file_diffs.iter().map(|f| f.len()).sum();
    let mode = app.current_mode().label();
    let view = if app.side_by_side { "side-by-side" } else { "unified" };
    let repo_count = app.repos.len();

    let status_text = format!(
        " changes v{} │ {} repo{} │ {} file{} │ {} │ {} │ q:quit ?:help",
        env!("CARGO_PKG_VERSION"),
        repo_count,
        if repo_count != 1 { "s" } else { "" },
        total_files,
        if total_files != 1 { "s" } else { "" },
        mode,
        view,
    );

    let mut error_text = String::new();
    if let Some(ref err) = app.last_error {
        error_text = format!(" │ {}", err);
    }

    let status = Paragraph::new(Line::from(vec![
        Span::styled(status_text, Style::default().fg(Color::White)),
        Span::styled(error_text, Style::default().fg(Color::Red)),
    ]))
    .style(Style::default().bg(Color::Rgb(40, 40, 50)));

    frame.render_widget(status, area);
}
