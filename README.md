# changes

A fast, live-updating terminal UI for visualizing git diffs. Watches your repo for changes and refreshes instantly — no need to re-run commands.

## Features

- **Live reload** — file watcher triggers instant diff refresh on save
- **Three diff modes** — unstaged (modified), staged, and branch diff (vs base branch)
- **Unified and side-by-side views** — toggle with `v`
- **Word-level inline diff** — highlights exactly which words changed within modified lines
- **Syntax highlighting** — language-aware coloring via syntect
- **Multi-repo support** — watch multiple repos in tabs, add/remove dynamically
- **Hunk expansion** — click gap indicators to reveal surrounding context
- **Fuzzy file picker** — press `f` to jump to any changed file
- **Graphite integration** — auto-detects parent branch via `gt parent`
- **Copy hunks** — press `y` to copy the focused hunk to clipboard
- **Collapse/expand** — toggle individual files or collapse/expand all

## Install

### Homebrew (macOS)

```sh
brew install Bradley-Butcher/tap/changes
```

### Pre-built binaries

Download from [GitHub Releases](https://github.com/Bradley-Butcher/Changes/releases):

```sh
# macOS (Apple Silicon)
curl -sSL https://github.com/Bradley-Butcher/Changes/releases/latest/download/changes-aarch64-apple-darwin.tar.gz | tar xz
sudo mv changes /usr/local/bin/

# macOS (Intel)
curl -sSL https://github.com/Bradley-Butcher/Changes/releases/latest/download/changes-x86_64-apple-darwin.tar.gz | tar xz
sudo mv changes /usr/local/bin/

# Linux (x86_64)
curl -sSL https://github.com/Bradley-Butcher/Changes/releases/latest/download/changes-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv changes /usr/local/bin/
```

### From source

```sh
cargo install --path .
```

## Usage

```sh
# Watch the current directory
changes

# Watch a specific repo
changes /path/to/repo

# Watch a directory containing multiple repos
changes /path/to/projects
```

## Keybindings

### Navigation

| Key | Action |
|-----|--------|
| `j` / `k` / `Up` / `Down` | Scroll line by line |
| `J` / `K` | Jump to previous / next file |
| `g` / `G` | Jump to top / bottom |
| `PgUp` / `PgDn` | Scroll by page |
| Mouse scroll | Scroll |

### Tabs

| Key | Action |
|-----|--------|
| `1`-`9` | Switch to tab N |
| `Tab` / `Shift+Tab` | Cycle tabs |
| Click tab | Switch to tab |

### Modes & Views

| Key | Action |
|-----|--------|
| `m` | Modified (unstaged) diff |
| `s` | Staged diff |
| `b` | Branch diff (vs base) |
| `v` | Toggle unified / side-by-side |

### Actions

| Key | Action |
|-----|--------|
| `a` | Add a repo to watch |
| `x` | Remove current repo tab |
| `f` | Fuzzy file picker |
| `Enter` / Click header | Toggle collapse file |
| `c` / `e` | Collapse / expand all files |
| `y` | Copy focused hunk to clipboard |
| `?` | Toggle help overlay |
| `q` / `Esc` | Quit |

## Development

```sh
# Run all checks (fmt, clippy, test, build)
make check

# Auto-fix formatting and lint issues
make fix

# Run tests only
make test

# Build release binary
make release
```

## License

MIT
