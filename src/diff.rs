use similar::TextDiff;

/// Byte ranges within a line that represent the *changed* characters.
/// Used for inline word-level diff highlighting.
pub type ChangedRanges = Vec<(usize, usize)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    pub content: String,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    pub fn first_new_lineno(&self) -> Option<u32> {
        self.lines.iter().find_map(|l| l.new_lineno)
    }
    pub fn last_new_lineno(&self) -> Option<u32> {
        self.lines.iter().rev().find_map(|l| l.new_lineno)
    }
    pub fn first_old_lineno(&self) -> Option<u32> {
        self.lines.iter().find_map(|l| l.old_lineno)
    }
    pub fn last_old_lineno(&self) -> Option<u32> {
        self.lines.iter().rev().find_map(|l| l.old_lineno)
    }
}

/// Number of hidden lines between two adjacent hunks.
pub fn gap_between_hunks(prev: &Hunk, next: &Hunk) -> usize {
    let prev_end = prev.last_new_lineno().unwrap_or(0) as usize;
    let next_start = next.first_new_lineno().unwrap_or(0) as usize;
    next_start.saturating_sub(prev_end + 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub hunks: Vec<Hunk>,
    pub additions: usize,
    pub deletions: usize,
    pub collapsed: bool,
    /// Total lines in the new version of the file (for bottom expand indicator).
    pub total_new_lines: usize,
    /// Pre-computed side-by-side data, cached to avoid recomputing on every frame.
    pub sbs_cache: Option<Vec<Vec<SideBySideLine>>>,
}

impl FileDiff {
    pub fn total_display_lines(&self) -> usize {
        if self.collapsed {
            return 1; // just the header
        }
        // file header + (hunk header + lines) per hunk
        1 + self.hunks.iter().map(|h| 1 + h.lines.len()).sum::<usize>()
    }

    /// Total lines in side-by-side mode (may differ from unified due to alignment).
    pub fn total_sbs_display_lines(&self) -> usize {
        if self.collapsed {
            return 1;
        }
        let sbs_lines: usize = match &self.sbs_cache {
            Some(hunks) => hunks.iter().map(|h| 1 + h.len()).sum(),
            None => self.hunks.iter().map(|h| 1 + h.lines.len()).sum(),
        };
        1 + sbs_lines
    }

    pub fn ensure_sbs_cache(&mut self) {
        if self.sbs_cache.is_none() {
            self.sbs_cache = Some(compute_side_by_side(&self.hunks));
        }
    }
}

#[derive(Debug, Clone)]
pub struct SideBySideLine {
    pub left: Option<DiffLine>,
    pub right: Option<DiffLine>,
    /// Character ranges that changed within the left line (for inline highlighting).
    pub left_changed: Option<ChangedRanges>,
    /// Character ranges that changed within the right line (for inline highlighting).
    pub right_changed: Option<ChangedRanges>,
}

pub fn compute_side_by_side(hunks: &[Hunk]) -> Vec<Vec<SideBySideLine>> {
    hunks.iter().map(align_hunk_lines).collect()
}

/// Use line-level diff to align old/new sides, then compute word-level inline diffs
/// for matched deletion/addition pairs.
fn align_hunk_lines(hunk: &Hunk) -> Vec<SideBySideLine> {
    let old_lines: Vec<&DiffLine> = hunk
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Context || l.kind == LineKind::Deletion)
        .collect();
    let new_lines: Vec<&DiffLine> = hunk
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Context || l.kind == LineKind::Addition)
        .collect();

    let old_text: String = old_lines
        .iter()
        .map(|l| format!("{}\n", l.content))
        .collect();
    let new_text: String = new_lines
        .iter()
        .map(|l| format!("{}\n", l.content))
        .collect();

    let diff = TextDiff::from_lines(&old_text, &new_text);
    let mut result = Vec::new();

    // Collect changes, then pair up delete/insert runs for inline diff
    let mut pending_dels: Vec<&DiffLine> = Vec::new();
    let mut pending_adds: Vec<&DiffLine> = Vec::new();

    let flush_pending =
        |result: &mut Vec<SideBySideLine>, dels: &mut Vec<&DiffLine>, adds: &mut Vec<&DiffLine>| {
            let max_len = dels.len().max(adds.len());
            for j in 0..max_len {
                let left = dels.get(j).map(|l| (*l).clone());
                let right = adds.get(j).map(|l| (*l).clone());
                let (left_changed, right_changed) = if let (Some(l), Some(r)) = (&left, &right) {
                    compute_inline_diff(&l.content, &r.content)
                } else {
                    (None, None)
                };
                result.push(SideBySideLine {
                    left,
                    right,
                    left_changed,
                    right_changed,
                });
            }
            dels.clear();
            adds.clear();
        };

    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Equal => {
                flush_pending(&mut result, &mut pending_dels, &mut pending_adds);
                let old_idx = change.old_index().unwrap();
                let new_idx = change.new_index().unwrap();
                result.push(SideBySideLine {
                    left: Some(old_lines[old_idx].clone()),
                    right: Some(new_lines[new_idx].clone()),
                    left_changed: None,
                    right_changed: None,
                });
            }
            similar::ChangeTag::Delete => {
                let old_idx = change.old_index().unwrap();
                pending_dels.push(old_lines[old_idx]);
            }
            similar::ChangeTag::Insert => {
                let new_idx = change.new_index().unwrap();
                pending_adds.push(new_lines[new_idx]);
            }
        }
    }
    flush_pending(&mut result, &mut pending_dels, &mut pending_adds);

    result
}

