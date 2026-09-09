use crate::app::{App, CompareRow};
use crate::diff::{FileStatus, LineKind};
use crate::git::{Base, BaseCandidates, DiffMode, RangeKind};
use crate::highlight::Highlighter;
use crate::outline::{self, OutlineRow, SymbolChange, hunk_context};
use crate::theme::theme;
use crate::viewport::{RowRef, chunk_end, side_by_side_gutter_width, side_by_side_pane_widths};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};
use unicode_width::UnicodeWidthStr;

const TAB_BAR_HEIGHT: u16 = 1;
/// Rows the outline / flow view spends on its heading before the first row.
pub const OUTLINE_HEADER_ROWS: u16 = 1;

/// The one popup frame: rounded, a muted border, bold title top-left, key hints along the
/// bottom edge, and a column of padding so text never touches the border.
fn popup_block<'a>(title: &'a str, hint: &'a str) -> Block<'a> {
    let t = theme();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.surface_raised))
        .title(Line::from(Span::styled(format!(" {title} "), t.bold())))
        .padding(Padding::horizontal(1))
        .style(t.popup_style());
    if !hint.is_empty() {
        block = block.title_bottom(
            Line::from(Span::styled(format!(" {hint} "), t.muted_style())).right_aligned(),
        );
    }
    block
}

/// A text-input row in a popup: accent prompt, the query, and a block cursor.
fn prompt_line(query: &str) -> Line<'static> {
    let t = theme();
    Line::from(vec![
        Span::styled("› ", t.accent_style()),
        Span::styled(query.to_string(), t.text_style()),
        Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
    ])
}

/// Truncate to `width` columns with an ellipsis.
fn fit_to_width(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Bold title on the left, muted key hints on the right, on the first row of a view.
fn draw_view_heading(frame: &mut Frame, area: Rect, title: &str, hint: &str) {
    let width = area.width as usize;
    let title_width = UnicodeWidthStr::width(title);
    let hint_width = UnicodeWidthStr::width(hint);
    let mut spans = vec![Span::styled(title.to_string(), theme().bold())];
    if title_width + 2 + hint_width <= width {
        spans.push(Span::raw(" ".repeat(width - title_width - hint_width)));
        spans.push(Span::styled(hint.to_string(), theme().muted_style()));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

/// Layout positions computed during rendering, needed for mouse hit-testing.
/// Kept separate from App so `draw()` doesn't require `&mut App`.
#[derive(Default)]
pub struct LayoutHints {
    pub tab_bar_row: u16,
    pub tab_positions: Vec<(u16, u16)>,
    /// Screen row of the timeline strip and the column span of each visible step.
    pub timeline_row: u16,
    pub timeline_positions: Vec<(usize, u16, u16)>,
    pub mode_badge_pos: (u16, u16),
    pub view_badge_pos: (u16, u16),
    pub status_bar_row: u16,
    pub content_y: u16,
    pub content_height: u16,
    pub content_width: u16,
}

/// Rows the timeline strip takes above the content when it is open.
pub const TIMELINE_ROWS: u16 = 2;

/// Screen rows: tab strip, a hairline rule, the timeline (when open), the content area,
/// the status line.
fn screen_chunks(area: Rect, timeline_rows: u16) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(TAB_BAR_HEIGHT),
            Constraint::Length(1),
            Constraint::Length(timeline_rows),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area)
}

/// The content area minus a one-column left margin and the scrollbar column on the right.
/// No box border: the file header bars give the diff its structure.
fn content_inner(area: Rect) -> Rect {
    Rect::new(
        area.x + 1,
        area.y,
        area.width.saturating_sub(2),
        area.height,
    )
}

/// The column the scrollbar occupies, at the right edge of the content area.
fn scrollbar_area(area: Rect) -> Rect {
    Rect::new(
        area.x + area.width.saturating_sub(1),
        area.y,
        1,
        area.height,
    )
}

pub fn diff_inner_area(area: Rect, timeline_rows: u16) -> Rect {
    let chunks = screen_chunks(area, timeline_rows);
    content_inner(chunks[3])
}

fn draw_scrollbar(frame: &mut Frame, area: Rect, total: usize, visible: usize, position: usize) {
    if total <= visible {
        return;
    }
    let mut state = ScrollbarState::new(total.saturating_sub(visible)).position(position);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .thumb_symbol("┃")
        .track_style(Style::default().fg(theme().surface_raised))
        .thumb_style(Style::default().fg(theme().muted));
    frame.render_stateful_widget(scrollbar, scrollbar_area(area), &mut state);
}

fn draw_rule(frame: &mut Frame, area: Rect) {
    let rule = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(
            rule,
            Style::default().fg(theme().surface_raised),
        )),
        area,
    );
}

pub fn draw(frame: &mut Frame, app: &App, highlighter: &Highlighter, hints: &mut LayoutHints) {
    let timeline_rows = app.timeline_rows();
    let chunks = screen_chunks(frame.area(), timeline_rows);

    // Compute content area top for mouse hit-testing
    let diff_inner = diff_inner_area(frame.area(), timeline_rows);
    hints.content_y = diff_inner.y;
    hints.content_height = diff_inner.height;
    hints.content_width = diff_inner.width;

    draw_tab_bar(frame, app, hints, chunks[0]);
    draw_rule(frame, chunks[1]);
    if timeline_rows > 0 {
        draw_timeline(frame, app, hints, chunks[2]);
    }
    draw_diff_area(frame, app, highlighter, chunks[3]);
    draw_status_bar(frame, app, hints, chunks[4]);

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
    } else if app.compare_picker.is_some() {
        draw_compare_picker(frame, app);
    } else if app.show_help {
        draw_help_overlay(frame);
    }
}

fn tab_label(index: usize, repo: &crate::app::RepoState) -> String {
    // "1 name  +12 -3  ✎2"
    let mut label = format!("{} {}", index + 1, repo.info.name);
    if !repo.files.is_empty() {
        let (adds, dels) = repo
            .files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
        label.push_str(&format!("  +{adds} -{dels}"));
    }
    if !repo.comments.is_empty() {
        label.push_str(&format!("  ✎{}", repo.comments.len()));
    }
    label
}

