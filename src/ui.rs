use crate::app::App;
use crate::diff::{FileStatus, LineKind};
use crate::highlight::Highlighter;
use crate::outline::{self, CallDirection, OutlineRow, SymbolChange, hunk_context};
use crate::viewport::{RowRef, chunk_end, side_by_side_gutter_width, side_by_side_pane_widths};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use unicode_width::UnicodeWidthStr;

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
const FG_FOCUS: Color = Color::Rgb(100, 180, 255);
const BG_TAB_ACTIVE: Color = Color::Rgb(50, 60, 85);
const BG_STATUS: Color = Color::Rgb(40, 40, 50);
const BG_POPUP: Color = Color::Rgb(30, 30, 40);
const BG_NOTE: Color = Color::Rgb(30, 30, 20);

const TAB_BAR_HEIGHT: u16 = 1;

/// Layout positions computed during rendering, needed for mouse hit-testing.
/// Kept separate from App so `draw()` doesn't require `&mut App`.
#[derive(Default)]
pub struct LayoutHints {
    pub tab_bar_row: u16,
    pub tab_positions: Vec<(u16, u16)>,
    pub mode_badge_pos: (u16, u16),
    pub view_badge_pos: (u16, u16),
    pub status_bar_row: u16,
    pub content_y: u16,
    pub content_height: u16,
    pub content_width: u16,
}

fn screen_chunks(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(TAB_BAR_HEIGHT), // tab bar
            Constraint::Min(1),                 // diff area
            Constraint::Length(1),              // status bar
        ])
        .split(area)
}

pub fn diff_inner_area(area: Rect) -> Rect {
    let chunks = screen_chunks(area);
    Block::default().borders(Borders::ALL).inner(chunks[1])
}

