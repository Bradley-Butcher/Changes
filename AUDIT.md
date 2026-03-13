# Code Audit — `changes`

Pre-open-source review. Severity: **Critical** > High > Medium > Low > Nit.

---

## Architecture

### HIGH: God struct — `App` holds everything

`App` has 26+ public fields spanning unrelated concerns: UI state (scroll, focus, flash), repo management (repos, diff_modes, file_diffs), overlay state (file picker, repo adder), and click-target geometry (tab_positions, mode_badge_pos, status_bar_row).

Every function takes `&mut App`. There's no separation between model and view state. This makes it hard to reason about what changed after any operation.

**Fix:** Split into focused structs:
- `RepoState` — per-repo: diffs, mode, base branch, branch name
- `ViewState` — scroll, focus, side_by_side
- `OverlayState` — file picker, repo adder, help
- `ClickTargets` — tab_positions, badge positions (computed each frame, shouldn't be persisted on App at all)

### HIGH: Parallel Vec indexing is fragile

`repos`, `diff_modes`, `file_diffs`, `base_branches`, `branch_names` are five parallel `Vec`s indexed by the same `usize`. Adding or removing a repo requires touching all five in sync. The `remove` block (app.rs:726-733) is a bug magnet — miss one vec and everything shifts.

**Fix:** Single `Vec<RepoState>` where `RepoState` bundles all per-repo data.

### MEDIUM: `run_loop` has a 10-parameter signature

```rust
async fn run_loop(
    terminal, app, watch_rx, diff_rx, diff_tx,
    base_rx, base_tx, highlighter, debouncer, shared_paths,
) -> Result<()>
```

This is a sign that the function is doing too much. It's the event loop, the input handler, the watcher manager, and the repo lifecycle manager all in one.

**Fix:** Bundle channels into an `EventBus` struct. Move watcher/repo management into methods.

### MEDIUM: `ui.rs` draw functions take `&mut App`

`draw()` takes `&mut App` solely to write back click-target positions (`tab_positions`, `mode_badge_pos`, etc.). This breaks the expectation that rendering is read-only and makes the data flow confusing.

**Fix:** Return click targets from `draw()` as a separate struct, or compute them on-demand from layout rather than caching.

---

## Correctness

### HIGH: Race condition in async diff results

```rust
// app.rs:778
if result.mode == app.diff_modes[result.repo_index] {
    app.apply_diff_result(result.repo_index, result.result);
}
```

The mode check prevents stale diffs for the wrong mode, but doesn't guard against repo index shifting. If the user removes a repo tab while a background diff is in flight, `result.repo_index` may now point to a different repo (or panic on out-of-bounds). Same issue with `BaseBranchResult`.

**Fix:** Use a stable repo ID (e.g., `PathBuf` or a `u64` generation counter) instead of index-based addressing for async results.

### MEDIUM: `unwrap()` on RwLock

```rust
// watcher.rs:26
let repo_paths = repo_paths.read().unwrap();
// app.rs:575
for p in shared_paths.read().unwrap().iter() {
// app.rs:711
shared_paths.write().unwrap().push(path.clone());
```

If the watcher callback panics (e.g., channel closed), the RwLock gets poisoned and every subsequent `unwrap()` panics, taking down the whole TUI without restoring the terminal.

**Fix:** Use `.read().unwrap_or_else(|e| e.into_inner())` or handle poison explicitly.

### MEDIUM: Hardcoded content area offset

```rust
// app.rs:1098, 1140
let content_row = (click_row as usize).saturating_sub(4) + app.scroll_offset;
```

The magic number `4` assumes tab bar = 3 rows + border = 1. If the layout ever changes (e.g., a toolbar), every click handler breaks silently.

**Fix:** Store the content area `Rect` after layout and reference it for click mapping.

### LOW: `get_syntax` returns a reference with lifetime tied to `&self`, but the cache lookup drops the borrow

```rust
// highlight.rs:32-38
let cache = self.syntax_cache.borrow();
if let Some(name) = cache.get(&ext) {
    if let Some(syn) = self.syntax_set.find_syntax_by_name(name) {
        return syn;
    }
}
drop(cache);
```

This works because `find_syntax_by_name` returns a reference to `self.syntax_set` (not to the cache), but it's subtle. A reviewer would have to verify that.

---

## Performance

### MEDIUM: `ensure_sbs_cache()` called eagerly on every refresh

```rust
// app.rs:362
file.ensure_sbs_cache();
```

Side-by-side data is computed for every file on every refresh, even if the user is in unified mode and may never switch. For large diffs this is wasted work.

**Fix:** Compute lazily — only when `side_by_side` is true and the file is visible.

### MEDIUM: `file_header_positions()` is O(n) and called repeatedly

`file_header_positions()` walks all files every time it's called. It's called from `jump_to_file`, `focused_file_from_scroll`, `file_and_hunk_at_row`, `copy_hunk_at_focus`, `find_expand_gap`, and `handle_mouse`. Some of these are called from the same event handler, computing the same positions multiple times.

**Fix:** Cache positions, invalidate on diff refresh / collapse toggle.

### LOW: Highlight state reset per line

`HighlightLines::new()` is called once per `highlight_line_content()` call. syntect's highlighter is stateful — it tracks scope across lines for multi-line strings, comments, etc. Creating a fresh one per line means multi-line constructs may not highlight correctly, and there's overhead in re-initializing.

**Fix:** Create one `HighlightLines` per file and feed it lines in order.

### LOW: `total_new_lines` reads entire file for every file in every diff

```rust
// git.rs:267-271
for file in &mut files {
    let path = repo_path.join(&file.path);
    if let Ok(content) = std::fs::read_to_string(&path) {
        file.total_new_lines = content.lines().count();
    }
}
```

This reads every changed file from disk just to count lines. For large binary files this could be slow or produce garbage.

**Fix:** Skip binary files. Consider using `BufRead::lines().count()` to avoid loading the full file into memory, or get line count from the diff metadata.

---

## Code Quality

### HIGH: 1235-line `app.rs` mixing data, logic, and event handling

The file contains the App struct, all its methods, terminal setup/teardown, the async event loop, all keyboard handlers, all mouse handlers, and the expand-gap walker. This should be several files.

**Fix:** Split into:
- `app/state.rs` — App struct and data methods
- `app/event.rs` — event loop
- `app/keys.rs` — keyboard handlers
- `app/mouse.rs` — mouse handlers

### MEDIUM: No tests

Zero test files. The diff computation, gap calculation, fuzzy matching, hunk expansion, and base branch detection are all pure-ish functions that are straightforward to test.

**Fix:** At minimum, test `compute_side_by_side`, `gap_between_hunks`, `filtered_file_indices`, `find_expand_gap`, `hunk_context`, `is_git_internal_path`, and `expand_gap`.

### MEDIUM: Error handling is inconsistent

- `git.rs` uses `anyhow::Result` properly
- `app.rs` mixes `anyhow::Result`, `Result<_, String>`, and silent `let _ =` drops
- `add_repo` returns `Result<usize, String>` instead of using a proper error type
- Clipboard failures are silently ignored everywhere

**Fix:** Define an `AppError` enum or use `anyhow` consistently. At least log clipboard failures to `last_error`.

### MEDIUM: All fields on `App` are `pub`

Every field is public, so any code anywhere can mutate any state. There's no encapsulation or invariant enforcement.

**Fix:** Make fields private, expose through methods that maintain invariants (e.g., `set_active_tab()` that also resets scroll).

### LOW: `DiffMode::label()` allocates a String every call

```rust
pub fn label(&self, base_branch: Option<&str>) -> String {
    match self {
        DiffMode::Unstaged => "Modified".to_string(),
```

This allocates on every status bar render for static strings.

**Fix:** Return `Cow<'_, str>`.

### LOW: Magic numbers scattered

- `Duration::from_millis(300)` — debounce interval
- `Duration::from_millis(400)` — double-click threshold
- `Duration::from_millis(50)` — flash tick
- `Duration::from_secs(2)` — graphite timeout
- `EXPAND_AMOUNT: usize = 20`
- `3` — scroll speed for mouse wheel
- `20` — page scroll amount

**Fix:** Named constants at module level.

### LOW: `find_base_branch` busy-waits with 50ms sleep

```rust
// git.rs:309
std::thread::sleep(std::time::Duration::from_millis(50));
```

Polling loop for the `gt` subprocess. This is fine for a 2-second timeout but not elegant.

**Fix:** Use `child.wait_timeout()` if available, or accept this as pragmatic.

---

## API / Public Interface

### MEDIUM: No `--help` description for what the tool does

```rust
#[command(name = "changes", version, about = "Live git diff viewer")]
```

The `about` is minimal. No `long_about`, no usage examples, no description of features.

**Fix:** Add `long_about` with feature list. Consider adding `--mode`, `--branch`, `--side-by-side` CLI flags.

### LOW: No configuration file support

Theme colors, keybinds, scroll speed, default mode — all hardcoded. Users can't customize anything without editing source.

**Not blocking for v0.1** but worth noting for roadmap.

---

## Dependencies

### LOW: `tokio` full features for minimal async usage

```toml
tokio = { version = "1", features = ["full"] }
```

The only async usage is `mpsc` channels and `tokio::time::sleep`. The `"full"` feature pulls in the entire tokio runtime.

**Fix:** `features = ["rt", "macros", "time", "sync"]`

### LOW: `unicode-width` is listed but never used

```toml
unicode-width = "0.2"
```

No `use unicode_width` anywhere in the source.

**Fix:** Remove.

---

## Summary — Priority Order

1. **Parallel Vec indexing** — fragile, will cause bugs as features are added
2. **Async result race with repo removal** — can panic
3. **God struct** — blocks all future refactoring
4. **No tests** — blocking for credible open source release
5. **`app.rs` needs splitting** — 1200+ lines, unreviable
6. **`run_loop` parameter count** — symptom of missing structure
7. **`draw` mutates App** — confusing data flow
8. **Inconsistent error handling** — unprofessional for OSS
9. **Eager SBS cache** — unnecessary perf cost
10. **RwLock unwrap** — can cascade-crash

Items 1-5 should be fixed before release. Items 6-10 are recommended.
