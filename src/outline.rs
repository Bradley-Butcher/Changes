//! The "shape" of a diff: a directory tree of changed files and the declarations each
//! hunk adds, removes, or touches. This is the altitude a reviewer wants before reading
//! lines, and it is derived purely from the diff text — no language servers involved.

use crate::diff::{FileDiff, FileStatus, LineKind};
use crate::symbols::{DefKind, SymbolIndex};
use std::collections::{BTreeMap, HashMap, HashSet};

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
/// How far below a root the tree is drawn.
const MAX_FLOW_DEPTH: usize = 8;
/// Unchanged callees folded into one context line before "+N more".
const MAX_CONTEXT_NAMES: usize = 5;

/// The flow view: the change as a diff of the call tree. For each entry point the change
/// reaches, its call tree as the code stands now, in call order: `+` on calls the diff
/// added, `-` on calls it removed, `~` on callees whose body changed, and unchanged
/// callees folded into one line of context so the order reads. Only branches that lead
/// to a change are opened. Objects handed to a framework show the hooks it will call
/// beneath the point they are built. Changed code no root reaches is listed after.
pub fn build_flow(files: &[FileDiff], index: &SymbolIndex) -> Vec<OutlineRow> {
    let mut flow = Flow::new(files, index);
    flow.plan();
    flow.render()
}

/// A root worth drawing: the step, how entry-point-like it is (auxiliary path, edges
/// matched by name, entry rank, route length; lower is better), and how many changed
/// functions it reaches.
type RootScore = (crate::symbols::PathStep, (bool, usize, u8, usize), usize);

/// A changed function as the flow sees it: where it is in the diff and how it changed.
#[derive(Clone)]
struct Changed {
    file_idx: usize,
    hunk_idx: usize,
    change: SymbolChange,
}

struct Flow<'a> {
    files: &'a [FileDiff],
    index: &'a SymbolIndex,
    /// Changed functions by (path, ident).
    changed: HashMap<(String, String), Changed>,
    /// New-side line numbers the diff added, per path.
    added_lines: HashMap<String, HashSet<u32>>,
    /// Functions on some route from a root to a change, by (path, line); these are
    /// the branches worth opening.
    on_path: HashSet<(String, u32)>,
    /// Roots to draw, best first: (path, line) of the root, or a top-level pseudo-root.
    roots: Vec<crate::symbols::PathStep>,
    /// Changed functions no route reaches.
    unreached: Vec<(String, String)>,
    /// Hooks drawn beneath a constructor, so the framework section skips them.
    placed_hooks: HashSet<(String, u32)>,
    /// Functions already drawn under the current root, against cycles and repeats.
    drawn: HashSet<(String, u32)>,
    /// Removed functions already drawn as `-` beneath their old caller.
    drawn_removed: HashSet<String>,
    rows: Vec<OutlineRow>,
}

impl<'a> Flow<'a> {
    fn new(files: &'a [FileDiff], index: &'a SymbolIndex) -> Self {
        let mut changed = HashMap::new();
        let mut added_lines: HashMap<String, HashSet<u32>> = HashMap::new();
        for (file_idx, file) in files.iter().enumerate() {
            for symbol in file_symbols_with(file, Some(index)) {
                if let Some(ident) = &symbol.ident {
                    changed.insert(
                        (file.path.clone(), ident.clone()),
                        Changed {
                            file_idx,
                            hunk_idx: symbol.hunk_idx,
                            change: symbol.change,
                        },
                    );
                }
            }
            let lines = added_lines.entry(file.path.clone()).or_default();
            for hunk in &file.hunks {
                for line in &hunk.lines {
                    if line.kind == LineKind::Addition
                        && let Some(lineno) = line.new_lineno
                    {
                        lines.insert(lineno);
                    }
                }
            }
        }
        Self {
            files,
            index,
            changed,
            added_lines,
            on_path: HashSet::new(),
            roots: Vec::new(),
            unreached: Vec::new(),
            placed_hooks: HashSet::new(),
            drawn: HashSet::new(),
            drawn_removed: HashSet::new(),
            rows: Vec::new(),
        }
    }

    fn change_of(&self, path: &str, ident: &str) -> Option<&Changed> {
        self.changed.get(&(path.to_string(), ident.to_string()))
    }

    fn def_of(&self, path: &str, ident: &str) -> Option<crate::symbols::Def> {
        self.index
            .defs_named(ident, path)
            .into_iter()
            .find(|def| def.kind == DefKind::Function && def.path == path)
            .cloned()
    }

