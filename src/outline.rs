//! The "shape" of a diff: a directory tree of changed files and the declarations each
//! hunk adds, removes, or touches. This is the altitude a reviewer wants before reading
//! lines, and it is derived purely from the diff text — no language servers involved.

use crate::diff::{FileDiff, FileStatus, LineKind};
use crate::symbols::{DefKind, SymbolIndex};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolChange {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// As shown: `fn parse`, `class Widget`, `## Install`.
    pub name: String,
    pub change: SymbolChange,
    pub hunk_idx: usize,
    /// Bare identifier for function-like symbols, the key into the call index.
    pub ident: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDirection {
    /// A place that calls the symbol.
    Incoming,
    /// Something the symbol calls.
    Outgoing,
}

/// One rendered row of the outline. `depth` is the tree nesting level for indentation and
/// `branch` is the connector drawn before the label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutlineRow {
    Dir {
        prefix: String,
        name: String,
        additions: usize,
        deletions: usize,
    },
    File {
        prefix: String,
        file_idx: usize,
        name: String,
        status: FileStatus,
        additions: usize,
        deletions: usize,
    },
    Symbol {
        prefix: String,
        file_idx: usize,
        path: String,
        symbol: Symbol,
    },
    /// Stands in for declarations beyond `MAX_SYMBOLS_PER_FILE`.
    More {
        prefix: String,
        file_idx: usize,
        count: usize,
    },
    /// Caller / callee counts under a function symbol; Enter expands it.
    Summary {
        prefix: String,
        file_idx: usize,
        path: String,
        ident: String,
        callers: usize,
        callees: usize,
        warning: Option<String>,
        expanded: bool,
    },
    /// One caller or callee of an expanded symbol.
    Call {
        prefix: String,
        direction: CallDirection,
        /// Qualified function name, e.g. `App::open_outline`.
        name: String,
        /// `path:line`, for display and for the "outside this diff" message.
        location: String,
        /// How this end of the edge relates to the diff, if at all.
        mark: Option<SymbolChange>,
        /// Set when the location is inside the diff and can be jumped to.
        file_idx: Option<usize>,
        hunk_idx: Option<usize>,
        /// Owning symbol, so collapse works from any row of the tree.
        parent: (usize, String),
        parent_path: String,
    },
}

/// Callers or callees listed per direction before "… N more".
pub const MAX_CALLS_SHOWN: usize = 8;

/// Long new files declare dozens of items; past this many the list stops adding signal.
pub const MAX_SYMBOLS_PER_FILE: usize = 12;

impl OutlineRow {
    /// Rows the cursor can land on; directories are grouping only.
    pub fn is_selectable(&self) -> bool {
        !matches!(self, OutlineRow::Dir { .. })
    }

    pub fn target(&self) -> Option<(usize, Option<usize>)> {
        match self {
            OutlineRow::Dir { .. } => None,
            OutlineRow::File { file_idx, .. } => Some((*file_idx, None)),
            OutlineRow::Symbol {
                file_idx, symbol, ..
            } => Some((*file_idx, Some(symbol.hunk_idx))),
            OutlineRow::More { file_idx, .. } => Some((*file_idx, None)),
            OutlineRow::Summary { .. } => None,
            OutlineRow::Call {
                file_idx, hunk_idx, ..
            } => file_idx.map(|file_idx| (file_idx, *hunk_idx)),
        }
    }

    /// The (file index, identifier) of the function symbol this row belongs to.
    pub fn symbol_index_key(&self) -> Option<(usize, &str)> {
        match self {
            OutlineRow::Symbol {
                file_idx, symbol, ..
            } => symbol.ident.as_deref().map(|ident| (*file_idx, ident)),
            OutlineRow::Summary {
                file_idx, ident, ..
            } => Some((*file_idx, ident.as_str())),
            OutlineRow::Call { parent, .. } => Some((parent.0, parent.1.as_str())),
            _ => None,
        }
    }

    /// Like `symbol_index_key` but keyed by path, stable across diff refreshes.
    pub fn symbol_key(&self) -> Option<(&str, &str)> {
        match self {
            OutlineRow::Symbol { path, symbol, .. } => {
                symbol.ident.as_deref().map(|ident| (path.as_str(), ident))
            }
            OutlineRow::Summary { path, ident, .. } => Some((path.as_str(), ident.as_str())),
            OutlineRow::Call {
                parent_path,
                parent,
                ..
            } => Some((parent_path.as_str(), parent.1.as_str())),
            _ => None,
        }
    }
}

