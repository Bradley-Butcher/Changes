# Hunk Comments

## Problem

You're reviewing agent changes across multiple repos. You spot issues in different files and hunks. You want to annotate as you go, then dump all your feedback to the agent in one shot.

## Flow

1. Scroll through the diff, reading code
2. See something worth commenting on — **right-click the hunk**
3. A comment box pops up anchored to that hunk. Type your note.
4. Press `Ctrl+D` or click outside the box to save and close
5. Keep scrolling, right-click more hunks, add more comments
6. When satisfied, press `Y` to copy all commented hunks + comments to clipboard
7. Paste into your agent

At any point you can press `C` to open the comments browser — see all your annotations, filter them, jump to any one, select which ones to copy, or delete ones you don't want.

---

## Adding a Comment (Right-Click)

Right-click anywhere on a hunk (hunk header, diff lines, or comment row if one already exists). A floating comment box appears anchored to the hunk header:

```
  @@ -115,7 +115,12 @@ impl App {
  ┌──────────────────────────────────────────────┐
  │ > _                                          │
  │                                              │
  │                                              │
  └──────────────────────────────────────────────┘
      if let Some(idx) = app.focused_file {
  +       file.collapsed = !file.collapsed;
```

- Positioned directly below the hunk header, overlaying diff content
- Multi-line: `Enter` inserts a newline, `Ctrl+D` saves and closes, `Esc` cancels
- 3 lines tall by default, grows up to 6 as you type
- If the hunk already has a comment, opens pre-filled for editing
- If the hunk header is near the bottom of the screen, the box flips above it
- Amber/yellow border and text throughout

Right-click resolves which hunk unambiguously from the click position — no "which hunk am I focused on" guessing. Uses `hunk_at_row()` on the content row under the cursor.

### Keyboard alternative

`n` also opens the comment box for the hunk at current scroll position (for keyboard-only users). Same box, same behavior.

---

## Inline Display

Saved comments appear as rows below the hunk header:

```
  @@ -115,7 +115,12 @@ impl App {               [!]
  ┃ use toggle_collapsed instead of manual
  ┃ invalidation — the method already handles this
      if let Some(idx) = app.focused_file {
  +       file.collapsed = !file.collapsed;
```

- `[!]` badge on the hunk header, right-aligned, amber — scannable at a glance
- Comment rows: `┃` prefix in amber, content in amber foreground, word-wrapped to terminal width. No truncation.
- Real `RowRef::Comment` rows in the layout — scroll naturally, participate in viewport math
- In side-by-side mode, comment rows span both panes (full width)
- Right-clicking a comment row opens it for editing (same as right-clicking the hunk)

---

## Comments Browser

Press `C` to open a popup overlay:

```
┌─ Review notes (3) ── type to filter ────────────────┐
│  > _                                                 │
│                                                      │
│  [x] src/app/mod.rs @115                             │
│      use toggle_collapsed instead of manual          │
│      invalidation — the method already handles this  │
│                                                      │
│  [x] src/ui.rs @450                                  │
│      hardcoded color — use BG_ADD_EMPH constant      │
│                                                      │
│  [ ] src/diff.rs @22                                 │
│      this clone is unnecessary, borrow instead       │
│                                                      │
│            ↵ jump  y copy selected  d del  esc close │
└──────────────────────────────────────────────────────┘
```

- All comments across all files in the active repo
- Each comment: file + line header, full text below (not truncated)
- **Checkboxes** — `Space` to toggle. All checked by default when opened.
- Type to filter (fuzzy match on file name + comment text)
- `Up` / `Down` to navigate between comments
- `Enter` — jump to the selected comment's hunk (scroll, uncollapse, close popup)
- `y` — copy only the **checked** comments + their hunks to clipboard
- `d` — delete selected comment
- `Esc` — close

This gives you selective copy. If you added 8 comments but only want to send 5 to your agent right now, uncheck 3 and press `y`.

---

## Copy Format

`Y` (from the diff view) copies **all** comments. `y` (from the browser) copies **checked** comments.

Both produce the same markdown format:

