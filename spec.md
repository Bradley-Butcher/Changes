# changes — Live Git Diff Viewer

## Overview

A terminal UI application that watches one or more git repositories and displays staged, unstaged, and untracked changes in real time with syntax highlighting. It is a read-only diff viewer — not a git workflow tool. Think "lazygit's diff pane, but prettier, multi-repo, and focused."

## Core Concepts

- **Multi-repo aware**: Point it at a directory containing multiple git repos (e.g. `~/adp-dev/`) and it discovers all immediate child directories that are git repositories.
- **Single-repo mode**: Point it at a single git repo and it works the same way, just with one tab.
- **Live**: Uses filesystem watching (not polling) to detect changes and update the UI instantly.
- **Read-only**: No staging, committing, branching, or pushing. The only interactive action is copying a changed block to the clipboard.

## UI Layout

```
┌─────────────────────────────────────────────────────────┐
│  repo-a │ repo-b │ repo-c                    [S/U] [±M] │  ← tab bar + mode toggles
├─────────────────────────────────────────────────────────┤
│  ▼ src/main.rs  (+12 -3)                                │  ← collapsible file header
│  │  @@ -42,6 +42,15 @@                                  │
│  │   fn main() {                                         │
│  │ -     let old = thing();                               │
│  │ +     let new = better_thing();                        │
│  │ +     let also = another_thing();                      │
│  │   }                                                   │
│  ▶ src/lib.rs  (+1 -1)                          [collapsed] │
│  ▼ Cargo.toml  (+2 -0)                                  │
│  │  ...                                                  │
│                                                          │
│  ? new_file.txt  (untracked)                             │
├─────────────────────────────────────────────────────────┤
│  changes v0.1.0 │ watching 3 repos │ 14 files changed   │  ← status bar
└─────────────────────────────────────────────────────────┘
```

## Features

### 1. Repository Discovery

- On startup, scan the target directory for immediate child directories containing a `.git` folder.
- If the target directory itself is a git repo, use single-repo mode.
- Each discovered repo gets its own tab.

### 2. Tab Bar

- Tabs labeled with the repo directory name.
- Keyboard navigation: `1`–`9` for direct tab select, `Tab` / `Shift+Tab` to cycle.
- Active tab is visually highlighted.

### 3. Diff Modes (Toggle)

Three diff modes, cycled with a keybinding:

| Mode | What it shows |
|------|---------------|
| **Unstaged** | `git diff` — working tree vs index |
| **Staged** | `git diff --cached` — index vs HEAD |
| **Branch** | `git diff main...HEAD` or, if Graphite is detected, `git diff $(gt parent)...HEAD` |

The current mode is shown in the status bar. Keybinding: `s` (staged), `u` (unstaged), `b` (branch).

#### Graphite Detection

- Check if `gt` CLI is available on `$PATH`.
- If available, run `gt parent` to determine the parent branch for the current repo.
- Fall back to `main` (then `master`) if Graphite is not installed or the command fails.

### 4. Diff Display

#### Unified View (default)
- Standard unified diff format with syntax-highlighted code.
- Context lines shown in muted color; additions in green; deletions in red.
- Hunk headers (`@@`) shown as section dividers.

#### Side-by-Side View
- Left pane: old version. Right pane: new version.
- Aligned by hunk so corresponding lines sit at the same vertical position.
- Toggle between unified and side-by-side with `v`.

#### Syntax Highlighting
- Use `syntect` or `tree-sitter` for highlighting, applied to both old and new content.
- Language detected from file extension.
- Highlighting respects the terminal's color capabilities (truecolor preferred, 256-color fallback).

### 5. File List & Collapsing

- Files listed in order: modified (staged/unstaged), then untracked.
- Each file has a collapsible header showing the filename and a `+N -M` summary.
- `Enter` or click on a file header to toggle collapse.
- `c` to collapse all, `e` to expand all.
- Untracked files shown with a `?` prefix and their content displayed as an "all additions" diff.