/// Build the outline rows for a set of files. With a symbol index, function symbols get a
/// summary of their callers and callees, expanded for keys in `expanded`.
pub fn build_outline(
    files: &[FileDiff],
    index: Option<&SymbolIndex>,
    expanded: &HashSet<(String, String)>,
) -> Vec<OutlineRow> {
    let mut root = DirNode::default();
    for (file_idx, file) in files.iter().enumerate() {
        let mut parts: Vec<&str> = file.path.split('/').collect();
        let name = parts.pop().unwrap_or("").to_string();
        let mut node = &mut root;
        for part in parts {
            node = node.dirs.entry(part.to_string()).or_default();
        }
        node.files.push((name, file_idx));
    }
    root.collapse_chains();

    let context = Context {
        files,
        index,
        expanded,
    };
    let mut rows = Vec::new();
    render_dir(&root, &context, "", true, &mut rows);
    rows
}

struct Context<'a> {
    files: &'a [FileDiff],
    index: Option<&'a SymbolIndex>,
    expanded: &'a HashSet<(String, String)>,
}

/// Markdown rendering of the outline, for pasting into an agent prompt or PR body.
pub fn outline_markdown(rows: &[OutlineRow]) -> String {
    let mut out = String::from("## Change outline\n\n```text\n");
    for row in rows {
        match row {
            OutlineRow::Dir {
                prefix,
                name,
                additions,
                deletions,
            } => {
                out.push_str(&format!("{prefix}{name}/  +{additions} -{deletions}\n"));
            }
            OutlineRow::File {
                prefix,
                name,
                status,
                additions,
                deletions,
                ..
            } => {
                out.push_str(&format!(
                    "{prefix}{} {name}  +{additions} -{deletions}\n",
                    status_glyph(*status)
                ));
            }
            OutlineRow::Symbol { prefix, symbol, .. } => {
                out.push_str(&format!(
                    "{prefix}{} {}\n",
                    change_glyph(symbol.change),
                    symbol.name
                ));
            }
            OutlineRow::More { prefix, count, .. } => {
                out.push_str(&format!("{prefix}… {count} more\n"));
            }
            OutlineRow::Summary {
                prefix,
                callers,
                callees,
                warning,
                ..
            } => {
                let text = match warning {
                    Some(warning) => format!("⚠ {warning}"),
                    None => format!("called by {callers} · calls {callees}"),
                };
                out.push_str(&format!("{prefix}{text}\n"));
            }
            OutlineRow::Call {
                prefix,
                direction,
                name,
                location,
                mark,
                ..
            } => {
                let arrow = match direction {
                    CallDirection::Incoming => "←",
                    CallDirection::Outgoing => "→",
                };
                let mark = mark
                    .map(|m| format!("{} ", change_glyph(m)))
                    .unwrap_or_default();
                out.push_str(&format!("{prefix}{arrow} {mark}{name}  {location}\n"));
            }
        }
    }
    out.push_str("```\n");
    out
}

pub fn status_glyph(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Modified => "M",
        FileStatus::Added => "A",
        FileStatus::Deleted => "D",
        FileStatus::Renamed => "R",
        FileStatus::Untracked => "?",
    }
}

pub fn change_glyph(change: SymbolChange) -> &'static str {
    match change {
        SymbolChange::Added => "+",
        SymbolChange::Removed => "-",
        SymbolChange::Modified => "~",
    }
}

#[derive(Default)]
struct DirNode {
    dirs: BTreeMap<String, DirNode>,
    files: Vec<(String, usize)>,
}

impl DirNode {
    /// Merge `a/` containing only `b/` into `a/b/`, the way a shallow tree reads best.
    fn collapse_chains(&mut self) {
        let names: Vec<String> = self.dirs.keys().cloned().collect();
        for name in names {
            let mut child = self.dirs.remove(&name).expect("key from iteration");
            let mut merged = name;
            while child.files.is_empty() && child.dirs.len() == 1 {
                let (sub_name, sub) = child.dirs.pop_first().expect("exactly one child");
                merged = format!("{merged}/{sub_name}");
                child = sub;
            }
            child.collapse_chains();
            self.dirs.insert(merged, child);
        }
    }

    fn totals(&self, files: &[FileDiff]) -> (usize, usize) {
        let own = self.files.iter().fold((0, 0), |(a, d), (_, idx)| {
            let file = &files[*idx];
            (a + file.additions, d + file.deletions)
        });
        self.dirs.values().fold(own, |(a, d), dir| {
            let (da, dd) = dir.totals(files);
            (a + da, d + dd)
        })
    }
}