fn draw_tab_bar(frame: &mut Frame, app: &App, hints: &mut LayoutHints, area: Rect) {
    hints.tab_bar_row = area.y;
    hints.tab_positions.clear();

    let labels: Vec<String> = app
        .repos
        .iter()
        .enumerate()
        .map(|(i, repo)| tab_label(i, repo))
        .collect();
    // Each tab is padded by a space on each side and followed by two columns of spacing.
    let widths: Vec<usize> = labels
        .iter()
        .map(|label| UnicodeWidthStr::width(label.as_str()) + 4)
        .collect();
    let total_width: usize = widths.iter().sum();
    let area_width = area.width as usize;

    // Scroll the strip so the active tab is always fully visible, leaving room for the
    // "‹" / "›" overflow markers when tabs are hidden on either side.
    let overflow = total_width > area_width;
    let marker_width = usize::from(overflow);
    let visible_width = area_width.saturating_sub(marker_width * 2);
    let active_end: usize = widths[..(app.active_tab + 1).min(widths.len())]
        .iter()
        .sum();
    let scroll = if overflow && active_end > visible_width {
        active_end - visible_width
    } else {
        0
    };

    let mut spans: Vec<Span> = Vec::new();
    let mut col = area.x as usize;
    if overflow {
        let marker = if scroll > 0 { "‹" } else { " " };
        spans.push(Span::styled(marker, Style::default().fg(theme().muted)));
        col += 1;
    }

    let mut consumed = 0usize; // width of tabs walked so far, in strip coordinates
    let strip_end = scroll + visible_width;
    let mut hidden_right = false;
    for (i, (label, width)) in labels.iter().zip(&widths).enumerate() {
        let tab_start = consumed;
        let tab_end = consumed + width;
        consumed = tab_end;
        if tab_end <= scroll {
            continue;
        }
        if tab_start >= strip_end {
            hidden_right = true;
            break;
        }
        if tab_end > strip_end + 1 {
            // Partially visible tab on the right: clip it and flag the overflow.
            hidden_right = true;
        }

        let repo = &app.repos[i];
        let is_active = i == app.active_tab;
        // The active tab is the accent color, underlined; others are plain or dim when
        // they have nothing to show. No background blocks.
        let style = if is_active {
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else if repo.files.is_empty() {
            Style::default().fg(theme().muted)
        } else {
            Style::default().fg(theme().text)
        };

        // Clip a tab that starts before the scroll offset (only happens on the left edge).
        let padded = format!(" {label} ");
        let skip = scroll.saturating_sub(tab_start);
        let shown: String = padded.chars().skip(skip).collect();
        let shown_width = UnicodeWidthStr::width(shown.as_str());
        let start_col = col as u16;
        spans.push(Span::styled(shown, style));
        spans.push(Span::raw("  "));
        col += shown_width + 2;
        hints.tab_positions.push((start_col, col as u16));
    }
    if overflow {
        let marker_col = area.x + area.width - 1;
        let marker = if hidden_right { "›" } else { " " };
        let strip = Rect::new(area.x, area.y, area.width.saturating_sub(1), 1);
        frame.render_widget(Paragraph::new(Line::from(spans)), strip);
        frame.render_widget(
            Paragraph::new(Span::styled(marker, Style::default().fg(theme().muted))),
            Rect::new(marker_col, area.y, 1, 1),
        );
    } else {
        if let Some(last) = hints.tab_positions.last_mut() {
            last.1 = area.x + area.width;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

/// The timeline: a strip of nodes from the base to the working tree with the cursor
/// highlighted, then a detail line for the step under the cursor. The strip windows
/// around the cursor when the commits do not fit, like the tab strip.
fn draw_timeline(frame: &mut Frame, app: &App, hints: &mut LayoutHints, area: Rect) {
    let t = theme();
    let Some(state) = app.timeline() else {
        return;
    };
    hints.timeline_row = area.y;
    hints.timeline_positions.clear();
    let strip = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
    let detail = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1);

    // One glyph per step so a whole branch fits on the strip; the detail line names the
    // step under the cursor. Base on the left, working tree on the right.
    let base_token = format!("○ {} ", state.base_label);
    let base_width = UnicodeWidthStr::width(base_token.as_str());
    let node_width = 2usize; // glyph plus a hairline
    let available = strip.width as usize;
    let count = state.steps.len();

    // Window around the cursor when the commits do not fit, with counts for the hidden.
    let marker_width = 10;
    let fits = available.saturating_sub(base_width) / node_width;
    let (first, last) = if count <= fits {
        (0, count - 1)
    } else {
        let visible = available.saturating_sub(marker_width * 2) / node_width;
        let half = visible / 2;
        let first = state.cursor.saturating_sub(half).min(count - visible);
        (first, (first + visible - 1).min(count - 1))
    };

    let mut spans: Vec<Span> = Vec::new();
    let mut col = strip.x as usize;
    let push = |spans: &mut Vec<Span>, col: &mut usize, text: String, style: Style| {
        *col += UnicodeWidthStr::width(text.as_str());
        spans.push(Span::styled(text, style));
    };
    if first == 0 {
        push(&mut spans, &mut col, base_token, t.muted_style());
    } else {
        push(
            &mut spans,
            &mut col,
            format!("‹ {first} more "),
            t.muted_style(),
        );
    }
    let rail = Style::default().fg(t.surface_raised);
    for index in first..=last {
        let step = &state.steps[index];
        let is_cursor = index == state.cursor;
        let included = state.since && index < state.cursor;
        let (glyph, style) = match (step.id.is_none(), is_cursor, included) {
            (_, true, _) => (
                "◉",
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            (true, false, _) => ("◌", t.muted_style()),
            (false, false, true) => ("●", t.text_style()),
            (false, false, false) => ("●", t.muted_style()),
        };
        let start = col as u16;
        push(&mut spans, &mut col, "─".to_string(), rail);
        push(&mut spans, &mut col, glyph.to_string(), style);
        hints.timeline_positions.push((index, start, col as u16));
    }
    if last + 1 < count {
        push(
            &mut spans,
            &mut col,
            format!("─ {} more ›", count - last - 1),
            t.muted_style(),
        );
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), strip);

    // Detail line: mode and position, the step, then counts and hints. The subject is
    // the only part allowed to give way when the line is too long.
    let step = state.current();
    let position = format!("{}/{}", state.cursor + 1, state.steps.len());
    let mode_word = if state.since { "SINCE " } else { "STEP " };
    let (adds, dels, file_count) = app
        .current_files()
        .map(|files| {
            let (a, d) = files
                .iter()
                .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
            (a, d, files.len())
        })
        .unwrap_or((0, 0, 0));
    let counts = format!(
        "  ·  {} file{}  ",
        file_count,
        if file_count == 1 { "" } else { "s" }
    );
    let hint = "< > scrub · s since/step · l close";
    let (sha, subject, when) = match &step.id {
        Some(_) => (
            step.short.clone(),
            step.subject.clone(),
            format!("  ·  {}", step.when),
        ),
        None => (
            String::new(),
            "working tree".to_string(),
            "  ·  uncommitted".to_string(),
        ),
    };
    let fixed_width = UnicodeWidthStr::width(mode_word)
        + UnicodeWidthStr::width(position.as_str())
        + 3
        + UnicodeWidthStr::width(sha.as_str())
        + 2
        + UnicodeWidthStr::width(when.as_str())
        + UnicodeWidthStr::width(counts.as_str())
        + format!("+{adds} -{dels}").len();
    let hint_room = UnicodeWidthStr::width(hint) + 3;
    let total = detail.width as usize;
    let subject_room = total
        .saturating_sub(fixed_width + hint_room)
        .max(total.saturating_sub(fixed_width) / 2);
    let subject = fit_to_width(&subject, subject_room);

    let mut detail_spans: Vec<Span> = vec![
        Span::styled(
            mode_word,
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(position, t.text_style()),
        Span::styled("   ", t.muted_style()),
    ];
    if !sha.is_empty() {
        detail_spans.push(Span::styled(sha, t.accent_style()));
        detail_spans.push(Span::raw("  "));
    }
    detail_spans.push(Span::styled(subject, t.text_style()));
    detail_spans.push(Span::styled(when, t.muted_style()));
    detail_spans.push(Span::styled(counts, t.muted_style()));
    detail_spans.push(Span::styled(
        format!("+{adds}"),
        Style::default().fg(t.add_fg),
    ));
    detail_spans.push(Span::styled(
        format!(" -{dels}"),
        Style::default().fg(t.del_fg),
    ));
    let used: usize = detail_spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    if used + hint_room <= total {
        detail_spans.push(Span::raw(
            " ".repeat(total - used - UnicodeWidthStr::width(hint)),
        ));
        detail_spans.push(Span::styled(hint, t.muted_style()));
    }
    frame.render_widget(Paragraph::new(Line::from(detail_spans)), detail);
}

fn draw_empty_state(frame: &mut Frame, app: &App, area: Rect) {
    let t = theme();
    let inner = content_inner(area);

    let repo = app.repos.get(app.active_tab);
    let loaded = repo.is_none_or(|r| r.loaded);
    let (headline, hint) = if loaded {
        empty_state_text(app.current_mode(), &app.current_bases())
    } else {
        (
            "computing diff…".to_string(),
            "Reading the repository. Large repos can take a few seconds.",
        )
    };
    let headline = headline.trim_start_matches("> ").to_string();
    let watching = repo
        .map(|r| format!("watching {}", r.info.path.display()))
        .unwrap_or_default();

    let logo = [
        r#"██████╗  ██╗  ██╗  █████╗  ███╗   ██╗  ██████╗  ███████╗ ███████╗"#,
        r#"██╔════╝  ██║  ██║ ██╔══██╗ ████╗  ██║ ██╔════╝  ██╔════╝ ██╔════╝"#,
        r#"██║       ███████║ ███████║ ██╔██╗ ██║ ██║  ███╗ █████╗   ███████╗"#,
        r#"██║       ██╔══██║ ██╔══██║ ██║╚██╗██║ ██║   ██║ ██╔══╝   ╚════██║"#,
        r#"╚██████╗  ██║  ██║ ██║  ██║ ██║ ╚████║ ╚██████╔╝ ███████╗ ███████║"#,
        r#" ╚═════╝  ╚═╝  ╚═╝ ╚═╝  ╚═╝ ╚═╝  ╚═══╝  ╚═════╝  ╚══════╝ ╚══════╝"#,
    ];
    let logo_fits = inner.height >= 12 && inner.width as usize >= logo[0].chars().count() + 2;

    // The logo when there is room, otherwise a small wordmark; then the state and what
    // to do next in quiet text.
    let mut lines: Vec<Line> = Vec::new();
    if logo_fits {
        for row in logo {
            lines.push(Line::from(Span::styled(
                row,
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "changes",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(headline, t.text_style())));
    lines.push(Line::from(Span::styled(hint, t.muted_style())));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(watching, t.muted_style())));
    let top_pad = inner.height.saturating_sub(lines.len() as u16) / 2;
    let area = Rect::new(
        inner.x,
        inner.y + top_pad,
        inner.width,
        inner.height.saturating_sub(top_pad),
    );
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

fn draw_diff_area(frame: &mut Frame, app: &App, highlighter: &Highlighter, area: Rect) {
    let files = match app.current_files() {
        Some(f) => f,
        None => {
            draw_empty_state(frame, app, area);
            return;
        }
    };
    let layout = match app.current_layout() {
        Some(layout) => layout,
        None => {
            draw_empty_state(frame, app, area);
            return;
        }
    };

    if files.is_empty() {
        draw_empty_state(frame, app, area);
        return;
    }

    if app.outline.is_some() {
        draw_outline(frame, app, area);
    } else if app.side_by_side {
        draw_side_by_side(frame, app, highlighter, files, layout, area);
    } else {
        draw_unified(frame, app, highlighter, files, layout, area);
    }
}

/// The change-shape view: directory tree, per-file counts, and changed declarations.
fn draw_outline(frame: &mut Frame, app: &App, area: Rect) {
    let Some(state) = app.outline.as_ref() else {
        return;
    };
    let indexing = app
        .repos
        .get(app.active_tab)
        .is_some_and(|repo| repo.symbols.is_none());
    let (title, hint) = match (indexing, state.flow) {
        (true, false) => ("Outline", "↵ open · y copy · o diff · indexing calls…"),
        (true, true) => ("Flow", "indexing calls…"),
        (false, true) => (
            "Flow",
            "routes from entry points to changed code · ↵ open · o outline · y copy · t diff",
        ),
        (false, false) => ("Outline", "↵ open · → callers · t flow · y copy · o diff"),
    };
    let content = content_inner(area);
    draw_view_heading(frame, content, title, hint);
    let inner = Rect::new(
        content.x,
        content.y + OUTLINE_HEADER_ROWS,
        content.width,
        content.height.saturating_sub(OUTLINE_HEADER_ROWS),
    );

    let height = inner.height as usize;
    let scroll = state.scroll.min(state.rows.len().saturating_sub(height));
    let width = inner.width as usize;

    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for (index, row) in state.rows.iter().enumerate().skip(scroll).take(height) {
        let selected = index == state.selected;
        let row_bg = if selected {
            Some(theme().surface_raised)
        } else {
            None
        };
        let with_bg = |style: Style| match row_bg {
            Some(bg) => style.bg(bg),
            None => style,
        };
        let mut spans: Vec<Span> = vec![Span::styled(" ", with_bg(Style::default()))];
        let counts: Option<(usize, usize)> = match row {
            OutlineRow::Dir {
                prefix,
                name,
                additions,
                deletions,
            } => {
                spans.push(Span::styled(
                    prefix.clone(),
                    with_bg(Style::default().fg(theme().muted)),
                ));
                spans.push(Span::styled(
                    format!("{name}/"),
                    with_bg(
                        Style::default()
                            .fg(theme().muted)
                            .add_modifier(Modifier::BOLD),
                    ),
                ));
                Some((*additions, *deletions))
            }
            OutlineRow::File {
                prefix,
                name,
                status,
                additions,
                deletions,
                ..
            } => {
                spans.push(Span::styled(
                    prefix.clone(),
                    with_bg(Style::default().fg(theme().muted)),
                ));
                spans.push(Span::styled(
                    format!("{} ", outline::status_glyph(*status)),
                    with_bg(
                        Style::default()
                            .fg(status_color(*status))
                            .add_modifier(Modifier::BOLD),
                    ),
                ));
                spans.push(Span::styled(
                    name.clone(),
                    with_bg(Style::default().fg(theme().text).add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    })),
                ));
                Some((*additions, *deletions))
            }
            OutlineRow::Symbol { prefix, symbol, .. } => {
                spans.push(Span::styled(
                    prefix.clone(),
                    with_bg(Style::default().fg(theme().muted)),
                ));
                let (glyph_color, name_color) = match symbol.change {
                    SymbolChange::Added => (theme().add_fg, theme().text),
                    SymbolChange::Removed => (theme().del_fg, theme().muted),
                    SymbolChange::Modified => (theme().warn, theme().text),
                };
                spans.push(Span::styled(
                    format!("{} ", outline::change_glyph(symbol.change)),
                    with_bg(
                        Style::default()
                            .fg(glyph_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                ));
                spans.push(Span::styled(
                    symbol.name.clone(),
                    with_bg(Style::default().fg(name_color)),
                ));
                None
            }
            OutlineRow::More { prefix, count, .. } => {
                spans.push(Span::styled(
                    prefix.clone(),
                    with_bg(Style::default().fg(theme().muted)),
                ));
                spans.push(Span::styled(
                    format!("… {count} more"),
                    with_bg(
                        Style::default()
                            .fg(theme().muted)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ));
                None
            }
            OutlineRow::Summary {
                prefix,
                callers,
                callees,
                warning,
                expanded,
                ..
            } => {
                spans.push(Span::styled(
                    prefix.clone(),
                    with_bg(Style::default().fg(theme().muted)),
                ));
                let caret = if *expanded { "▾ " } else { "▸ " };
                spans.push(Span::styled(
                    caret,
                    with_bg(Style::default().fg(theme().muted)),
                ));
                match warning {
                    Some(warning) => {
                        spans.push(Span::styled(
                            format!("⚠ {warning}"),
                            with_bg(
                                Style::default()
                                    .fg(theme().warn)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ));
                        spans.push(Span::styled(
                            format!("  · calls {callees}"),
                            with_bg(Style::default().fg(theme().muted)),
                        ));
                    }
                    None => {
                        spans.push(Span::styled(
                            format!("called by {callers} · calls {callees}"),
                            with_bg(Style::default().fg(theme().muted)),
                        ));
                    }
                }
                None
            }
            OutlineRow::Section {
                prefix,
                label,
                count,
            } => {
                spans.push(Span::styled(
                    prefix.clone(),
                    with_bg(Style::default().fg(theme().muted)),
                ));
                let text = if *count > 0 {
                    format!("{label} ({count})")
                } else {
                    label.to_string()
                };
                spans.push(Span::styled(
                    text,
                    with_bg(
                        Style::default()
                            .fg(theme().accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                ));
                None
            }
            OutlineRow::Flow {
                depth,
                name,
                location,
                mark,
                file_idx,
                is_target,
                warning,
                ..
            } => {
                // Mark column first, then indentation by depth: the diff idiom.
                let (glyph, glyph_color) = match mark {
                    Some(SymbolChange::Added) => ("+", theme().add_fg),
                    Some(SymbolChange::Removed) => ("-", theme().del_fg),
                    Some(SymbolChange::Modified) => ("~", theme().warn),
                    None => (" ", theme().muted),
                };
                spans.push(Span::styled(
                    format!("{glyph} "),
                    with_bg(
                        Style::default()
                            .fg(glyph_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                ));
                spans.push(Span::styled("  ".repeat(*depth), with_bg(Style::default())));
                let name_style = if *is_target {
                    Style::default()
                        .fg(if file_idx.is_some() {
                            theme().text
                        } else {
                            theme().muted
                        })
                        .add_modifier(Modifier::BOLD)
                } else if mark.is_some() {
                    Style::default().fg(theme().text)
                } else {
                    Style::default().fg(theme().muted)
                };
                spans.push(Span::styled(name.clone(), with_bg(name_style)));
                spans.push(Span::styled(
                    format!("  {location}"),
                    with_bg(Style::default().fg(theme().muted)),
                ));
                if let Some(warning) = warning {
                    spans.push(Span::styled(
                        format!("   ⚠ {warning}"),
                        with_bg(
                            Style::default()
                                .fg(theme().warn)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ));
                }
                None
            }
            OutlineRow::Call {
                prefix,
                name,
                location,
                mark,
                file_idx,
                ..
            } => {
                spans.push(Span::styled(
                    prefix.clone(),
                    with_bg(Style::default().fg(theme().muted)),
                ));
                if let Some(mark) = mark {
                    let color = match mark {
                        SymbolChange::Added => theme().add_fg,
                        SymbolChange::Removed => theme().del_fg,
                        SymbolChange::Modified => theme().warn,
                    };
                    spans.push(Span::styled(
                        format!("{} ", outline::change_glyph(*mark)),
                        with_bg(Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    ));
                }
                let name_color = if file_idx.is_some() {
                    theme().text
                } else {
                    theme().muted
                };
                spans.push(Span::styled(
                    name.clone(),
                    with_bg(Style::default().fg(name_color)),
                ));
                spans.push(Span::styled(
                    format!("  {location}"),
                    with_bg(Style::default().fg(theme().muted)),
                ));
                None
            }
        };
        let used: usize = spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        if let Some((additions, deletions)) = counts {
            let adds = format!("+{additions}");
            let dels = format!("-{deletions}");
            let tail = adds.len() + 1 + dels.len() + 1;
            let pad = width.saturating_sub(used + tail);
            spans.push(Span::styled(" ".repeat(pad), with_bg(Style::default())));
            spans.push(Span::styled(
                adds,
                with_bg(Style::default().fg(theme().add_fg)),
            ));
            spans.push(Span::styled(" ", with_bg(Style::default())));
            spans.push(Span::styled(
                dels,
                with_bg(Style::default().fg(theme().del_fg)),
            ));
            spans.push(Span::styled(" ", with_bg(Style::default())));
        } else if selected {
            spans.push(Span::styled(
                " ".repeat(width.saturating_sub(used)),
                with_bg(Style::default()),
            ));
        }
        lines.push(Line::from(spans));
    }
    if state.rows.is_empty() {
        let text = if state.flow && indexing {
            "  Indexing calls… the flow view appears when the index is ready"
        } else if state.flow {
            "  No changed functions to trace: types, data and prose have no call routes"
        } else {
            "  No changes to outline"
        };
        lines.push(Line::from(Span::styled(
            text,
            Style::default().fg(theme().muted),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);

    draw_scrollbar(frame, inner, state.rows.len(), height, scroll);
}

fn status_color(status: FileStatus) -> Color {
    match status {
        FileStatus::Modified => theme().muted,
        FileStatus::Added | FileStatus::Untracked => theme().add_fg,
        FileStatus::Deleted => theme().del_fg,
        FileStatus::Renamed => theme().accent,
    }
}

/// The gutter bar between line numbers and code. Heavier and brighter for the hunk
/// that `y` / `n` will act on, so the keyboard target is always visible.
fn gutter_separator<'a>(focused: bool) -> Span<'a> {
    if focused {
        Span::styled(
            " ┃",
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" │", Style::default().fg(theme().surface_raised))
    }
}

/// Style the inline caller / callee line: arrows in blue, warnings in yellow, names muted.
fn call_context_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(" ")];
    for (i, part) in text.split(" · ").enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(theme().muted)));
        }
        if let Some(rest) = part.strip_prefix('⚠') {
            spans.push(Span::styled(
                format!("⚠{rest}"),
                Style::default()
                    .fg(theme().warn)
                    .add_modifier(Modifier::BOLD),
            ));
        } else if let Some(rest) = part.strip_prefix('↑').or_else(|| part.strip_prefix('↓')) {
            let arrow = &part[..part.len() - rest.len()];
            spans.push(Span::styled(
                arrow.to_string(),
                Style::default().fg(theme().accent),
            ));
            spans.push(Span::styled(
                rest.to_string(),
                Style::default().fg(theme().muted),
            ));
        } else {
            spans.push(Span::styled(
                part.to_string(),
                Style::default()
                    .fg(theme().muted)
                    .add_modifier(Modifier::ITALIC),
            ));
        }
    }
    spans
}

/// Row shown between hunks: an expand indicator when lines are hidden, otherwise a plain break.
fn hunk_header_line<'a>(
    hunk: Option<&crate::diff::Hunk>,
    hunk_idx: usize,
    gap_before: usize,
    has_comment: bool,
    focused: bool,
    numbers_width: usize,
) -> Line<'a> {
    let mut spans = Vec::new();
    if gap_before > 0 {
        spans.push(Span::styled(
            format_expand_indicator(gap_before, numbers_width),
            Style::default().fg(theme().accent),
        ));
        spans.push(gutter_separator(focused));
        if let Some(ctx) = hunk.and_then(|hunk| hunk_context(&hunk.header)) {
            spans.push(Span::styled(
                format!(" {}", ctx),
                Style::default().fg(theme().accent),
            ));
        }
    } else if hunk_idx > 0 || focused {
        spans.push(Span::raw(" ".repeat(numbers_width)));
        spans.push(gutter_separator(focused));
    }
    if has_comment {
        spans.push(Span::styled(" [!]", Style::default().fg(theme().note)));
    }
    Line::from(spans)
}

fn draw_unified(
    frame: &mut Frame,
    app: &App,
    highlighter: &Highlighter,
    files: &[crate::diff::FileDiff],
    layout: &crate::viewport::DiffLayout,
    area: Rect,
) {
    let inner_area = content_inner(area);

    let visible_height = inner_area.height as usize;
    let total_lines = layout.total_lines();
    let mut visible_rows = app.visible_row_range(total_lines, visible_height);
    let mut lines: Vec<Line> = Vec::with_capacity(visible_height);
    let focused_hunk = app.focused_hunk();

    // Pin the current file's header to the top once its own header has scrolled away.
    if let Some(file_idx) = app.sticky_header_file()
        && let Some(file) = files.get(file_idx)
    {
        lines.push(build_file_header(
            file,
            app.focused_file == Some(file_idx),
            inner_area.width,
        ));
        visible_rows.next();
    }

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
                let lno_w = layout.lineno_width(file_idx);
                lines.push(hunk_header_line(
                    file.hunks.get(hunk_idx),
                    hunk_idx,
                    gap_before,
                    layout.hunk_has_comment(file_idx, hunk_idx),
                    focused_hunk == Some((file_idx, hunk_idx)),
                    lno_w * 2 + 1,
                ));
            }
            RowRef::Comment {
                file_idx,
                hunk_idx,
                wrap_idx,
            } => {
                if let Some(text) = layout.comment_line_text(file_idx, hunk_idx, wrap_idx) {
                    let lno_w = layout.lineno_width(file_idx);
                    let gutter = " ".repeat(lno_w * 2 + 1) + " ┃";
                    lines.push(Line::from(vec![
                        Span::styled(gutter, Style::default().fg(theme().note)),
                        Span::styled(format!(" {}", text), Style::default().fg(theme().note)),
                    ]));
                }
            }
            RowRef::CallContext {
                file_idx,
                hunk_idx,
                line_idx,
            } => {
                if let Some(text) = layout.call_context_text(file_idx, hunk_idx, line_idx) {
                    let lno_w = layout.lineno_width(file_idx);
                    let focused = focused_hunk == Some((file_idx, hunk_idx));
                    let mut spans = vec![
                        Span::raw(" ".repeat(lno_w * 2 + 1)),
                        gutter_separator(focused),
                    ];
                    spans.extend(call_context_spans(text));
                    lines.push(Line::from(spans));
                }
            }
            RowRef::UnifiedLine {
                file_idx,
                hunk_idx,
                line_idx,
                chunk_idx,
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
                let focused = focused_hunk == Some((file_idx, hunk_idx));
                let chunk_start = layout.chunk_start(row).unwrap_or(0);
                lines.push(build_unified_line(
                    line,
                    &file.path,
                    highlighter,
                    flashing,
                    focused,
                    layout.lineno_width(file_idx),
                    inner_area.width as usize,
                    chunk_idx,
                    chunk_start,
                    file.is_whole_file_change(),
                    layout.emphasis(file_idx, hunk_idx, line_idx),
                ));
            }
            RowRef::GapTail { gap_after, .. } if gap_after > 0 => {
                let Some(file_idx) = layout.row_file_idx(row) else {
                    continue;
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format_expand_indicator(gap_after, layout.lineno_width(file_idx) * 2 + 1),
                        Style::default().fg(theme().accent),
                    ),
                    gutter_separator(false),
                ]));
            }
            RowRef::Blank { .. } | RowRef::GapTail { .. } => lines.push(Line::from("")),
            RowRef::SideBySideLine { .. } => {}
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner_area);
    draw_scrollbar(
        frame,
        area,
        total_lines,
        visible_height,
        app.current_scroll_offset(),
    );
}

fn draw_side_by_side(
    frame: &mut Frame,
    app: &App,
    highlighter: &Highlighter,
    files: &[crate::diff::FileDiff],
    layout: &crate::viewport::DiffLayout,
    area: Rect,
) {
    let inner_area = content_inner(area);

    let (left_width, right_width) = side_by_side_pane_widths(inner_area.width as usize);
    let left_rect = Rect::new(
        inner_area.x,
        inner_area.y,
        left_width as u16,
        inner_area.height,
    );
    let divider_rect = Rect::new(
        inner_area.x + left_width as u16,
        inner_area.y,
        u16::from(inner_area.width > left_width as u16),
        inner_area.height,
    );
    let right_rect = Rect::new(
        divider_rect.x + divider_rect.width,
        inner_area.y,
        right_width as u16,
        inner_area.height,
    );

    let visible_height = inner_area.height as usize;
    let total_lines = layout.total_lines();
    let mut visible_rows = app.visible_row_range(total_lines, visible_height);

    let mut left_lines: Vec<Line> = Vec::with_capacity(visible_height);
    let mut right_lines: Vec<Line> = Vec::with_capacity(visible_height);
    let mut divider_lines: Vec<Line> = Vec::with_capacity(visible_height);
    let focused_hunk = app.focused_hunk();
    let divider = Line::from(Span::styled("│", Style::default().fg(theme().muted)));
    let header_divider = Line::from(Span::styled(" ", Style::default().bg(theme().surface)));
    let header_fill = |width: u16| {
        Line::from(Span::styled(
            " ".repeat(width as usize),
            Style::default().bg(theme().surface),
        ))
    };

    if let Some(file_idx) = app.sticky_header_file()
        && let Some(file) = files.get(file_idx)
    {
        left_lines.push(build_file_header(
            file,
            app.focused_file == Some(file_idx),
            left_rect.width,
        ));
        divider_lines.push(header_divider.clone());
        right_lines.push(header_fill(right_rect.width));
        visible_rows.next();
    }

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
                left_lines.push(build_file_header(file, is_focused, left_rect.width));
                divider_lines.push(header_divider.clone());
                right_lines.push(header_fill(right_rect.width));
            }
            RowRef::HunkHeader {
                file_idx,
                hunk_idx,
                gap_before,
            } => {
                let Some(file) = files.get(file_idx) else {
                    continue;
                };
                let lno_w = layout.lineno_width(file_idx);
                let focused = focused_hunk == Some((file_idx, hunk_idx));
                left_lines.push(hunk_header_line(
                    file.hunks.get(hunk_idx),
                    hunk_idx,
                    gap_before,
                    layout.hunk_has_comment(file_idx, hunk_idx),
                    focused,
                    lno_w,
                ));
                right_lines.push(if hunk_idx > 0 || gap_before > 0 || focused {
                    Line::from(vec![
                        Span::raw(" ".repeat(lno_w)),
                        gutter_separator(focused),
                    ])
                } else {
                    Line::from("")
                });
                divider_lines.push(divider.clone());
            }
            RowRef::SideBySideLine {
                file_idx,
                hunk_idx,
                line_idx,
                chunk_idx,
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
                let lno_w = layout.lineno_width(file_idx);
                let focused = focused_hunk == Some((file_idx, hunk_idx));
                let left = build_sbs_line(
                    &sbs_line.left,
                    &sbs_line.left_changed,
                    &file.path,
                    highlighter,
                    left_rect.width as usize,
                    lno_w,
                    focused,
                    PaneSide::Left,
                    chunk_idx,
                    layout.chunk_start(row),
                    file.is_whole_file_change(),
                )
                .unwrap_or_else(|| sbs_continuation(lno_w, focused));
                let right = build_sbs_line(
                    &sbs_line.right,
                    &sbs_line.right_changed,
                    &file.path,
                    highlighter,
                    right_rect.width as usize,
                    lno_w,
                    focused,
                    PaneSide::Right,
                    chunk_idx,
                    layout.right_chunk_start(row),
                    file.is_whole_file_change(),
                )
                .unwrap_or_else(|| sbs_continuation(lno_w, focused));
                left_lines.push(left);
                right_lines.push(right);
                divider_lines.push(divider.clone());
            }
            RowRef::GapTail {
                file_idx,
                gap_after,
                ..
            } if gap_after > 0 => {
                let lno_w = layout.lineno_width(file_idx);
                let expand_line = Line::from(vec![
                    Span::styled(
                        format_expand_indicator(gap_after, lno_w),
                        Style::default().fg(theme().accent),
                    ),
                    gutter_separator(false),
                ]);
                left_lines.push(expand_line);
                right_lines.push(Line::from(vec![
                    Span::raw(" ".repeat(lno_w)),
                    gutter_separator(false),
                ]));
                divider_lines.push(divider.clone());
            }
            RowRef::Comment {
                file_idx,
                hunk_idx,
                wrap_idx,
            } => {
                if let Some(text) = layout.comment_line_text(file_idx, hunk_idx, wrap_idx) {
                    let lno_w = layout.lineno_width(file_idx);
                    let comment_line = Line::from(vec![
                        Span::styled(" ".repeat(lno_w) + " ┃", Style::default().fg(theme().note)),
                        Span::styled(format!(" {}", text), Style::default().fg(theme().note)),
                    ]);
                    left_lines.push(comment_line);
                    right_lines.push(Line::from(""));
                    divider_lines.push(divider.clone());
                }
            }
            RowRef::CallContext {
                file_idx,
                hunk_idx,
                line_idx,
            } => {
                if let Some(text) = layout.call_context_text(file_idx, hunk_idx, line_idx) {
                    let lno_w = layout.lineno_width(file_idx);
                    let focused = focused_hunk == Some((file_idx, hunk_idx));
                    let mut spans = vec![Span::raw(" ".repeat(lno_w)), gutter_separator(focused)];
                    spans.extend(call_context_spans(text));
                    left_lines.push(Line::from(spans));
                    right_lines.push(Line::from(vec![
                        Span::raw(" ".repeat(lno_w)),
                        gutter_separator(focused),
                    ]));
                    divider_lines.push(divider.clone());
                }
            }
            RowRef::Blank { .. } | RowRef::GapTail { .. } => {
                left_lines.push(Line::from(""));
                right_lines.push(Line::from(""));
                divider_lines.push(Line::from(""));
            }
            RowRef::UnifiedLine { .. } => {}
        }
    }

    frame.render_widget(Paragraph::new(left_lines), left_rect);
    if divider_rect.width > 0 {
        frame.render_widget(Paragraph::new(divider_lines), divider_rect);
    }
    frame.render_widget(Paragraph::new(right_lines), right_rect);
    draw_scrollbar(
        frame,
        area,
        total_lines,
        visible_height,
        app.current_scroll_offset(),
    );
}

#[derive(Clone, Copy)]
enum PaneSide {
    Left,
    Right,
}

/// Gutter-only row used when one side of a wrapped pair has fewer chunks than the other.
fn sbs_continuation<'a>(lno_w: usize, focused: bool) -> Line<'a> {
    Line::from(vec![
        Span::raw(" ".repeat(lno_w)),
        gutter_separator(focused),
    ])
}

/// Byte range of the wrapped chunk starting at `chunk_start`. The layout already knows
/// where every chunk begins, so a row costs one `chunk_end` walk over its own text only.
fn chunk_range_from(content: &str, available: usize, chunk_start: usize) -> std::ops::Range<usize> {
    let start = chunk_start.min(content.len());
    if available == 0 {
        return start..content.len();
    }
    start..chunk_end(content, start, available)
}

#[allow(clippy::too_many_arguments)]
fn build_unified_line<'a>(
    line: &crate::diff::DiffLine,
    file_path: &str,
    highlighter: &Highlighter,
    is_flashing: bool,
    is_focused: bool,
    lno_width: usize,
    content_width: usize,
    chunk_idx: usize,
    chunk_start: usize,
    plain: bool,
    emphasis: Option<&[(usize, usize)]>,
) -> Line<'a> {
    // In a file that is entirely new or entirely deleted every line is on one side, so
    // per-line markers carry no information; show it as source with a header badge.
    let prefix = match line.kind {
        _ if plain => "  ",
        LineKind::Context => "  ",
        LineKind::Addition => "+ ",
        LineKind::Deletion => "- ",
    };

    let bg = if is_flashing {
        Some(theme().flash)
    } else if plain {
        None
    } else {
        match line.kind {
            LineKind::Addition => Some(theme().add_bg),
            LineKind::Deletion => Some(theme().del_bg),
            _ => None,
        }
    };

    let prefix_style = match line.kind {
        _ if plain => Style::default().fg(theme().muted),
        LineKind::Addition => Style::default()
            .fg(theme().add_fg)
            .bg(bg.unwrap_or_default()),
        LineKind::Deletion => Style::default()
            .fg(theme().del_fg)
            .bg(bg.unwrap_or_default()),
        _ => Style::default().fg(theme().muted),
    };

    // Gutter width: line numbers + separator + prefix
    let gutter_width = lno_width * 2 + 1 + 2 + prefix.len(); // "NNNN NNNN │+ "
    let available = content_width.saturating_sub(gutter_width);
    let range = chunk_range_from(&line.content, available, chunk_start);

    let mut spans = if chunk_idx == 0 {
        vec![
            Span::styled(
                format_lineno(line, lno_width),
                Style::default().fg(theme().muted),
            ),
            gutter_separator(is_focused),
            Span::styled(prefix.to_string(), prefix_style),
        ]
    } else {
        vec![
            Span::raw(" ".repeat(lno_width * 2 + 1)),
            gutter_separator(is_focused),
            Span::raw("  "),
        ]
    };
    let mut highlighted =
        highlighter.highlight_line_content(&line.content[range.clone()], file_path, bg);
    if !is_flashing && let Some(ranges) = emphasis {
        let local = ranges_for_chunk(ranges, range.start, range.end);
        let emph = match line.kind {
            LineKind::Addition => theme().add_emph,
            LineKind::Deletion => theme().del_emph,
            LineKind::Context => theme().flash,
        };
        apply_inline_emphasis(&mut highlighted.spans, &local, emph);
    }
    spans.append(&mut highlighted.spans);
    Line::from(spans)
}

