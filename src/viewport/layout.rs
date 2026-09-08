use super::row::{RowRef, ViewKind};
use crate::app::HunkComment;
use crate::diff::{FileDiff, gap_between_hunks};
use crate::outline;
use crate::symbols::SymbolIndex;
use std::collections::HashMap;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone)]
pub struct DiffLayout {
    content_width: usize,
    rows: Vec<RowRef>,
    file_header_rows: Vec<usize>,
    /// Wrapped comment text lines, keyed by (file_idx, hunk_idx).
    comment_lines: HashMap<(usize, usize), Vec<String>>,
    /// Inline caller / callee lines per hunk, keyed by (file_idx, hunk_idx).
    call_context: HashMap<(usize, usize), Vec<String>>,
    /// Every hunk in display order with the rows it occupies (header through last line).
    hunk_rows: Vec<HunkRows>,
    /// Gutter digit count per file, computed once here instead of per rendered row.
    lineno_widths: Vec<usize>,
    /// Byte offset where each row's wrapped chunk starts, per side. Unified rows use
    /// `left`. `NO_CHUNK` marks a side with no content on that row.
    chunk_starts: Vec<ChunkStarts>,
}

const NO_CHUNK: u32 = u32::MAX;

#[derive(Debug, Clone, Copy)]
struct ChunkStarts {
    left: u32,
    right: u32,
}

const NO_CHUNKS: ChunkStarts = ChunkStarts {
    left: NO_CHUNK,
    right: NO_CHUNK,
};

#[derive(Debug, Clone)]
struct HunkRows {
    file_idx: usize,
    hunk_idx: usize,
    rows: Range<usize>,
}

impl DiffLayout {
    pub fn build(
        files: &[FileDiff],
        view_kind: ViewKind,
        comments: &[HunkComment],
        content_width: usize,
    ) -> Self {
        Self::build_with_index(files, view_kind, comments, content_width, None)
    }