fn render_dir(
    node: &DirNode,
    context: &Context<'_>,
    prefix: &str,
    is_root: bool,
    rows: &mut Vec<OutlineRow>,
) {
    let files = context.files;
    let dir_count = node.dirs.len();
    let total = dir_count + node.files.len();
    for (position, (name, child)) in node.dirs.iter().enumerate() {
        let last = position + 1 == total;
        let (branch, child_prefix) = connectors(prefix, last, is_root);
        let (additions, deletions) = child.totals(files);
        rows.push(OutlineRow::Dir {
            prefix: branch,
            name: name.clone(),
            additions,
            deletions,
        });
        render_dir(child, context, &child_prefix, false, rows);
    }
    for (position, (name, file_idx)) in node.files.iter().enumerate() {
        let last = dir_count + position + 1 == total;
        let (branch, child_prefix) = connectors(prefix, last, is_root);
        let file = &files[*file_idx];
        rows.push(OutlineRow::File {
            prefix: branch,
            file_idx: *file_idx,
            name: name.clone(),
            status: file.status,
            additions: file.additions,
            deletions: file.deletions,
        });
        let mut symbols = file_symbols(file);
        let hidden = symbols.len().saturating_sub(MAX_SYMBOLS_PER_FILE);
        symbols.truncate(MAX_SYMBOLS_PER_FILE);
        let count = symbols.len() + usize::from(hidden > 0);
        for (position, symbol) in symbols.into_iter().enumerate() {
            let is_last = position + 1 == count;
            let (branch, symbol_prefix) = connectors(&child_prefix, is_last, false);
            let ident = symbol.ident.clone();
            let change = symbol.change;
            rows.push(OutlineRow::Symbol {
                prefix: branch,
                file_idx: *file_idx,
                path: file.path.clone(),
                symbol,
            });
            if let (Some(index), Some(ident)) = (context.index, ident) {
                push_call_rows(
                    rows,
                    context,
                    index,
                    *file_idx,
                    &ident,
                    change,
                    &symbol_prefix,
                );
            }
        }
        if hidden > 0 {
            let (branch, _) = connectors(&child_prefix, true, false);
            rows.push(OutlineRow::More {
                prefix: branch,
                file_idx: *file_idx,
                count: hidden,
            });
        }
    }
}

/// Summary line plus, when expanded, the callers and callees of one function symbol.
fn push_call_rows(
    rows: &mut Vec<OutlineRow>,
    context: &Context<'_>,
    index: &SymbolIndex,
    file_idx: usize,
    ident: &str,
    change: SymbolChange,
    prefix: &str,
) {
    let file = &context.files[file_idx];
    let Some(CallSummary {
        callers,
        callees,
        warning,
    }) = call_summary(index, file, ident, change)
    else {
        return; // unknown to the index (unsupported language, or only in the old tree)
    };
    let expanded = context
        .expanded
        .contains(&(file.path.clone(), ident.to_string()));
    let (branch, tree_prefix) = connectors(prefix, true, false);
    rows.push(OutlineRow::Summary {
        prefix: branch,
        file_idx,
        path: file.path.clone(),
        ident: ident.to_string(),
        callers: callers.len(),
        callees: callees.len(),
        warning,
        expanded,
    });
    if !expanded {
        return;
    }

    let parent = (file_idx, ident.to_string());
    let shown_callers = callers.len().min(MAX_CALLS_SHOWN);
    let shown_callees = callees.len().min(MAX_CALLS_SHOWN);
    let total = shown_callers
        + usize::from(callers.len() > shown_callers)
        + shown_callees
        + usize::from(callees.len() > shown_callees);
    let mut position = 0usize;
    let mut next_branch = |rows: &mut Vec<OutlineRow>, row: OutlineRow| {
        position += 1;
        let (branch, _) = connectors(&tree_prefix, position == total, false);
        rows.push(match row {
            OutlineRow::Call {
                direction,
                name,
                location,
                mark,
                file_idx,
                hunk_idx,
                parent,
                parent_path,
                ..
            } => OutlineRow::Call {
                prefix: branch,
                direction,
                name,
                location,
                mark,
                file_idx,
                hunk_idx,
                parent,
                parent_path,
            },
            OutlineRow::More {
                file_idx, count, ..
            } => OutlineRow::More {
                prefix: branch,
                file_idx,
                count,
            },
            other => other,
        });
    };

    for caller in callers.iter().take(shown_callers) {
        let (target_file, hunk_idx, mark) = locate_in_diff(
            context.files,
            &caller.path,
            caller.line,
            caller.from_name.as_deref(),
        );
        next_branch(
            rows,
            OutlineRow::Call {
                prefix: String::new(),
                direction: CallDirection::Incoming,
                name: caller
                    .from
                    .clone()
                    .unwrap_or_else(|| "(top level)".to_string()),
                location: format!("{}:{}", caller.path, caller.line),
                mark,
                file_idx: target_file,
                hunk_idx,
                parent: parent.clone(),
                parent_path: file.path.clone(),
            },
        );
    }
    if callers.len() > shown_callers {
        next_branch(
            rows,
            OutlineRow::More {
                prefix: String::new(),
                file_idx,
                count: callers.len() - shown_callers,
            },
        );
    }
    for (name, targets) in callees.iter().take(shown_callees) {
        let target = targets[0];
        let (target_file, hunk_idx, mark) =
            locate_in_diff(context.files, &target.path, target.line, Some(name));
        let mut display = target.display.clone();
        if targets.len() > 1 {
            display.push_str(&format!(" (+{} more definitions)", targets.len() - 1));
        }
        next_branch(
            rows,
            OutlineRow::Call {
                prefix: String::new(),
                direction: CallDirection::Outgoing,
                name: display,
                location: format!("{}:{}", target.path, target.line),
                mark,
                file_idx: target_file,
                hunk_idx,
                parent: parent.clone(),
                parent_path: file.path.clone(),
            },
        );
    }
    if callees.len() > shown_callees {
        next_branch(
            rows,
            OutlineRow::More {
                prefix: String::new(),
                file_idx,
                count: callees.len() - shown_callees,
            },
        );
    }
}