#[allow(clippy::too_many_arguments)]
fn build_sbs_line<'a>(
    line_opt: &Option<crate::diff::DiffLine>,
    changed_ranges: &Option<crate::diff::ChangedRanges>,
    file_path: &str,
    highlighter: &Highlighter,
    pane_width: usize,
    lno_width: usize,
    is_focused: bool,
    side: PaneSide,
    chunk_idx: usize,
    chunk_start: Option<usize>,
    plain: bool,
) -> Option<Line<'a>> {
    let Some(line) = line_opt else {
        return (chunk_idx == 0).then(|| {
            Line::from(vec![
                Span::raw(" ".repeat(lno_width)),
                gutter_separator(is_focused),
                Span::styled(" ~", Style::default().fg(theme().muted)),
            ])
        });
    };

    let bg = match line.kind {
        _ if plain => None,
        LineKind::Addition => Some(theme().add_bg),
        LineKind::Deletion => Some(theme().del_bg),
        _ => None,
    };

    let prefix = match line.kind {
        _ if plain => "  ",
        LineKind::Addition => "+ ",
        LineKind::Deletion => "- ",
        _ => "  ",
    };

    let prefix_style = match line.kind {
        _ if plain => Style::default().fg(theme().muted),
        LineKind::Addition => Style::default().fg(theme().add_fg),
        LineKind::Deletion => Style::default().fg(theme().del_fg),
        _ => Style::default().fg(theme().muted),
    };

    let gutter_width = side_by_side_gutter_width(lno_width);
    let available = pane_width.saturating_sub(gutter_width);
    let range = chunk_range_from(&line.content, available, chunk_start?);

    let mut spans = if chunk_idx == 0 {
        let lineno = match side {
            PaneSide::Left => line.old_lineno,
            PaneSide::Right => line.new_lineno,
        };
        let lineno = match lineno {
            Some(n) => format!("{n:>lno_width$}"),
            None => " ".repeat(lno_width),
        };
        vec![
            Span::styled(lineno, Style::default().fg(theme().muted)),
            gutter_separator(is_focused),
            Span::styled(prefix.to_string(), prefix_style),
        ]
    } else {
        vec![
            Span::raw(" ".repeat(lno_width)),
            gutter_separator(is_focused),
            Span::raw("  "),
        ]
    };
    let mut highlighted =
        highlighter.highlight_line_content(&line.content[range.clone()], file_path, bg);
    apply_chunk_emphasis(
        &mut highlighted.spans,
        changed_ranges.as_deref(),
        line.kind,
        range.start,
        range.end,
    );
    spans.append(&mut highlighted.spans);
    Some(Line::from(spans))
}

