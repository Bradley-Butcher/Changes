use similar::TextDiff;

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
}

pub fn compute_side_by_side(hunks: &[Hunk]) -> Vec<Vec<SideBySideLine>> {
    hunks
        .iter()
        .map(|hunk| {
            let old_text: String = hunk
                .lines
                .iter()
                .filter(|l| l.kind == LineKind::Context || l.kind == LineKind::Deletion)
                .map(|l| format!("{}\n", l.content))
                .collect();
            let new_text: String = hunk
                .lines
                .iter()
                .filter(|l| l.kind == LineKind::Context || l.kind == LineKind::Addition)
                .map(|l| format!("{}\n", l.content))
                .collect();

            let diff = TextDiff::from_lines(&old_text, &new_text);
            let mut result = Vec::new();

            for change in diff.iter_all_changes() {
                match change.tag() {
                    similar::ChangeTag::Equal => {
                        let content = change.as_str().unwrap_or("").trim_end().to_string();
                        result.push(SideBySideLine {
                            left: Some(DiffLine {
                                kind: LineKind::Context,
                                content: content.clone(),
                                old_lineno: change.old_index().map(|i| i as u32 + 1),
                                new_lineno: change.new_index().map(|i| i as u32 + 1),
                            }),
                            right: Some(DiffLine {
                                kind: LineKind::Context,
                                content,
                                old_lineno: change.old_index().map(|i| i as u32 + 1),
                                new_lineno: change.new_index().map(|i| i as u32 + 1),
                            }),
                        });
                    }
                    similar::ChangeTag::Delete => {
                        let content = change.as_str().unwrap_or("").trim_end().to_string();
                        result.push(SideBySideLine {
                            left: Some(DiffLine {
                                kind: LineKind::Deletion,
                                content,
                                old_lineno: change.old_index().map(|i| i as u32 + 1),
                                new_lineno: None,
                            }),
                            right: None,
                        });
                    }
                    similar::ChangeTag::Insert => {
                        let content = change.as_str().unwrap_or("").trim_end().to_string();
                        result.push(SideBySideLine {
                            left: None,
                            right: Some(DiffLine {
                                kind: LineKind::Addition,
                                content,
                                old_lineno: None,
                                new_lineno: change.new_index().map(|i| i as u32 + 1),
                            }),
                        });
                    }
                }
            }

            result
        })
        .collect()
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
