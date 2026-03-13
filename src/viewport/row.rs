#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Unified,
    SideBySide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRef {
    FileHeader {
        file_idx: usize,
    },
    HunkHeader {
        file_idx: usize,
        hunk_idx: usize,
        gap_before: usize,
    },
    UnifiedLine {
        file_idx: usize,
        hunk_idx: usize,
        line_idx: usize,
    },
    SideBySideLine {
        file_idx: usize,
        hunk_idx: usize,
        line_idx: usize,
    },
    GapTail {
        file_idx: usize,
        gap_idx: usize,
        gap_after: usize,
    },
    Comment {
        file_idx: usize,
        hunk_idx: usize,
        /// Index within the wrapped comment text lines.
        wrap_idx: usize,
    },
    Blank {
        file_idx: usize,
    },
}

impl RowRef {
    pub fn file_idx(self) -> usize {
        match self {
            RowRef::FileHeader { file_idx }
            | RowRef::HunkHeader { file_idx, .. }
            | RowRef::UnifiedLine { file_idx, .. }
            | RowRef::SideBySideLine { file_idx, .. }
            | RowRef::GapTail { file_idx, .. }
            | RowRef::Comment { file_idx, .. }
            | RowRef::Blank { file_idx } => file_idx,
        }
    }
}
