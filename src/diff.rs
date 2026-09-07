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
            return 1;
        }
        1 + self
            .hunks
            .iter()
            .map(|hunk| 1 + hunk.lines.len())
            .sum::<usize>()
    }

    pub fn total_sbs_display_lines(&self) -> usize {
        if self.collapsed {
            return 1;
        }
        let hunk_lines: usize = match &self.sbs_cache {
            Some(hunks) => hunks.iter().map(|hunk| 1 + hunk.len()).sum(),
            None => self.hunks.iter().map(|hunk| 1 + hunk.lines.len()).sum(),
        };
        1 + hunk_lines
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

/// Lines above which alignment work is split across threads.
const PARALLEL_SBS_LINES: usize = 8_000;

pub fn compute_side_by_side(hunks: &[Hunk]) -> Vec<Vec<SideBySideLine>> {
    let total_lines: usize = hunks.iter().map(|hunk| hunk.lines.len()).sum();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    if total_lines < PARALLEL_SBS_LINES || hunks.len() < 2 || threads < 2 {
        return hunks.iter().map(align_hunk_lines).collect();
    }
    let chunk_size = hunks.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let handles: Vec<_> = hunks
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || chunk.iter().map(align_hunk_lines).collect::<Vec<_>>())
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("side-by-side worker panicked"))
            .collect()
    })
}

/// Build the side-by-side cache for every file that lacks one. Large diffs are spread
/// across threads, largest files first, so a refresh in side-by-side view stays quick.
pub fn ensure_sbs_caches(files: &mut [FileDiff]) {
    let mut missing: Vec<&mut FileDiff> = files
        .iter_mut()
        .filter(|file| file.sbs_cache.is_none())
        .collect();
    let total_lines: usize = missing
        .iter()
        .flat_map(|file| &file.hunks)
        .map(|hunk| hunk.lines.len())
        .sum();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    if missing.len() == 1 {
        // One file: parallelism (if any) happens across its hunks instead.
        missing[0].ensure_sbs_cache();
        return;
    }
    if total_lines < PARALLEL_SBS_LINES || threads < 2 {
        for file in missing {
            file.sbs_cache = Some(file.hunks.iter().map(align_hunk_lines).collect());
        }
        return;
    }

    // Longest-processing-time-first assignment keeps the threads evenly loaded.
    let line_count = |file: &FileDiff| file.hunks.iter().map(|h| h.lines.len()).sum::<usize>();
    missing.sort_by_key(|file| std::cmp::Reverse(line_count(file)));
    let mut groups: Vec<(usize, Vec<&mut FileDiff>)> =
        (0..threads).map(|_| (0, Vec::new())).collect();
    for file in missing {
        let lines = line_count(file);
        let (load, group) = groups
            .iter_mut()
            .min_by_key(|(load, _)| *load)
            .expect("at least one group");
        *load += lines;
        group.push(file);
    }
    std::thread::scope(|scope| {
        for (_, group) in groups {
            scope.spawn(move || {
                for file in group {
                    file.sbs_cache = Some(file.hunks.iter().map(align_hunk_lines).collect());
                }
            });
        }
    });
}

/// Pair up the two sides of a hunk. Git's hunk is already an alignment: context lines
/// appear on both sides, and each run of deletions is matched positionally with the run
/// of additions that follows it. Matched pairs get a word-level inline diff.
fn align_hunk_lines(hunk: &Hunk) -> Vec<SideBySideLine> {
    let mut result = Vec::with_capacity(hunk.lines.len());
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

    for line in &hunk.lines {
        match line.kind {
            LineKind::Context => {
                flush_pending(&mut result, &mut pending_dels, &mut pending_adds);
                result.push(SideBySideLine {
                    left: Some(line.clone()),
                    right: Some(line.clone()),
                    left_changed: None,
                    right_changed: None,
                });
            }
            LineKind::Deletion => pending_dels.push(line),
            LineKind::Addition => pending_adds.push(line),
        }
    }
    flush_pending(&mut result, &mut pending_dels, &mut pending_adds);

    result
}

/// Lines longer than this (minified bundles, data blobs) skip word-level emphasis: the
/// diff is quadratic in the worst case and the result is unreadable anyway.
const MAX_INLINE_DIFF_BYTES: usize = 4096;

/// Compute word-level diff between two lines.
/// Returns byte ranges of changed words in each line.
fn compute_inline_diff(old: &str, new: &str) -> (Option<ChangedRanges>, Option<ChangedRanges>) {
    if old.len() > MAX_INLINE_DIFF_BYTES || new.len() > MAX_INLINE_DIFF_BYTES {
        return (None, None);
    }
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