fn apply_chunk_emphasis(
    spans: &mut Vec<Span<'_>>,
    ranges: Option<&[(usize, usize)]>,
    kind: LineKind,
    chunk_start: usize,
    chunk_end: usize,
) {
    let Some(emphasis) = (match kind {
        LineKind::Addition => Some(theme().add_emph),
        LineKind::Deletion => Some(theme().del_emph),
        LineKind::Context => None,
    }) else {
        return;
    };
    let Some(ranges) = ranges else {
        return;
    };
    let local_ranges = ranges_for_chunk(ranges, chunk_start, chunk_end);
    apply_inline_emphasis(spans, &local_ranges, emphasis);
}

fn ranges_for_chunk(
    ranges: &[(usize, usize)],
    chunk_start: usize,
    chunk_end: usize,
) -> Vec<(usize, usize)> {
    ranges
        .iter()
        .filter_map(|&(start, end)| {
            let start = start.max(chunk_start);
            let end = end.min(chunk_end);
            (start < end).then(|| (start - chunk_start, end - chunk_start))
        })
        .collect()
}

/// Build a full-width centered file header banner.
fn build_file_header<'a>(file: &crate::diff::FileDiff, is_focused: bool, width: u16) -> Line<'a> {
    let collapse = if file.collapsed { "▸" } else { "▾" };

    // Yellow is for warnings; a modified file is the ordinary case and stays quiet.
    let (status_char, status_fg) = match file.status {
        FileStatus::Modified => ("M", theme().muted),
        FileStatus::Added => ("A", theme().add_fg),
        FileStatus::Deleted => ("D", theme().del_fg),
        FileStatus::Renamed => ("R", theme().accent),
        FileStatus::Untracked => ("?", theme().add_fg),
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

    let bg = theme().surface;
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
            Style::default().fg(theme().muted).bg(bg),
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
                .fg(theme().muted)
                .bg(bg)
                .add_modifier(underline),
        ));
        spans.push(Span::styled(
            arrow.to_string(),
            Style::default().fg(theme().muted).bg(bg),
        ));
        if !dir.is_empty() {
            spans.push(Span::styled(
                dir.to_string(),
                Style::default()
                    .fg(theme().muted)
                    .bg(bg)
                    .add_modifier(underline),
            ));
        }
        spans.push(Span::styled(
            filename.to_string(),
            Style::default()
                .fg(theme().text)
                .bg(bg)
                .add_modifier(Modifier::BOLD | underline),
        ));
        UnicodeWidthStr::width(old_path_str)
            + UnicodeWidthStr::width(arrow)
            + UnicodeWidthStr::width(dir)
            + UnicodeWidthStr::width(filename)
    } else {
        if !dir.is_empty() {
            spans.push(Span::styled(
                dir.to_string(),
                Style::default()
                    .fg(theme().muted)
                    .bg(bg)
                    .add_modifier(underline),
            ));
        }
        spans.push(Span::styled(
            filename.to_string(),
            Style::default()
                .fg(theme().text)
                .bg(bg)
                .add_modifier(Modifier::BOLD | underline),
        ));
        UnicodeWidthStr::width(dir) + UnicodeWidthStr::width(filename)
    };
    spans.push(Span::styled("  ", Style::default().bg(bg)));
    let badge = match file.status {
        FileStatus::Added | FileStatus::Untracked => "new file  ",
        FileStatus::Deleted => "deleted  ",
        _ => "",
    };
    if !badge.is_empty() {
        spans.push(Span::styled(
            badge,
            Style::default()
                .fg(theme().muted)
                .bg(bg)
                .add_modifier(Modifier::ITALIC),
        ));
    }
    let used = 1
        + UnicodeWidthStr::width(collapse)
        + 1
        + status_char.len()
        + 2
        + path_display_len
        + 2
        + badge.len()
        + adds.len()
        + 2
        + dels.len();
    spans.push(Span::styled(
        adds,
        Style::default().fg(theme().add_fg).bg(bg),
    ));
    spans.push(Span::styled("  ", Style::default().bg(bg)));
    spans.push(Span::styled(
        dels,
        Style::default().fg(theme().del_fg).bg(bg),
    ));
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
    buf
}