pub fn draw(frame: &mut Frame, app: &App, highlighter: &Highlighter, hints: &mut LayoutHints) {
    let chunks = screen_chunks(frame.area());

    // Compute content area top for mouse hit-testing
    let diff_inner = diff_inner_area(frame.area());
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

fn tab_label(index: usize, repo: &crate::app::RepoState) -> String {
    // " 1 name +12 -3 ✎2 "
    let mut label = format!(" {} {} ", index + 1, repo.info.name);
    if !repo.files.is_empty() {
        let (adds, dels) = repo
            .files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
        label.push_str(&format!("+{adds} -{dels} "));
    }
    if !repo.comments.is_empty() {
        label.push_str(&format!("✎{} ", repo.comments.len()));
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
    // Each tab is followed by one column of spacing.
    let widths: Vec<usize> = labels
        .iter()
        .map(|label| UnicodeWidthStr::width(label.as_str()) + 1)
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
        spans.push(Span::styled(marker, Style::default().fg(FG_MUTED)));
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
        let style = if is_active {
            Style::default()
                .fg(Color::Yellow)
                .bg(BG_TAB_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else if repo.files.is_empty() {
            Style::default().fg(FG_MUTED)
        } else {
            Style::default().fg(Color::White)
        };

        // Clip a tab that starts before the scroll offset (only happens on the left edge).
        let skip = scroll.saturating_sub(tab_start);
        let shown: String = label.chars().skip(skip).collect();
        let shown_width = UnicodeWidthStr::width(shown.as_str());
        let start_col = col as u16;
        spans.push(Span::styled(shown, style));
        spans.push(Span::raw(" "));
        col += shown_width + 1;
        hints.tab_positions.push((start_col, col as u16));
    }
    if overflow {
        let marker_col = area.x + area.width - 1;
        let marker = if hidden_right { "›" } else { " " };
        let strip = Rect::new(area.x, area.y, area.width.saturating_sub(1), 1);
        frame.render_widget(Paragraph::new(Line::from(spans)), strip);
        frame.render_widget(
            Paragraph::new(Span::styled(marker, Style::default().fg(FG_MUTED))),
            Rect::new(marker_col, area.y, 1, 1),
        );
    } else {
        if let Some(last) = hints.tab_positions.last_mut() {
            last.1 = area.x + area.width;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

fn draw_empty_state(frame: &mut Frame, app: &App, area: Rect) {
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

    let repo = app.repos.get(app.active_tab);
    let base = repo.and_then(|r| r.base_branch.as_deref());
    let loaded = repo.is_none_or(|r| r.loaded);
    let (headline, hint) = match (app.current_mode(), base) {
        _ if !loaded => (
            "> computing diff ...".to_string(),
            "Reading the repository. Large repos can take a few seconds.",
        ),
        (crate::git::DiffMode::Unstaged, _) => (
            "> I see no changes ... working tree clean".to_string(),
            "Watching for edits. Press s for staged changes or b for the branch diff.",
        ),
        (crate::git::DiffMode::Staged, _) => (
            "> nothing staged".to_string(),
            "Press m to see unstaged changes or b for the branch diff.",
        ),
        (crate::git::DiffMode::Branch, Some(base)) => (
            format!("> no changes vs {base}"),
            "Press m to see unstaged changes or s for staged changes.",
        ),
        (crate::git::DiffMode::Branch, None) => (
            "> base branch not detected".to_string(),
            "No main/master branch or gt parent found. Press m for unstaged changes.",
        ),
    };
    let watching = repo
        .map(|r| format!("watching {}", r.info.path.display()))
        .unwrap_or_default();

    // Logo, blank, prompt, headline, blank, hint, watching
    let content_height = if inner.height >= 14 { 12u16 } else { 5 };
    let top_pad = inner.height.saturating_sub(content_height) / 2;

    let mut lines: Vec<Line> = Vec::new();
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }
    if inner.height >= 14 {
        for logo_line in &logo_lines {
            lines.push(Line::from(Span::styled(
                *logo_line,
                Style::default().fg(FG_FOCUS).add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "> git status",
        Style::default().fg(FG_MUTED),
    )));
    lines.push(Line::from(Span::styled(
        headline,
        Style::default().fg(FG_MUTED).add_modifier(Modifier::ITALIC),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::White),
    )));
    lines.push(Line::from(Span::styled(
        watching,
        Style::default().fg(FG_MUTED),
    )));

    let para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(para, inner);
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
    let title = if indexing {
        " Outline — ↵ open · y copy as markdown · o full diff · indexing calls… "
    } else {
        " Outline — ↵ open · →/← callers & callees · y copy as markdown · o full diff "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(Style::default().fg(FG_MUTED));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    let scroll = state.scroll.min(state.rows.len().saturating_sub(height));
    let width = inner.width as usize;

    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for (index, row) in state.rows.iter().enumerate().skip(scroll).take(height) {
        let selected = index == state.selected;
        let row_bg = if selected { Some(BG_TAB_ACTIVE) } else { None };
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
                    with_bg(Style::default().fg(FG_MUTED)),
                ));
                spans.push(Span::styled(
                    format!("{name}/"),
                    with_bg(
                        Style::default()
                            .fg(FG_PATH_DIR)
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
                    with_bg(Style::default().fg(FG_MUTED)),
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
                    with_bg(Style::default().fg(FG_PATH_FILE).add_modifier(if selected {
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
                    with_bg(Style::default().fg(FG_MUTED)),
                ));
                let (glyph_color, name_color) = match symbol.change {
                    SymbolChange::Added => (FG_ADD, Color::White),
                    SymbolChange::Removed => (FG_DEL, FG_MUTED),
                    SymbolChange::Modified => (FG_STATUS_M, Color::White),
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
                    with_bg(Style::default().fg(FG_MUTED)),
                ));
                spans.push(Span::styled(
                    format!("… {count} more"),
                    with_bg(Style::default().fg(FG_MUTED).add_modifier(Modifier::ITALIC)),
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
                    with_bg(Style::default().fg(FG_MUTED)),
                ));
                let caret = if *expanded { "▾ " } else { "▸ " };
                spans.push(Span::styled(caret, with_bg(Style::default().fg(FG_MUTED))));
                match warning {
                    Some(warning) => {
                        spans.push(Span::styled(
                            format!("⚠ {warning}"),
                            with_bg(
                                Style::default()
                                    .fg(FG_STATUS_M)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ));
                        spans.push(Span::styled(
                            format!("  · calls {callees}"),
                            with_bg(Style::default().fg(FG_MUTED)),
                        ));
                    }
                    None => {
                        spans.push(Span::styled(
                            format!("called by {callers} · calls {callees}"),
                            with_bg(Style::default().fg(FG_MUTED)),
                        ));
                    }
                }
                None
            }
            OutlineRow::Call {
                prefix,
                direction,
                name,
                location,
                mark,
                file_idx,
                ..
            } => {
                spans.push(Span::styled(
                    prefix.clone(),
                    with_bg(Style::default().fg(FG_MUTED)),
                ));
                let arrow = match direction {
                    CallDirection::Incoming => "← ",
                    CallDirection::Outgoing => "→ ",
                };
                spans.push(Span::styled(arrow, with_bg(Style::default().fg(FG_HUNK))));
                if let Some(mark) = mark {
                    let color = match mark {
                        SymbolChange::Added => FG_ADD,
                        SymbolChange::Removed => FG_DEL,
                        SymbolChange::Modified => FG_STATUS_M,
                    };
                    spans.push(Span::styled(
                        format!("{} ", outline::change_glyph(*mark)),
                        with_bg(Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    ));
                }
                let name_color = if file_idx.is_some() {
                    Color::White
                } else {
                    FG_PATH_DIR
                };
                spans.push(Span::styled(
                    name.clone(),
                    with_bg(Style::default().fg(name_color)),
                ));
                spans.push(Span::styled(
                    format!("  {location}"),
                    with_bg(Style::default().fg(FG_MUTED)),
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
            spans.push(Span::styled(adds, with_bg(Style::default().fg(FG_ADD))));
            spans.push(Span::styled(" ", with_bg(Style::default())));
            spans.push(Span::styled(dels, with_bg(Style::default().fg(FG_DEL))));
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
        lines.push(Line::from(Span::styled(
            "  No changes to outline",
            Style::default().fg(FG_MUTED),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);

    if state.rows.len() > height {
        let mut scrollbar_state =
            ScrollbarState::new(state.rows.len().saturating_sub(height)).position(scroll);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut scrollbar_state,
        );
    }
}

fn status_color(status: FileStatus) -> Color {
    match status {
        FileStatus::Modified => FG_STATUS_M,
        FileStatus::Added => FG_STATUS_A,
        FileStatus::Deleted => FG_STATUS_D,
        FileStatus::Renamed => FG_STATUS_R,
        FileStatus::Untracked => FG_MUTED,
    }
}

/// The gutter bar between line numbers and code. Heavier and brighter for the hunk
/// that `y` / `n` will act on, so the keyboard target is always visible.
fn gutter_separator<'a>(focused: bool) -> Span<'a> {
    if focused {
        Span::styled(
            " ┃",
            Style::default().fg(FG_FOCUS).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" │", Style::default().fg(FG_MUTED))
    }
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
            Style::default().fg(FG_EXPAND),
        ));
        spans.push(gutter_separator(focused));
        if let Some(ctx) = hunk.and_then(|hunk| hunk_context(&hunk.header)) {
            spans.push(Span::styled(
                format!(" {}", ctx),
                Style::default().fg(FG_HUNK),
            ));
        }
    } else if hunk_idx > 0 || focused {
        spans.push(Span::raw(" ".repeat(numbers_width)));
        spans.push(gutter_separator(focused));
    }
    if has_comment {
        spans.push(Span::styled(" [!]", Style::default().fg(FG_COMMENT)));
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
    let inner_area = Block::default().borders(Borders::ALL).inner(area);
    let block = Block::default().borders(Borders::ALL);
    frame.render_widget(block, area);

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
                        Span::styled(gutter, Style::default().fg(FG_COMMENT)),
                        Span::styled(format!(" {}", text), Style::default().fg(FG_COMMENT)),
                    ]));
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
                ));
            }
            RowRef::GapTail { gap_after, .. } if gap_after > 0 => {
                let Some(file_idx) = layout.row_file_idx(row) else {
                    continue;
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format_expand_indicator(gap_after, layout.lineno_width(file_idx) * 2 + 1),
                        Style::default().fg(FG_EXPAND),
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
    let divider = Line::from(Span::styled("│", Style::default().fg(FG_MUTED)));
    let header_divider = Line::from(Span::styled(" ", Style::default().bg(BG_HEADER)));
    let header_fill = |width: u16| {
        Line::from(Span::styled(
            " ".repeat(width as usize),
            Style::default().bg(BG_HEADER),
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
                        Style::default().fg(FG_EXPAND),
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
                        Span::styled(" ".repeat(lno_w) + " ┃", Style::default().fg(FG_COMMENT)),
                        Span::styled(format!(" {}", text), Style::default().fg(FG_COMMENT)),
                    ]);
                    left_lines.push(comment_line);
                    right_lines.push(Line::from(""));
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

    if total_lines > visible_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines.saturating_sub(visible_height))
            .position(app.current_scroll_offset());
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
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
        Some(BG_FLASH)
    } else if plain {
        None
    } else {
        match line.kind {
            LineKind::Addition => Some(BG_ADD),
            LineKind::Deletion => Some(BG_DEL),
            _ => None,
        }
    };

    let prefix_style = match line.kind {
        _ if plain => Style::default().fg(FG_MUTED),
        LineKind::Addition => Style::default().fg(FG_ADD).bg(bg.unwrap_or_default()),
        LineKind::Deletion => Style::default().fg(FG_DEL).bg(bg.unwrap_or_default()),
        _ => Style::default().fg(FG_MUTED),
    };

    // Gutter width: line numbers + separator + prefix
    let gutter_width = lno_width * 2 + 1 + 2 + prefix.len(); // "NNNN NNNN │+ "
    let available = content_width.saturating_sub(gutter_width);
    let range = chunk_range_from(&line.content, available, chunk_start);

    let mut spans = if chunk_idx == 0 {
        vec![
            Span::styled(
                format_lineno(line, lno_width),
                Style::default().fg(FG_MUTED),
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
    let mut highlighted = highlighter.highlight_line_content(&line.content[range], file_path, bg);
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
                Span::styled(" ~", Style::default().fg(FG_MUTED)),
            ])
        });
    };

    let bg = match line.kind {
        _ if plain => None,
        LineKind::Addition => Some(BG_ADD),
        LineKind::Deletion => Some(BG_DEL),
        _ => None,
    };

    let prefix = match line.kind {
        _ if plain => "  ",
        LineKind::Addition => "+ ",
        LineKind::Deletion => "- ",
        _ => "  ",
    };

    let prefix_style = match line.kind {
        _ if plain => Style::default().fg(FG_MUTED),
        LineKind::Addition => Style::default().fg(FG_ADD),
        LineKind::Deletion => Style::default().fg(FG_DEL),
        _ => Style::default().fg(FG_MUTED),
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
            Span::styled(lineno, Style::default().fg(FG_MUTED)),
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
        LineKind::Addition => Some(BG_ADD_EMPH),
        LineKind::Deletion => Some(BG_DEL_EMPH),
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
        UnicodeWidthStr::width(old_path_str)
            + UnicodeWidthStr::width(arrow)
            + UnicodeWidthStr::width(dir)
            + UnicodeWidthStr::width(filename)
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
                .fg(FG_MUTED)
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
    let repo = app.repos.get(app.active_tab);
    let base = repo.and_then(|r| r.base_branch.as_deref());
    let mode = app.current_mode().label(base);
    let view = if app.outline.is_some() {
        "outline"
    } else if app.side_by_side {
        "side-by-side"
    } else {
        "unified"
    };

    let branch_name = repo
        .and_then(|r| r.branch_name.as_deref())
        .unwrap_or("HEAD");
    let file_count = repo.map(|r| r.files.len()).unwrap_or(0);
    let note_count = repo.map(|r| r.comments.len()).unwrap_or(0);
    let (total_add, total_del): (usize, usize) = repo
        .map(|r| {
            r.files
                .iter()
                .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions))
        })
        .unwrap_or((0, 0));

    let mut spans: Vec<Span> = vec![Span::styled(
        format!(
            "  {} │ {} file{}",
            branch_name,
            file_count,
            if file_count != 1 { "s" } else { "" },
        ),
        Style::default().fg(Color::White),
    )];
    spans.push(Span::styled(
        format!(" +{}", total_add),
        Style::default().fg(FG_ADD),
    ));
    spans.push(Span::styled(
        format!(" -{}", total_del),
        Style::default().fg(FG_DEL),
    ));
    if note_count > 0 {
        spans.push(Span::styled(
            format!(
                " │ ✎ {} note{}",
                note_count,
                if note_count != 1 { "s" } else { "" }
            ),
            Style::default().fg(FG_COMMENT),
        ));
    }

    // Mode and view as highlighted badges — track positions for click handling
    spans.push(Span::raw("  "));
    let col_before_mode: u16 = area.x
        + spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()) as u16)
            .sum::<u16>();
    let mode_text = format!(" {} ", mode);
    let mode_width = UnicodeWidthStr::width(mode_text.as_str()) as u16;
    spans.push(Span::styled(
        mode_text,
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    hints.mode_badge_pos = (col_before_mode, col_before_mode + mode_width);

    spans.push(Span::raw(" "));
    let col_before_view: u16 = area.x
        + spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()) as u16)
            .sum::<u16>();
    let view_text = format!(" {} ", view);
    let view_width = UnicodeWidthStr::width(view_text.as_str()) as u16;
    spans.push(Span::styled(
        view_text,
        Style::default()
            .fg(Color::Black)
            .bg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    ));
    hints.view_badge_pos = (col_before_view, col_before_view + view_width);
    hints.status_bar_row = area.y;

    // Transient message, error, or warning takes precedence over the key hints.
    let message: Option<(String, Style)> = if let Some((ref msg, _)) = app.status_message {
        Some((msg.clone(), Style::default().fg(FG_COMMENT)))
    } else if let Some(ref err) = app.last_error {
        Some((err.clone(), Style::default().fg(Color::Red)))
    } else if app.current_mode() == crate::git::DiffMode::Branch && base.is_none() {
        Some((
            "base branch not detected — press m for unstaged".to_string(),
            Style::default().fg(Color::Yellow),
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
        spans.push(Span::raw("  "));
        spans.push(Span::styled(msg, style));
    } else {
        // Key hints, from the most useful down, dropped from the right when space is tight.
        let hints_full: [(&str, &str); 8] = if app.outline.is_some() {
            [
                ("↵", "open"),
                ("j/k", "move"),
                ("y", "copy outline"),
                ("o", "full diff"),
                ("m/s/b", "mode"),
                ("f", "find"),
                ("?", "help"),
                ("q", "quit"),
            ]
        } else {
            [
                ("y", "copy hunk"),
                ("n", "note"),
                ("Y", "copy notes"),
                ("]/[", "hunk"),
                ("o", "outline"),
                ("J/K", "file"),
                ("?", "help"),
                ("q", "quit"),
            ]
        };
        let mut hint_spans: Vec<Span> = Vec::new();
        let mut hint_width = 0usize;
        let budget = remaining.saturating_sub(3);
        for (key, label) in hints_full {
            let piece_width = UnicodeWidthStr::width(key) + 1 + UnicodeWidthStr::width(label) + 2;
            if hint_width + piece_width > budget {
                break;
            }
            hint_spans.push(Span::styled(
                key.to_string(),
                Style::default().fg(Color::White),
            ));
            hint_spans.push(Span::styled(
                format!(" {label}  "),
                Style::default().fg(FG_MUTED),
            ));
            hint_width += piece_width;
        }
        let padding = remaining.saturating_sub(hint_width);
        spans.push(Span::raw(" ".repeat(padding)));
        spans.extend(hint_spans);
    }

    let status = Paragraph::new(Line::from(spans)).style(Style::default().bg(BG_STATUS));

    frame.render_widget(status, area);
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

    let title = format!(
        " Find file  {}/{}  — ↑↓ move · ↵ jump · Esc close ",
        filtered.len(),
        total
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().bg(BG_POPUP));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height < 2 {
        return;
    }

    // Input line with cursor
    let input_text = format!(" > {}_", picker.query);
    let input_line = Paragraph::new(Line::from(Span::styled(
        input_text,
        Style::default().fg(Color::Yellow),
    )))
    .style(Style::default().bg(BG_POPUP));
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
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(100, 180, 255))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White).bg(BG_POPUP)
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching files",
            Style::default().fg(FG_MUTED).bg(BG_POPUP),
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

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Add repo — type a path · space check · ↵ add · Esc close ")
        .style(Style::default().bg(BG_POPUP));
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    if inner.height < 2 {
        return;
    }

    // Input line
    let input_text = format!(" > {}_", adder.query);
    let input_line = Paragraph::new(Line::from(Span::styled(
        input_text,
        Style::default().fg(Color::Yellow),
    )))
    .style(Style::default().bg(BG_POPUP));
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
        .style(Style::default().bg(BG_POPUP));
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
            Style::default().fg(Color::Green).bg(BG_POPUP)
        } else {
            Style::default().fg(Color::White).bg(BG_POPUP)
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    if adder.results.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No git repos here — type a path like ../other-project or /abs/path/",
            Style::default().fg(FG_MUTED).bg(BG_POPUP),
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

    let title = format!(" {} — j/k scroll, q/esc/p close ", preview.path);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().bg(Color::Rgb(25, 25, 35)));
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

    let para = Paragraph::new(visible_lines).style(Style::default().bg(Color::Rgb(25, 25, 35)));

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
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))];
    for (key, action) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("  {key:<14}"), Style::default().fg(Color::Yellow)),
            Span::styled(*action, Style::default().fg(Color::White)),
        ]));
    }
    lines.push(Line::from(""));
    lines
}

