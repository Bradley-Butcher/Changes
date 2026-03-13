mod watcher_tests {
    use std::path::PathBuf;

    /// Mirrors the logic from src/watcher.rs — returns true for noisy .git paths.
    fn is_git_internal_path(path: &std::path::Path) -> bool {
        let components: Vec<_> = path.components().collect();
        let git_pos = components.iter().position(|c| c.as_os_str() == ".git");
        let Some(pos) = git_pos else {
            return false;
        };
        let remaining: Vec<_> = components[pos + 1..].iter().collect();
        if remaining.is_empty() {
            return true;
        }
        let first = remaining[0].as_os_str().to_string_lossy();
        match first.as_ref() {
            "HEAD" | "index" | "MERGE_HEAD" | "REBASE_HEAD" | "CHERRY_PICK_HEAD" => false,
            "refs" => false,
            _ => true,
        }
    }

    #[test]
    fn test_non_git_path_passes_through() {
        assert!(!is_git_internal_path(&PathBuf::from("/repo/src/main.rs")));
    }

    #[test]
    fn test_git_head_allowed() {
        assert!(!is_git_internal_path(&PathBuf::from("/repo/.git/HEAD")));
    }

    #[test]
    fn test_git_index_allowed() {
        assert!(!is_git_internal_path(&PathBuf::from("/repo/.git/index")));
    }

    #[test]
    fn test_git_refs_allowed() {
        assert!(!is_git_internal_path(&PathBuf::from(
            "/repo/.git/refs/heads/main"
        )));
    }

    #[test]
    fn test_git_merge_head_allowed() {
        assert!(!is_git_internal_path(&PathBuf::from(
            "/repo/.git/MERGE_HEAD"
        )));
    }

    #[test]
    fn test_git_objects_filtered() {
        assert!(is_git_internal_path(&PathBuf::from(
            "/repo/.git/objects/pack/abc123"
        )));
    }

    #[test]
    fn test_git_logs_filtered() {
        assert!(is_git_internal_path(&PathBuf::from(
            "/repo/.git/logs/HEAD"
        )));
    }

    #[test]
    fn test_git_hooks_filtered() {
        assert!(is_git_internal_path(&PathBuf::from(
            "/repo/.git/hooks/pre-commit"
        )));
    }

    #[test]
    fn test_bare_git_dir_filtered() {
        assert!(is_git_internal_path(&PathBuf::from("/repo/.git")));
    }

    #[test]
    fn test_commit_editmsg_filtered() {
        assert!(is_git_internal_path(&PathBuf::from(
            "/repo/.git/COMMIT_EDITMSG"
        )));
    }
}

mod hunk_context_tests {
    /// Mirrors hunk_context from src/ui.rs
    fn hunk_context(header: &str) -> Option<&str> {
        let rest = header.strip_prefix("@@")?;
        let end = rest.find("@@")?;
        let after = rest[end + 2..].trim();
        if after.is_empty() {
            None
        } else {
            Some(after)
        }
    }

    #[test]
    fn test_extracts_function_name() {
        assert_eq!(
            hunk_context("@@ -10,5 +10,7 @@ fn foo()"),
            Some("fn foo()")
        );
    }

    #[test]
    fn test_no_function_context() {
        assert_eq!(hunk_context("@@ -10,5 +10,7 @@"), None);
    }

    #[test]
    fn test_with_whitespace_only_after() {
        assert_eq!(hunk_context("@@ -10,5 +10,7 @@   "), None);
    }

    #[test]
    fn test_impl_block() {
        assert_eq!(
            hunk_context("@@ -1,3 +1,5 @@ impl Foo"),
            Some("impl Foo")
        );
    }

    #[test]
    fn test_not_a_hunk_header() {
        assert_eq!(hunk_context("not a header"), None);
    }
}

mod gap_tests {
    #[derive(Debug, Clone)]
    struct DiffLine {
        #[allow(dead_code)]
        old_lineno: Option<u32>,
        new_lineno: Option<u32>,
    }

    #[derive(Debug, Clone)]
    struct Hunk {
        lines: Vec<DiffLine>,
    }

    impl Hunk {
        fn last_new_lineno(&self) -> Option<u32> {
            self.lines.iter().rev().find_map(|l| l.new_lineno)
        }
        fn first_new_lineno(&self) -> Option<u32> {
            self.lines.iter().find_map(|l| l.new_lineno)
        }
    }

    fn gap_between_hunks(prev: &Hunk, next: &Hunk) -> usize {
        let prev_end = prev.last_new_lineno().unwrap_or(0) as usize;
        let next_start = next.first_new_lineno().unwrap_or(0) as usize;
        next_start.saturating_sub(prev_end + 1)
    }

    #[test]
    fn test_adjacent_hunks_no_gap() {
        let prev = Hunk {
            lines: vec![DiffLine {
                old_lineno: Some(10),
                new_lineno: Some(10),
            }],
        };
        let next = Hunk {
            lines: vec![DiffLine {
                old_lineno: Some(11),
                new_lineno: Some(11),
            }],
        };
        assert_eq!(gap_between_hunks(&prev, &next), 0);
    }

    #[test]
    fn test_gap_of_five() {
        let prev = Hunk {
            lines: vec![DiffLine {
                old_lineno: Some(10),
                new_lineno: Some(10),
            }],
        };
        let next = Hunk {
            lines: vec![DiffLine {
                old_lineno: Some(16),
                new_lineno: Some(16),
            }],
        };
        assert_eq!(gap_between_hunks(&prev, &next), 5);
    }

    #[test]
    fn test_overlapping_hunks() {
        let prev = Hunk {
            lines: vec![DiffLine {
                old_lineno: Some(15),
                new_lineno: Some(15),
            }],
        };
        let next = Hunk {
            lines: vec![DiffLine {
                old_lineno: Some(10),
                new_lineno: Some(10),
            }],
        };
        assert_eq!(gap_between_hunks(&prev, &next), 0);
    }

    #[test]
    fn test_empty_hunks() {
        let prev = Hunk { lines: vec![] };
        let next = Hunk { lines: vec![] };
        assert_eq!(gap_between_hunks(&prev, &next), 0);
    }
}

mod fuzzy_match_tests {
    /// Mirrors the fuzzy matching logic from App::filtered_file_indices
    fn fuzzy_match(path: &str, query: &str) -> bool {
        let path = path.to_lowercase();
        let query = query.to_lowercase();
        let mut chars = query.chars();
        let mut current = chars.next();
        for c in path.chars() {
            if let Some(q) = current {
                if c == q {
                    current = chars.next();
                }
            } else {
                break;
            }
        }
        current.is_none()
    }

    #[test]
    fn test_exact_match() {
        assert!(fuzzy_match("src/main.rs", "src/main.rs"));
    }

    #[test]
    fn test_subsequence_match() {
        assert!(fuzzy_match("src/main.rs", "smr"));
    }

    #[test]
    fn test_no_match() {
        assert!(!fuzzy_match("src/main.rs", "xyz"));
    }

    #[test]
    fn test_empty_query_matches_all() {
        assert!(fuzzy_match("anything.rs", ""));
    }

    #[test]
    fn test_case_insensitive() {
        assert!(fuzzy_match("src/Main.RS", "main"));
    }

    #[test]
    fn test_query_longer_than_path() {
        assert!(!fuzzy_match("ab", "abc"));
    }
}