/// Expand indicator right-aligned in a gutter of `numbers_width` columns: `    ↕ NN`
fn format_expand_indicator(gap: usize, numbers_width: usize) -> String {
    let gap_str = format!("{}", gap);
    let used = 1 + 1 + gap_str.len(); // "↕" + " " + digits
    let pad = numbers_width.saturating_sub(used);
    format!("{}↕ {}", " ".repeat(pad), gap_str)
}

fn draw_status_bar(frame: &mut Frame, app: &App, hints: &mut LayoutHints, area: Rect) {
    let t = theme();
    let repo = app.repos.get(app.active_tab);
    let bases = app.current_bases();
    let mode = app.current_mode().label(&bases);
    let view = if app.outline.is_some() {
        if app.outline.as_ref().is_some_and(|o| o.flow) {
            "flow"
        } else {
            "outline"
        }
    } else if app.side_by_side {
        "side-by-side"
    } else {
        "unified"
    };

    let branch_name = bases.branch.as_deref().unwrap_or("HEAD");
    let file_count = repo.map(|r| r.files.len()).unwrap_or(0);
    let note_count = repo.map(|r| r.comments.len()).unwrap_or(0);
    let (total_add, total_del): (usize, usize) = repo
        .map(|r| {
            r.files
                .iter()
                .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions))
        })
        .unwrap_or((0, 0));

    // One accent pill for the comparison; everything else is quiet text.
    let mut spans: Vec<Span> = Vec::new();
    let mode_text = format!(" {} ", mode.to_uppercase());
    let mode_width = UnicodeWidthStr::width(mode_text.as_str()) as u16;
    spans.push(Span::styled(mode_text, t.pill()));
    hints.mode_badge_pos = (area.x, area.x + mode_width);

    spans.push(Span::styled(
        format!("  {branch_name}"),
        Style::default().fg(t.text),
    ));
    spans.push(Span::styled(
        format!(
            "  ·  {} file{}  ",
            file_count,
            if file_count != 1 { "s" } else { "" }
        ),
        t.muted_style(),
    ));
    spans.push(Span::styled(
        format!("+{total_add}"),
        Style::default().fg(t.add_fg),
    ));
    spans.push(Span::styled(
        format!(" -{total_del}"),
        Style::default().fg(t.del_fg),
    ));
    if note_count > 0 {
        spans.push(Span::styled(
            format!(
                "  ·  ✎ {} note{}",
                note_count,
                if note_count != 1 { "s" } else { "" }
            ),
            Style::default().fg(t.note),
        ));
    }
    spans.push(Span::styled("  ·  ", t.muted_style()));
    let col_before_view: u16 = area.x
        + spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()) as u16)
            .sum::<u16>();
    let view_width = UnicodeWidthStr::width(view) as u16;
    spans.push(Span::styled(
        view.to_string(),
        t.muted_style().add_modifier(Modifier::UNDERLINED),
    ));
    hints.view_badge_pos = (col_before_view, col_before_view + view_width);
    hints.status_bar_row = area.y;

    // Transient message, error, or warning takes precedence over the key hints.
    let message: Option<(String, Style)> = if let Some((ref msg, _)) = app.status_message {
        Some((msg.clone(), Style::default().fg(t.text)))
    } else if let Some(ref err) = app.last_error {
        Some((err.clone(), Style::default().fg(t.del_fg)))
    } else if !app.branch_base_resolved() {
        Some((
            "nothing to compare against — press B to pick a base or m for local".to_string(),
            t.warn_style(),
        ))
    } else {
        None
    };

    let used_width: usize = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    let remaining = (area.width as usize).saturating_sub(used_width);

    if let Some((msg, style)) = message {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(msg, style));
    } else {
        // A few key hints for the current view, dropped from the right when space is tight.
        let in_flow = app.outline.as_ref().is_some_and(|o| o.flow);
        let hints_full: &[(&str, &str)] = if app.timeline().is_some() && app.outline.is_none() {
            &[
                ("< >", "scrub"),
                ("s", "since/step"),
                ("y", "copy"),
                ("o", "outline"),
                ("l", "close timeline"),
            ]
        } else if in_flow {
            &[
                ("↵", "open"),
                ("o", "outline"),
                ("y", "copy"),
                ("t", "diff"),
                ("?", "help"),
            ]
        } else if app.outline.is_some() {
            &[
                ("↵", "open"),
                ("t", "flow"),
                ("y", "copy"),
                ("o", "diff"),
                ("?", "help"),
            ]
        } else {
            &[
                ("y", "copy"),
                ("n", "note"),
                ("o", "outline"),
                ("t", "flow"),
                ("l", "timeline"),
                ("?", "help"),
            ]
        };
        let mut hint_spans: Vec<Span> = Vec::new();
        let mut hint_width = 0usize;
        let budget = remaining.saturating_sub(3);
        for (key, label) in hints_full {
            let piece_width = UnicodeWidthStr::width(*key) + 1 + UnicodeWidthStr::width(*label) + 3;
            if hint_width + piece_width > budget {
                break;
            }
            hint_spans.push(Span::styled(key.to_string(), Style::default().fg(t.text)));
            hint_spans.push(Span::styled(format!(" {label}   "), t.muted_style()));
            hint_width += piece_width;
        }
        let padding = remaining.saturating_sub(hint_width);
        spans.push(Span::raw(" ".repeat(padding)));
        spans.extend(hint_spans);
    }

    let status = Paragraph::new(Line::from(spans)).style(Style::default().bg(t.surface));

    frame.render_widget(status, area);
}