### 6. Scrolling & Navigation

- `j` / `k` or arrow keys to scroll line-by-line.
- `J` / `K` to jump between file headers.
- `g` / `G` for top / bottom.
- Mouse scroll supported.

### 7. Click-to-Copy

- Double-click (or keybinding `y`) on a line within a hunk copies the entire changed block (contiguous added/removed lines) to the system clipboard.
- The copied text is prefixed with a header:

```
// src/main.rs:42-48
+     let new = better_thing();
+     let also = another_thing();
```

- Visual flash feedback on the copied region.

### 8. Live Watching

- Use filesystem events (via `notify` crate) to detect changes to tracked and untracked files within each repo.
- On change detection, re-run the relevant `git diff` and update only the affected repo tab.
- Debounce rapid changes (50ms window) to avoid thrashing.

### 9. Status Bar

- Shows: app version, number of repos watched, total changed file count across all repos.
- Shows current diff mode (Unstaged / Staged / Branch).
- Shows errors inline (e.g. "repo-b: git error") without crashing.

## Keybindings Summary

| Key | Action |
|-----|--------|
| `1`–`9` | Switch to repo tab N |
| `Tab` / `Shift+Tab` | Cycle tabs |
| `u` | Unstaged diff mode |
| `s` | Staged diff mode |
| `b` | Branch diff mode |
| `v` | Toggle unified / side-by-side |
| `Enter` | Toggle collapse on focused file |
| `c` | Collapse all files |
| `e` | Expand all files |
| `j` / `k` / `↑` / `↓` | Scroll |
| `J` / `K` | Jump to prev / next file |
| `g` / `G` | Top / bottom |
| `y` | Copy focused hunk to clipboard |
| `q` / `Esc` | Quit |

## Tech Stack

| Concern | Crate | Version | Why this crate |
|---------|-------|---------|----------------|
| TUI framework | `ratatui` | 0.30 | Industry-standard Rust TUI. 2700+ crates built on it. |
| Terminal backend | `crossterm` | 0.29 | Default backend for ratatui. Cross-platform terminal manipulation, mouse events, raw mode. |
| Async runtime | `tokio` | 1.x | Needed for async channels between watcher thread and UI loop. |
| File watching | `notify` | 8.x | OS-native filesystem events (FSEvents on macOS). |
| Watch debouncing | `notify-debouncer-mini` | 0.7 | Recommended companion to `notify` — coalesces rapid fs events. |
| Git operations | `git2` | 0.20 | libgit2 bindings. Compute diffs in-process without shelling out to `git`. |
| Syntax highlighting | `syntect` | 5.x | Sublime Text syntax definitions. Language detection by extension, truecolor + 256-color output. |
| Clipboard | `arboard` | 3.x | OS clipboard read/write (maintained by 1Password). |
| Diff alignment | `similar` | 2.x | Computes unified/inline diffs programmatically for side-by-side alignment. |
| CLI args | `clap` | 4.x (derive) | Derive-based arg parsing. |
| Error handling | `anyhow` | 1.x | Ergonomic error propagation. |
| Text measurement | `unicode-width` | 0.2 | Correct terminal column widths for Unicode. |

### Ratatui Widgets Used

| Widget | Purpose |
|--------|---------|
| **Tabs** | Repo tab bar across the top. Handles selection highlighting. |
| **Paragraph** | Renders styled diff lines (syntax-highlighted, colored additions/deletions). Each file's diff content is a Paragraph with pre-computed `Line`/`Span` styles. |
| **Block** | Wraps each section (tab bar, diff area, status bar) with borders and titles. Also wraps collapsible file headers. |
| **Scrollbar** | Vertical scrollbar in the diff area for long diffs. Tracks scroll position relative to total content height. |
| **List** | File list in the left gutter (if we move to a two-pane layout). Initially, files and diffs are interleaved in a single scrollable Paragraph. |

### Ratatui Features Used