/// What the index knows about one changed function.
pub struct CallSummary<'a> {
    pub callers: Vec<crate::symbols::Caller>,
    pub callees: Vec<(String, Vec<&'a crate::symbols::Def>)>,
    pub warning: Option<String>,
}

/// Callers, callees and the review warning for a function symbol, or None when the
/// index has never seen it (unsupported language, or a name that only existed before).
pub fn call_summary<'a>(
    index: &'a SymbolIndex,
    file: &FileDiff,
    ident: &str,
    change: SymbolChange,
) -> Option<CallSummary<'a>> {
    // Removed functions have no definition left; their remaining callers still matter.
    let def = index
        .defs_named(ident, &file.path)
        .into_iter()
        .find(|def| def.kind == DefKind::Function)
        .cloned();
    let callers = index.callers(ident, &file.path, def.as_ref());
    let callees = def
        .as_ref()
        .map(|def| index.callees(def))
        .unwrap_or_default();
    if def.is_none() && callers.is_empty() {
        return None;
    }
    let warning = match change {
        SymbolChange::Added if callers.is_empty() && !is_entry_point(ident, &file.path) => {
            Some("no callers".to_string())
        }
        SymbolChange::Removed if !callers.is_empty() => {
            Some(format!("still called by {}", callers.len()))
        }
        _ => None,
    };
    Some(CallSummary {
        callers,
        callees,
        warning,
    })
}

/// One-line call context for a hunk, shown under its header in the diff:
/// `fn parse · ↑ called by main, other · ↓ calls tokenize, +2 more`. `label` names the
/// function when the hunk touches more than one. Fitted to `width` columns.
pub fn inline_call_context(summary: &CallSummary<'_>, label: Option<&str>, width: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(label) = label {
        parts.push(label.to_string());
    }
    if let Some(warning) = &summary.warning {
        parts.push(format!("⚠ {warning}"));
    }
    if !summary.callers.is_empty() {
        let names: Vec<String> = summary
            .callers
            .iter()
            .map(|c| c.from.clone().unwrap_or_else(|| "(top level)".to_string()))
            .collect();
        parts.push(format!("↑ called by {}", join_limited(&names, 4)));
    } else if summary.warning.is_none() {
        parts.push("↑ no callers".to_string());
    }
    if !summary.callees.is_empty() {
        let names: Vec<String> = summary
            .callees
            .iter()
            .map(|(_, targets)| targets[0].display.clone())
            .collect();
        parts.push(format!("↓ calls {}", join_limited(&names, 4)));
    }
    fit_width(&parts.join(" · "), width)
}

/// `a, b, c, +N more` — a deduplicated, capped list.
fn join_limited(names: &[String], max: usize) -> String {
    let mut unique: Vec<&String> = Vec::new();
    for name in names {
        if !unique.contains(&name) {
            unique.push(name);
        }
    }
    let shown: Vec<&str> = unique.iter().take(max).map(|s| s.as_str()).collect();
    let mut out = shown.join(", ");
    if unique.len() > max {
        out.push_str(&format!(", +{} more", unique.len() - max));
    }
    out
}

