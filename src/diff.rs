use similar::TextDiff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
    HunkHeader,
    FileHeader,
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
}

impl FileDiff {
    pub fn total_display_lines(&self) -> usize {
        if self.collapsed {
            return 1; // just the header
        }
        1 + self.hunks.iter().map(|h| h.lines.len()).sum::<usize>()
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