    /// Like `build`, adding a call-context line under each hunk that changes a function
    /// the index knows about.
    pub fn build_with_index(
        files: &[FileDiff],
        view_kind: ViewKind,
        comments: &[HunkComment],
        content_width: usize,
        index: Option<&SymbolIndex>,
    ) -> Self {
        let mut rows = Vec::new();
        let mut chunk_starts: Vec<ChunkStarts> = Vec::new();
        let mut file_header_rows = Vec::with_capacity(files.len());
        let mut hunk_rows = Vec::new();
        let lineno_widths: Vec<usize> = files.iter().map(line_number_width).collect();

        // Pre-compute wrapped comment lines keyed by (file_idx, hunk_idx)
        let comment_width = content_width.saturating_sub(4).max(20);
        let mut comment_lines: HashMap<(usize, usize), Vec<String>> = HashMap::new();
        for c in comments {
            comment_lines.insert(
                (c.file_idx, c.hunk_idx),
                wrap_comment(&c.text, comment_width),
            );
        }

        let mut call_context: HashMap<(usize, usize), Vec<String>> = HashMap::new();

        for (file_idx, file) in files.iter().enumerate() {
            let lno_w = lineno_widths[file_idx];
            if let Some(index) = index
                && !file.collapsed
            {
                let text_width = content_width.saturating_sub(lno_w * 2 + 5);
                for (hunk_idx, lines) in hunk_call_context(index, file, text_width) {
                    call_context.insert((file_idx, hunk_idx), lines);
                }
            }
            file_header_rows.push(rows.len());
            rows.push(RowRef::FileHeader { file_idx });
            chunk_starts.push(NO_CHUNKS);

            if file.collapsed {
                rows.push(RowRef::Blank { file_idx });
                chunk_starts.push(NO_CHUNKS);
                continue;
            }

            for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
                let hunk_start_row = rows.len();
                let gap_before = if hunk_idx > 0 {
                    gap_between_hunks(&file.hunks[hunk_idx - 1], hunk)
                } else {
                    hunk.first_new_lineno().unwrap_or(1) as usize - 1
                };
                rows.push(RowRef::HunkHeader {
                    file_idx,
                    hunk_idx,
                    gap_before,
                });
                chunk_starts.push(NO_CHUNKS);

                if let Some(lines) = call_context.get(&(file_idx, hunk_idx)) {
                    for line_idx in 0..lines.len() {
                        rows.push(RowRef::CallContext {
                            file_idx,
                            hunk_idx,
                            line_idx,
                        });
                        chunk_starts.push(NO_CHUNKS);
                    }
                }

                // Emit comment rows after hunk header
                if let Some(lines) = comment_lines.get(&(file_idx, hunk_idx)) {
                    for wrap_idx in 0..lines.len() {
                        rows.push(RowRef::Comment {
                            file_idx,
                            hunk_idx,
                            wrap_idx,
                        });
                        chunk_starts.push(NO_CHUNKS);
                    }
                }

                match view_kind {
                    ViewKind::Unified => {
                        let available = unified_content_width_for(lno_w, content_width);
                        let mut starts = Vec::new();
                        for (line_idx, line) in hunk.lines.iter().enumerate() {
                            chunk_start_offsets(&line.content, available, &mut starts);
                            for (chunk_idx, &start) in starts.iter().enumerate() {
                                rows.push(RowRef::UnifiedLine {
                                    file_idx,
                                    hunk_idx,
                                    line_idx,
                                    chunk_idx,
                                });
                                chunk_starts.push(ChunkStarts {
                                    left: start as u32,
                                    right: NO_CHUNK,
                                });
                            }
                        }
                    }
                    ViewKind::SideBySide => {
                        let (left_available, right_available) =
                            side_by_side_content_widths_for(lno_w, content_width);
                        if let Some(lines) = file
                            .sbs_cache
                            .as_ref()
                            .and_then(|cache| cache.get(hunk_idx))
                        {
                            let mut left_starts = Vec::new();
                            let mut right_starts = Vec::new();
                            for (line_idx, line) in lines.iter().enumerate() {
                                left_starts.clear();
                                right_starts.clear();
                                if let Some(left) = &line.left {
                                    chunk_start_offsets(
                                        &left.content,
                                        left_available,
                                        &mut left_starts,
                                    );
                                }
                                if let Some(right) = &line.right {
                                    chunk_start_offsets(
                                        &right.content,
                                        right_available,
                                        &mut right_starts,
                                    );
                                }
                                let count = left_starts.len().max(right_starts.len()).max(1);
                                for chunk_idx in 0..count {
                                    rows.push(RowRef::SideBySideLine {
                                        file_idx,
                                        hunk_idx,
                                        line_idx,
                                        chunk_idx,
                                    });
                                    chunk_starts.push(ChunkStarts {
                                        left: left_starts
                                            .get(chunk_idx)
                                            .map_or(NO_CHUNK, |&s| s as u32),
                                        right: right_starts
                                            .get(chunk_idx)
                                            .map_or(NO_CHUNK, |&s| s as u32),
                                    });
                                }
                            }
                        } else {
                            for line_idx in 0..hunk.lines.len() {
                                rows.push(RowRef::SideBySideLine {
                                    file_idx,
                                    hunk_idx,
                                    line_idx,
                                    chunk_idx: 0,
                                });
                                chunk_starts.push(ChunkStarts { left: 0, right: 0 });
                            }
                        }
                    }
                }
                hunk_rows.push(HunkRows {
                    file_idx,
                    hunk_idx,
                    rows: hunk_start_row..rows.len(),
                });
            }

            let gap_after = if !file.hunks.is_empty() && file.total_new_lines > 0 {
                let last_new = file
                    .hunks
                    .last()
                    .and_then(|h| h.last_new_lineno())
                    .unwrap_or(0) as usize;
                file.total_new_lines.saturating_sub(last_new)
            } else {
                0
            };
            rows.push(if gap_after > 0 {
                RowRef::GapTail {
                    file_idx,
                    gap_idx: file.hunks.len(),
                    gap_after,
                }
            } else {
                RowRef::Blank { file_idx }
            });
            chunk_starts.push(NO_CHUNKS);
        }