```markdown
## Review comments

### src/app/mod.rs:115-127
> use toggle_collapsed instead of manual
> invalidation — the method already handles this

\`\`\`diff
   if let Some(idx) = app.focused_file {
+      file.collapsed = !file.collapsed;
+      app.invalidate_layouts(self.active_tab);
\`\`\`

### src/ui.rs:450-462
> hardcoded color — use BG_ADD_EMPH constant

\`\`\`diff
-  Style::default().bg(Color::Rgb(40, 90, 40))
+  Style::default().bg(BG_ADD_EMPH)
\`\`\`
```

- Heading: `file:line-range`
- Blockquote: full comment (multi-line preserved)
- Diff block: hunk content
- Ordered by file path, then position
- After copying, flash all included hunks briefly as confirmation

---

## Clearing

| Key | Action |
|-----|--------|
| `N` | Remove comment from current hunk |
| `D` | Clear all comments in active repo |

`D` shows brief status bar confirmation: "Cleared 5 notes"

---

## All Keybindings

### Diff view
| Key | Action |
|-----|--------|
| Right-click hunk | Add/edit comment (floating input) |
| `n` | Add/edit comment on current hunk (keyboard alternative) |
| `N` | Remove comment from current hunk |
| `C` | Open comments browser |
| `Y` | Copy all commented hunks to clipboard |
| `D` | Clear all comments |

### Comment input
| Key | Action |
|-----|--------|
| `Enter` | New line |
| `Ctrl+D` | Save and close |
| `Esc` | Cancel |

### Comments browser
| Key | Action |
|-----|--------|
| `Up` / `Down` | Navigate between comments |
| `Space` | Toggle checkbox |
| `Enter` | Jump to hunk |
| `y` | Copy checked comments to clipboard |
| `d` | Delete selected comment |
| Type | Filter |
| `Esc` | Close |

---

## Data Model

```rust
pub struct HunkComment {
    pub file_idx: usize,
    pub hunk_idx: usize,
    pub text: String,
}

pub struct CommentInputState {
    pub file_idx: usize,
    pub hunk_idx: usize,
    pub text: String,
    pub cursor_line: usize,
    pub cursor_col: usize,
    /// Hunk header row in layout, for positioning the floating box
    pub anchor_row: usize,
}

pub struct CommentBrowserState {
    pub query: String,
    pub selected: usize,
    pub checked: HashSet<usize>,
}
```

Comments live on `RepoState`:

```rust
pub struct RepoState {
    // ... existing fields ...
    pub comments: Vec<HunkComment>,
}
```

UI state on `App`:

```rust
pub struct App {
    // ... existing fields ...
    pub comment_input: Option<CommentInputState>,
    pub comment_browser: Option<CommentBrowserState>,
}
```

Clear comments in `apply_diff_result`. Close `comment_input` on diff refresh.

---

## Layout

New `RowRef` variant:

```rust
RowRef::Comment {
    file_idx: usize,
    hunk_idx: usize,
    line_idx: usize,  // which line of the wrapped comment
}
```

`DiffLayout::build` takes a `&[HunkComment]` parameter. After emitting a `HunkHeader`, checks for a matching comment and emits `Comment` rows (one per wrapped line).

Adding/removing comments invalidates layouts.

---

## Rendering

### Floating input
- Overlay anchored to hunk header's screen position
- Amber border, amber text, visible cursor
- If anchor is near screen bottom, flip above the hunk header
- If anchor scrolls off-screen while input is open, pin to top/bottom edge

### Comment rows
- `┃` prefix in amber, matching line number gutter width
- Content in amber, word-wrapped, no background
- Multiple rows for multi-line comments

### Hunk header badge
- `[!]` right-aligned on hunk header, amber
- Only when comment exists

### Comments browser
- Popup overlay (same pattern as file picker)
- Amber accent
- Checkboxes: `[x]` / `[ ]` before each comment group
- All checked by default on open
- Hint bar at bottom

---

## Edge Cases