fn draw_help_overlay(frame: &mut Frame) {
    let area = frame.area();

    let mut left: Vec<Line> = Vec::new();
    left.extend(help_section(
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
    ));
    left.extend(help_section(
        "Modes & views",
        &[
            ("m/s/b", "Modified / staged / branch"),
            ("v", "Unified ↔ side-by-side"),
            ("o", "Outline: files, symbols, callers"),
            ("p", "Preview focused .md file"),
        ],
    ));
    left.extend(help_section(
        "Repos",
        &[
            ("Tab/Shift+Tab", "Cycle tabs"),
            ("1-9", "Jump to tab"),
            ("a", "Add repo"),
            ("x", "Remove current tab"),
        ],
    ));

    let mut right: Vec<Line> = Vec::new();
    right.extend(help_section(
        "Review",
        &[
            ("y", "Copy focused hunk"),
            ("n", "Add / edit note on hunk"),
            ("N", "Remove note on hunk"),
            ("Y", "Copy all notes as markdown"),
            ("C", "Browse notes"),
            ("D", "Clear all notes"),
        ],
    ));
    right.extend(help_section(
        "Mouse",
        &[
            ("Click", "Select hunk / toggle file"),
            ("Double-click", "Copy hunk"),
            ("Right-click", "Add note to hunk"),
            ("Middle-click", "Copy focused hunk"),
            ("Click ↕ N", "Expand hidden lines"),
            ("Click badge", "Cycle mode / view"),
        ],
    ));
    right.extend(help_section(
        "General",
        &[
            ("?", "Toggle this help"),
            ("Esc", "Close popup"),
            ("q  Ctrl+C", "Quit"),
        ],
    ));
    right.push(Line::from(Span::styled(
        "The ┃ gutter bar marks the hunk y / n act on.",
        Style::default().fg(FG_MUTED),
    )));

    let column_width = 44u16;
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
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Help — ? or Esc to close ")
                .style(Style::default().bg(BG_POPUP)),
        )
        .style(Style::default().fg(Color::White).bg(BG_POPUP));

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
        " {} note · {} — ↵ newline · Ctrl+D save · Esc cancel ",
        if editing { "Edit" } else { "New" },
        file_label
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().bg(BG_NOTE).fg(FG_COMMENT));
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
                    Style::default().fg(Color::White).bg(BG_NOTE),
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
                    Style::default().fg(Color::White).bg(BG_NOTE),
                ),
            ]));
        } else {
            display_lines.push(Line::from(Span::styled(
                format!(" {}", text_line),
                Style::default().fg(Color::White).bg(BG_NOTE),
            )));
        }

        // +1 for the \n between lines
        byte_count = line_end + if i < text_lines.len() - 1 { 1 } else { 0 };
    }

    let para = Paragraph::new(display_lines).style(Style::default().bg(BG_NOTE));
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
        format!(" Notes ({}) — type to filter ", comments.len())
    } else {
        format!(
            " Notes {}/{} — type to filter ",
            filtered_count,
            comments.len()
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().bg(BG_NOTE).fg(FG_COMMENT));
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
    .style(Style::default().bg(BG_NOTE));
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(input_line, input_area);

    // Hint bar at bottom
    let hint_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    let hint = Paragraph::new(Line::from(Span::styled(
        " ↵ jump to hunk   y copy checked   space check/uncheck   d delete   Esc close",
        Style::default().fg(FG_MUTED),
    )))
    .style(Style::default().bg(BG_NOTE));
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
            Style::default().fg(FG_MUTED).bg(BG_NOTE),
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
                Style::default()
                    .fg(Color::Black)
                    .bg(FG_COMMENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(FG_COMMENT).bg(BG_NOTE)
            };

            lines.push(Line::from(Span::styled(
                format!(" {} {} @{}", check, file_name, lineno),
                header_style,
            )));

            // Show comment text
            let text_style = if is_selected {
                Style::default().fg(Color::White).bg(Color::Rgb(40, 40, 30))
            } else {
                Style::default().fg(Color::White).bg(BG_NOTE)
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
    use super::{LayoutHints, chunk_end, draw, ranges_for_chunk};
    use crate::app::App;
    use crate::diff::{DiffLine, FileDiff, FileStatus, Hunk, LineKind, SideBySideLine};
    use crate::git::RepoInfo;
    use crate::highlight::Highlighter;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

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
        // A 19x8 terminal gives the diff an inner area of 17x4; the 13-column gutter
        // leaves four content columns, so the line wraps into three chunks.
        app.layout.content_width = 17;
        app.layout.content_height = 4;
        app.prepare_active_layout();
        app.jump_active_viewport_bottom();

        let backend = TestBackend::new(19, 8);
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

        let backend = TestBackend::new(28, 8);
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
}