        debug_assert_eq!(rows.len(), chunk_starts.len());
        Self {
            content_width,
            rows,
            file_header_rows,
            comment_lines,
            call_context,
            hunk_rows,
            lineno_widths,
            chunk_starts,
        }
    }

    /// Text of a `CallContext` row.
    pub fn call_context_text(
        &self,
        file_idx: usize,
        hunk_idx: usize,
        line_idx: usize,
    ) -> Option<&str> {
        self.call_context
            .get(&(file_idx, hunk_idx))?
            .get(line_idx)
            .map(String::as_str)
    }

    /// Gutter digit count for a file, precomputed at build time.
    pub fn lineno_width(&self, file_idx: usize) -> usize {
        self.lineno_widths.get(file_idx).copied().unwrap_or(4)
    }

    /// Byte offset where the wrapped chunk on `row` starts in its source line, for the
    /// unified view or the left side-by-side pane. `None` when that side has no content.
    pub fn chunk_start(&self, row: usize) -> Option<usize> {
        self.chunk_starts
            .get(row)
            .filter(|starts| starts.left != NO_CHUNK)
            .map(|starts| starts.left as usize)
    }

    /// Byte offset where the right pane's wrapped chunk on `row` starts.
    pub fn right_chunk_start(&self, row: usize) -> Option<usize> {
        self.chunk_starts
            .get(row)
            .filter(|starts| starts.right != NO_CHUNK)
            .map(|starts| starts.right as usize)
    }

    pub fn content_width(&self) -> usize {
        self.content_width
    }

    pub fn total_lines(&self) -> usize {
        self.rows.len()
    }

    pub fn row(&self, row: usize) -> Option<RowRef> {
        self.rows.get(row).copied()
    }

    pub fn row_file_idx(&self, row: usize) -> Option<usize> {
        self.row(row).map(|r| r.file_idx())
    }

    pub fn file_header_row(&self, file_idx: usize) -> Option<usize> {
        self.file_header_rows.get(file_idx).copied()
    }

    pub fn next_file_header_row(&self, after: usize) -> Option<usize> {
        self.file_header_rows
            .iter()
            .copied()
            .find(|&row| row > after)
    }

    pub fn prev_file_header_row(&self, before: usize) -> Option<usize> {
        self.file_header_rows
            .iter()
            .rev()
            .copied()
            .find(|&row| row < before)
    }

    pub fn focused_file_at_scroll(&self, scroll_offset: usize) -> Option<usize> {
        self.row_file_idx(scroll_offset)
    }

    pub fn hunk_at_row(&self, row: usize) -> Option<(usize, usize)> {
        match self.row(row)? {
            RowRef::HunkHeader {
                file_idx, hunk_idx, ..
            }
            | RowRef::UnifiedLine {
                file_idx, hunk_idx, ..
            }
            | RowRef::SideBySideLine {
                file_idx, hunk_idx, ..
            }
            | RowRef::Comment {
                file_idx, hunk_idx, ..
            }
            | RowRef::CallContext {
                file_idx, hunk_idx, ..
            } => Some((file_idx, hunk_idx)),
            _ => None,
        }
    }

    pub fn hunk_at_or_after_row(&self, row: usize, file_idx: usize) -> Option<(usize, usize)> {
        for idx in row..self.rows.len() {
            match self.rows[idx] {
                RowRef::HunkHeader {
                    file_idx: current_file,
                    hunk_idx,
                    ..
                }
                | RowRef::UnifiedLine {
                    file_idx: current_file,
                    hunk_idx,
                    ..
                }
                | RowRef::SideBySideLine {
                    file_idx: current_file,
                    hunk_idx,
                    ..
                }
                | RowRef::Comment {
                    file_idx: current_file,
                    hunk_idx,
                    ..
                }
                | RowRef::CallContext {
                    file_idx: current_file,
                    hunk_idx,
                    ..
                } if current_file == file_idx => return Some((file_idx, hunk_idx)),
                _ if self.rows.get(idx).map(|r| r.file_idx()) != Some(file_idx) => break,
                _ => {}
            }
        }
        None
    }

    /// Rows occupied by a hunk, from its header row through its last line.
    pub fn hunk_row_range(&self, file_idx: usize, hunk_idx: usize) -> Option<Range<usize>> {
        self.hunk_rows
            .iter()
            .find(|hunk| hunk.file_idx == file_idx && hunk.hunk_idx == hunk_idx)
            .map(|hunk| hunk.rows.clone())
    }

    /// The first hunk with any row inside `visible`.
    pub fn first_hunk_in_rows(&self, visible: Range<usize>) -> Option<(usize, usize)> {
        self.hunk_rows
            .iter()
            .find(|hunk| hunk.rows.start < visible.end && hunk.rows.end > visible.start)
            .map(|hunk| (hunk.file_idx, hunk.hunk_idx))
    }

    /// The hunk that starts after `row`, if any.
    pub fn next_hunk_after_row(&self, row: usize) -> Option<(usize, usize)> {
        self.hunk_rows
            .iter()
            .find(|hunk| hunk.rows.start > row)
            .map(|hunk| (hunk.file_idx, hunk.hunk_idx))
    }

    /// The last hunk that starts before `row`, if any.
    pub fn prev_hunk_before_row(&self, row: usize) -> Option<(usize, usize)> {
        self.hunk_rows
            .iter()
            .rev()
            .find(|hunk| hunk.rows.start < row)
            .map(|hunk| (hunk.file_idx, hunk.hunk_idx))
    }

    pub fn expand_gap_at_row(&self, row: usize) -> Option<(usize, usize)> {
        match self.row(row)? {
            RowRef::HunkHeader {
                file_idx,
                hunk_idx,
                gap_before,
            } if gap_before > 0 => Some((file_idx, hunk_idx)),
            RowRef::GapTail {
                file_idx,
                gap_idx,
                gap_after,
            } if gap_after > 0 => Some((file_idx, gap_idx)),
            _ => None,
        }
    }

    /// Get the wrapped comment line text for a Comment row.
    pub fn comment_line_text(
        &self,
        file_idx: usize,
        hunk_idx: usize,
        wrap_idx: usize,
    ) -> Option<&str> {
        self.comment_lines
            .get(&(file_idx, hunk_idx))?
            .get(wrap_idx)
            .map(|s| s.as_str())
    }

    /// Check if a hunk has a comment.
    pub fn hunk_has_comment(&self, file_idx: usize, hunk_idx: usize) -> bool {
        self.comment_lines.contains_key(&(file_idx, hunk_idx))
    }
}