/// Headline and hint for a loaded diff with no files in it.
fn empty_state_text(mode: &DiffMode, bases: &BaseCandidates) -> (String, &'static str) {
    match mode {
        DiffMode::Range { kind, .. } => match kind {
            RangeKind::Step => (
                "> this step changed nothing".to_string(),
                "Press > or < to move along the timeline, or l to leave it.",
            ),
            RangeKind::Since => (
                "> nothing has changed up to this point".to_string(),
                "Press > to move forward along the timeline, or l to leave it.",
            ),
        },
        DiffMode::Local => (
            "> I see no changes ... working tree clean".to_string(),
            "Watching for edits. Press b for the branch diff or B to pick what to compare.",
        ),
        DiffMode::Staged => (
            "> nothing staged".to_string(),
            "Press m for local changes or b for the branch diff.",
        ),
        DiffMode::Unstaged => (
            "> no unstaged changes".to_string(),
            "Press m for local changes or b for the branch diff.",
        ),
        DiffMode::Branch { base, commits_only } => match bases.resolve(base) {
            Some(name) if *commits_only => (
                format!("> no commits vs {name}"),
                "Press B and untick commits only to include uncommitted work, or m for local changes.",
            ),
            Some(name) => (
                format!("> no changes vs {name}"),
                "Press m for local changes or B to compare against something else.",
            ),
            None => match base {
                Base::Upstream => (
                    format!(
                        "> no upstream for {}",
                        bases.branch.as_deref().unwrap_or("this branch")
                    ),
                    "Push the branch first, or press B to pick another base.",
                ),
                Base::Ref(name) => (
                    format!("> {name} not found"),
                    "Press B to pick another base.",
                ),
                Base::Root => (
                    "> the repository is empty".to_string(),
                    "Nothing has been committed or written here yet.",
                ),
                Base::Parent | Base::Trunk => (
                    "> base branch not detected".to_string(),
                    "No main/master branch or gt parent found. Press B to type a ref, or m for local changes.",
                ),
            },
        },
    }
}

fn draw_compare_picker(frame: &mut Frame, app: &App) {
    let Some(picker) = app.compare_picker.as_ref() else {
        return;
    };
    let rows = app.compare_rows();
    let current = app.current_mode();
    let commits_only = app
        .repos
        .get(app.active_tab)
        .is_some_and(|r| r.commits_only);

    let area = frame.area();
    let width = 62u16.min(area.width.saturating_sub(4));
    let height = (rows.len() as u16 + 2).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height.saturating_sub(height) / 3;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let block = popup_block("Compare against", "↑↓ move · ↵ select · esc");
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut lines: Vec<Line> = Vec::new();
    for (idx, row) in rows.iter().enumerate().take(inner.height as usize) {
        let is_selected = idx == picker.selected;
        let is_current = match (row, current) {
            (CompareRow::Base { base, .. }, DiffMode::Branch { base: cur, .. }) => base == cur,
            (CompareRow::Staged, DiffMode::Staged) | (CompareRow::Unstaged, DiffMode::Unstaged) => {
                true
            }
            _ => false,
        };
        let marker = if is_current { "▸" } else { " " };
        let (name, detail): (String, String) = match row {
            CompareRow::Base { name, detail, .. } => (name.clone(), (*detail).to_string()),
            CompareRow::Staged => ("Staged".to_string(), "index only".to_string()),
            CompareRow::Unstaged => ("Unstaged".to_string(), "working tree vs index".to_string()),
            CompareRow::CommitsOnly => (
                format!("[{}] commits only", if commits_only { "x" } else { " " }),
                "exclude uncommitted work".to_string(),
            ),
            CompareRow::CustomRef => (
                format!("> {}_", picker.query),
                if picker.query.is_empty() {
                    "type a branch, tag or commit".to_string()
                } else {
                    "↵ compare against this ref".to_string()
                },
            ),
        };
        let name_width = 20usize;
        let padded = format!(
            " {marker} {name:<name_width$} {detail}",
            name_width = name_width
        );
        let style = if is_selected {
            theme().selection().fg(theme().text)
        } else {
            Style::default().fg(theme().text).bg(theme().popup_bg)
        };
        lines.push(Line::from(Span::styled(padded, style)));
    }
    let list = Paragraph::new(lines).style(Style::default().bg(theme().popup_bg));
    frame.render_widget(list, inner);
}