fn fit_width(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if width == 0 || UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Functions nobody is expected to call: program entry points and tests.
fn is_entry_point(ident: &str, path: &str) -> bool {
    ident == "main"
        || ident.starts_with("test_")
        || path.contains("/tests/")
        || path.starts_with("tests/")
        || path.ends_with("_test.go")
        || path.ends_with(".test.ts")
        || path.ends_with(".test.js")
        || path.ends_with(".spec.ts")
        || path.ends_with(".spec.js")
}

/// Where a `path:line` from the index sits in the diff: the file index and the hunk that
/// contains the line, plus how the diff touched it. `+` when that very line was added,
/// `~` when the enclosing function `ident` is one of the file's changed symbols.
fn locate_in_diff(
    files: &[FileDiff],
    path: &str,
    line: u32,
    ident: Option<&str>,
) -> (Option<usize>, Option<usize>, Option<SymbolChange>) {
    let Some((file_idx, file)) = files.iter().enumerate().find(|(_, f)| f.path == path) else {
        return (None, None, None);
    };
    let mut containing_hunk = None;
    let mut line_added = false;
    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        for diff_line in &hunk.lines {
            if diff_line.new_lineno == Some(line) {
                containing_hunk = Some(hunk_idx);
                line_added = diff_line.kind == LineKind::Addition;
            }
        }
    }
    let symbol_change = ident.and_then(|ident| {
        file_symbols(file)
            .into_iter()
            .find(|symbol| symbol.ident.as_deref() == Some(ident))
            .map(|symbol| (symbol.change, symbol.hunk_idx))
    });
    let mark = if line_added {
        Some(SymbolChange::Added)
    } else {
        symbol_change.map(|(change, _)| change)
    };
    let hunk_idx = containing_hunk.or(symbol_change.map(|(_, hunk_idx)| hunk_idx));
    (Some(file_idx), hunk_idx, mark)
}

/// Tree connectors: the branch drawn before this entry, and the prefix its children get.
fn connectors(prefix: &str, last: bool, is_root: bool) -> (String, String) {
    if is_root {
        return (String::new(), String::new());
    }
    if last {
        (format!("{prefix}└── "), format!("{prefix}    "))
    } else {
        (format!("{prefix}├── "), format!("{prefix}│   "))
    }
}

/// Declarations a file's hunks add, remove, or sit inside, in hunk order.
pub fn file_symbols(file: &FileDiff) -> Vec<Symbol> {
    // Whole-file additions or deletions: list what the file declares, all one kind.
    let whole_file = match file.status {
        FileStatus::Added | FileStatus::Untracked => Some(SymbolChange::Added),
        FileStatus::Deleted => Some(SymbolChange::Removed),
        _ => None,
    };

    let mut symbols: Vec<Symbol> = Vec::new();
    let mut push = |name: String, change: SymbolChange, hunk_idx: usize| {
        if let Some(existing) = symbols.iter_mut().find(|s| s.name == name) {
            // Removed then added in the same diff is a rewrite, not two events.
            if existing.change != change {
                existing.change = SymbolChange::Modified;
            }
            return;
        }
        let ident = function_ident(&name);
        symbols.push(Symbol {
            name,
            change,
            hunk_idx,
            ident,
        });
    };

    let markdown = is_markdown_path(&file.path);
    let symbol_name = |line: &str| {
        if markdown {
            heading_name(line)
        } else {
            declaration_name(line)
        }
    };

    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        let mut declared_here = false;
        // In prose, an unchanged heading inside the hunk tells us which section was
        // edited; code gets the same from git's hunk-header context instead.
        let mut enclosing_section: Option<String> = None;
        let mut seen_change = false;
        for line in &hunk.lines {
            let change = match (line.kind, whole_file) {
                (_, Some(change)) => change,
                (LineKind::Addition, None) => SymbolChange::Added,
                (LineKind::Deletion, None) => SymbolChange::Removed,
                (LineKind::Context, None) => {
                    if markdown
                        && !seen_change
                        && let Some(heading) = heading_name(&line.content)
                    {
                        enclosing_section = Some(heading);
                    }
                    continue;
                }
            };
            seen_change = true;
            if let Some(name) = symbol_name(&line.content) {
                push(name, change, hunk_idx);
                declared_here = true;
            }
        }
        if !declared_here
            && whole_file.is_none()
            && let Some(section) = enclosing_section
        {
            push(section, SymbolChange::Modified, hunk_idx);
            continue;
        }
        // A hunk that edits the body of something is a modification of the enclosing
        // declaration, which git names in the hunk header.
        if !declared_here
            && whole_file.is_none()
            && let Some(context) = hunk_context(&hunk.header)
            && let Some(name) = symbol_name(context).or_else(|| short_context(context))
            // `mod x;` / `use` lines are what git picks as context for import edits; they
            // are not the item being modified.
            && !name.starts_with("mod ")
            && !name.starts_with("module ")
            && !name.starts_with("use ")
        {
            push(name, SymbolChange::Modified, hunk_idx);
        }
    }
    symbols
}