/// Call-context lines per hunk: one per changed function the index knows, at most three.
fn hunk_call_context(
    index: &SymbolIndex,
    file: &FileDiff,
    width: usize,
) -> Vec<(usize, Vec<String>)> {
    let symbols = outline::file_symbols_with(file, Some(index));
    let mut by_hunk: Vec<(usize, Vec<(String, outline::CallSummary<'_>)>)> = Vec::new();
    for symbol in symbols {
        let Some(ident) = symbol.ident.as_deref() else {
            continue;
        };
        let Some(summary) = outline::call_summary(index, file, ident, symbol.change) else {
            continue;
        };
        let entry = match by_hunk
            .iter_mut()
            .find(|(hunk, _)| *hunk == symbol.hunk_idx)
        {
            Some(entry) => entry,
            None => {
                by_hunk.push((symbol.hunk_idx, Vec::new()));
                by_hunk.last_mut().expect("just pushed")
            }
        };
        if entry.1.len() >= 3 {
            continue;
        }
        entry.1.push((symbol.name.clone(), summary));
    }
    by_hunk
        .into_iter()
        .map(|(hunk_idx, entries)| {
            let multiple = entries.len() > 1;
            let lines = entries
                .iter()
                .map(|(name, summary)| {
                    outline::inline_call_context(summary, multiple.then_some(name.as_str()), width)
                })
                .collect();
            (hunk_idx, lines)
        })
        .collect()
}

pub(crate) fn line_number_width(file: &FileDiff) -> usize {
    let max_lineno = file
        .hunks
        .iter()
        .flat_map(|hunk| {
            hunk.lines
                .iter()
                .filter_map(|line| line.new_lineno.or(line.old_lineno))
        })
        .max()
        .unwrap_or(0);
    let digits = if max_lineno == 0 {
        1
    } else {
        max_lineno.ilog10() as usize + 1
    };
    digits.max(4)
}

fn unified_content_width_for(line_number_width: usize, total_width: usize) -> usize {
    // "NNNN NNNN │+ " — two line numbers, a space, the separator, and the prefix.
    total_width.saturating_sub(line_number_width * 2 + 5)
}

/// Widths of the left and right side-by-side panes, leaving one column for the divider.
pub(crate) fn side_by_side_pane_widths(total_width: usize) -> (usize, usize) {
    let usable = total_width.saturating_sub(1);
    let left_pane_width = usable.div_ceil(2);
    let right_pane_width = usable / 2;
    (left_pane_width, right_pane_width)
}

/// Gutter width of one side-by-side pane: "NNNN │+ ".
pub(crate) fn side_by_side_gutter_width(line_number_width: usize) -> usize {
    line_number_width + 4
}

fn side_by_side_content_widths_for(line_number_width: usize, total_width: usize) -> (usize, usize) {
    let (left_pane_width, right_pane_width) = side_by_side_pane_widths(total_width);
    let gutter = side_by_side_gutter_width(line_number_width);
    (
        left_pane_width.saturating_sub(gutter),
        right_pane_width.saturating_sub(gutter),
    )
}

/// Byte offsets where each wrapped chunk of `content` starts when wrapped at `max_width`
/// columns. Always yields at least one chunk. Reuses `out` to avoid per-line allocation.
fn chunk_start_offsets(content: &str, max_width: usize, out: &mut Vec<usize>) {
    out.clear();
    out.push(0);
    if content.is_empty() || max_width == 0 || display_width(content) <= max_width {
        return;
    }
    let mut start = 0;
    loop {
        start = chunk_end(content, start, max_width);
        if start >= content.len() {
            break;
        }
        out.push(start);
    }
}

/// Printable ASCII only: every byte is exactly one column and its own grapheme, so
/// wrapping needs no Unicode segmentation. Tabs and other controls take the slow path
/// to keep their zero-width treatment identical to `unicode-width`.
fn is_plain_ascii(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| (0x20..0x7f).contains(byte))
}

/// Display width with a fast path for plain ASCII.
fn display_width(content: &str) -> usize {
    if is_plain_ascii(content.as_bytes()) {
        content.len()
    } else {
        UnicodeWidthStr::width(content)
    }
}

pub(crate) fn chunk_end(content: &str, start: usize, max_width: usize) -> usize {
    let rest = &content[start..];
    // Only the bytes that could land in this chunk matter for the fast path; scanning the
    // whole remainder would make wrapping a long line quadratic.
    // Byte-slice the window: a multibyte character inside it fails the ASCII test anyway,
    // so no char-boundary check is needed.
    let window = &rest.as_bytes()[..rest.len().min(max_width)];
    // A combining mark right after the window would belong to the last ASCII character's
    // grapheme; it is always non-ASCII, so an ASCII (or absent) next byte is a safe cut.
    let next_is_ascii = rest.as_bytes().get(window.len()).is_none_or(u8::is_ascii);
    let end = if next_is_ascii && is_plain_ascii(window) {
        // One column per byte: the chunk ends at `max_width` bytes or the end of the line.
        start + window.len()
    } else {
        let mut width = 0;
        let mut end = start;
        for (offset, grapheme) in rest.grapheme_indices(true) {
            let grapheme_end = start + offset + grapheme.len();
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if width + grapheme_width > max_width && width > 0 {
                break;
            }
            width += grapheme_width;
            end = grapheme_end;
            if width >= max_width {
                break;
            }
        }
        end
    };

    if end < content.len()
        && let Some((offset, character)) = content[start..end]
            .char_indices()
            .rev()
            .find(|(_, character)| character.is_whitespace())
    {
        return start + offset + character.len_utf8();
    }
    end
}

/// Word-wrap a comment string to fit within `max_width` characters.
fn wrap_comment(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        if raw_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in raw_line.split_whitespace() {
            if current.is_empty() {
                current = word.to_string();
            } else if UnicodeWidthStr::width(current.as_str()) + 1 + UnicodeWidthStr::width(word)
                <= max_width
            {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current);
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::DiffLayout;
    use crate::diff::{DiffLine, FileDiff, FileStatus, Hunk, LineKind, SideBySideLine};
    use crate::viewport::{RowRef, ViewKind, ViewportState};

    fn sample_file() -> FileDiff {
        FileDiff {
            path: "AUDIT.md".to_string(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: vec![Hunk {
                header: "@@ -10,1 +12,1 @@ heading".to_string(),
                lines: vec![DiffLine {
                    kind: LineKind::Context,
                    content: "line".to_string(),
                    old_lineno: Some(10),
                    new_lineno: Some(12),
                }],
            }],
            additions: 1,
            deletions: 0,
            collapsed: false,
            total_new_lines: 20,
            sbs_cache: None,
        }
    }

    #[test]
    fn unified_layout_tracks_gap_and_tail_rows() {
        let layout = DiffLayout::build(&[sample_file()], ViewKind::Unified, &[], 80);
        assert_eq!(layout.total_lines(), 4);
        assert_eq!(layout.row(0), Some(RowRef::FileHeader { file_idx: 0 }));
        assert_eq!(
            layout.row(1),
            Some(RowRef::HunkHeader {
                file_idx: 0,
                hunk_idx: 0,
                gap_before: 11,
            })
        );
        assert_eq!(layout.expand_gap_at_row(1), Some((0, 0)));
        assert_eq!(
            layout.row(3),
            Some(RowRef::GapTail {
                file_idx: 0,
                gap_idx: 1,
                gap_after: 8,
            })
        );
    }
    #[test]
    fn wrapped_line_continuations_are_physical_rows_reachable_by_scrolling() {
        let mut file = sample_file();
        file.hunks[0].lines[0].content = "abcdefghijkl".to_string();
        file.total_new_lines = 12;

        // A four-column content area wraps this logical line into three chunks.
        let layout = DiffLayout::build(&[file], ViewKind::Unified, &[], 17);
        assert_eq!(layout.total_lines(), 6);
        assert_eq!(
            layout.row(2),
            Some(RowRef::UnifiedLine {
                file_idx: 0,
                hunk_idx: 0,
                line_idx: 0,
                chunk_idx: 0,
            })
        );
        assert_eq!(
            layout.row(4),
            Some(RowRef::UnifiedLine {
                file_idx: 0,
                hunk_idx: 0,
                line_idx: 0,
                chunk_idx: 2,
            })
        );

        let mut viewport = ViewportState::default();
        viewport.jump_to_bottom(layout.total_lines(), 2);
        assert_eq!(viewport.visible_range(layout.total_lines(), 2), 4..6);
    }

    #[test]
    fn side_by_side_layout_uses_the_taller_wrapped_side() {
        let mut file = sample_file();
        file.total_new_lines = 12;
        file.sbs_cache = Some(vec![vec![SideBySideLine {
            left: Some(DiffLine {
                content: "abcdefghijkl".to_string(),
                ..file.hunks[0].lines[0].clone()
            }),
            right: Some(DiffLine {
                content: "abcdef".to_string(),
                ..file.hunks[0].lines[0].clone()
            }),
            left_changed: None,
            right_changed: None,
        }]]);

        // 26 columns minus the divider gives 13- and 12-column panes; each pane spends
        // eight columns on its gutter, leaving five and four content columns.
        let layout = DiffLayout::build(&[file], ViewKind::SideBySide, &[], 26);
        assert_eq!(layout.total_lines(), 6);
        assert_eq!(
            layout.row(4),
            Some(RowRef::SideBySideLine {
                file_idx: 0,
                hunk_idx: 0,
                line_idx: 0,
                chunk_idx: 2,
            })
        );
        assert_eq!(layout.hunk_at_row(4), Some((0, 0)));
    }
}