| Feature | Purpose |
|---------|---------|
| `crossterm` (default) | Backend for terminal I/O. |
| `underline-color` (default) | Enables colored underlines for richer styling. |

### Crossterm Features Used

| Feature | Purpose |
|---------|---------|
| `event-stream` | Async `EventStream` for non-blocking keyboard/mouse input in the tokio event loop. |
| Mouse capture | `EnableMouseCapture` / `DisableMouseCapture` for scroll wheel and double-click support. |
| Raw mode | `enable_raw_mode` for character-by-character input (no line buffering). |
| Alternate screen | `EnterAlternateScreen` so the TUI doesn't clobber the user's terminal history. |

### Syntect Usage

- Load `ThemeSet::load_defaults()` and `SyntaxSet::load_defaults_newlines()` at startup.
- Detect syntax from file extension via `SyntaxSet::find_syntax_for_file()`.
- Highlight each line with `HighlightLines::highlight_line()`.
- Convert syntect `Style` → ratatui `Style` (foreground color mapping, truecolor when available, 256-color fallback).
- Apply highlighting to both context lines and changed lines. Additions/deletions get a green/red background tint _on top of_ the syntax colors.

### git2 Usage

- `Repository::open()` to open each discovered repo.
- `repo.diff_index_to_workdir()` for unstaged changes.
- `repo.diff_tree_to_index()` for staged changes (comparing HEAD tree to index).
- `repo.diff_tree_to_tree()` for branch diffs (comparing merge-base of main/parent to HEAD).
- `repo.merge_base()` to find the common ancestor for branch diffs.
- `repo.statuses()` for the file-level summary (modified, staged, untracked).
- Diff iteration via `diff.foreach()` or `diff.print()` to extract hunks and lines.
- For untracked files: read file content directly and present as an all-additions diff.

### notify + debouncer Usage

- Create a `notify_debouncer_mini::new_debouncer()` with a 50ms timeout.
- Watch each repo's working directory recursively (`RecursiveMode::Recursive`).
- Debounced events sent over a `std::sync::mpsc` channel.
- On receiving events, determine which repo was affected by path prefix matching.
- Re-run the git diff for only the affected repo and update that tab's state.

### similar Usage

- `similar::TextDiff::from_lines()` to compute diffs for side-by-side view.
- Iterate `TextDiff::ops()` to get `DiffOp` (Equal, Delete, Insert, Replace) chunks.
- For side-by-side: align old/new lines by op, inserting blank padding lines where one side has additions/deletions the other doesn't.
- For unified: use the ops to build the standard `+`/`-`/` ` line output with hunk headers.

### arboard Usage

- `arboard::Clipboard::new()` to get clipboard handle.
- On copy action (`y` or double-click): build a string with file path + line range header, then the changed lines, and call `clipboard.set_text()`.

### Architecture: Event Loop

The application runs a single-threaded tokio runtime with a main loop that `select!`s over:

1. **Crossterm events** (via `crossterm::event::EventStream`) — keyboard and mouse input.
2. **File watcher events** (via `tokio::sync::mpsc` channel bridged from the debouncer's std channel) — triggers diff recomputation.
3. **Tick timer** (200ms) — for UI animations like the copy-flash feedback.

Each iteration: process event → update app state → render frame.

## CLI Interface

```
changes [PATH]
```

- `PATH` defaults to `.` (current directory).
- If `PATH` is a git repo, single-repo mode.
- If `PATH` contains git repos as immediate children, multi-repo mode.
- If `PATH` is neither, exit with an error.

## Non-Goals

- Staging / unstaging files or hunks.
- Committing, pushing, pulling, or any write git operations.
- Branch switching or creation.
- Merge conflict resolution.
- Remote operations.
- Log / history browsing.

## Future Considerations (Out of Scope for v1)

- Configurable keybindings.
- Custom themes.
- Deeper repo discovery (recursive).
- Integration with other diff tools.
- Filtering files by pattern.
