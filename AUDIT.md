# Code Audit — `changes`

Pre-open-source review. Severity: **Critical** > High > Medium > Low > Nit.

**Status:** All critical and high items resolved. Most medium items resolved.

---

## Architecture

### ~~HIGH: God struct — `App` holds everything~~ FIXED

Parallel vecs replaced with `Vec<RepoState>`. Click-target geometry extracted to `LayoutHints` struct returned from draw. `draw()` now takes `&App` (immutable) + `&mut LayoutHints`.

### ~~HIGH: Parallel Vec indexing is fragile~~ FIXED

Replaced with single `Vec<RepoState>` bundling all per-repo data.

### ~~MEDIUM: `run_loop` has a 10-parameter signature~~ FIXED

Channels bundled into `Channels` struct. Down from 10 params to 6.

### ~~MEDIUM: `ui.rs` draw functions take `&mut App`~~ FIXED

`draw()` now takes `&App` + `&mut LayoutHints`. Layout positions written to the hints struct, applied back to `app.layout` by the caller.

---

## Correctness

### ~~HIGH: Race condition in async diff results~~ FIXED

`DiffResult` and `BaseBranchResult` now use stable `u64` repo IDs instead of indices. Stale results for removed repos are silently discarded.

### ~~MEDIUM: `unwrap()` on RwLock~~ FIXED

All `unwrap()` calls on `RwLock` replaced with `.unwrap_or_else(|e| e.into_inner())`.

### MEDIUM: Hardcoded content area offset

```rust
let content_row = (click_row as usize).saturating_sub(4) + app.scroll_offset;
```

The magic number `4` assumes tab bar = 3 rows + border = 1. If the layout changes, click handlers break silently.

**Fix:** Store the content area `Rect` after layout and reference it for click mapping.

### LOW: `get_syntax` lifetime subtlety

Works correctly but the cache-drop-then-return pattern requires careful reading. Acceptable.

---

## Performance

### ~~MEDIUM: `ensure_sbs_cache()` called eagerly on every refresh~~ FIXED

SBS cache is now computed lazily — only when `side_by_side` is true. `ensure_sbs_caches()` is called once before each draw, and is a no-op when caches already exist or unified mode is active.

### MEDIUM: `file_header_positions()` is O(n) and called repeatedly

Called from multiple event handlers in the same frame. Worth caching and invalidating on diff refresh / collapse toggle.

### LOW: Highlight state reset per line

`HighlightLines::new()` per line means multi-line constructs may not highlight correctly. Creating one per file and feeding lines in order would be more correct.

### LOW: `total_new_lines` reads entire file for every changed file

Consider using `BufRead::lines().count()` or skipping binary files.

---

## Code Quality

### ~~HIGH: 1235-line `app.rs`~~ FIXED

Split into `app/mod.rs`, `app/keys.rs`, `app/mouse.rs`.

### ~~MEDIUM: No tests~~ FIXED

25 unit tests covering watcher path filtering, hunk context extraction, gap calculation, and fuzzy matching.

### MEDIUM: Error handling is inconsistent

`add_repo` returns `Result<usize, String>` (deliberate — user-facing messages). `DiffResult` uses `Result<_, String>` for thread-boundary ergonomics. Clipboard failures are silently ignored. Acceptable for v0.1 but worth revisiting.

### MEDIUM: All fields on `App` are `pub`

No encapsulation. Worth revisiting after v0.1 stabilizes the API surface.

### ~~LOW: `DiffMode::label()` allocates a String every call~~ FIXED

Now returns `Cow<'static, str>`. Static variants avoid allocation.

### ~~LOW: Magic numbers scattered~~ FIXED

Named constants: `FLASH_DURATION`, `FLASH_TICK`, `IDLE_TICK`, `SCROLL_SPEED`, `PAGE_SCROLL`, `DOUBLE_CLICK_MS`, `DEBOUNCE_DURATION`, `GRAPHITE_TIMEOUT`, `GRAPHITE_POLL_INTERVAL`, `EXPAND_AMOUNT`.

### ~~LOW: Dead code / `#[allow(dead_code)]`~~ FIXED

Removed unused `LineKind::HunkHeader` and `LineKind::FileHeader` variants. Removed incorrect `#[allow(dead_code)]` from `first_old_lineno` (it's actually used).

### LOW: `find_base_branch` busy-waits with 50ms sleep

Polling loop for `gt` subprocess. Pragmatic given the 2-second timeout. Named constant added.

---

## API / Public Interface

### MEDIUM: Minimal CLI help

The `about` is one line. No `long_about`, usage examples, or additional CLI flags (`--mode`, `--branch`, `--side-by-side`).

### LOW: No configuration file support

Theme, keybinds, scroll speed all hardcoded. Not blocking for v0.1.

---

## Dependencies

### ~~LOW: `tokio` full features~~ FIXED

Trimmed to `["rt-multi-thread", "macros", "time", "sync"]`.

### ~~LOW: Unused `unicode-width` dependency~~ FIXED (previous commit)

Removed.

---

## Tooling

### Added

- `rustfmt.toml` — edition 2024, max_width 100
- `clippy.toml` — threshold config
- `.github/workflows/ci.yml` — fmt, clippy, test, build, cross-compile checks
- `Makefile` — `make check`, `make fmt`, `make lint`, `make test`, `make release`, `make install`
- Zero clippy warnings across all targets
- `cargo fmt --check` passes clean

---

## Summary — Priority Order

1. ~~**Parallel Vec indexing**~~ FIXED
2. ~~**Async result race with repo removal**~~ FIXED
3. ~~**God struct**~~ FIXED (RepoState + LayoutHints extraction)
4. ~~**No tests**~~ FIXED (25 tests)
5. ~~**`app.rs` needs splitting**~~ FIXED (3 modules)
6. ~~**`run_loop` parameter count**~~ FIXED (Channels struct)
7. ~~**`draw` mutates App**~~ FIXED (LayoutHints)
8. **Inconsistent error handling** — acceptable for v0.1
9. ~~**Eager SBS cache**~~ FIXED
10. ~~**RwLock unwrap**~~ FIXED

**Remaining items for post-v0.1:** hardcoded content area offset, file_header_positions caching, per-file highlighter state, total_new_lines perf, pub field encapsulation, CLI flags, config file support.
