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
    /// One step of the flow view: a function on a route from an entry point down to
    /// changed code, indented by depth. Unchanged steps have no mark.
    Flow {
        depth: usize,
        name: String,
        location: String,
        mark: Option<SymbolChange>,
        file_idx: Option<usize>,
        hunk_idx: Option<usize>,
        /// A changed function, as opposed to unchanged context on the route.
        is_target: bool,
        warning: Option<String>,
    },
    /// Heading inside an expanded symbol: "called by (2)" or "calls (33)".
    Section {
        prefix: String,
        label: &'static str,
        count: usize,
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
        !matches!(self, OutlineRow::Dir { .. } | OutlineRow::Section { .. })
    }

    pub fn target(&self) -> Option<(usize, Option<usize>)> {
        match self {
            OutlineRow::Dir { .. } => None,
            OutlineRow::File { file_idx, .. } => Some((*file_idx, None)),
            OutlineRow::Symbol {
                file_idx, symbol, ..
            } => Some((*file_idx, Some(symbol.hunk_idx))),
            OutlineRow::More { file_idx, .. } => Some((*file_idx, None)),
            OutlineRow::Summary { .. } | OutlineRow::Section { .. } => None,
            OutlineRow::Call {
                file_idx, hunk_idx, ..
            }
            | OutlineRow::Flow {
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

/// Routes per changed function gathered before choosing the one to draw. Enough that a
/// long product route survives alongside shorter routes from examples and tests.
const MAX_FLOW_ROUTES: usize = 6;

/// The flow view: a call-tree diff rooted at entry points. Every route from a call-graph
/// root down to a changed function is merged into one tree per root; unchanged steps
/// are context, changed ones carry their mark. Roots with the most changed code first,
/// then functions no route reaches, then removed functions.
pub fn build_flow(files: &[FileDiff], index: &SymbolIndex) -> Vec<OutlineRow> {
    // Every changed function symbol across the diff, keyed for marking route steps.
    let mut changed: Vec<(usize, Symbol)> = Vec::new();
    for (file_idx, file) in files.iter().enumerate() {
        for symbol in file_symbols_with(file, Some(index)) {
            if symbol.ident.is_some() {
                changed.push((file_idx, symbol));
            }
        }
    }
    let change_of = |path: &str, ident: &str| -> Option<SymbolChange> {
        changed
            .iter()
            .find(|(file_idx, symbol)| {
                files[*file_idx].path == path && symbol.ident.as_deref() == Some(ident)
            })
            .map(|(_, symbol)| symbol.change)
    };

    let mut roots: Vec<FlowNode> = Vec::new();
    let mut unreachable: Vec<(usize, Symbol)> = Vec::new();
    // Methods a framework calls (an overridden base-class method, a dunder), with
    // what calls them.
    let mut hooks: Vec<(usize, Symbol, String)> = Vec::new();
    let mut removed: Vec<(usize, Symbol)> = Vec::new();
    let mut truncated: Vec<FlowNode> = Vec::new();

    for (file_idx, symbol) in &changed {
        let file = &files[*file_idx];
        let ident = symbol.ident.as_deref().expect("filtered above");
        if symbol.change == SymbolChange::Removed {
            if !ident.starts_with("test_") && !crate::symbols::is_test_path(&file.path) {
                removed.push((*file_idx, symbol.clone()));
            }
            continue;
        }
        let Some(def) = index
            .defs_named(ident, &file.path)
            .into_iter()
            .find(|def| def.kind == DefKind::Function && def.path == file.path)
            .cloned()
        else {
            continue; // unsupported language, or the index has not caught up yet
        };
        if def.is_test {
            continue; // tests are not user-facing code
        }
        let mut paths = index.paths_to_roots(&def, MAX_FLOW_ROUTES);
        let only_itself = paths.len() == 1 && paths[0].steps.len() == 1;
        if only_itself && !is_entry_point(ident, &file.path) {
            match &def.hook_of {
                Some(hook) => hooks.push((*file_idx, symbol.clone(), hook.clone())),
                None => unreachable.push((*file_idx, symbol.clone())),
            }
            continue;
        }
        // Draw the route from the most entry-point-like root (main, a handler, product
        // code over examples), shortest among those; count the rest on the target so the
        // tree keeps one copy of each subtree.
        paths.sort_by_key(|route| {
            let root = &route.steps[0];
            (
                is_auxiliary_path(&root.path),
                route.ambiguous_edges(),
                entry_point_rank(&root.name, &root.path),
                route.steps.len(),
            )
        });
        let other_routes = paths.len().saturating_sub(1);
        if let Some(route) = paths.into_iter().next() {
            let forest = if route.complete {
                &mut roots
            } else {
                &mut truncated
            };
            insert_route(forest, &route.steps, files, &change_of, other_routes);
        }
    }

    // A changed function nothing calls may still be the root of routes to other changed
    // code (it calls them). Warn on that root rather than listing it twice.
    unreachable.retain(|(file_idx, symbol)| {
        let path = &files[*file_idx].path;
        let ident = symbol.ident.as_deref().unwrap_or_default();
        let Some(def) = index
            .defs_named(ident, path)
            .into_iter()
            .find(|d| d.path == *path)
        else {
            return true;
        };
        match roots
            .iter_mut()
            .find(|root| root.path == def.path && root.line == def.line)
        {
            Some(root) => {
                let from_tests = !index.callers(ident, path, None).is_empty();
                root.warning = Some(match symbol.change {
                    SymbolChange::Added if from_tests => "only called from tests".to_string(),
                    SymbolChange::Added => "no callers".to_string(),
                    _ => "no callers found (registered by name?)".to_string(),
                });
                false
            }
            None => true,
        }
    });

    // A hook that roots routes of its own (it calls other changed code) is drawn there,
    // with the framework noted, rather than listed twice.
    hooks.retain(|(file_idx, symbol, hook)| {
        let path = &files[*file_idx].path;
        let ident = symbol.ident.as_deref().unwrap_or_default();
        let Some(def) = index
            .defs_named(ident, path)
            .into_iter()
            .find(|d| d.path == *path)
        else {
            return true;
        };
        match roots
            .iter_mut()
            .find(|root| root.path == def.path && root.line == def.line)
        {
            Some(root) => {
                root.note = Some(hook.clone());
                false
            }
            None => true,
        }
    });

    // Product entry points first, examples and benches last, more changed code first.
    roots.sort_by_key(|root| {
        (
            is_auxiliary_path(&root.path),
            std::cmp::Reverse(root.targets()),
        )
    });
    let mut rows = Vec::new();
    for root in &roots {
        root.emit(0, &mut rows);
    }
    if !truncated.is_empty() {
        rows.push(OutlineRow::Section {
            prefix: String::new(),
            label: "routes longer than the search depth (entry point not reached)",
            count: 0,
        });
        for root in &truncated {
            root.emit(1, &mut rows);
        }
    }
    if !hooks.is_empty() {
        rows.push(OutlineRow::Section {
            prefix: String::new(),
            label: "called by a framework",
            count: hooks.len(),
        });
        for (file_idx, symbol, hook) in hooks {
            let ident = symbol.ident.as_deref().unwrap_or_default();
            let display = index
                .defs_named(ident, &files[file_idx].path)
                .first()
                .map(|def| def.display.clone())
                .unwrap_or_else(|| ident.to_string());
            rows.push(OutlineRow::Flow {
                depth: 1,
                name: display,
                location: format!("{}  ← {hook}", files[file_idx].path),
                mark: Some(symbol.change),
                file_idx: Some(file_idx),
                hunk_idx: Some(symbol.hunk_idx),
                is_target: true,
                warning: None,
            });
        }
    }
    if !unreachable.is_empty() {
        rows.push(OutlineRow::Section {
            prefix: String::new(),
            label: "no route from any entry point",
            count: unreachable.len(),
        });
        for (file_idx, symbol) in unreachable {
            let ident = symbol.ident.as_deref().unwrap_or_default();
            let display = index
                .defs_named(ident, &files[file_idx].path)
                .first()
                .map(|def| def.display.clone())
                .unwrap_or_else(|| ident.to_string());
            // A modified function nothing calls is usually registered by name (a
            // decorator, a route table, a plugin hook) rather than forgotten.
            let from_tests = !index.callers(ident, &files[file_idx].path, None).is_empty();
            let warning = match symbol.change {
                SymbolChange::Added if from_tests => "only called from tests",
                SymbolChange::Added => "no callers",
                _ => "no callers found (registered by name?)",
            };
            rows.push(OutlineRow::Flow {
                depth: 1,
                name: display,
                location: files[file_idx].path.clone(),
                mark: Some(symbol.change),
                file_idx: Some(file_idx),
                hunk_idx: Some(symbol.hunk_idx),
                is_target: true,
                warning: Some(warning.to_string()),
            });
        }
    }
    if !removed.is_empty() {
        rows.push(OutlineRow::Section {
            prefix: String::new(),
            label: "removed",
            count: removed.len(),
        });
        for (file_idx, symbol) in removed {
            let ident = symbol.ident.as_deref().unwrap_or_default();
            let survivors = index.callers_of_removed(ident, &files[file_idx].path).len();
            rows.push(OutlineRow::Flow {
                depth: 1,
                name: ident.to_string(),
                location: files[file_idx].path.clone(),
                mark: Some(SymbolChange::Removed),
                file_idx: Some(file_idx),
                hunk_idx: Some(symbol.hunk_idx),
                is_target: true,
                warning: (survivors > 0).then(|| format!("still called by {survivors}")),
            });
        }
    }
    rows
}

struct FlowNode {
    path: String,
    line: u32,
    name: String,
    /// The edge from this node to its child was matched by name only.
    ambiguous: bool,
    warning: Option<String>,
    /// The framework that calls this root, when nothing in the repository does.
    note: Option<String>,
    mark: Option<SymbolChange>,
    file_idx: Option<usize>,
    hunk_idx: Option<usize>,
    is_target: bool,
    /// Routes to this target beyond the drawn one.
    other_routes: usize,
    children: Vec<FlowNode>,
}

impl FlowNode {
    /// Changed functions in this subtree, for ordering roots.
    fn targets(&self) -> usize {
        usize::from(self.is_target) + self.children.iter().map(FlowNode::targets).sum::<usize>()
    }

    fn emit(&self, depth: usize, rows: &mut Vec<OutlineRow>) {
        let mut location = format!("{}:{}", self.path, self.line);
        if let Some(note) = &self.note {
            location.push_str(&format!("  ← {note}"));
        }
        if self.ambiguous {
            location.push_str("  (matched by name)");
        }
        rows.push(OutlineRow::Flow {
            depth,
            name: self.name.clone(),
            location,
            mark: self.mark,
            file_idx: self.file_idx,
            hunk_idx: self.hunk_idx,
            is_target: self.is_target,
            warning: self.warning.clone(),
        });
        for child in &self.children {
            child.emit(depth + 1, rows);
        }
    }
}

/// Merge one root-first route into the forest, sharing prefixes with existing routes.
fn insert_route(
    forest: &mut Vec<FlowNode>,
    steps: &[crate::symbols::PathStep],
    files: &[FileDiff],
    change_of: &dyn Fn(&str, &str) -> Option<SymbolChange>,
    other_routes: usize,
) {
    let mut level = forest;
    let last = steps.len().saturating_sub(1);
    for (i, step) in steps.iter().enumerate() {
        let position = match level
            .iter()
            .position(|node| node.path == step.path && node.line == step.line)
        {
            Some(position) => position,
            None => {
                let mark = if step.name.is_empty() {
                    None
                } else {
                    change_of(&step.path, &step.name)
                };
                let (file_idx, hunk_idx, _) =
                    locate_in_diff(files, &step.path, step.line, Some(&step.name));
                level.push(FlowNode {
                    path: step.path.clone(),
                    line: step.line,
                    name: step.display.clone(),
                    ambiguous: step.ambiguous,
                    warning: None,
                    note: None,
                    mark,
                    file_idx,
                    hunk_idx,
                    is_target: false,
                    other_routes: 0,
                    children: Vec::new(),
                });
                level.len() - 1
            }
        };
        if i == last {
            level[position].is_target = true;
            level[position].other_routes = other_routes;
        }
        level = &mut level[position].children;
    }
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
            OutlineRow::Section {
                prefix,
                label,
                count,
            } => {
                if *count > 0 {
                    out.push_str(&format!("{prefix}{label} ({count})\n"));
                } else {
                    out.push_str(&format!("{prefix}{label}\n"));
                }
            }
            OutlineRow::Flow {
                depth,
                name,
                location,
                mark,
                warning,
                ..
            } => {
                let mark = mark.map(change_glyph).unwrap_or(" ");
                let warning = warning
                    .as_ref()
                    .map(|w| format!("   ⚠ {w}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "{mark} {}{name}  {location}{warning}\n",
                    "  ".repeat(*depth)
                ));
            }
            OutlineRow::Call {
                prefix,
                name,
                location,
                mark,
                ..
            } => {
                let mark = mark
                    .map(|m| format!("{} ", change_glyph(m)))
                    .unwrap_or_default();
                out.push_str(&format!("{prefix}{mark}{name}  {location}\n"));
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
        let mut symbols = file_symbols_with(file, context.index);
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
        ..
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

    // Two labelled groups, each its own subtree, so the counts in the summary line
    // match what is listed under each heading.
    struct Group {
        label: &'static str,
        count: usize,
        rows: Vec<OutlineRow>,
    }
    let mut groups: Vec<Group> = Vec::new();

    if !callers.is_empty() {
        let mut group_rows = Vec::new();
        for caller in callers.iter().take(MAX_CALLS_SHOWN) {
            let (target_file, hunk_idx, mark) = locate_in_diff(
                context.files,
                &caller.path,
                caller.line,
                caller.from_name.as_deref(),
            );
            group_rows.push(OutlineRow::Call {
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
            });
        }
        if callers.len() > MAX_CALLS_SHOWN {
            group_rows.push(OutlineRow::More {
                prefix: String::new(),
                file_idx,
                count: callers.len() - MAX_CALLS_SHOWN,
            });
        }
        groups.push(Group {
            label: "called by",
            count: callers.len(),
            rows: group_rows,
        });
    }
    if !callees.is_empty() {
        let mut group_rows = Vec::new();
        for (name, targets) in callees.iter().take(MAX_CALLS_SHOWN) {
            let target = targets[0];
            let (target_file, hunk_idx, mark) =
                locate_in_diff(context.files, &target.path, target.line, Some(name));
            let mut display = target.display.clone();
            if targets.len() > 1 {
                display.push_str(&format!(" (+{} more definitions)", targets.len() - 1));
            }
            group_rows.push(OutlineRow::Call {
                prefix: String::new(),
                direction: CallDirection::Outgoing,
                name: display,
                location: format!("{}:{}", target.path, target.line),
                mark,
                file_idx: target_file,
                hunk_idx,
                parent: parent.clone(),
                parent_path: file.path.clone(),
            });
        }
        if callees.len() > MAX_CALLS_SHOWN {
            group_rows.push(OutlineRow::More {
                prefix: String::new(),
                file_idx,
                count: callees.len() - MAX_CALLS_SHOWN,
            });
        }
        groups.push(Group {
            label: "calls",
            count: callees.len(),
            rows: group_rows,
        });
    }

    let group_count = groups.len();
    for (g, group) in groups.into_iter().enumerate() {
        let (heading_branch, item_prefix) = connectors(&tree_prefix, g + 1 == group_count, false);
        rows.push(OutlineRow::Section {
            prefix: heading_branch,
            label: group.label,
            count: group.count,
        });
        let items = group.rows.len();
        for (i, row) in group.rows.into_iter().enumerate() {
            let (branch, _) = connectors(&item_prefix, i + 1 == items, false);
            rows.push(match row {
                OutlineRow::Call {
                    prefix: _,
                    direction,
                    name,
                    location,
                    mark,
                    file_idx,
                    hunk_idx,
                    parent,
                    parent_path,
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
        }
    }
}

/// What the index knows about one changed function.
pub struct CallSummary<'a> {
    pub callers: Vec<crate::symbols::Caller>,
    pub callees: Vec<(String, Vec<&'a crate::symbols::Def>)>,
    pub warning: Option<String>,
    /// The framework that calls this method when nothing in the repository does.
    pub hook_of: Option<String>,
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
    let production = callers.iter().filter(|c| !c.from_test).count();
    let hook = def.as_ref().and_then(|d| d.hook_of.clone());
    if def.as_ref().is_some_and(|d| d.is_test) {
        return Some(CallSummary {
            callers,
            callees,
            warning: None,
            hook_of: hook,
        });
    }
    let warning = match change {
        SymbolChange::Added if callers.is_empty() && hook.is_some() => None,
        SymbolChange::Added if callers.is_empty() && !is_entry_point(ident, &file.path) => {
            Some("no callers".to_string())
        }
        SymbolChange::Added if production == 0 && !is_entry_point(ident, &file.path) => {
            Some("only called from tests".to_string())
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
        hook_of: hook,
    })
}

/// One-line call context for a hunk, shown under its header in the diff:
/// `fn parse · called by main, other · calls tokenize, +2 more`. `label` names the
/// function when the hunk touches more than one. Fitted to `width` columns.
pub fn inline_call_context(summary: &CallSummary<'_>, label: Option<&str>, width: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(label) = label {
        parts.push(label.to_string());
    }
    if let Some(warning) = &summary.warning {
        parts.push(format!("⚠ {warning}"));
    }
    let (called, used): (Vec<_>, Vec<_>) = summary
        .callers
        .iter()
        .partition(|caller| !caller.is_reference);
    let names = |callers: &[&crate::symbols::Caller]| -> Vec<String> {
        callers
            .iter()
            .map(|c| c.from.clone().unwrap_or_else(|| "(top level)".to_string()))
            .collect()
    };
    if !called.is_empty() {
        parts.push(format!("called by {}", join_limited(&names(&called), 4)));
    }
    if !used.is_empty() {
        parts.push(format!("used by {}", join_limited(&names(&used), 4)));
    }
    if summary.callers.is_empty() && summary.warning.is_none() {
        match &summary.hook_of {
            Some(hook) => parts.push(format!("called by {hook}")),
            None => parts.push("no callers".to_string()),
        }
    }
    if !summary.callees.is_empty() {
        let names: Vec<String> = summary
            .callees
            .iter()
            .map(|(_, targets)| targets[0].display.clone())
            .collect();
        parts.push(format!("calls {}", join_limited(&names, 4)));
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

/// How much a root looks like where a user's action enters: 0 for `main` and top-level
/// code, 1 for handler-like names, 2 for anything else (an unused public function, say).
fn entry_point_rank(name: &str, path: &str) -> u8 {
    if name == "main" || (name.is_empty() && !is_auxiliary_path(path)) {
        0
    } else if name.starts_with("handle")
        || name.starts_with("on_")
        || name.starts_with("run")
        || name.starts_with("serve")
        || name.starts_with("route")
        || name.starts_with("cli")
        || name.starts_with("command")
    {
        1
    } else {
        2
    }
}

/// Examples, benchmarks and scripts: real callers, but not the product's entry points.
fn is_auxiliary_path(path: &str) -> bool {
    path.starts_with("examples/")
        || path.starts_with("benches/")
        || path.starts_with("scripts/")
        || path.starts_with("tools/")
        || crate::symbols::is_test_path(path)
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

/// Record a declaration; a name removed then added in the same diff is one rewrite.
fn push_symbol(symbols: &mut Vec<Symbol>, name: String, change: SymbolChange, hunk_idx: usize) {
    if let Some(existing) = symbols.iter_mut().find(|s| s.name == name) {
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
}

/// Add a modified function found via the index, unless the hunk already declared it.
fn push_ident(symbols: &mut Vec<Symbol>, name: String, ident: String, hunk_idx: usize) {
    if symbols
        .iter()
        .any(|s| s.ident.as_deref() == Some(ident.as_str()))
    {
        return;
    }
    symbols.push(Symbol {
        name,
        change: SymbolChange::Modified,
        hunk_idx,
        ident: Some(ident),
    });
}

/// New-side line numbers a hunk touches: its added lines, or for a pure deletion the
/// line where the deleted block used to be.
fn hunk_changed_lines(hunk: &crate::diff::Hunk) -> Vec<u32> {
    let mut lines: Vec<u32> = hunk
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Addition)
        .filter_map(|l| l.new_lineno)
        .collect();
    if lines.is_empty() {
        // The context line right after the deletion sits where the block was.
        let mut after_deletion = false;
        for l in &hunk.lines {
            if l.kind == LineKind::Deletion {
                after_deletion = true;
            } else if after_deletion && let Some(n) = l.new_lineno {
                lines.push(n);
                break;
            }
        }
        if lines.is_empty()
            && let Some(n) = hunk.first_new_lineno()
        {
            lines.push(n);
        }
    }
    lines.dedup();
    lines
}

/// `fn App::open_outline` / `def Widget.render`: the index's display name with the
/// language's declaration keyword, matching what hunk lines produce.
fn declaration_label(display: &str, path: &str) -> String {
    let keyword = match path.rsplit('.').next() {
        Some("py" | "pyi") => "def",
        Some("go") => "func",
        Some("js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts") => "function",
        _ => "fn",
    };
    format!("{keyword} {display}")
}

/// Declarations a file's hunks add, remove, or sit inside, in hunk order.
pub fn file_symbols(file: &FileDiff) -> Vec<Symbol> {
    file_symbols_with(file, None)
}

/// Like `file_symbols`, but with an index the function enclosing each body edit is
/// found from parsed line ranges rather than git's hunk-header heuristic, which names
/// the nearest unindented line: a `class` or `impl` rather than the method inside it.
pub fn file_symbols_with(file: &FileDiff, index: Option<&SymbolIndex>) -> Vec<Symbol> {
    // Whole-file additions or deletions: list what the file declares, all one kind.
    let whole_file = match file.status {
        FileStatus::Added | FileStatus::Untracked => Some(SymbolChange::Added),
        FileStatus::Deleted => Some(SymbolChange::Removed),
        _ => None,
    };

    let mut symbols: Vec<Symbol> = Vec::new();

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
                push_symbol(&mut symbols, name, change, hunk_idx);
                declared_here = true;
            }
        }
        if !declared_here
            && whole_file.is_none()
            && let Some(section) = enclosing_section
        {
            push_symbol(&mut symbols, section, SymbolChange::Modified, hunk_idx);
            continue;
        }
        // With an index, attribute a body edit to the parsed function around it.
        if !declared_here
            && whole_file.is_none()
            && let Some(index) = index
        {
            let mut found = false;
            for line in hunk_changed_lines(hunk) {
                if let Some(def) = index.function_at(&file.path, line) {
                    let name = declaration_label(&def.display, &file.path);
                    push_ident(&mut symbols, name, def.name.clone(), hunk_idx);
                    found = true;
                }
            }
            if found {
                continue;
            }
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
            push_symbol(&mut symbols, name, SymbolChange::Modified, hunk_idx);
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
        // Rust / TS constants: `const NAME: Type = ...`, `static NAME: Type`.
        if matches!(word, "const" | "static")
            && let Some(name) = identifier(rest[word.len()..].trim_start())
            && name
                .chars()
                .all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit())
            && rest[word.len()..].trim_start()[name.len()..]
                .trim_start()
                .starts_with(':')
        {
            return Some(format!("const {name}"));
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
        assert_eq!(
            declaration_name("pub const MAX_SYMBOLS: usize = 12;"),
            Some("const MAX_SYMBOLS".into())
        );
        assert_eq!(
            declaration_name("static COUNTER: AtomicU32 = AtomicU32::new(0);"),
            Some("const COUNTER".into())
        );
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
    fn flow_view_roots_routes_at_entry_points_and_lists_orphans() {
        use super::build_flow;
        use crate::symbols::SymbolIndex;
        let root = std::env::temp_dir().join(format!("changes-flow-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() { run(); }\nfn run() { handle(); }\nfn handle() { leaf(); }\nfn leaf() {}\nfn orphan() {}\n#[cfg(test)]\nmod tests { #[test] fn t() { super::orphan(); } }\n",
        )
        .unwrap();
        let index = SymbolIndex::build(&root, &["src/main.rs".to_string()]);
        // The diff added `leaf` and `orphan`.
        let file = file(
            "src/main.rs",
            FileStatus::Modified,
            vec![Hunk {
                header: "@@ -3,0 +4,2 @@".to_string(),
                lines: vec![
                    line(LineKind::Addition, "fn leaf() {}"),
                    line(LineKind::Addition, "fn orphan() {}"),
                ],
            }],
        );
        let rows = build_flow(&[file], &index);
        let rendered: Vec<String> = rows
            .iter()
            .map(|row| match row {
                OutlineRow::Flow {
                    depth,
                    name,
                    mark,
                    warning,
                    ..
                } => format!(
                    "{}{}{name}{}",
                    mark.map(super::change_glyph).unwrap_or(" "),
                    "  ".repeat(*depth + 1),
                    warning
                        .as_ref()
                        .map(|w| format!(" ⚠ {w}"))
                        .unwrap_or_default()
                ),
                OutlineRow::Section { label, count, .. } => format!("[{label} {count}]"),
                other => panic!("unexpected row {other:?}"),
            })
            .collect();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            rendered,
            vec![
                "   main",
                "     run",
                "       handle",
                "+        leaf",
                "[no route from any entry point 1]",
                "+    orphan ⚠ only called from tests",
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