/// The bare identifier of a function-like symbol name (`fn parse` → `parse`, Go method
/// `Start` → `Start`); None for types, impls, headings, and prose contexts.
fn function_ident(name: &str) -> Option<String> {
    match name.split_once(' ') {
        Some((keyword, rest)) => {
            matches!(keyword, "fn" | "def" | "func" | "function" | "macro_rules!")
                .then(|| rest.to_string())
        }
        None if name.starts_with('#') || name.contains('…') => None,
        None => Some(name.to_string()),
    }
}

fn is_markdown_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".mdx")
}

/// ATX headings are the declarations of a markdown file: `## Install` stays as written,
/// level included, so the outline reads like the document's table of contents.
pub fn heading_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.bytes().take_while(|&b| b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    if !rest.starts_with([' ', '\t']) {
        return None; // `#hashtag`, not a heading
    }
    let text = rest.trim().trim_end_matches('#').trim();
    if text.is_empty() {
        return None;
    }
    Some(format!("{} {text}", "#".repeat(level)))
}

/// The function context git appends after the second `@@` of a hunk header.
pub fn hunk_context(header: &str) -> Option<&str> {
    let rest = header.strip_prefix("@@")?;
    let end = rest.find("@@")?;
    let after = rest[end + 2..].trim();
    if after.is_empty() { None } else { Some(after) }
}

/// Fallback when the context line is not a recognisable declaration: keep it short.
fn short_context(context: &str) -> Option<String> {
    let trimmed = context.trim().trim_end_matches(['{', ':', '(']).trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut words = trimmed.split_whitespace();
    let short: Vec<&str> = words.by_ref().take(4).collect();
    let mut out = short.join(" ");
    if words.next().is_some() {
        out.push('…');
    }
    Some(out)
}

const DECL_MODIFIERS: &[&str] = &[
    "pub",
    "pub(crate)",
    "pub(super)",
    "export",
    "default",
    "async",
    "static",
    "unsafe",
    "const",
    "extern",
    "abstract",
    "final",
    "override",
    "private",
    "protected",
    "public",
    "declare",
    "inline",
    "virtual",
    "@",
];

const DECL_KEYWORDS: &[&str] = &[
    "fn",
    "func",
    "function",
    "def",
    "class",
    "struct",
    "enum",
    "trait",
    "interface",
    "impl",
    "type",
    "mod",
    "module",
    "macro_rules!",
    "protocol",
    "object",
    "record",
];

/// Name of the declaration on a source line, if the line starts one. Language-agnostic:
/// strips visibility and other modifiers, then looks for a declaration keyword followed by
/// an identifier. `impl Foo for Bar` yields `impl Foo for Bar`; `func (r *T) Name` yields
/// `Name`; `const foo = (` and `foo = function` in JavaScript yield `foo`.
pub fn declaration_name(line: &str) -> Option<String> {
    let mut rest = line.trim_start();
    // Only top-level and lightly indented declarations; deeply nested lines are locals.
    if line.len() - rest.len() > 8 {
        return None;
    }
    loop {
        let word = rest.split_whitespace().next()?;
        // `const` introduces a JavaScript binding as well as a Rust constant; decide
        // based on what follows rather than treating it as a modifier.
        if matches!(word, "const" | "let" | "var")
            && let Some(binding) = javascript_binding(rest)
        {
            return Some(binding);
        }
        if DECL_MODIFIERS.contains(&word) || word.starts_with("pub(") || word.starts_with('@') {
            rest = rest[word.len()..].trim_start();
            continue;
        }
        break;
    }
    // Generics may sit directly on the keyword: `impl<T> Foo for Bar<T>`.
    let keyword_end = rest
        .find(|c: char| c.is_whitespace() || c == '<')
        .unwrap_or(rest.len());
    let (keyword, after) = rest.split_at(keyword_end);
    if !DECL_KEYWORDS.contains(&keyword) {
        return None;
    }
    match keyword {
        "impl" => {
            let body = after.split(['{', ';']).next()?.trim();
            let body = strip_generics(body);
            (!body.is_empty()).then(|| format!("impl {body}"))
        }
        _ if after.starts_with('<') => None, // `fn<` is never a declaration
        "func" if after.trim_start().starts_with('(') => {
            // Go method: func (r *Receiver) Name(
            let close = after.find(')')?;
            let name = identifier(after[close + 1..].trim_start())?;
            Some(name.to_string())
        }
        _ => {
            let name = identifier(after.trim_start())?;
            // `type Alias = ...` in TypeScript vs `type` as a variable word in prose.
            (!name.is_empty()).then(|| format!("{keyword} {name}"))
        }
    }
}