- **Right-click on non-hunk area** (file header, gap, blank): does nothing.
- **Right-click on collapsed file**: does nothing (hunks aren't visible).
- **Comment on hunk, then collapse file**: comment persists in data, shows in browser, included in copy. Not visible inline until expanded.
- **Diff refresh while input is open**: close input, clear all comments.
- **One comment per hunk**: right-click on already-commented hunk opens for edit.
- **Long comments in browser**: shown in full, browser scrolls. No truncation.
- **Browser opened with no comments**: shows "No review notes yet — right-click a hunk to add one".

---

## Implementation Plan

Eight steps, each builds on the previous. Every step ends with `cargo clippy && cargo test` passing.

### Step 1: Data model + structs

**Files:** `src/app/mod.rs`

Add the new types alongside the existing state structs:

```rust
pub struct HunkComment {
    pub file_idx: usize,
    pub hunk_idx: usize,
    pub text: String,
}

pub struct CommentInputState {
    pub file_idx: usize,
    pub hunk_idx: usize,
    pub text: String,
    pub cursor_line: usize,
    pub cursor_col: usize,
    /// Layout row of the hunk header — used to position the floating input
    pub anchor_row: usize,
}

pub struct CommentBrowserState {
    pub query: String,
    pub selected: usize,
    pub checked: std::collections::HashSet<usize>,
}
```

Add `comments: Vec<HunkComment>` to `RepoState` (next to `files`, `base_branch`, etc).

Add `comment_input: Option<CommentInputState>` and `comment_browser: Option<CommentBrowserState>` to `App` (next to `file_picker`, `repo_adder`).

Initialize both to `None` / `Vec::new()` in constructors.

Add helper methods on `App`:
- `find_comment(&self, file_idx, hunk_idx) -> Option<&HunkComment>` — linear scan of active repo's comments
- `find_comment_mut(&mut self, file_idx, hunk_idx) -> Option<&mut HunkComment>`
- `add_or_update_comment(&mut self, file_idx, hunk_idx, text)` — upsert + invalidate layouts
- `remove_comment(&mut self, file_idx, hunk_idx)` — remove + invalidate layouts
- `clear_comments(&mut self)` — clear all on active repo + invalidate layouts
- `format_comments_markdown(&self, indices: Option<&[usize]>) -> String` — format selected (or all) comments as the markdown copy payload. Reuses `copy_hunk` line-formatting logic.

Add `comments.clear()` to `apply_diff_result` and close `comment_input` on diff refresh.

### Step 2: RowRef::Comment + layout integration

**Files:** `src/viewport/row.rs`, `src/viewport/layout.rs`

Add new variant to `RowRef`:
```rust
Comment {
    file_idx: usize,
    hunk_idx: usize,
    line_idx: usize,
}
```

Update `RowRef::file_idx()` match to include `Comment`.

Change `DiffLayout::build` signature to accept comments:
```rust
pub fn build(files: &[FileDiff], view_kind: ViewKind, comments: &[HunkComment], content_width: usize) -> Self
```

`content_width` is needed to word-wrap comment text into the correct number of `Comment` rows. After emitting each `HunkHeader` row, check if `comments` contains a match for `(file_idx, hunk_idx)`. If so, word-wrap the comment text to `content_width - gutter_width` and emit one `Comment` row per wrapped line.

**Why wrap at layout time, not render time:** The viewport relies on 1:1 row-to-screen-line. If a single `Comment` row wraps to 3 screen lines at render time, `total_lines()` is wrong, scroll offsets break, and the scrollbar lies. Wrapping at layout time means the row count is accurate. Layout already gets rebuilt on resize (invalidated), so width changes are handled.

Update all call sites of `DiffLayout::build`:
- `ensure_layout` in `app/mod.rs` — pass `&repo.comments` and content width

Add `pub content_width: u16` to `LayoutHints`, populate it in `draw()` alongside `content_y`/`content_height` (from `inner_area.width`). `ensure_layout` uses `self.layout.content_width` for wrapping. On first render it'll be 0 — default to 80 if 0.

### Step 3: Render comment rows inline

**Files:** `src/ui.rs`

Add `FG_COMMENT: Color = Color::Rgb(220, 180, 60)` (amber) to the color constants.

In `draw_unified` and `draw_side_by_side`, add a match arm for `RowRef::Comment`:
- Unified: render as `┃ {comment text}` — `┃` padded to line number gutter width, amber foreground, no background.
- SBS: render the comment full-width on the left pane, empty on the right (or span both — render on left, right gets blank).

For `HunkHeader` rendering in both draw functions, check if the hunk has a comment (`app.repos[active_tab].comments` lookup). If so, append `[!]` badge to the hunk header spans, right-aligned, amber.

The badge needs the width of the render area to right-align. The hunk header already has access to the inner area width.

### Step 4: Right-click to open comment input

**Files:** `src/app/mouse.rs`

In `handle_mouse`, add a `MouseEventKind::Down(MouseButton::Right)` arm:
1. Compute `content_row` from click position (same math as left-click)
2. Call `hunk_at_row(content_row)` on the layout to resolve `(file_idx, hunk_idx)`. This already matches `HunkHeader`, `UnifiedLine`, and `SideBySideLine`. Update it to also match `RowRef::Comment` — a comment row belongs to its hunk, so right-clicking a comment should resolve to that hunk for editing.
3. If a hunk is found, find the anchor row: scan backward from `content_row` to find the `HunkHeader` row for this `(file_idx, hunk_idx)`. Store as `anchor_row` on the input state.
4. Pre-fill text if a comment already exists for this hunk
6. Set `app.comment_input = Some(CommentInputState { ... })`
7. Return `true` (needs redraw)

Need a new layout method: `fn hunk_header_row_for(&self, file_idx, hunk_idx) -> Option<usize>` that scans for the matching `HunkHeader` row index. Or — compute anchor_row from the click: walk backward from content_row to find the `HunkHeader`.

### Step 5: Comment input keyboard handling + rendering

**Files:** `src/app/keys.rs`, `src/runtime.rs`, `src/ui.rs`

**Key handling:**

In `runtime.rs`, add `comment_input` modal check before the existing file_picker/repo_adder checks:
```rust
if app.comment_input.is_some() {
    keys::handle_comment_input_key(app, key);
    needs_redraw = true;
    continue;
}
```

New function `handle_comment_input_key` in `keys.rs`:
- `Esc` → cancel, set `comment_input = None`
- `Ctrl+D` → save: call `app.add_or_update_comment(file_idx, hunk_idx, text)`, set `comment_input = None`
- `Enter` → insert `\n` into text, advance cursor
- `Backspace` → delete char before cursor (handle line joins)
- `Left` / `Right` / `Up` / `Down` → move cursor within text
- Any other `Char(c)` → insert at cursor position

Also add `n` in `handle_key` (main key handler) to open comment input via keyboard — same as right-click but uses `copy_hunk_at_focus` logic to find the target hunk.

**Rendering:**

In `draw()` in `ui.rs`, after rendering overlays, if `app.comment_input.is_some()`:

1. Compute screen position from `anchor_row`:
   - `screen_y = anchor_row - scroll_offset + content_y + 1` (the +1 puts it below the hunk header)
   - If `screen_y + box_height > content_y + content_height`, flip above: `screen_y = anchor_row - scroll_offset + content_y - box_height`
   - Clamp to content area bounds
2. Render a `Block` with amber borders at that position
3. Inside: render the text with a cursor (highlight cursor position or show `_`)
4. Box width: ~60% of terminal width or `min(60, area.width - 4)`
5. Box height: `min(max(3, line_count + 1), 6)`

### Step 6: `n`, `N`, `D`, `Y` keybindings in diff view

**Files:** `src/app/keys.rs`, `src/app/mod.rs`

In `handle_key`:

- `n` → open comment input for focused hunk (keyboard path, described in step 5)
- `N` → `app.remove_comment(file_idx, hunk_idx)` for the focused hunk. Resolve hunk the same way as `copy_hunk_at_focus`.
- `D` → `app.clear_comments()`. Show confirmation via `app.status_message` (new field, see below).
- `Y` → `let text = app.format_comments_markdown(None)` (all comments). If non-empty, copy to clipboard. Flash all commented hunks (see below).

**New `status_message` field:** Add `pub status_message: Option<(String, Instant)>` to `App`. Rendered in the status bar (replaces error when present). Auto-clears when `Instant::now() > until` in the tick handler. Used for "Cleared 5 notes", "Copied 3 notes", etc. This is not a hack on `last_error` — it's a dedicated field for transient user feedback.

**Multi-hunk flash:** Change `pub flash: Option<FlashState>` to `pub flash: Vec<FlashState>`. Update `is_hunk_flashing` to check `self.flash.iter().any(|f| ...)`. Update tick handler to drain expired entries. Update existing single-hunk flash in `copy_hunk` to push one entry. `Y` pushes an entry per commented hunk. The vec is empty 99% of the time — no performance concern.

### Step 7: Comments browser

**Files:** `src/app/keys.rs`, `src/ui.rs`, `src/app/mod.rs`

**Opening:**

`C` in `handle_key` → set `app.comment_browser = Some(CommentBrowserState { query: String::new(), selected: 0, checked: all_indices_set })` where `all_indices_set` is `(0..comments.len()).collect()`.

**Key handling:**

New `handle_comment_browser_key` in `keys.rs`, dispatched from `runtime.rs` before other modals:
- `Esc` → close (`comment_browser = None`)
- `Up` / `Down` → navigate `selected` (clamped to filtered list length)
- `Space` → toggle `checked` for selected index
- `Enter` → jump to hunk: get `(file_idx, hunk_idx)` from selected comment, uncollapse file if needed (`toggle_collapsed`), `jump_to_file`-style scroll, close browser
- `d` → remove selected comment from `repo.comments`, update selected index, if empty close browser
- `y` → `format_comments_markdown(Some(&checked_indices))`, copy to clipboard, close browser
- `Backspace` → pop from query
- `Char(c)` → push to query, reset selected to 0

**Filtering:**

`filtered_comment_indices(&self) -> Vec<usize>` on App — fuzzy match the query against `file_path + comment_text` for each comment in the active repo. Same pattern as `filtered_file_indices`.

**Rendering:**

`draw_comment_browser` in `ui.rs` — popup overlay, same structure as `draw_file_picker`:
- Centered popup, amber border, title "Review notes (N)"
- Input line with `> {query}_`
- Scrollable list of comment groups, each showing:
  - `[x]` or `[ ]` checkbox, file name, `@line`, amber header color
  - Comment text below, indented, normal color, full text (wraps within popup width)
- Selected group highlighted (amber background on header line)
- Hint bar at bottom: `↵ jump  y copy  ␣ toggle  d del  esc close`

Add to `draw()` overlay dispatch: `if app.comment_browser.is_some() { draw_comment_browser(frame, app); }`

### Step 8: Wire everything together + test

**Files:** `src/runtime.rs`, all files for final integration

- Ensure `comment_input` modal is checked first in the event loop (before `file_picker`, `repo_adder`)
- Ensure `comment_browser` modal is checked after `comment_input` but before `file_picker`
- Mouse right-click must not trigger when a modal is open (comment input, browser, file picker, repo adder, help)
- Diff refresh (`apply_diff_result`) clears comments and closes input/browser
- Tab switching: comments persist per-repo (already on `RepoState`), input/browser close on tab switch
- Resize: comment input recalculates position on next draw (anchor_row is stable, screen position is recomputed)

### Files touched (summary)

| File | Changes |
|------|---------|
| `src/app/mod.rs` | Structs, helper methods, `comments` on RepoState, `comment_input`/`comment_browser` on App, `format_comments_markdown`, clear on refresh |
| `src/app/keys.rs` | `handle_comment_input_key`, `handle_comment_browser_key`, `n`/`N`/`D`/`Y`/`C` in main handler |
| `src/app/mouse.rs` | Right-click handler |
| `src/viewport/row.rs` | `RowRef::Comment` variant, update `file_idx()` |
| `src/viewport/layout.rs` | `build()` signature change, comment row emission, word-wrap logic |
| `src/ui.rs` | `FG_COMMENT` color, comment row rendering in unified + SBS, `[!]` badge on hunk headers, floating input overlay, comment browser popup, `content_width` in LayoutHints |
| `src/runtime.rs` | Modal dispatch for comment_input and comment_browser |