    /// Find a route to a root for every changed function; remember the roots and every
    /// function along the way.
    fn plan(&mut self) {
        let mut keys: Vec<(String, String)> = self.changed.keys().cloned().collect();
        keys.sort();
        let mut root_scores: Vec<RootScore> = Vec::new();
        for (path, ident) in keys {
            let change = self.changed[&(path.clone(), ident.clone())].change;
            if change == SymbolChange::Removed {
                // A removed function is drawn as `-` under whoever used to call it.
                for caller in self.index.callers_of_removed(&ident, &path) {
                    if caller.from_test {
                        continue;
                    }
                    match caller.from_name.as_deref() {
                        Some(from) => {
                            if let Some(def) = self.def_of(&caller.path, from) {
                                self.mark_routes(&def, &mut root_scores);
                            }
                        }
                        None => {
                            let root = top_level_step(&caller.path, caller.line);
                            let score = (is_auxiliary_path(&caller.path), 0, 0, 1);
                            if !root_scores
                                .iter()
                                .any(|(r, _, _)| r.path == root.path && r.name.is_empty())
                            {
                                root_scores.push((root, score, 1));
                            }
                        }
                    }
                }
                continue;
            }
            let Some(def) = self.def_of(&path, &ident) else {
                continue; // unsupported language, or the index has not caught up yet
            };
            if def.is_test {
                continue;
            }
            if !self.mark_routes(&def, &mut root_scores) {
                self.unreached.push((path, ident));
            }
        }
        root_scores.sort_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)));
        self.roots = root_scores.into_iter().map(|(root, _, _)| root).collect();
    }

    /// Record the best route from `def` up to a root. Returns false when nothing but
    /// tests reaches it and it is not an entry point itself.
    fn mark_routes(&mut self, def: &crate::symbols::Def, root_scores: &mut Vec<RootScore>) -> bool {
        let mut paths = self.index.paths_to_roots(def, MAX_FLOW_ROUTES);
        let only_itself = paths.len() == 1 && paths[0].steps.len() == 1;
        if only_itself && !is_entry_point(&def.name, &def.path) {
            // Nothing calls it. A hook is drawn under its class's constructor when a
            // route reaches that, otherwise listed with its framework.
            return false;
        }
        paths.sort_by_key(|route| {
            let root = &route.steps[0];
            (
                is_auxiliary_path(&root.path),
                route.ambiguous_edges(),
                entry_point_rank(&root.name, &root.path),
                route.steps.len(),
            )
        });
        let Some(route) = paths.into_iter().next() else {
            return false;
        };
        for step in &route.steps {
            self.on_path.insert((step.path.clone(), step.line));
        }
        let root = route.steps[0].clone();
        let score = (
            is_auxiliary_path(&root.path),
            route.ambiguous_edges(),
            entry_point_rank(&root.name, &root.path),
            route.steps.len(),
        );
        // One pseudo-root per file's top-level code, whichever line reached it first.
        match root_scores.iter_mut().find(|(r, _, _)| {
            r.path == root.path
                && (r.line == root.line || (r.name.is_empty() && root.name.is_empty()))
        }) {
            Some(entry) => {
                entry.1 = entry.1.min(score);
                entry.2 += 1;
            }
            None => root_scores.push((root, score, 1)),
        }
        true
    }

    fn render(mut self) -> Vec<OutlineRow> {
        let roots = std::mem::take(&mut self.roots);
        for root in &roots {
            self.drawn.clear();
            if root.name.is_empty() {
                // Top-level code: draw what it calls as if it were a function.
                self.push_row(
                    0,
                    root.display.clone(),
                    format!("{}:{}", root.path, root.line),
                    None,
                    None,
                    None,
                );
                self.draw_top_level(&root.path, 1);
            } else if let Some(def) = self.def_of(&root.path, &root.name) {
                self.draw_function(&def, 0, None, None);
            }
        }

        let unreached = std::mem::take(&mut self.unreached);
        let mut orphans = Vec::new();
        let mut hooks = Vec::new();
        for (path, ident) in unreached {
            match self.def_of(&path, &ident) {
                Some(def) if def.hook_of.is_some() => hooks.push((path, ident, def)),
                _ => orphans.push((path, ident)),
            }
        }
        hooks.retain(|(_, _, def)| !self.placed_hooks.contains(&(def.path.clone(), def.line)));
        if !hooks.is_empty() {
            self.rows.push(OutlineRow::Section {
                prefix: String::new(),
                label: "called by a framework",
                count: hooks.len(),
            });
            for (path, ident, def) in hooks {
                let changed = self.change_of(&path, &ident).cloned();
                let hook = def.hook_of.clone().unwrap_or_default();
                self.push_row(
                    1,
                    def.display.clone(),
                    format!("{path}  ← {hook}"),
                    changed.as_ref().map(|c| c.change),
                    changed.as_ref().map(|c| (c.file_idx, c.hunk_idx)),
                    None,
                );
            }
        }
        if !orphans.is_empty() {
            self.rows.push(OutlineRow::Section {
                prefix: String::new(),
                label: "no route from any entry point",
                count: orphans.len(),
            });
            for (path, ident) in orphans {
                let changed = self.change_of(&path, &ident).cloned();
                let display = self
                    .index
                    .defs_named(&ident, &path)
                    .first()
                    .map(|def| def.display.clone())
                    .unwrap_or_else(|| ident.clone());
                let from_tests = !self.index.callers(&ident, &path, None).is_empty();
                // A modified function nothing calls is usually registered by name (a
                // decorator, a plugin hook) rather than forgotten.
                let warning = match changed.as_ref().map(|c| c.change) {
                    Some(SymbolChange::Added) if from_tests => "only called from tests",
                    Some(SymbolChange::Added) => "no callers",
                    _ => "no callers found (registered by name?)",
                };
                self.push_row(
                    1,
                    display,
                    path.clone(),
                    changed.as_ref().map(|c| c.change),
                    changed.as_ref().map(|c| (c.file_idx, c.hunk_idx)),
                    Some(warning.to_string()),
                );
            }
        }
        // Removed functions whose old callers are gone too (a deleted file, say).
        let mut removed: Vec<(String, String)> = self
            .changed
            .iter()
            .filter(|(_, c)| c.change == SymbolChange::Removed)
            .map(|(key, _)| key.clone())
            .filter(|(path, ident)| {
                !is_test_like(ident, path) && !self.drawn_removed.contains(ident)
            })
            .collect();
        removed.sort();
        if !removed.is_empty() {
            self.rows.push(OutlineRow::Section {
                prefix: String::new(),
                label: "removed",
                count: removed.len(),
            });
            for (path, ident) in removed {
                let changed = self.change_of(&path, &ident).cloned();
                let survivors = self.index.callers_of_removed(&ident, &path).len();
                self.push_row(
                    1,
                    ident.clone(),
                    path.clone(),
                    Some(SymbolChange::Removed),
                    changed.as_ref().map(|c| (c.file_idx, c.hunk_idx)),
                    (survivors > 0).then(|| format!("still called by {survivors}")),
                );
            }
        }
        self.rows
    }

    #[allow(clippy::too_many_arguments)]
    fn push_row(
        &mut self,
        depth: usize,
        name: String,
        location: String,
        mark: Option<SymbolChange>,
        target: Option<(usize, usize)>,
        warning: Option<String>,
    ) {
        self.rows.push(OutlineRow::Flow {
            depth,
            name,
            location,
            mark,
            file_idx: target.map(|(file_idx, _)| file_idx),
            hunk_idx: target.map(|(_, hunk_idx)| hunk_idx),
            is_target: mark.is_some(),
            warning,
        });
    }

    /// Draw `def` and, when it leads to a change, the calls it makes in order.
    fn draw_function(
        &mut self,
        def: &crate::symbols::Def,
        depth: usize,
        call_added: Option<bool>,
        hook: Option<&str>,
    ) {
        let key = (def.path.clone(), def.line);
        let changed = self.change_of(&def.path, &def.name).cloned();
        let mark = match (call_added, changed.as_ref().map(|c| c.change)) {
            (_, Some(SymbolChange::Added)) | (Some(true), _) => Some(SymbolChange::Added),
            (_, Some(SymbolChange::Modified)) => Some(SymbolChange::Modified),
            _ => None,
        };
        let mut location = format!("{}:{}", def.path, def.line);
        if let Some(hook) = hook {
            location.push_str(&format!("  ← {hook}"));
        }
        let repeat = !self.drawn.insert(key.clone());
        if repeat {
            location.push_str("  (above)");
        }
        let name = match hook {
            Some(_) => format!("↳ {}", def.display),
            None => def.display.clone(),
        };
        self.push_row(
            depth,
            name,
            location,
            mark,
            changed.as_ref().map(|c| (c.file_idx, c.hunk_idx)),
            None,
        );
        if repeat || depth >= MAX_FLOW_DEPTH {
            return;
        }
        let opens = self.on_path.contains(&key)
            || changed
                .as_ref()
                .is_some_and(|c| c.change == SymbolChange::Added);
        if !opens {
            return;
        }
        self.draw_calls(def, depth + 1);
    }

    /// Top-level code of a file (`if __name__ == "__main__"`, a route table): the calls
    /// it makes outside any function.
    fn draw_top_level(&mut self, path: &str, depth: usize) {
        let sites = self.index.top_level_call_sites(path);
        self.draw_sites(path, sites, depth);
    }

    fn draw_calls(&mut self, def: &crate::symbols::Def, depth: usize) {
        let sites = self.index.call_sites(def);
        let removed = self.removed_calls(def);
        self.draw_sites_with_removed(&def.path, sites, removed, depth);
    }

    fn draw_sites(&mut self, path: &str, sites: Vec<crate::symbols::CallSite<'a>>, depth: usize) {
        self.draw_sites_with_removed(path, sites, Vec::new(), depth);
    }

    /// Draw callees in call order. Unchanged callees that lead nowhere are folded into
    /// context lines; the rest get a line each, opened when they lead to a change.
    fn draw_sites_with_removed(
        &mut self,
        path: &str,
        sites: Vec<crate::symbols::CallSite<'a>>,
        removed: Vec<(u32, String)>,
        depth: usize,
    ) {
        // One entry per target, first call wins; a later call on an added line still
        // counts as new if the first one was.
        let added = self.added_lines.get(path).cloned().unwrap_or_default();
        let mut entries: Vec<(u32, crate::symbols::Def, bool, bool)> = Vec::new(); // line, target, call added, reference
        for site in sites {
            for target in site.targets {
                let is_new = added.contains(&site.line);
                match entries
                    .iter_mut()
                    .find(|(_, t, _, _)| t.path == target.path && t.line == target.line)
                {
                    Some(entry) => entry.2 |= is_new,
                    None => entries.push((site.line, target.clone(), is_new, site.is_reference)),
                }
            }
        }
        let mut items: Vec<(u32, Item)> = entries
            .into_iter()
            .map(|(line, target, is_new, is_reference)| {
                (line, Item::Call(target, is_new, is_reference))
            })
            .collect();
        for (line, name) in removed {
            items.push((line, Item::Removed(name)));
        }
        // A removed call sits after whatever survives on the line it was anchored to.
        items.sort_by_key(|(line, item)| (*line, matches!(item, Item::Removed(_))));

        // A function whose calls are all unchanged has no story to tell below it.
        let anything_matters = items.iter().any(|(_, item)| match item {
            Item::Removed(_) => true,
            Item::Call(target, is_new, _) => {
                *is_new
                    || self.change_of(&target.path, &target.name).is_some()
                    || self.on_path.contains(&(target.path.clone(), target.line))
            }
        });
        if !anything_matters {
            return;
        }
        let mut context: Vec<String> = Vec::new();
        let flush = |context: &mut Vec<String>, this: &mut Self| {
            if context.is_empty() {
                return;
            }
            let shown: Vec<&str> = context
                .iter()
                .take(MAX_CONTEXT_NAMES)
                .map(String::as_str)
                .collect();
            let mut text = shown.join(" · ");
            if context.len() > MAX_CONTEXT_NAMES {
                text.push_str(&format!(" · +{} more", context.len() - MAX_CONTEXT_NAMES));
            }
            this.push_row(depth, text, String::new(), None, None, None);
            context.clear();
        };
        for (_, item) in items {
            match item {
                Item::Removed(name) => {
                    flush(&mut context, self);
                    self.push_row(
                        depth,
                        name,
                        String::new(),
                        Some(SymbolChange::Removed),
                        None,
                        None,
                    );
                }
                Item::Call(target, is_new, _is_reference) => {
                    let key = (target.path.clone(), target.line);
                    let changed = self.change_of(&target.path, &target.name);
                    let opens = self.on_path.contains(&key)
                        || changed.is_some_and(|c| c.change == SymbolChange::Added);
                    let matters = is_new || changed.is_some() || opens;
                    if !matters {
                        context.push(target.display.clone());
                        continue;
                    }
                    flush(&mut context, self);
                    self.draw_function(&target, depth, Some(is_new), None);
                    // An object handed to a framework: the hooks it will call.
                    let is_constructor = target.container.is_some()
                        && matches!(
                            target.name.as_str(),
                            "__init__" | "__new__" | "new" | "constructor"
                        );
                    if is_constructor {
                        let hooks: Vec<crate::symbols::Def> = self
                            .index
                            .hook_methods(&target)
                            .into_iter()
                            .cloned()
                            .collect();
                        // Unchanged hooks fold into one line, like unchanged calls.
                        let mut quiet: Vec<&crate::symbols::Def> = Vec::new();
                        for hook in &hooks {
                            self.placed_hooks.insert((hook.path.clone(), hook.line));
                            let matters = self.change_of(&hook.path, &hook.name).is_some()
                                || self.on_path.contains(&(hook.path.clone(), hook.line));
                            if matters {
                                let framework = hook.hook_of.clone().unwrap_or_default();
                                self.draw_function(hook, depth + 1, None, Some(&framework));
                            } else {
                                quiet.push(hook);
                            }
                        }
                        if let Some(first) = quiet.first() {
                            let names: Vec<&str> = quiet.iter().map(|h| h.name.as_str()).collect();
                            let framework = first.hook_of.clone().unwrap_or_default();
                            self.push_row(
                                depth + 1,
                                format!("↳ {}", names.join(" · ")),
                                format!("← {framework}"),
                                None,
                                None,
                                None,
                            );
                        }
                    }
                }
            }
        }
        flush(&mut context, self);
    }

    /// Calls the diff removed from `def`: names followed by `(` on deleted lines of its
    /// hunks that resolve in the index and no longer appear among its calls. Each is
    /// anchored to the new-side line just above where it was.
    fn removed_calls(&mut self, def: &crate::symbols::Def) -> Vec<(u32, String)> {
        let Some(changed) = self.change_of(&def.path, &def.name).cloned() else {
            return Vec::new();
        };
        let Some(file) = self.files.get(changed.file_idx) else {
            return Vec::new();
        };
        let still_called = self.index.call_names_in(def);
        let mut out = Vec::new();
        for hunk in &file.hunks {
            let mut anchor = hunk.first_new_lineno().unwrap_or(def.line);
            let in_def = |line: u32| line >= def.line && line <= def.end_line;
            for line in &hunk.lines {
                match line.kind {
                    LineKind::Deletion => {
                        if !in_def(anchor) {
                            continue;
                        }
                        for name in called_names(&line.content) {
                            // A name the diff removed is no longer in the index; one
                            // that survives elsewhere is. Anything else is a library.
                            let removed_here = self
                                .change_of(&def.path, &name)
                                .is_some_and(|c| c.change == SymbolChange::Removed);
                            if still_called.contains(&name)
                                || out.iter().any(|(_, n)| *n == name)
                                || (!removed_here
                                    && self.index.defs_named(&name, &def.path).is_empty())
                            {
                                continue;
                            }
                            self.drawn_removed.insert(name.clone());
                            out.push((anchor, name));
                        }
                    }
                    _ => {
                        if let Some(lineno) = line.new_lineno {
                            anchor = lineno;
                        }
                    }
                }
            }
        }
        out
    }
}