fn draw_file_picker(frame: &mut Frame, app: &App) {
    let picker = match app.file_picker.as_ref() {
        Some(p) => p,
        None => return,
    };
    let filtered = app.filtered_file_indices();
    let total = app.current_files().map(Vec::len).unwrap_or(0);

    let area = frame.area();
    let width = 70u16.min(area.width.saturating_sub(4));
    let max_items = 20u16;
    // border (2) + input line + at least one row for the list or its empty message
    let item_rows = (filtered.len().max(1) as u16).min(max_items);
    let height = (item_rows + 3).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height.saturating_sub(height) / 3; // Upper third
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let title = format!("Find file  {}/{}", filtered.len(), total);
    let block = popup_block(&title, "↑↓ move · ↵ open · esc");
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height < 2 {
        return;
    }

    // Input line with cursor
    let input_line =
        Paragraph::new(prompt_line(&picker.query)).style(Style::default().bg(theme().popup_bg));
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(input_line, input_area);

    // File list
    let list_area = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
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
            theme().selection().fg(theme().text)
        } else {
            Style::default().fg(theme().text).bg(theme().popup_bg)
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching files",
            Style::default().fg(theme().muted).bg(theme().popup_bg),
        )));
    }

    let list = Paragraph::new(lines);
    frame.render_widget(list, list_area);
}

fn draw_repo_adder(frame: &mut Frame, app: &App) {
    let adder = match app.repo_adder.as_ref() {
        Some(a) => a,
        None => return,
    };

    let area = frame.area();
    let width = 70u16.min(area.width.saturating_sub(4));
    let max_items = 15u16;
    let item_rows = (adder.results.len().max(1) as u16).min(max_items);
    let height = (item_rows + 3).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height.saturating_sub(height) / 3;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let block = popup_block("Add repo", "type a path · space check · ↵ add · esc");
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height < 2 {
        return;
    }

    // Input line
    let input_line =
        Paragraph::new(prompt_line(&adder.query)).style(Style::default().bg(theme().popup_bg));
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
            Style::default().fg(theme().del_fg),
        )))
        .style(Style::default().bg(theme().popup_bg));
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
            theme().selection().fg(theme().text)
        } else if is_checked {
            Style::default().fg(theme().add_fg).bg(theme().popup_bg)
        } else {
            Style::default().fg(theme().text).bg(theme().popup_bg)
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    if adder.results.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No git repos here — type a path like ../other-project or /abs/path/",
            Style::default().fg(theme().muted).bg(theme().popup_bg),
        )));
    }

    let list = Paragraph::new(lines);
    frame.render_widget(list, list_area);
}

fn draw_markdown_preview(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = area.width.saturating_sub(6).min(120);
    let height = area.height.saturating_sub(4);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    let preview = match app.markdown_preview.as_ref() {
        Some(preview) => preview,
        None => return,
    };

    frame.render_widget(Clear, popup_area);

    let block = popup_block(&preview.path, "j/k scroll · esc");
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let needs_render = app
        .markdown_render_cache
        .borrow()
        .as_ref()
        .is_none_or(|(width, _)| *width != inner.width);
    if needs_render {
        let mut skin = termimad::MadSkin::default_dark();
        skin.paragraph
            .set_bg(termimad::crossterm::style::Color::Reset);
        skin.bold.set_bg(termimad::crossterm::style::Color::Reset);
        skin.italic.set_bg(termimad::crossterm::style::Color::Reset);
        skin.strikeout
            .set_bg(termimad::crossterm::style::Color::Reset);
        let fmt_text = skin.text(&preview.content, Some(inner.width as usize));
        let rendered = format!("{fmt_text}");
        let text = ansi_to_tui::IntoText::into_text(&rendered)
            .unwrap_or_else(|_| ratatui::text::Text::raw(preview.content.clone()));
        *app.markdown_render_cache.borrow_mut() = Some((inner.width, text.lines));
    }

    let render_cache = app.markdown_render_cache.borrow();
    let rendered_lines = &render_cache.as_ref().expect("rendered above").1;
    let total_lines = rendered_lines.len();
    let scroll = preview
        .scroll
        .min(total_lines.saturating_sub(inner.height as usize));
    let visible_lines: Vec<Line> = rendered_lines
        .iter()
        .skip(scroll)
        .take(inner.height as usize)
        .cloned()
        .collect();

    let para = Paragraph::new(visible_lines).style(theme().popup_style());

    frame.render_widget(para, inner);

    // Scrollbar
    if total_lines > inner.height as usize {
        let mut scrollbar_state =
            ScrollbarState::new(total_lines.saturating_sub(inner.height as usize)).position(scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, popup_area, &mut scrollbar_state);
    }
}

fn help_section<'a>(title: &'a str, rows: &[(&'a str, &'a str)]) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from(Span::styled(
        title,
        Style::default()
            .fg(theme().accent)
            .add_modifier(Modifier::BOLD),
    ))];
    for (key, action) in rows {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {key:<width$}", width = HELP_KEY_WIDTH - 2),
                theme().accent_style(),
            ),
            Span::styled(*action, theme().text_style()),
        ]));
    }
    lines.push(Line::from(""));
    lines
}

/// One help column: titled sections of (key, action) rows.
type HelpColumn = &'static [(&'static str, &'static [(&'static str, &'static str)])];

/// Width of one help column when two fit side by side.
const HELP_COLUMN_WIDTH: u16 = 44;
/// Columns the key label occupies in each row, including the two-space indent.
const HELP_KEY_WIDTH: usize = 16;

const HELP_LEFT: HelpColumn = &[
    (
        "Navigation",
        &[
            ("j/k  ↑/↓", "Scroll one line"),
            ("Ctrl+D/U", "Scroll half a page"),
            ("PgDn/PgUp", "Scroll a page"),
            ("g/G", "Top / bottom"),
            ("]/[", "Next / previous hunk"),
            ("J/K", "Next / previous file"),
            ("f", "Find file"),
            ("Enter", "Collapse / expand file"),
            ("c/e", "Collapse / expand all"),
        ],
    ),
    (
        "Compare & views",
        &[
            ("m", "Local: uncommitted vs HEAD"),
            ("b", "Branch: all work vs base"),
            ("B", "Pick what to compare against"),
            ("v", "Unified ↔ side-by-side"),
            ("o", "Outline: files and symbols"),
            ("t", "Flow: routes into the change"),
            ("l", "Timeline: scrub the commits"),
            ("< >  s", "Timeline step / since mode"),
            ("p", "Preview focused .md file"),
        ],
    ),
    (
        "Repos",
        &[
            ("Tab/Shift+Tab", "Cycle tabs"),
            ("1-9", "Jump to tab"),
            ("a", "Add repo"),
            ("x", "Remove current tab"),
        ],
    ),
];

const HELP_RIGHT: HelpColumn = &[
    (
        "Review",
        &[
            ("y", "Copy focused hunk"),
            ("n", "Add / edit note on hunk"),
            ("N", "Remove note on hunk"),
            ("Y", "Copy all notes as markdown"),
            ("C", "Browse notes"),
            ("D", "Clear all notes"),
        ],
    ),
    (
        "Mouse",
        &[
            ("Click", "Select hunk / toggle file"),
            ("Double-click", "Copy hunk"),
            ("Right-click", "Add note to hunk"),
            ("Middle-click", "Copy focused hunk"),
            ("Click ↕ N", "Expand hidden lines"),
            ("Click badge", "Compare picker / cycle view"),
        ],
    ),
    (
        "General",
        &[
            ("?", "Toggle this help"),
            ("Esc", "Close popup"),
            ("q  Ctrl+C", "Quit"),
        ],
    ),
];

fn help_column_lines(column: HelpColumn) -> Vec<Line<'static>> {
    column
        .iter()
        .flat_map(|(title, rows)| help_section(title, rows))
        .collect()
}