/// Compute word-level diff between two lines.
/// Returns byte ranges of changed words in each line.
fn compute_inline_diff(old: &str, new: &str) -> (Option<ChangedRanges>, Option<ChangedRanges>) {
    if old == new {
        return (None, None);
    }

    let diff = TextDiff::from_words(old, new);
    let mut old_ranges = Vec::new();
    let mut new_ranges = Vec::new();
    let mut old_pos = 0usize;
    let mut new_pos = 0usize;

    for change in diff.iter_all_changes() {
        let value = change.value();
        let byte_len = value.len();

        match change.tag() {
            similar::ChangeTag::Equal => {
                old_pos += byte_len;
                new_pos += byte_len;
            }
            similar::ChangeTag::Delete => {
                old_ranges.push((old_pos, old_pos + byte_len));
                old_pos += byte_len;
            }
            similar::ChangeTag::Insert => {
                new_ranges.push((new_pos, new_pos + byte_len));
                new_pos += byte_len;
            }
        }
    }

    // Merge adjacent ranges (consecutive changed words should be one highlight)
    let left = if old_ranges.is_empty() {
        None
    } else {
        Some(merge_ranges(old_ranges))
    };
    let right = if new_ranges.is_empty() {
        None
    } else {
        Some(merge_ranges(new_ranges))
    };
    (left, right)
}

/// Merge adjacent or overlapping byte ranges into contiguous spans.
fn merge_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if ranges.len() <= 1 {
        return ranges;
    }
    ranges.sort_by_key(|r| r.0);
    let mut merged = vec![ranges[0]];
    for &(start, end) in &ranges[1..] {
        let last = merged.last_mut().unwrap();
        if start <= last.1 {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::{DiffLine, Hunk, gap_between_hunks};

    #[test]
    fn adjacent_hunks_have_no_gap() {
        let prev = Hunk {
            header: String::new(),
            lines: vec![DiffLine {
                kind: super::LineKind::Context,
                content: String::new(),
                old_lineno: Some(10),
                new_lineno: Some(10),
            }],
        };
        let next = Hunk {
            header: String::new(),
            lines: vec![DiffLine {
                kind: super::LineKind::Context,
                content: String::new(),
                old_lineno: Some(11),
                new_lineno: Some(11),
            }],
        };
        assert_eq!(gap_between_hunks(&prev, &next), 0);
    }

    #[test]
    fn gap_of_five() {
        let prev = Hunk {
            header: String::new(),
            lines: vec![DiffLine {
                kind: super::LineKind::Context,
                content: String::new(),
                old_lineno: Some(10),
                new_lineno: Some(10),
            }],
        };
        let next = Hunk {
            header: String::new(),
            lines: vec![DiffLine {
                kind: super::LineKind::Context,
                content: String::new(),
                old_lineno: Some(16),
                new_lineno: Some(16),
            }],
        };
        assert_eq!(gap_between_hunks(&prev, &next), 5);
    }

    #[test]
    fn overlapping_hunks_have_no_gap() {
        let prev = Hunk {
            header: String::new(),
            lines: vec![DiffLine {
                kind: super::LineKind::Context,
                content: String::new(),
                old_lineno: Some(15),
                new_lineno: Some(15),
            }],
        };
        let next = Hunk {
            header: String::new(),
            lines: vec![DiffLine {
                kind: super::LineKind::Context,
                content: String::new(),
                old_lineno: Some(10),
                new_lineno: Some(10),
            }],
        };
        assert_eq!(gap_between_hunks(&prev, &next), 0);
    }

    #[test]
    fn empty_hunks_have_no_gap() {
        let prev = Hunk {
            header: String::new(),
            lines: vec![],
        };
        let next = Hunk {
            header: String::new(),
            lines: vec![],
        };
        assert_eq!(gap_between_hunks(&prev, &next), 0);
    }
}