enum Item {
    /// A resolved callee: the target, whether the call line is new, a reference.
    Call(crate::symbols::Def, bool, bool),
    /// A call the diff deleted.
    Removed(String),
}

/// The pseudo-root for a file's top-level code.
fn top_level_step(path: &str, line: u32) -> crate::symbols::PathStep {
    crate::symbols::PathStep {
        display: format!("{} (top level)", path.rsplit('/').next().unwrap_or(path)),
        name: String::new(),
        path: path.to_string(),
        line,
        ambiguous: false,
    }
}

/// Identifiers followed by `(` in a line of code, the candidates for calls it made.
fn called_names(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut names = Vec::new();
    let mut start = None;
    for (i, &b) in bytes.iter().enumerate() {
        let ident = b.is_ascii_alphanumeric() || b == b'_';
        match (ident, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                let declared = ["fn ", "def ", "func ", "function ", "async "]
                    .iter()
                    .any(|keyword| text[..s].ends_with(keyword));
                if b == b'(' && !bytes[s].is_ascii_digit() && !declared {
                    names.push(text[s..i].to_string());
                }
                start = None;
            }
            _ => {}
        }
    }
    names
}

/// Tests and their fixtures, which the flow leaves out.
fn is_test_like(ident: &str, path: &str) -> bool {
    ident.starts_with("test_") || crate::symbols::is_test_path(path)
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

    fn render_flow(rows: &[OutlineRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                OutlineRow::Flow {
                    depth,
                    name,
                    mark,
                    warning,
                    location,
                    ..
                } => format!(
                    "{}{}{name}{}{}",
                    mark.map(super::change_glyph).unwrap_or(" "),
                    "  ".repeat(*depth + 1),
                    match location.find('←') {
                        Some(pos) => format!("  {}", &location[pos..]),
                        None => String::new(),
                    },
                    warning
                        .as_ref()
                        .map(|w| format!(" ⚠ {w}"))
                        .unwrap_or_default()
                ),
                OutlineRow::Section { label, count, .. } => format!("[{label} {count}]"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn flow_is_a_call_tree_diff_in_call_order() {
        use super::build_flow;
        use crate::symbols::SymbolIndex;
        let root = std::env::temp_dir().join(format!("changes-flow-diff-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        // After the change: main calls setup, fresh (new, calls inner), finish; a call
        // to old was removed. Attempt is handed to a framework and overrides on_save.
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    setup();\n    fresh();\n    finish();\n    let a = Attempt::new();\n}\n\
             fn setup() {}\nfn fresh() { inner(); }\nfn inner() {}\nfn finish() {}\n\
             struct Attempt;\nimpl Attempt { fn new() -> Self { Attempt } }\n\
             impl Callback for Attempt {\n    fn on_save(&self) { inner(); }\n    fn on_log(&self) {}\n}\n",
        )
        .unwrap();
        let index = SymbolIndex::build(&root, &["src/main.rs".to_string()]);
        let numbered =
            |kind: LineKind, content: &str, old: Option<u32>, new: Option<u32>| DiffLine {
                old_lineno: old,
                new_lineno: new,
                ..line(kind, content)
            };
        let file = file(
            "src/main.rs",
            FileStatus::Modified,
            vec![
                Hunk {
                    header: "@@ -1,5 +1,6 @@".to_string(),
                    lines: vec![
                        numbered(LineKind::Context, "fn main() {", Some(1), Some(1)),
                        numbered(LineKind::Context, "    setup();", Some(2), Some(2)),
                        numbered(LineKind::Deletion, "    old();", Some(3), None),
                        numbered(LineKind::Addition, "    fresh();", None, Some(3)),
                        numbered(LineKind::Context, "    finish();", Some(4), Some(4)),
                        numbered(
                            LineKind::Addition,
                            "    let a = Attempt::new();",
                            None,
                            Some(5),
                        ),
                        numbered(LineKind::Context, "}", Some(5), Some(6)),
                    ],
                },
                Hunk {
                    header: "@@ -8,1 +8,2 @@".to_string(),
                    lines: vec![
                        numbered(LineKind::Deletion, "fn old() {}", Some(8), None),
                        numbered(LineKind::Addition, "fn fresh() { inner(); }", None, Some(8)),
                        numbered(LineKind::Addition, "fn inner() {}", None, Some(9)),
                    ],
                },
                Hunk {
                    header: "@@ -11,0 +12,5 @@".to_string(),
                    lines: vec![
                        numbered(
                            LineKind::Addition,
                            "impl Attempt { fn new() -> Self { Attempt } }",
                            None,
                            Some(12),
                        ),
                        numbered(
                            LineKind::Addition,
                            "impl Callback for Attempt {",
                            None,
                            Some(13),
                        ),
                        numbered(
                            LineKind::Addition,
                            "    fn on_save(&self) { inner(); }",
                            None,
                            Some(14),
                        ),
                        numbered(
                            LineKind::Addition,
                            "    fn on_log(&self) {}",
                            None,
                            Some(15),
                        ),
                        numbered(LineKind::Addition, "}", None, Some(16)),
                    ],
                },
            ],
        );
        let rows = build_flow(&[file], &index);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            render_flow(&rows),
            [
                "~  main",
                "     setup",
                "-    old",
                "+    fresh",
                "+      inner",
                "     finish",
                "+    Attempt::new",
                "+      ↳ Attempt::on_save  ← Callback",
                "+        inner",
                "+      ↳ Attempt::on_log  ← Callback",
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
                    DiffLine {
                        new_lineno: Some(4),
                        old_lineno: None,
                        ..line(LineKind::Addition, "fn leaf() {}")
                    },
                    DiffLine {
                        new_lineno: Some(5),
                        old_lineno: None,
                        ..line(LineKind::Addition, "fn orphan() {}")
                    },
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