fn draw_help_overlay(frame: &mut Frame) {
    let area = frame.area();

    let left = help_column_lines(HELP_LEFT);
    let mut right = help_column_lines(HELP_RIGHT);
    right.push(Line::from(Span::styled(
        "The ┃ gutter bar marks the hunk y / n act on.",
        Style::default().fg(theme().muted),
    )));

    let column_width = HELP_COLUMN_WIDTH;
    let two_columns = area.width >= column_width * 2 + 6;
    let (width, body): (u16, Vec<Line>) = if two_columns {
        let height = left.len().max(right.len());
        let mut merged = Vec::with_capacity(height);
        let blank = Line::from("");
        for i in 0..height {
            let l = left.get(i).unwrap_or(&blank);
            let r = right.get(i).unwrap_or(&blank);
            let l_width: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
            let mut spans = l.spans.clone();
            spans.push(Span::raw(
                " ".repeat((column_width as usize).saturating_sub(l_width)),
            ));
            spans.extend(r.spans.iter().cloned());
            merged.push(Line::from(spans));
        }
        (column_width * 2 + 4, merged)
    } else {
        // Narrow terminals may clip the bottom, so the review keys go first.
        let mut merged = right;
        merged.extend(left);
        (column_width + 4, merged)
    };

    let width = width.min(area.width.saturating_sub(2));
    let height = (body.len() as u16 + 2).min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let help = Paragraph::new(body)
        .block(popup_block("Help", "? or esc to close"))
        .style(theme().popup_style());

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
            format!("{}:{}", name, lineno)
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

    let preferred_y =
        if anchor_screen_y + box_height < app.layout.content_y + app.layout.content_height {
            anchor_screen_y
        } else {
            anchor_screen_y.saturating_sub(box_height + 1)
        };
    // On a very short terminal the content area may start below the last row the box fits
    // on; keep min <= max so `clamp` cannot panic.
    let max_y = area.height.saturating_sub(box_height);
    let min_y = app.layout.content_y.min(max_y);
    let y = preferred_y.clamp(min_y, max_y);

    let x = (area.width.saturating_sub(box_width)) / 2;
    let popup_area = Rect::new(x, y, box_width, box_height);

    frame.render_widget(Clear, popup_area);

    let editing = app.find_comment(input.file_idx, input.hunk_idx).is_some();
    let title = format!(
        "{} note · {}",
        if editing { "Edit" } else { "New" },
        file_label
    );
    let block = popup_block(&title, "↵ newline · ctrl-d save · esc cancel")
        .border_style(Style::default().fg(theme().note));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Render text with cursor
    let mut display_lines: Vec<Line> = Vec::new();
    let mut byte_count = 0;
    for (i, text_line) in text_lines.iter().enumerate() {
        let line_start = byte_count;
        let line_end = line_start + text_line.len();

        if input.cursor_pos >= line_start && input.cursor_pos <= line_end {
            let cursor_col = input.cursor_pos - line_start;
            let before = &text_line[..cursor_col.min(text_line.len())];
            let after = &text_line[cursor_col.min(text_line.len())..];
            let cursor_len = after.chars().next().map(char::len_utf8).unwrap_or_default();
            let cursor_char = if after.is_empty() {
                " "
            } else {
                &after[..cursor_len]
            };
            let after_cursor = &after[cursor_len..];
            display_lines.push(Line::from(vec![
                Span::styled(
                    format!(" {}", before),
                    Style::default().fg(theme().text).bg(theme().popup_bg),
                ),
                Span::styled(
                    cursor_char.to_string(),
                    Style::default().add_modifier(Modifier::REVERSED),
                ),
                Span::styled(
                    after_cursor.to_string(),
                    Style::default().fg(theme().text).bg(theme().popup_bg),
                ),
            ]));
        } else {
            display_lines.push(Line::from(Span::styled(
                format!(" {}", text_line),
                Style::default().fg(theme().text).bg(theme().popup_bg),
            )));
        }

        // +1 for the \n between lines
        byte_count = line_end + if i < text_lines.len() - 1 { 1 } else { 0 };
    }

    let para = Paragraph::new(display_lines).style(Style::default().bg(theme().popup_bg));
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

    let filtered_count = app.filtered_comment_indices().len();
    let title = if browser.query.is_empty() {
        format!("Notes  {}", comments.len())
    } else {
        format!("Notes  {}/{}", filtered_count, comments.len())
    };
    let block = popup_block(
        &title,
        "type to filter · ↵ jump · y copy · space check · d delete",
    )
    .border_style(Style::default().fg(theme().note));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height < 3 {
        return;
    }

    // Input line
    let input_line =
        Paragraph::new(prompt_line(&browser.query)).style(Style::default().bg(theme().popup_bg));
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(input_line, input_area);

    // Hint bar at bottom
    let hint_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    let hint = Paragraph::new(Line::from(Span::styled(
        " ↵ jump to hunk   y copy checked   space check/uncheck   d delete   Esc close",
        Style::default().fg(theme().muted),
    )))
    .style(Style::default().bg(theme().popup_bg));
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
            Style::default().fg(theme().muted).bg(theme().popup_bg),
        )));
    } else {
        let filtered = app.filtered_comment_indices();

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
                theme().selection().fg(theme().note)
            } else {
                Style::default().fg(theme().note).bg(theme().popup_bg)
            };

            lines.push(Line::from(Span::styled(
                format!(" {} {} @{}", check, file_name, lineno),
                header_style,
            )));

            // Show comment text
            let text_style = if is_selected {
                Style::default().fg(theme().text).bg(Color::Rgb(40, 40, 30))
            } else {
                Style::default().fg(theme().text).bg(theme().popup_bg)
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
    use super::{
        HELP_COLUMN_WIDTH, HELP_KEY_WIDTH, HELP_LEFT, HELP_RIGHT, LayoutHints, chunk_end, draw,
        ranges_for_chunk,
    };
    use crate::app::App;
    use crate::diff::{DiffLine, FileDiff, FileStatus, Hunk, LineKind, SideBySideLine};
    use crate::git::RepoInfo;
    use crate::highlight::Highlighter;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn wraps_on_utf8_display_width_boundaries() {
        let content = "aé界b";
        assert_eq!(chunk_end(content, 0, 2), 3);
        assert_eq!(chunk_end(content, 3, 2), 6);
    }

    #[test]
    fn wrapping_keeps_unicode_sequences_together() {
        assert_eq!(chunk_end("👩‍💻b", 0, 2), "👩‍💻".len());
        assert_eq!(chunk_end("e\u{301}b", 0, 1), "e\u{301}".len());
    }

    #[test]
    fn chunk_range_from_covers_one_chunk_only() {
        use super::chunk_range_from;
        assert_eq!(chunk_range_from("abcdefghij", 4, 0), 0..4);
        assert_eq!(chunk_range_from("abcdefghij", 4, 8), 8..10);
        assert_eq!(chunk_range_from("short", 10, 0), 0..5);
        assert_eq!(chunk_range_from("", 10, 0), 0..0);
    }

    #[test]
    fn inline_ranges_follow_wrapped_chunks() {
        assert_eq!(ranges_for_chunk(&[(2, 8)], 5, 10), vec![(0, 3)]);
    }

    #[test]
    fn bottom_of_viewport_renders_the_last_wrapped_chunk() {
        let mut app = App::new(vec![RepoInfo {
            name: "repo".to_string(),
            path: PathBuf::from("/tmp/repo"),
        }]);
        app.repos[0].files = vec![FileDiff {
            path: "src/lib.rs".to_string(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: vec![Hunk {
                header: "@@ -10,1 +12,1 @@".to_string(),
                lines: vec![DiffLine {
                    kind: LineKind::Context,
                    content: "abcdefghijkl".to_string(),
                    old_lineno: Some(10),
                    new_lineno: Some(12),
                }],
            }],
            additions: 0,
            deletions: 0,
            collapsed: false,
            total_new_lines: 12,
            sbs_cache: None,
        }];
        // A 19x7 terminal (tab strip, rule, four content rows, status) gives the diff an
        // inner area of 17x4; the 13-column gutter leaves four content columns, so the
        // line wraps into three chunks.
        app.layout.content_width = 17;
        app.layout.content_height = 4;
        app.prepare_active_layout();
        app.jump_active_viewport_bottom();

        let backend = TestBackend::new(19, 7);
        let mut terminal = Terminal::new(backend).unwrap();
        let highlighter = Highlighter::new();
        let mut hints = LayoutHints::default();
        terminal
            .draw(|frame| draw(frame, &app, &highlighter, &mut hints))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let screen: String = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect();
        assert!(
            screen.contains("ijkl"),
            "screen did not contain final chunk: {screen:?}"
        );
        assert!(
            !screen.contains("abcd"),
            "first chunk should be hidden behind the pinned file header"
        );
    }
    #[test]
    fn side_by_side_bottom_renders_chunk_using_the_right_pane_width() {
        let mut app = App::new(vec![RepoInfo {
            name: "repo".to_string(),
            path: PathBuf::from("/tmp/repo"),
        }]);
        let source_line = DiffLine {
            kind: LineKind::Context,
            content: "line".to_string(),
            old_lineno: Some(10),
            new_lineno: Some(12),
        };
        app.repos[0].files = vec![FileDiff {
            path: "src/lib.rs".to_string(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: vec![Hunk {
                header: "@@ -10,1 +12,1 @@".to_string(),
                lines: vec![source_line.clone()],
            }],
            additions: 0,
            deletions: 0,
            collapsed: false,
            total_new_lines: 12,
            sbs_cache: Some(vec![vec![SideBySideLine {
                left: Some(DiffLine {
                    content: "a".to_string(),
                    ..source_line.clone()
                }),
                right: Some(DiffLine {
                    content: "abcdefghijkl".to_string(),
                    ..source_line
                }),
                left_changed: None,
                right_changed: None,
            }]]),
        }];
        app.side_by_side = true;
        // A 28-column terminal leaves a 26-column inner area: 13- and 12-column panes
        // around the divider, and eight-column gutters leave the right pane four columns.
        app.layout.content_width = 26;
        app.layout.content_height = 4;
        app.prepare_active_layout();
        app.jump_active_viewport_bottom();

        let backend = TestBackend::new(28, 7);
        let mut terminal = Terminal::new(backend).unwrap();
        let highlighter = Highlighter::new();
        let mut hints = LayoutHints::default();
        terminal
            .draw(|frame| draw(frame, &app, &highlighter, &mut hints))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let screen: String = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect();
        assert!(
            screen.contains("ijkl"),
            "screen did not contain the right pane's final chunk: {screen:?}"
        );
    }

    #[test]
    fn help_rows_fit_their_column() {
        let key_budget = HELP_KEY_WIDTH - 2;
        let action_budget = HELP_COLUMN_WIDTH as usize - HELP_KEY_WIDTH;
        for (title, rows) in HELP_LEFT.iter().chain(HELP_RIGHT) {
            for (key, action) in rows.iter() {
                assert!(
                    UnicodeWidthStr::width(*key) <= key_budget,
                    "{title}: key {key:?} overflows its {key_budget} columns"
                );
                assert!(
                    UnicodeWidthStr::width(*action) <= action_budget,
                    "{title}: {action:?} is wider than {action_budget} columns and breaks the second column"
                );
            }
        }
    }
}
