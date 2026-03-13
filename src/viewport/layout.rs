use super::row::{RowRef, ViewKind};
use crate::app::HunkComment;
use crate::diff::{FileDiff, gap_between_hunks};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct DiffLayout {
    rows: Vec<RowRef>,
    file_header_rows: Vec<usize>,
    /// Wrapped comment text lines, keyed by (file_idx, hunk_idx).
    comment_lines: HashMap<(usize, usize), Vec<String>>,
}

impl DiffLayout {
    pub fn build(
        files: &[FileDiff],
        view_kind: ViewKind,
        comments: &[HunkComment],
        content_width: usize,
    ) -> Self {
        let mut rows = Vec::new();
        let mut file_header_rows = Vec::with_capacity(files.len());

        // Pre-compute wrapped comment lines keyed by (file_idx, hunk_idx)
        let comment_width = content_width.saturating_sub(4).max(20);
        let mut comment_lines: HashMap<(usize, usize), Vec<String>> = HashMap::new();
        for c in comments {
            comment_lines.insert(
                (c.file_idx, c.hunk_idx),
                wrap_comment(&c.text, comment_width),
            );
        }

        for (file_idx, file) in files.iter().enumerate() {
            file_header_rows.push(rows.len());
            rows.push(RowRef::FileHeader { file_idx });

            if file.collapsed {
                rows.push(RowRef::Blank { file_idx });
                continue;
            }

            for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
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

                // Emit comment rows after hunk header
                if let Some(lines) = comment_lines.get(&(file_idx, hunk_idx)) {
                    for wrap_idx in 0..lines.len() {
                        rows.push(RowRef::Comment {
                            file_idx,
                            hunk_idx,
                            wrap_idx,
                        });
                    }
                }

                let line_count = match view_kind {
                    ViewKind::Unified => hunk.lines.len(),
                    ViewKind::SideBySide => file
                        .sbs_cache
                        .as_ref()
                        .and_then(|cache| cache.get(hunk_idx))
                        .map(|lines| lines.len())
                        .unwrap_or(hunk.lines.len()),
                };

                for line_idx in 0..line_count {
                    rows.push(match view_kind {
                        ViewKind::Unified => RowRef::UnifiedLine {
                            file_idx,
                            hunk_idx,
                            line_idx,
                        },
                        ViewKind::SideBySide => RowRef::SideBySideLine {
                            file_idx,
                            hunk_idx,
                            line_idx,
                        },
                    });
                }
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
        }

        Self {
            rows,
            file_header_rows,
            comment_lines,
        }
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
                } if current_file == file_idx => return Some((file_idx, hunk_idx)),
                _ if self.rows.get(idx).map(|r| r.file_idx()) != Some(file_idx) => break,
                _ => {}
            }
        }
        None
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
            } else if current.len() + 1 + word.len() <= max_width {
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
    use crate::diff::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};
    use crate::viewport::{RowRef, ViewKind};

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
}