/// `const name = (…) =>`, `let name = function`, `name: function(` and `name(…) {` methods.
fn javascript_binding(rest: &str) -> Option<String> {
    let (keyword, after) = rest.split_once(char::is_whitespace)?;
    if !matches!(keyword, "const" | "let" | "var") {
        return None;
    }
    let after = after.trim_start();
    let name = identifier(after)?;
    let tail = after[name.len()..].trim_start();
    let tail = tail.strip_prefix('=')?.trim_start();
    let is_function = tail.starts_with("function")
        || tail.starts_with("async")
        || (tail.starts_with('(') && tail.contains("=>"))
        || tail
            .split_once("=>")
            .is_some_and(|(params, _)| identifier(params.trim()).is_some());
    is_function.then(|| name.to_string())
}

fn identifier(text: &str) -> Option<&str> {
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '!' || c == '$'))
        .unwrap_or(text.len());
    let name = &text[..end];
    let first = name.chars().next()?;
    (first.is_alphabetic() || first == '_' || first == '$').then_some(name)
}

fn strip_generics(text: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        OutlineRow, SymbolChange, build_outline, declaration_name, file_symbols, hunk_context,
        outline_markdown,
    };
    use crate::diff::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};
    use std::collections::HashSet;

    fn line(kind: LineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.to_string(),
            old_lineno: Some(1),
            new_lineno: Some(1),
        }
    }

    fn file(path: &str, status: FileStatus, hunks: Vec<Hunk>) -> FileDiff {
        let additions = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == LineKind::Addition)
            .count();
        let deletions = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == LineKind::Deletion)
            .count();
        FileDiff {
            path: path.to_string(),
            old_path: None,
            status,
            hunks,
            additions,
            deletions,
            collapsed: false,
            total_new_lines: 0,
            sbs_cache: None,
        }
    }

    #[test]
    fn hunk_context_extracts_the_text_after_the_second_marker() {
        assert_eq!(hunk_context("@@ -10,5 +10,7 @@ fn foo()"), Some("fn foo()"));
        assert_eq!(hunk_context("@@ -1,3 +1,5 @@ impl Foo"), Some("impl Foo"));
        assert_eq!(hunk_context("@@ -10,5 +10,7 @@"), None);
        assert_eq!(hunk_context("@@ -10,5 +10,7 @@   "), None);
        assert_eq!(hunk_context("not a header"), None);
    }

    #[test]
    fn recognises_declarations_across_languages() {
        assert_eq!(
            declaration_name("pub fn parse(x: u8) {"),
            Some("fn parse".into())
        );
        assert_eq!(
            declaration_name("pub(crate) struct Foo<T> {"),
            Some("struct Foo".into())
        );
        assert_eq!(
            declaration_name("impl<T: Clone> Iterator for Foo<T> {"),
            Some("impl Iterator for Foo".into())
        );
        assert_eq!(
            declaration_name("    def run(self):"),
            Some("def run".into())
        );
        assert_eq!(
            declaration_name("class Widget(Base):"),
            Some("class Widget".into())
        );
        assert_eq!(
            declaration_name("func (s *Server) Start() error {"),
            Some("Start".into())
        );
        assert_eq!(declaration_name("func main() {"), Some("func main".into()));
        assert_eq!(
            declaration_name("export default async function load() {"),
            Some("function load".into())
        );
        assert_eq!(
            declaration_name("export const handler = async (req) => {"),
            Some("handler".into())
        );
        assert_eq!(declaration_name("const total = items.length;"), None);
        assert_eq!(declaration_name("let x = 5;"), None);
        assert_eq!(declaration_name("            fn deeply_nested() {}"), None);
        assert_eq!(declaration_name("    return type_name;"), None);
    }

    #[test]
    fn symbols_come_from_added_removed_lines_and_hunk_context() {
        let f = file(
            "src/lib.rs",
            FileStatus::Modified,
            vec![
                Hunk {
                    header: "@@ -1,3 +1,4 @@ fn existing()".to_string(),
                    lines: vec![
                        line(LineKind::Context, "fn existing() {"),
                        line(LineKind::Addition, "    let y = 2;"),
                    ],
                },
                Hunk {
                    header: "@@ -10,3 +11,3 @@".to_string(),
                    lines: vec![
                        line(LineKind::Deletion, "fn old_name() {}"),
                        line(LineKind::Addition, "fn new_name() {}"),
                    ],
                },
                Hunk {
                    header: "@@ -20,2 +21,3 @@".to_string(),
                    lines: vec![
                        line(LineKind::Deletion, "pub fn resize(w: u16) {"),
                        line(LineKind::Addition, "pub fn resize(w: u16, h: u16) {"),
                    ],
                },
            ],
        );
        let symbols = file_symbols(&f);
        let summary: Vec<(String, SymbolChange, usize)> = symbols
            .into_iter()
            .map(|s| (s.name, s.change, s.hunk_idx))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("fn existing".into(), SymbolChange::Modified, 0),
                ("fn old_name".into(), SymbolChange::Removed, 1),
                ("fn new_name".into(), SymbolChange::Added, 1),
                ("fn resize".into(), SymbolChange::Modified, 2),
            ]
        );
    }

    #[test]
    fn markdown_headings_are_the_symbols_of_prose_files() {
        use super::heading_name;
        assert_eq!(heading_name("## Install"), Some("## Install".into()));
        assert_eq!(heading_name("# Title ##"), Some("# Title".into()));
        assert_eq!(heading_name("#hashtag"), None);
        assert_eq!(heading_name("####### too deep"), None);
        assert_eq!(heading_name("plain text"), None);

        let f = file(
            "docs/README.md",
            FileStatus::Modified,
            vec![
                Hunk {
                    header: "@@ -1,3 +1,4 @@".to_string(),
                    lines: vec![
                        line(LineKind::Context, "## Install"),
                        line(LineKind::Context, ""),
                        line(LineKind::Addition, "Run `brew install changes`."),
                    ],
                },
                Hunk {
                    header: "@@ -20,2 +21,3 @@ Some paragraph text".to_string(),
                    lines: vec![
                        line(LineKind::Deletion, "## Keybindings"),
                        line(LineKind::Addition, "## Keys"),
                    ],
                },
            ],
        );
        let names: Vec<(String, SymbolChange)> = file_symbols(&f)
            .into_iter()
            .map(|s| (s.name, s.change))
            .collect();
        assert_eq!(
            names,
            vec![
                ("## Install".into(), SymbolChange::Modified),
                ("## Keybindings".into(), SymbolChange::Removed),
                ("## Keys".into(), SymbolChange::Added),
            ]
        );
    }

    #[test]
    fn whole_file_additions_list_declarations_as_added() {
        let f = file(
            "new.py",
            FileStatus::Untracked,
            vec![Hunk {
                header: "@@ -0,0 +1,3 @@".to_string(),
                lines: vec![
                    line(LineKind::Addition, "class Thing:"),
                    line(LineKind::Addition, "    def run(self):"),
                    line(LineKind::Addition, "        pass"),
                ],
            }],
        );
        let names: Vec<(String, SymbolChange)> = file_symbols(&f)
            .into_iter()
            .map(|s| (s.name, s.change))
            .collect();
        assert_eq!(
            names,
            vec![
                ("class Thing".into(), SymbolChange::Added),
                ("def run".into(), SymbolChange::Added),
            ]
        );
    }

    #[test]
    fn outline_groups_files_into_a_shallow_tree() {
        let files = vec![
            file("src/app/keys.rs", FileStatus::Modified, vec![]),
            file("src/app/mod.rs", FileStatus::Modified, vec![]),
            file("src/ui.rs", FileStatus::Modified, vec![]),
            file("README.md", FileStatus::Modified, vec![]),
            file("docs/guide/intro.md", FileStatus::Added, vec![]),
        ];
        let rows = build_outline(&files, None, &HashSet::new());
        let rendered: Vec<String> = rows
            .iter()
            .map(|row| match row {
                OutlineRow::Dir { prefix, name, .. } => format!("{prefix}{name}/"),
                OutlineRow::File { prefix, name, .. } => format!("{prefix}{name}"),
                OutlineRow::Symbol { prefix, symbol, .. } => format!("{prefix}{}", symbol.name),
                OutlineRow::More { prefix, count, .. } => format!("{prefix}… {count} more"),
                other => panic!("unexpected row without an index: {other:?}"),
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                "docs/guide/",
                "└── intro.md",
                "src/",
                "├── app/",
                "│   ├── keys.rs",
                "│   └── mod.rs",
                "└── ui.rs",
                "README.md",
            ]
        );
        assert!(rows[0].target().is_none());
        assert_eq!(rows[1].target(), Some((4, None)));
        assert!(outline_markdown(&rows).contains("├── app/  +0 -0"));
    }
}
