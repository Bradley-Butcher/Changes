//! A repository-wide index of function definitions and call sites, built with tree-sitter.
//!
//! Resolution is by name, not by type: `run()` matches every `run` in the repo. That is
//! the same trade-off GitHub's code navigation makes, and it is fast enough to keep up
//! with a live diff. Results carry counts and locations so a wrong hit is easy to spot.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

/// Files larger than this are skipped: they are generated or vendored in practice.
const MAX_INDEXED_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Function,
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    /// Bare identifier, the key calls resolve against.
    pub name: String,
    /// Qualified for display: `App::open_outline`, `Widget.render`.
    pub display: String,
    /// The impl / class / module the definition sits in, when nested.
    pub container: Option<String>,
    pub kind: DefKind,
    /// A test: `#[test]`, inside `mod tests`, `test_*`, or in a test file. Tests call
    /// everything, so they are excluded from routes and from "no callers".
    pub is_test: bool,
    /// What calls this when nothing in the repository does: the base class or trait a
    /// method overrides (`TrainerCallback`, `Handler`), or the runtime for a dunder.
    pub hook_of: Option<String>,
    pub path: String,
    /// 1-based line of the declaration.
    pub line: u32,
    pub end_line: u32,
    start_byte: usize,
    end_byte: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The called name as written: `parse` for both `parse(x)` and `self.parse(x)`.
    pub name: String,
    pub path: String,
    pub line: u32,
    /// Index into the same file's `defs` of the innermost enclosing definition.
    pub enclosing: Option<usize>,
    /// The receiver type when the call site states it: `Foo` for `Foo::bar()`, the
    /// enclosing container for `self.bar()` / `this.bar()`. None for `x.bar()`.
    pub qualifier: Option<String>,
    /// Written as a method call on some receiver (`x.bar()`), not a bare `bar()`.
    pub is_method: bool,
    /// The name is used as a value rather than called: passed as an argument, stored in
    /// a table, assigned. Route tables and callbacks are wired this way.
    pub is_reference: bool,
    /// A Rust macro invocation, which needs no import to reach its definition.
    pub is_macro: bool,
    /// Written with an explicit path (`a::b()`), so no import of `b` is needed.
    pub is_scoped: bool,
    /// The receiver when it is a plain name: `unittest` in `unittest.main()`, `ns` in
    /// `ns.render()`. Whether that name was imported says whether the call can reach
    /// a free function in another module or is a method on some object.
    pub receiver: Option<String>,
}

impl Call {
    /// Whether this call could target `def`, given what the call site says. Only within
    /// one language: a `run()` in a shell script says nothing about a Python `run`. A
    /// stated receiver type must match the definition's container. A bare `bar()` never
    /// hits a method, and in Rust or Go `x.bar()` never hits a free function; Python and
    /// JavaScript allow `module.bar()`, so there an unknown receiver keeps both open.
    fn may_target(&self, def: &Def) -> bool {
        if language_for_path(&self.path) != language_for_path(&def.path) {
            return false;
        }
        match (&self.qualifier, &def.container) {
            (Some(qualifier), Some(container)) => qualifier == container,
            (Some(_), None) => false,
            (None, Some(_)) => self.is_method,
            (None, None) => {
                !self.is_method
                    || matches!(
                        language_for_path(&self.path),
                        Some(Lang::Python | Lang::JavaScript | Lang::TypeScript | Lang::Tsx)
                    )
            }
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileSymbols {
    pub path: String,
    pub defs: Vec<Def>,
    pub calls: Vec<Call>,
    /// Names brought in by `use` / `import` statements, so a bare call can be tied
    /// to a definition in another file. `*` marks a glob import.
    pub imports: Vec<Import>,
}

/// One imported name and where it was imported from: `crate::git::diff`, `..models`,
/// `./util`. The module narrows which same-named definition the import can mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub name: String,
    pub module: String,
}

impl Import {
    /// Could this import, written in `from_path`, bring in the definition at `def_path`?
    /// Rust `crate::` and `super::` stay within one crate and an external crate name
    /// must appear in the definition's path; Python dotted modules must appear as
    /// directories; relative JavaScript paths resolve against the importing file. Other
    /// forms (package aliases, bare package names) cannot be checked and pass.
    fn reaches(&self, from_path: &str, def_path: &str) -> bool {
        let module = self.module.as_str();
        match language_for_path(from_path) {
            Some(Lang::Rust) => {
                let first = module.split("::").next().unwrap_or("");
                match first {
                    "crate" | "super" | "self" | "" => {
                        crate_root(from_path) == crate_root(def_path)
                    }
                    name => {
                        let wanted = name.replace('-', "_");
                        crate_root(from_path) == crate_root(def_path)
                            || def_path
                                .split('/')
                                .any(|component| component.replace('-', "_") == wanted)
                    }
                }
            }
            Some(Lang::Python) => {
                let dots = module.len() - module.trim_start_matches('.').len();
                let rest = module.trim_start_matches('.');
                if dots > 0 {
                    // `.x` is a sibling module, `..x` the parent package's.
                    let mut dir = Path::new(from_path).parent().unwrap_or(Path::new(""));
                    for _ in 1..dots {
                        dir = dir.parent().unwrap_or(Path::new(""));
                    }
                    return Path::new(def_path).starts_with(dir);
                }
                if rest.is_empty() {
                    return true;
                }
                let as_dirs = format!("/{}", rest.replace('.', "/"));
                let in_def = format!("/{def_path}");
                in_def.contains(&format!("{as_dirs}/")) || in_def.contains(&format!("{as_dirs}."))
            }
            Some(Lang::JavaScript | Lang::TypeScript | Lang::Tsx) if module.starts_with('.') => {
                let dir = Path::new(from_path).parent().unwrap_or(Path::new(""));
                let mut resolved = dir.to_path_buf();
                for segment in module.split('/') {
                    match segment {
                        "." | "" => {}
                        ".." => {
                            resolved.pop();
                        }
                        other => resolved.push(other),
                    }
                }
                let resolved = resolved.to_string_lossy().to_string();
                let def_stem = def_path
                    .rsplit_once('.')
                    .map(|(stem, _)| stem)
                    .unwrap_or(def_path);
                def_stem == resolved
                    || def_stem.strip_suffix("/index") == Some(resolved.as_str())
                    || def_stem.starts_with(&format!("{resolved}/"))
            }
            _ => true,
        }
    }
}

/// The `src` directory a Rust file belongs to (`crates/app/src`), or the top level.
fn crate_root(path: &str) -> &str {
    match path.find("/src/") {
        Some(pos) => &path[..pos + 4],
        None => match path.strip_prefix("src/") {
            Some(_) => "src",
            None => "",
        },
    }
}

/// One resolved call inside a function, for drawing its call tree in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite<'a> {
    pub name: String,
    pub line: u32,
    pub is_reference: bool,
    pub targets: Vec<&'a Def>,
}

/// A call site pointing at some definition, with the function it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub path: String,
    pub line: u32,
    /// Display name of the function containing the call, if any.
    pub from: Option<String>,
    /// Bare name of that function, for matching against the diff's symbols.
    pub from_name: Option<String>,
    /// The call sits in a test function or test file.
    pub from_test: bool,
    /// The name is used as a value there, not called.
    pub is_reference: bool,
}

/// One function on a route from an entry point down to a changed function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStep {
    pub display: String,
    /// Bare identifier; empty for top-level code outside any function.
    pub name: String,
    pub path: String,
    pub line: u32,
    /// The call from this step to the next one down was matched by name alone and
    /// several definitions could be its target.
    pub ambiguous: bool,
}

/// A route from a root of the call graph (something nothing calls, or top-level code)
/// down to a function, highest first. `complete` is false when the walk hit the depth
/// limit before reaching a root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPath {
    pub steps: Vec<PathStep>,
    pub complete: bool,
}

impl CallPath {
    /// Edges matched by name alone; fewer means a more trustworthy route.
    pub fn ambiguous_edges(&self) -> usize {
        self.steps.iter().filter(|s| s.ambiguous).count()
    }
}

/// Longest route followed upward before giving up on reaching a root. Product routes
/// through worker threads and dispatch layers are commonly ten deep.
pub const MAX_PATH_DEPTH: usize = 12;
/// Upper bound on call-graph nodes visited per function, so a hub cannot stall a build.
const MAX_PATH_NODES: usize = 4_000;

#[derive(Debug, Default, Clone)]
pub struct SymbolIndex {
    files: HashMap<String, Arc<FileSymbols>>,
    defs_by_name: HashMap<String, Vec<(String, usize)>>,
    calls_by_name: HashMap<String, Vec<(String, usize)>>,
}

impl SymbolIndex {
    /// Index every supported file under `root` from the given repo-relative paths.
    pub fn build(root: &Path, paths: &[String]) -> Self {
        let mut index = Self::default();
        index.reindex(root, paths);
        index
    }

    /// A copy of this index with `paths` re-parsed (or dropped if they no longer exist).
    /// Per-file data is shared, so this is cheap even for large repositories.
    pub fn with_updated_files(&self, root: &Path, paths: &[String]) -> Self {
        let mut index = self.clone();
        index.reindex(root, paths);
        index
    }

    fn reindex(&mut self, root: &Path, paths: &[String]) {
        // Lookups are maintained per file, so a refresh costs only the changed files:
        // a full rebuild over tens of thousands of files would be felt on every edit.
        for path in paths {
            if let Some(old) = self.files.remove(path) {
                self.remove_from_lookups(&old);
            }
        }
        let parsed = parse_files(root, paths);
        for file in parsed {
            self.add_to_lookups(&file);
            self.files.insert(file.path.clone(), Arc::new(file));
        }
    }

    fn add_to_lookups(&mut self, file: &FileSymbols) {
        for (idx, def) in file.defs.iter().enumerate() {
            self.defs_by_name
                .entry(def.name.clone())
                .or_default()
                .push((file.path.clone(), idx));
        }
        for (idx, call) in file.calls.iter().enumerate() {
            self.calls_by_name
                .entry(call.name.clone())
                .or_default()
                .push((file.path.clone(), idx));
        }
    }

    fn remove_from_lookups(&mut self, file: &FileSymbols) {
        for def in &file.defs {
            if let Some(entries) = self.defs_by_name.get_mut(&def.name) {
                entries.retain(|(path, _)| path != &file.path);
                if entries.is_empty() {
                    self.defs_by_name.remove(&def.name);
                }
            }
        }
        for call in &file.calls {
            if let Some(entries) = self.calls_by_name.get_mut(&call.name) {
                entries.retain(|(path, _)| path != &file.path);
                if entries.is_empty() {
                    self.calls_by_name.remove(&call.name);
                }
            }
        }
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn def_count(&self) -> usize {
        self.files.values().map(|f| f.defs.len()).sum()
    }

    /// The innermost function whose span contains line `line` of `path`, if any.
    pub fn function_at(&self, path: &str, line: u32) -> Option<&Def> {
        self.files
            .get(path)?
            .defs
            .iter()
            .filter(|def| def.kind == DefKind::Function && def.line <= line && line <= def.end_line)
            .min_by_key(|def| def.end_line - def.line)
    }

    /// Definitions of `name`, the one in `prefer_path` first.
    pub fn defs_named(&self, name: &str, prefer_path: &str) -> Vec<&Def> {
        let mut defs: Vec<&Def> = self
            .defs_by_name
            .get(name)
            .map(|entries| {
                entries
                    .iter()
                    .map(|(path, idx)| &self.files[path].defs[*idx])
                    .collect()
            })
            .unwrap_or_default();
        defs.sort_by_key(|def| def.path != prefer_path);
        defs
    }

    /// Whether a bare (unqualified, non-method) call in `file` can reach `def` in some
    /// other file. A local definition of the same name wins, as it does in every
    /// supported language. Rust, Python and JavaScript then need the name imported;
    /// Go shares names across a package's files, and Rust macros travel on their own.
    fn bare_call_reaches(&self, call: &Call, file: &FileSymbols, def: &Def) -> bool {
        if call.qualifier.is_some() || call.is_scoped {
            return true;
        }
        if call.is_method {
            // `unittest.main()` reaches a free `main` only if `unittest` was imported
            // from where that `main` lives; on anything else it is a method call.
            return match (&call.receiver, &def.container) {
                (Some(receiver), None) => {
                    file.imports.is_empty()
                        && !matches!(language_for_path(&file.path), Some(Lang::Python))
                        || file.imports.iter().any(|import| {
                            &import.name == receiver && import.reaches(&file.path, &def.path)
                        })
                }
                _ => true,
            };
        }
        if file.path == def.path {
            return true;
        }
        if file
            .defs
            .iter()
            .any(|d| d.name == call.name && d.kind == DefKind::Function)
        {
            return false;
        }
        if call.is_macro {
            return true;
        }
        match language_for_path(&file.path) {
            Some(Lang::Go) | None => true,
            Some(_) => file.imports.iter().any(|import| {
                (import.name == "*" || import.name == call.name)
                    && import.reaches(&file.path, &def.path)
            }),
        }
    }

    /// Function definitions a call could target, after every rule this index knows.
    /// For `x.finish()` on an unknown receiver, methods of classes the calling file
    /// defines or imports win over same-named methods of classes it never mentions.
    fn targets_of<'a>(&'a self, call: &Call, file: &FileSymbols) -> Vec<&'a Def> {
        let targets: Vec<&Def> = self
            .defs_named(&call.name, &file.path)
            .into_iter()
            .filter(|def| {
                def.kind == DefKind::Function
                    && call.may_target(def)
                    && self.bare_call_reaches(call, file, def)
            })
            .collect();
        if !call.is_method || call.qualifier.is_some() || targets.len() < 2 {
            return targets;
        }
        let known = |def: &Def| {
            def.path == file.path
                || def.container.as_deref().is_some_and(|class| {
                    file.imports.iter().any(|import| import.name == class)
                        || file
                            .defs
                            .iter()
                            .any(|d| d.name == class && d.kind == DefKind::Type)
                })
        };
        if targets.iter().any(|def| known(def)) {
            targets.into_iter().filter(|def| known(def)).collect()
        } else {
            targets
        }
    }

    /// Every call site that may target `name` (or `def` precisely, when known), outside
    /// the definition itself. Same-file callers first.
    pub fn callers(&self, name: &str, def_path: &str, def: Option<&Def>) -> Vec<Caller> {
        // A constructor is invoked through its class: `Recorder(...)`, `new Store()`.
        let constructor_class = def
            .filter(|d| matches!(d.name.as_str(), "__init__" | "__new__" | "constructor"))
            .and_then(|d| d.container.clone());
        let mut entries: Vec<&(String, usize)> = self
            .calls_by_name
            .get(name)
            .map(|v| v.iter().collect())
            .unwrap_or_default();
        if let Some(class) = &constructor_class
            && let Some(class_calls) = self.calls_by_name.get(class)
        {
            entries.extend(class_calls.iter());
        }
        if entries.is_empty() {
            return Vec::new();
        }
        let def_line = def.map(|d| d.line);
        let mut callers: Vec<Caller> = entries
            .into_iter()
            .filter_map(|(path, idx)| {
                let file = &self.files[path];
                let call = &file.calls[*idx];
                let is_class_call = constructor_class.as_deref() == Some(call.name.as_str());
                if let Some(def) = def
                    && !is_class_call
                    && !call.may_target(def)
                {
                    return None;
                }
                if def.is_none() && language_for_path(path) != language_for_path(def_path) {
                    return None;
                }
                if let Some(def) = def
                    && !is_class_call
                    && !self.bare_call_reaches(call, file, def)
                {
                    return None;
                }
                let enclosing = call.enclosing.map(|i| &file.defs[i]);
                // A recursive call from inside the definition is not a caller.
                if let (Some(enclosing), Some(def_line)) = (enclosing, def_line)
                    && enclosing.path == def_path
                    && enclosing.line == def_line
                {
                    return None;
                }
                Some(Caller {
                    path: path.clone(),
                    line: call.line,
                    from: enclosing.map(|d| d.display.clone()),
                    from_name: enclosing.map(|d| d.name.clone()),
                    from_test: enclosing.is_some_and(|d| d.is_test) || is_test_path(path),
                    is_reference: call.is_reference,
                })
            })
            .collect();
        callers.sort_by(|a, b| {
            (a.path != def_path, &a.path, a.line).cmp(&(b.path != def_path, &b.path, b.line))
        });
        callers
    }

    /// Call sites that still name a function the diff removed from `path`, resolved as
    /// if it were still defined there: a bare call elsewhere must import it from that
    /// module, so an unrelated `main` in another script does not count.
    pub fn callers_of_removed(&self, name: &str, path: &str) -> Vec<Caller> {
        let ghost = Def {
            name: name.to_string(),
            display: name.to_string(),
            container: None,
            kind: DefKind::Function,
            is_test: false,
            hook_of: None,
            path: path.to_string(),
            line: 0,
            end_line: 0,
            start_byte: 0,
            end_byte: 0,
        };
        self.callers(name, path, Some(&ghost))
    }

    /// Callers that are not tests.
    pub fn production_callers(&self, name: &str, def_path: &str, def: Option<&Def>) -> Vec<Caller> {
        self.callers(name, def_path, def)
            .into_iter()
            .filter(|caller| !caller.from_test)
            .collect()
    }

    /// Distinct non-test functions containing calls that may target `def`, plus a
    /// pseudo-step for top-level code. Excludes `def` itself.
    fn calling_defs(&self, def: &Def) -> Vec<PathStep> {
        let mut steps: Vec<PathStep> = Vec::new();
        for caller in self.production_callers(&def.name, &def.path, Some(def)) {
            // Could this call site have meant a different `name`? Recover the call to
            // ask what it says about its receiver.
            let ambiguous = self
                .files
                .get(&caller.path)
                .and_then(|file| {
                    file.calls
                        .iter()
                        .find(|c| c.name == def.name && c.line == caller.line)
                })
                .is_some_and(|call| {
                    call.qualifier.is_none()
                        && self
                            .files
                            .get(&caller.path)
                            .is_some_and(|file| self.targets_of(call, file).len() > 1)
                });
            let step = match &caller.from {
                Some(from) => {
                    let Some(enclosing) = self.files.get(&caller.path).and_then(|file| {
                        file.defs.iter().find(|d| {
                            d.display == *from && d.line <= caller.line && caller.line <= d.end_line
                        })
                    }) else {
                        continue;
                    };
                    PathStep {
                        display: enclosing.display.clone(),
                        name: enclosing.name.clone(),
                        path: enclosing.path.clone(),
                        line: enclosing.line,
                        ambiguous,
                    }
                }
                None => PathStep {
                    display: format!(
                        "{} (top level)",
                        caller.path.rsplit('/').next().unwrap_or(&caller.path)
                    ),
                    name: String::new(),
                    path: caller.path.clone(),
                    line: caller.line,
                    ambiguous,
                },
            };
            if !steps
                .iter()
                .any(|s| s.path == step.path && s.line == step.line)
            {
                steps.push(step);
            }
        }
        steps
    }

    /// Routes from call-graph roots down to `def`, shortest first, at most `max_paths`.
    /// A breadth-first walk over callers; cycles and hubs are bounded by a visited set
    /// and `MAX_PATH_NODES`. With no complete route within `MAX_PATH_DEPTH`, the longest
    /// partial routes are returned flagged incomplete.
    pub fn paths_to_roots(&self, def: &Def, max_paths: usize) -> Vec<CallPath> {
        use std::collections::VecDeque;
        let start = PathStep {
            display: def.display.clone(),
            name: def.name.clone(),
            path: def.path.clone(),
            line: def.line,
            ambiguous: false,
        };
        // Chains are stored deepest-first (the changed function at index 0) and reversed
        // on output so paths read highest → deepest.
        let mut queue: VecDeque<Vec<PathStep>> = VecDeque::from([vec![start]]);
        let mut visited: std::collections::HashSet<(String, u32)> =
            std::collections::HashSet::from([(def.path.clone(), def.line)]);
        let mut complete = Vec::new();
        let mut partial = Vec::new();
        let mut explored = 0usize;

        while let Some(chain) = queue.pop_front() {
            if complete.len() >= max_paths || explored > MAX_PATH_NODES {
                break;
            }
            explored += 1;
            let top = chain.last().expect("chains are never empty");
            let callers = if top.name.is_empty() {
                Vec::new() // top-level code is a root
            } else {
                let as_def = self.defs_named(&top.name, &top.path);
                as_def
                    .into_iter()
                    .find(|d| d.path == top.path && d.line == top.line)
                    .map(|d| self.calling_defs(d))
                    .unwrap_or_default()
            };
            if callers.is_empty() {
                let mut steps = chain.clone();
                steps.reverse();
                complete.push(CallPath {
                    steps,
                    complete: true,
                });
                continue;
            }
            if chain.len() >= MAX_PATH_DEPTH {
                let mut steps = chain.clone();
                steps.reverse();
                partial.push(CallPath {
                    steps,
                    complete: false,
                });
                continue;
            }
            for caller in callers {
                if !visited.insert((caller.path.clone(), caller.line)) {
                    continue;
                }
                let mut next = chain.clone();
                next.push(caller);
                queue.push_back(next);
            }
        }
        if complete.is_empty() {
            partial.truncate(max_paths);
            partial
        } else {
            complete
        }
    }

    /// Every call site inside `def` that resolves to a function in this index, in source
    /// order. A call to a class (`Attempt(...)`, `new Store()`, `Store::new()`) resolves
    /// to its constructor. Library calls drop out; recursion is skipped.
    pub fn call_sites(&self, def: &Def) -> Vec<CallSite<'_>> {
        let Some(file) = self.files.get(&def.path) else {
            return Vec::new();
        };
        let Some(def_idx) = file
            .defs
            .iter()
            .position(|candidate| candidate.line == def.line && candidate.name == def.name)
        else {
            return Vec::new();
        };
        let mut sites = Vec::new();
        for call in file.calls.iter().filter(|c| c.enclosing == Some(def_idx)) {
            if call.name == def.name {
                continue;
            }
            let mut targets = self.targets_of(call, file);
            if targets.is_empty() && !call.is_method {
                // `Attempt(...)`: the class is the target, its constructor the code run.
                targets = self
                    .defs_named(&call.name, &def.path)
                    .into_iter()
                    .filter(|d| d.kind == DefKind::Type && call.may_target(d))
                    .filter_map(|class| self.constructor_of(class))
                    .collect();
            }
            if targets.is_empty() {
                continue;
            }
            sites.push(CallSite {
                name: call.name.clone(),
                line: call.line,
                is_reference: call.is_reference,
                targets,
            });
        }
        sites
    }

    /// Calls made by a file's top-level code (a `__main__` block, a route table), in
    /// source order, resolved like `call_sites`.
    pub fn top_level_call_sites(&self, path: &str) -> Vec<CallSite<'_>> {
        let Some(file) = self.files.get(path) else {
            return Vec::new();
        };
        let mut sites = Vec::new();
        for call in file.calls.iter().filter(|c| c.enclosing.is_none()) {
            let mut targets = self.targets_of(call, file);
            if targets.is_empty() && !call.is_method {
                targets = self
                    .defs_named(&call.name, path)
                    .into_iter()
                    .filter(|d| d.kind == DefKind::Type && call.may_target(d))
                    .filter_map(|class| self.constructor_of(class))
                    .collect();
            }
            if targets.is_empty() {
                continue;
            }
            sites.push(CallSite {
                name: call.name.clone(),
                line: call.line,
                is_reference: call.is_reference,
                targets,
            });
        }
        sites
    }

    /// Every name called inside `def` as the code stands, resolved or not.
    pub fn call_names_in(&self, def: &Def) -> std::collections::HashSet<String> {
        let Some(file) = self.files.get(&def.path) else {
            return std::collections::HashSet::new();
        };
        let Some(def_idx) = file
            .defs
            .iter()
            .position(|candidate| candidate.line == def.line && candidate.name == def.name)
        else {
            return std::collections::HashSet::new();
        };
        file.calls
            .iter()
            .filter(|c| c.enclosing == Some(def_idx))
            .map(|c| c.name.clone())
            .collect()
    }

    /// The constructor of a class, when it defines one.
    pub fn constructor_of(&self, class: &Def) -> Option<&Def> {
        self.files.get(&class.path)?.defs.iter().find(|d| {
            d.kind == DefKind::Function
                && d.container.as_deref() == Some(class.name.as_str())
                && matches!(
                    d.name.as_str(),
                    "__init__" | "__new__" | "new" | "constructor"
                )
        })
    }

    /// Methods of the class `constructor` builds that a framework calls: overrides of
    /// a base class or trait (or dunders) which nothing in the repository calls.
    pub fn hook_methods(&self, constructor: &Def) -> Vec<&Def> {
        let Some(class) = constructor.container.as_deref() else {
            return Vec::new();
        };
        let Some(file) = self.files.get(&constructor.path) else {
            return Vec::new();
        };
        file.defs
            .iter()
            .filter(|d| {
                d.kind == DefKind::Function
                    && d.container.as_deref() == Some(class)
                    && d.line != constructor.line
                    && d.hook_of.is_some()
                    && self
                        .production_callers(&d.name, &d.path, Some(d))
                        .is_empty()
            })
            .collect()
    }

    /// Names called from inside `def` that resolve to a definition in this index, in
    /// first-call order and deduplicated. Library calls (`push`, `format`) drop out.
    pub fn callees(&self, def: &Def) -> Vec<(String, Vec<&Def>)> {
        let Some(file) = self.files.get(&def.path) else {
            return Vec::new();
        };
        let Some(def_idx) = file
            .defs
            .iter()
            .position(|candidate| candidate.line == def.line && candidate.name == def.name)
        else {
            return Vec::new();
        };
        let mut seen = Vec::new();
        let mut callees = Vec::new();
        for call in file.calls.iter().filter(|c| c.enclosing == Some(def_idx)) {
            if seen.contains(&call.name) || call.name == def.name {
                continue;
            }
            let targets = self.targets_of(call, file);
            if targets.is_empty() {
                continue;
            }
            seen.push(call.name.clone());
            callees.push((call.name.clone(), targets));
        }
        callees
    }
}

/// Repo-relative paths worth indexing: tracked files plus untracked ones git doesn't
/// ignore, since an agent's brand-new file is exactly what needs wiring up.
pub fn indexable_paths(repo: &git2::Repository) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    if let Ok(index) = repo.index() {
        for entry in index.iter() {
            let path = String::from_utf8_lossy(&entry.path).to_string();
            if language_for_path(&path).is_some() {
                paths.push(path);
            }
        }
    }
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .exclude_submodules(true);
    if let Ok(statuses) = repo.statuses(Some(&mut options)) {
        for entry in statuses.iter() {
            if entry.status().contains(git2::Status::WT_NEW)
                && let Some(path) = entry.path()
                && language_for_path(path).is_some()
            {
                paths.push(path.to_string());
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Lang {
    Rust,
    Python,
    Go,
    JavaScript,
    TypeScript,
    Tsx,
}

/// Dependency and build output directories: never part of the change under review, and
/// large enough to dominate indexing when an untracked app lacks a `.gitignore` yet.
pub fn is_vendored_path(path: &str) -> bool {
    path.split('/').any(|segment| {
        matches!(
            segment,
            "node_modules"
                | "vendor"
                | "target"
                | "dist"
                | "build"
                | ".venv"
                | "venv"
                | "site-packages"
                | "__pycache__"
                | ".next"
                | ".nuxt"
                | ".react-router"
                | ".turbo"
                | ".cache"
                | "coverage"
        )
    })
}

fn language_for_path(path: &str) -> Option<Lang> {
    if is_vendored_path(path) {
        return None;
    }
    let ext = Path::new(path).extension()?.to_str()?;
    Some(match ext {
        "rs" => Lang::Rust,
        "py" | "pyi" => Lang::Python,
        "go" => Lang::Go,
        "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
        "ts" | "mts" | "cts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        _ => return None,
    })
}

impl Lang {
    fn grammar(self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    /// Definitions and call sites. Every pattern captures `@name` plus one of
    /// `@def.function`, `@def.type`, or `@call`.
    fn query_source(self) -> &'static str {
        match self {
            Lang::Rust => RUST_QUERY,
            Lang::Python => PYTHON_QUERY,
            Lang::Go => GO_QUERY,
            Lang::JavaScript => JAVASCRIPT_QUERY,
            Lang::TypeScript => TYPESCRIPT_QUERY,
            Lang::Tsx => TSX_QUERY_FULL.get_or_init(|| format!("{TYPESCRIPT_QUERY}{TSX_QUERY}")),
        }
    }

    /// Ancestor node kinds whose name qualifies a nested definition.
    fn container_kinds(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &["impl_item", "trait_item", "mod_item"],
            Lang::Python => &["class_definition"],
            Lang::Go => &[],
            Lang::JavaScript | Lang::TypeScript | Lang::Tsx => &[
                "class_declaration",
                "class",
                "abstract_class_declaration",
                "interface_declaration",
            ],
        }
    }

    fn separator(self) -> &'static str {
        match self {
            Lang::Rust | Lang::Go => "::",
            _ => ".",
        }
    }
}

static TSX_QUERY_FULL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

const RUST_QUERY: &str = r#"
(function_item name: (identifier) @name) @def.function
(function_signature_item name: (identifier) @name) @def.function
(macro_definition name: (identifier) @name) @def.function
(struct_item name: (type_identifier) @name) @def.type
(enum_item name: (type_identifier) @name) @def.type
(union_item name: (type_identifier) @name) @def.type
(trait_item name: (type_identifier) @name) @def.type
(type_item name: (type_identifier) @name) @def.type
(call_expression function: (identifier) @name) @call
(call_expression function: (scoped_identifier path: (_) @qualifier name: (identifier) @name)) @call
(call_expression function: (field_expression value: (_) @receiver field: (field_identifier) @name)) @call
(call_expression function: (generic_function function: (identifier) @name)) @call
(call_expression function: (generic_function function: (scoped_identifier path: (_) @qualifier name: (identifier) @name))) @call
(macro_invocation macro: (identifier) @name) @call
(macro_invocation (token_tree) @macro_body)
(use_declaration) @import
(arguments (identifier) @name) @ref
(arguments (scoped_identifier path: (_) @qualifier name: (identifier) @name)) @ref
(let_declaration value: (identifier) @name) @ref
(field_initializer value: (identifier) @name) @ref
(array_expression (identifier) @name) @ref
(tuple_expression (identifier) @name) @ref
"#;

const PYTHON_QUERY: &str = r#"
(function_definition name: (identifier) @name) @def.function
(class_definition name: (identifier) @name) @def.type
(call function: (identifier) @name) @call
(call function: (attribute object: (_) @receiver attribute: (identifier) @name)) @call
(import_from_statement) @import
(import_statement) @import
(decorator (identifier) @name) @ref
(decorator (attribute attribute: (identifier) @name)) @ref
(argument_list (identifier) @name) @ref
(argument_list (attribute object: (_) @receiver attribute: (identifier) @name)) @ref
(keyword_argument value: (identifier) @name) @ref
(pair value: (identifier) @name) @ref
(pair value: (attribute object: (_) @receiver attribute: (identifier) @name)) @ref
(list (identifier) @name) @ref
(tuple (identifier) @name) @ref
(assignment right: (identifier) @name) @ref
(return_statement (identifier) @name) @ref
"#;

const GO_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(method_declaration name: (field_identifier) @name) @def.function
(type_spec name: (type_identifier) @name) @def.type
(call_expression function: (identifier) @name) @call
(call_expression function: (selector_expression operand: (_) @receiver field: (field_identifier) @name)) @call
(argument_list (identifier) @name) @ref
(argument_list (selector_expression operand: (_) @receiver field: (field_identifier) @name)) @ref
(literal_element (identifier) @name) @ref
(expression_list (identifier) @name) @ref
"#;

const JAVASCRIPT_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(method_definition name: (property_identifier) @name) @def.function
(variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @def.function
(class_declaration name: (identifier) @name) @def.type
(call_expression function: (identifier) @name) @call
(call_expression function: (member_expression object: (_) @receiver property: (property_identifier) @name)) @call
(new_expression constructor: (identifier) @name) @call
(import_statement) @import
(arguments (identifier) @name) @ref
(arguments (member_expression object: (_) @receiver property: (property_identifier) @name)) @ref
(pair value: (identifier) @name) @ref
(pair value: (member_expression object: (_) @receiver property: (property_identifier) @name)) @ref
(array (identifier) @name) @ref
(variable_declarator value: (identifier) @name) @ref
(assignment_expression right: (identifier) @name) @ref
(jsx_expression (identifier) @name) @ref
(jsx_expression (member_expression object: (_) @receiver property: (property_identifier) @name)) @ref
"#;

const TYPESCRIPT_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(method_definition name: (property_identifier) @name) @def.function
(method_signature name: (property_identifier) @name) @def.function
(function_signature name: (identifier) @name) @def.function
(variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @def.function
(class_declaration name: (type_identifier) @name) @def.type
(abstract_class_declaration name: (type_identifier) @name) @def.type
(interface_declaration name: (type_identifier) @name) @def.type
(type_alias_declaration name: (type_identifier) @name) @def.type
(enum_declaration name: (identifier) @name) @def.type
(call_expression function: (identifier) @name) @call
(call_expression function: (member_expression object: (_) @receiver property: (property_identifier) @name)) @call
(new_expression constructor: (identifier) @name) @call
(import_statement) @import
(arguments (identifier) @name) @ref
(arguments (member_expression object: (_) @receiver property: (property_identifier) @name)) @ref
(pair value: (identifier) @name) @ref
(pair value: (member_expression object: (_) @receiver property: (property_identifier) @name)) @ref
(array (identifier) @name) @ref
(variable_declarator value: (identifier) @name) @ref
(assignment_expression right: (identifier) @name) @ref
"#;

/// TSX adds the JSX attribute positions (`onClick={handler}`) to the TypeScript query.
const TSX_QUERY: &str = r#"
(jsx_expression (identifier) @name) @ref
(jsx_expression (member_expression object: (_) @receiver property: (property_identifier) @name)) @ref
"#;

/// Parse the listed files, spreading the work across threads for large batches.
fn parse_files(root: &Path, paths: &[String]) -> Vec<FileSymbols> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    if paths.len() < 32 || threads < 2 {
        let mut parsers = Parsers::default();
        return paths
            .iter()
            .filter_map(|path| parse_file(&mut parsers, root, path))
            .collect();
    }
    let chunk_size = paths.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let handles: Vec<_> = paths
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut parsers = Parsers::default();
                    chunk
                        .iter()
                        .filter_map(|path| parse_file(&mut parsers, root, path))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("symbol indexer panicked"))
            .collect()
    })
}

/// One parser and compiled query per language, reused across files on a thread.
#[derive(Default)]
struct Parsers {
    by_lang: HashMap<Lang, (Parser, Query)>,
}

impl Parsers {
    fn get(&mut self, lang: Lang) -> &mut (Parser, Query) {
        self.by_lang.entry(lang).or_insert_with(|| {
            let grammar = lang.grammar();
            let mut parser = Parser::new();
            parser
                .set_language(&grammar)
                .expect("grammar matches linked tree-sitter version");
            let query = Query::new(&grammar, lang.query_source()).expect("query compiles");
            (parser, query)
        })
    }
}

fn parse_file(parsers: &mut Parsers, root: &Path, path: &str) -> Option<FileSymbols> {
    let lang = language_for_path(path)?;
    let full = root.join(path);
    let metadata = std::fs::metadata(&full).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_INDEXED_FILE_BYTES {
        return None;
    }
    let source = std::fs::read(&full).ok()?;
    Some(parse_source(parsers, lang, path, &source))
}

fn parse_source(parsers: &mut Parsers, lang: Lang, path: &str, source: &[u8]) -> FileSymbols {
    let (parser, query) = parsers.get(lang);
    let Some(tree) = parser.parse(source, None) else {
        return FileSymbols {
            path: path.to_string(),
            ..Default::default()
        };
    };
    let name_capture = query.capture_index_for_name("name");
    let qualifier_capture = query.capture_index_for_name("qualifier");
    let receiver_capture = query.capture_index_for_name("receiver");
    let capture_names = query.capture_names();

    let mut defs: Vec<Def> = Vec::new();
    let mut imports: Vec<Import> = Vec::new();
    // name, line, byte offset, receiver qualifier, written as a method call, a reference
    let mut raw_calls: Vec<RawCall> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        let mut name: Option<&str> = None;
        let mut node: Option<(Node, &str)> = None;
        let mut qualifier = Qualifier::Unknown;
        let mut is_method = false;
        let mut is_scoped = false;
        let mut receiver: Option<String> = None;
        for capture in m.captures() {
            if Some(capture.index) == name_capture {
                name = capture.node.utf8_text(source).ok();
            } else if Some(capture.index) == qualifier_capture {
                is_scoped = true;
                // `a::b::Foo::bar()` — the type is the last path segment.
                if let Ok(text) = capture.node.utf8_text(source) {
                    let last = text.rsplit("::").next().unwrap_or(text);
                    let last = strip_generics(last);
                    // `Type::f()` names a receiver type; `module::f()` says nothing
                    // about it. Types are capitalised by convention, modules are not.
                    qualifier = match last.as_str() {
                        "self" | "Self" => Qualifier::SelfType,
                        _ if last.starts_with(|c: char| c.is_uppercase()) => Qualifier::Named(last),
                        _ => Qualifier::Unknown,
                    };
                }
            } else if Some(capture.index) == receiver_capture {
                is_method = true;
                if let Ok(text) = capture.node.utf8_text(source) {
                    qualifier = match text {
                        "self" | "Self" | "this" | "cls" => Qualifier::SelfType,
                        _ => Qualifier::Unknown,
                    };
                    if capture.node.kind() == "identifier" {
                        receiver = Some(text.to_string());
                    }
                }
            } else {
                node = Some((capture.node, capture_names[capture.index as usize]));
            }
        }
        if let Some((node, "macro_body")) = node {
            raw_calls.extend(macro_body_calls(node, source));
            continue;
        }
        if let Some((node, "import")) = node {
            if let Ok(text) = node.utf8_text(source) {
                imports.extend(imported_names(text));
            }
            continue;
        }
        let (Some(name), Some((node, capture_name))) = (name, node) else {
            continue;
        };
        let line = node.start_position().row as u32 + 1;
        match capture_name {
            "call" | "ref" => {
                // The value `x` in `f(x)`, `[x]`, `y = x` is a reference worth keeping
                // only when it could name a function; single letters and constants
                // never do, and a bare value cannot be a method.
                let is_reference = capture_name == "ref";
                if is_reference && !plausible_function_reference(name) {
                    continue;
                }
                raw_calls.push(RawCall {
                    name: name.to_string(),
                    line,
                    offset: node.start_byte(),
                    qualifier,
                    is_method: is_method && !is_reference,
                    is_reference,
                    is_macro: node.kind() == "macro_invocation",
                    is_scoped,
                    receiver: receiver.take(),
                });
            }
            "def.function" | "def.type" => {
                // Rust methods match both a method and a function pattern; keep one.
                if defs
                    .iter()
                    .any(|d| d.start_byte == node.start_byte() && d.name == name)
                {
                    continue;
                }
                let kind = if capture_name == "def.type" {
                    DefKind::Type
                } else {
                    DefKind::Function
                };
                let container = container_name(node, lang, source);
                let display = match &container {
                    Some(container) => format!("{container}{}{name}", lang.separator()),
                    None => name.to_string(),
                };
                let is_test = is_test_path(path)
                    || container.as_deref() == Some("tests")
                    || name.starts_with("test_")
                    || has_test_attribute(node, source);
                let hook_of = if kind == DefKind::Function {
                    hook_of(node, lang, source, name, container.is_some())
                } else {
                    None
                };
                defs.push(Def {
                    name: name.to_string(),
                    display,
                    container,
                    kind,
                    is_test,
                    hook_of,
                    path: path.to_string(),
                    line,
                    end_line: node.end_position().row as u32 + 1,
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                });
            }
            _ => {}
        }
    }
    defs.sort_by_key(|d| d.start_byte);

    // One node can match a call pattern and a reference pattern; keep the call.
    raw_calls.sort_by_key(|raw| (raw.offset, raw.is_reference));
    raw_calls.dedup_by(|a, b| a.name == b.name && a.offset == b.offset);
    let calls = raw_calls
        .into_iter()
        // `x.len()` on an unknown receiver is almost always the standard library, and
        // would otherwise link every collection call to any local method named `len`.
        .filter(|raw| {
            !(matches!(raw.qualifier, Qualifier::Unknown) && is_ubiquitous_method(&raw.name))
        })
        .map(|raw| {
            let enclosing = innermost_def(&defs, raw.offset);
            let qualifier = match raw.qualifier {
                Qualifier::Named(name) => Some(name),
                Qualifier::SelfType => enclosing.and_then(|i| defs[i].container.clone()),
                Qualifier::Unknown => None,
            };
            Call {
                name: raw.name,
                path: path.to_string(),
                line: raw.line,
                enclosing,
                qualifier,
                is_method: raw.is_method,
                is_reference: raw.is_reference,
                is_macro: raw.is_macro,
                is_scoped: raw.is_scoped,
                receiver: raw.receiver,
            }
        })
        .collect();
    imports.sort_by(|a, b| (&a.name, &a.module).cmp(&(&b.name, &b.module)));
    imports.dedup();

    FileSymbols {
        path: path.to_string(),
        defs,
        calls,
        imports,
    }
}

/// The names an import statement brings into scope, with the module they come from;
/// `*` for a glob: `use a::b::{c, d as e}`, `from x import y as z`,
/// `import {a, b as c} from "m"`.
fn imported_names(text: &str) -> Vec<Import> {
    let text = text.trim().trim_end_matches(';');
    let mut out = Vec::new();
    let push = |out: &mut Vec<Import>, names: &str, module: &str| {
        let module = module
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        for piece in names.split([',', '{', '}', '(', ')', '\n']) {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if piece == "*" {
                out.push(Import {
                    name: "*".to_string(),
                    module: module.clone(),
                });
                continue;
            }
            let name = piece.rsplit(" as ").next().unwrap_or(piece).trim();
            let name = name.rsplit("::").next().unwrap_or(name);
            let name = name.rsplit('.').next().unwrap_or(name);
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                out.push(Import {
                    name: name.to_string(),
                    module: module.clone(),
                });
            }
        }
    };
    if let Some(rest) = text.strip_prefix("from ") {
        // Python: `from a.b import c, d as e`
        if let Some((module, names)) = rest.split_once(" import ") {
            push(&mut out, names, module);
        }
    } else if let Some(rest) = text.strip_prefix("import ") {
        // JavaScript: `import {a, b as c} from "./m"`; `import * as ns` is a receiver.
        if let Some((names, module)) = rest.rsplit_once(" from ") {
            let names = names.trim_end_matches(" type").trim_start_matches("type ");
            push(&mut out, names, module);
        } else {
            // Python `import a.b`, `import a.b as c`: the receiver of `c.f()` calls.
            for piece in rest.split(',') {
                let piece = piece.trim();
                let (module, binding) = match piece.split_once(" as ") {
                    Some((module, alias)) => (module.trim(), alias.trim()),
                    None => (piece, piece.split('.').next().unwrap_or(piece)),
                };
                if !binding.is_empty() && binding.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    out.push(Import {
                        name: binding.to_string(),
                        module: module.to_string(),
                    });
                }
            }
        }
    } else if let Some(rest) = text
        .strip_prefix("pub use ")
        .or_else(|| text.strip_prefix("pub(crate) use "))
        .or_else(|| text.strip_prefix("use "))
    {
        // Rust: `use a::b::{c, d::e, *}` — the module is everything before the group.
        let (module, names) = match rest.split_once('{') {
            Some((module, names)) => (module.trim_end_matches("::"), names),
            None => match rest.rsplit_once("::") {
                Some((module, name)) => (module, name),
                None => ("", rest),
            },
        };
        push(&mut out, names, module);
    }
    out
}

/// Calls written inside a Rust macro invocation. tree-sitter parses `format!(...)` and
/// friends as opaque token trees, so `name(`, `Type::name(` and `self.name(` are found by
/// scanning the tokens in order.
/// A call or reference as the parser saw it, before enclosing definitions are known.
struct RawCall {
    name: String,
    line: u32,
    offset: usize,
    qualifier: Qualifier,
    is_method: bool,
    is_reference: bool,
    is_macro: bool,
    /// Written with an explicit path (`a::b()`), which needs no import of `b`.
    is_scoped: bool,
    receiver: Option<String>,
}

fn macro_body_calls(body: Node, source: &[u8]) -> Vec<RawCall> {
    let mut leaves: Vec<Node> = Vec::new();
    collect_leaves(body, &mut leaves);
    let text = |node: &Node| node.utf8_text(source).unwrap_or("");
    let mut calls = Vec::new();
    for (i, leaf) in leaves.iter().enumerate() {
        if leaf.kind() != "identifier" || leaves.get(i + 1).map(|n| n.kind()) != Some("(") {
            continue;
        }
        let name = text(leaf);
        // `foo!(` is a nested macro, and `|x|(` / `if(` are not calls; the grammar
        // already gives keywords their own kinds, so only `!` needs excluding.
        if leaves.get(i + 1).is_some_and(|n| n.kind() == "!") {
            continue;
        }
        let qualifier = match (
            leaves.get(i.wrapping_sub(1)).map(|n| n.kind()),
            leaves.get(i.wrapping_sub(2)),
        ) {
            (Some("::"), Some(owner)) if owner.kind() == "identifier" => {
                let owner = text(owner);
                match owner {
                    "self" | "Self" => Qualifier::SelfType,
                    _ if owner.starts_with(|c: char| c.is_uppercase()) => {
                        Qualifier::Named(owner.to_string())
                    }
                    _ => Qualifier::Unknown,
                }
            }
            (Some("."), Some(receiver)) if receiver.kind() == "self" => Qualifier::SelfType,
            _ => Qualifier::Unknown,
        };
        let is_method = leaves.get(i.wrapping_sub(1)).map(|n| n.kind()) == Some(".");
        calls.push(RawCall {
            name: name.to_string(),
            line: leaf.start_position().row as u32 + 1,
            offset: leaf.start_byte(),
            is_scoped: matches!(qualifier, Qualifier::Named(_)),
            qualifier,
            is_method,
            is_reference: false,
            is_macro: false,
            receiver: None,
        });
    }
    calls
}

/// Could a bare name used as a value be a function? Constants (`MAX`, `None`, `True`),
/// single letters and the usual loop variables are not worth an edge.
fn plausible_function_reference(name: &str) -> bool {
    name.len() > 2
        && name.chars().any(|c| c.is_lowercase())
        && !matches!(
            name,
            "self"
                | "cls"
                | "this"
                | "None"
                | "True"
                | "False"
                | "true"
                | "false"
                | "null"
                | "undefined"
                | "args"
                | "kwargs"
                | "value"
                | "result"
                | "data"
                | "err"
                | "error"
                | "path"
                | "name"
                | "item"
                | "items"
                | "line"
                | "lines"
        )
}

/// Who calls a method nothing in the repository calls: the base class or trait it
/// overrides a method of, or the runtime for a dunder. `None` for plain functions.
fn hook_of(node: Node, lang: Lang, source: &[u8], name: &str, is_method: bool) -> Option<String> {
    if !is_method {
        return None;
    }
    let mut current = node.parent();
    while let Some(ancestor) = current {
        match (lang, ancestor.kind()) {
            (Lang::Rust, "impl_item") => {
                return ancestor
                    .child_by_field_name("trait")
                    .and_then(|t| t.utf8_text(source).ok())
                    .map(strip_generics);
            }
            (Lang::Python, "class_definition") => {
                let bases = ancestor
                    .child_by_field_name("superclasses")
                    .and_then(|b| b.utf8_text(source).ok())
                    .map(|text| {
                        text.trim_matches(|c| c == '(' || c == ')')
                            .trim()
                            .to_string()
                    })
                    .filter(|text| !text.is_empty());
                return match bases {
                    Some(bases) => {
                        Some(bases.split(',').next().unwrap_or(&bases).trim().to_string())
                    }
                    None if name.starts_with("__") && name.ends_with("__") => {
                        Some("the runtime".to_string())
                    }
                    None => None,
                };
            }
            (
                Lang::JavaScript | Lang::TypeScript | Lang::Tsx,
                "class_declaration" | "class" | "abstract_class_declaration",
            ) => {
                let mut cursor = ancestor.walk();
                let heritage = ancestor
                    .children(&mut cursor)
                    .find(|child| child.kind() == "class_heritage");
                return heritage.and_then(|h| {
                    let text = h.utf8_text(source).ok()?;
                    let text = text
                        .trim_start_matches("extends")
                        .trim()
                        .split(|c: char| c == '<' || c == '{' || c == ',' || c.is_whitespace())
                        .next()?
                        .trim();
                    (!text.is_empty()).then(|| strip_generics(text))
                });
            }
            _ => {}
        }
        current = ancestor.parent();
    }
    None
}

/// Rust `#[test]` / `#[tokio::test]` (or any `..._test]`) attribute directly above a
/// function item.
fn has_test_attribute(node: Node, source: &[u8]) -> bool {
    let mut previous = node.prev_sibling();
    while let Some(sibling) = previous {
        if sibling.kind() != "attribute_item" {
            break;
        }
        let text = sibling.utf8_text(source).unwrap_or("");
        if text.contains("test]") || text.contains("test(") || text.starts_with("#[cfg(test)") {
            return true;
        }
        previous = sibling.prev_sibling();
    }
    false
}

/// Test files by convention across the supported languages.
pub fn is_test_path(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    path.starts_with("tests/")
        || path.starts_with("test/")
        || path.starts_with("e2e/")
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/e2e/")
        || path.contains("/__tests__/")
        || path.contains("/testdata/")
        || path.contains("/fixtures/")
        || file.starts_with("test_")
        || file == "conftest.py"
        || file == "tests.rs"
        || file == "test.rs"
        || file.ends_with("_test.py")
        || file.ends_with("_tests.py")
        || file.ends_with("_test.rs")
        || file.ends_with("_tests.rs")
        || file.ends_with("_test.go")
        || file.ends_with(".test.ts")
        || file.ends_with(".test.tsx")
        || file.ends_with(".test.js")
        || file.ends_with(".test.jsx")
        || file.ends_with(".spec.ts")
        || file.ends_with(".spec.tsx")
        || file.ends_with(".spec.js")
}

fn collect_leaves<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>) {
    if node.child_count() == 0 {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_leaves(child, out);
    }
}

/// Method names so common in standard libraries that an unqualified call says nothing
/// about which definition it targets.
fn is_ubiquitous_method(name: &str) -> bool {
    matches!(
        name,
        "len"
            | "is_empty"
            | "new"
            | "default"
            | "get"
            | "set"
            | "push"
            | "pop"
            | "insert"
            | "remove"
            | "contains"
            | "clear"
            | "iter"
            | "into_iter"
            | "map"
            | "filter"
            | "collect"
            | "clone"
            | "to_string"
            | "to_owned"
            | "as_ref"
            | "as_str"
            | "unwrap"
            | "expect"
            | "next"
            | "first"
            | "last"
            | "join"
            | "split"
            | "trim"
            | "find"
            | "write"
            | "read"
            | "send"
            | "append"
            | "extend"
            | "keys"
            | "values"
            | "items"
            | "add"
            | "update"
            | "apply"
            | "call"
            | "then"
            | "catch"
            | "toString"
            | "String"
            | "format"
    )
}

enum Qualifier {
    Unknown,
    /// `self.x()` / `Self::x()` / `this.x()`: resolved to the enclosing container.
    SelfType,
    Named(String),
}

/// Index of the innermost function definition whose span contains `offset`.
fn innermost_def(defs: &[Def], offset: usize) -> Option<usize> {
    defs.iter()
        .enumerate()
        .filter(|(_, def)| {
            def.kind == DefKind::Function && def.start_byte <= offset && offset < def.end_byte
        })
        .min_by_key(|(_, def)| def.end_byte - def.start_byte)
        .map(|(idx, _)| idx)
}

/// Name of the nearest enclosing impl / class / module, used to qualify methods.
fn container_name(node: Node, lang: Lang, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if lang.container_kinds().contains(&ancestor.kind()) {
            let named = ancestor
                .child_by_field_name("type")
                .or_else(|| ancestor.child_by_field_name("name"))?;
            let text = named.utf8_text(source).ok()?;
            return Some(strip_generics(text));
        }
        current = ancestor.parent();
    }
    None
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
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{DefKind, Import, Lang, Parsers, SymbolIndex, imported_names, parse_source};

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "changes-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &std::path::Path, path: &str, source: &str) {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, source).unwrap();
    }

    #[test]
    fn import_statements_yield_names_and_modules() {
        let names = |text: &str| -> Vec<(String, String)> {
            imported_names(text)
                .into_iter()
                .map(|i| (i.name, i.module))
                .collect()
        };
        assert_eq!(
            names("use crate::git::{diff, snapshots::record as rec};"),
            [
                ("diff".to_string(), "crate::git".to_string()),
                ("rec".to_string(), "crate::git".to_string())
            ]
        );
        assert_eq!(
            names("use super::*;"),
            [("*".to_string(), "super".to_string())]
        );
        assert_eq!(
            names("from ..models import User, load as load_user"),
            [
                ("User".to_string(), "..models".to_string()),
                ("load_user".to_string(), "..models".to_string())
            ]
        );
        assert_eq!(
            names("import { parse, render as draw } from './view'"),
            [
                ("parse".to_string(), "./view".to_string()),
                ("draw".to_string(), "./view".to_string())
            ]
        );
        assert_eq!(
            names("import os.path, json as js"),
            [
                ("os".to_string(), "os.path".to_string()),
                ("js".to_string(), "json".to_string())
            ]
        );

        let import = |name: &str, module: &str| Import {
            name: name.to_string(),
            module: module.to_string(),
        };
        // Rust: crate-relative paths stay in the crate; a crate name must be in the path.
        assert!(
            import("change", "super").reaches("crates/app/src/grpc/a.rs", "crates/app/src/lib.rs")
        );
        assert!(
            !import("change", "super")
                .reaches("crates/app/src/grpc/a.rs", "tools/bench/src/main.rs")
        );
        assert!(
            import("change", "bench_tools")
                .reaches("crates/app/src/a.rs", "crates/bench-tools/src/lib.rs")
        );
        // Python: relative imports resolve against the file; dotted ones name directories.
        assert!(import("load", ".models").reaches("pkg/api/views.py", "pkg/api/models.py"));
        assert!(!import("load", ".models").reaches("pkg/api/views.py", "other/models.py"));
        assert!(import("load", "pkg.core.io").reaches("app/main.py", "src/pkg/core/io.py"));
        assert!(!import("load", "pkg.core.io").reaches("app/main.py", "src/pkg/util.py"));
        // JavaScript: relative paths resolve; packages cannot be checked.
        assert!(import("parse", "./view").reaches("web/src/app.ts", "web/src/view.ts"));
        assert!(
            import("parse", "../lib/view")
                .reaches("web/src/pages/a.tsx", "web/src/lib/view/index.ts")
        );
        assert!(!import("parse", "./view").reaches("web/src/app.ts", "web/src/other.ts"));
        assert!(import("parse", "@app/view").reaches("web/src/app.ts", "anywhere.ts"));
    }

    #[test]
    fn bare_calls_need_an_import_and_a_local_definition_wins() {
        let root = temp_root("symbols-imports");
        write(
            &root,
            "tool/a/run.py",
            "def helper():\n    pass\n\ndef main():\n    helper()\n",
        );
        // A copy of the script elsewhere: its `main` calls its own `helper`.
        write(
            &root,
            "tool/b/run.py",
            "def helper():\n    pass\n\ndef main():\n    helper()\n",
        );
        // A caller that imports `helper` from a, and one that names it without importing.
        write(
            &root,
            "tool/use_a.py",
            "from a.run import helper\n\ndef go():\n    helper()\n",
        );
        write(&root, "tool/loose.py", "def go():\n    helper()\n");
        // Module-qualified calls: `unittest.main()` is not this repository's `main`,
        // `entry.main()` on an imported module is.
        write(&root, "tool/a/main.py", "def main():\n    pass\n");
        write(
            &root,
            "tool/t.py",
            "import unittest\nimport a.main as entry\n\ndef go():\n    unittest.main()\n    entry.main()\n",
        );
        let paths: Vec<String> = [
            "tool/a/run.py",
            "tool/b/run.py",
            "tool/use_a.py",
            "tool/loose.py",
            "tool/a/main.py",
            "tool/t.py",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let index = SymbolIndex::build(&root, &paths);
        std::fs::remove_dir_all(&root).unwrap();

        let helper_a = index
            .defs_named("helper", "tool/a/run.py")
            .into_iter()
            .find(|d| d.path == "tool/a/run.py")
            .cloned()
            .unwrap();
        let callers: Vec<(String, Option<String>)> = index
            .callers("helper", &helper_a.path, Some(&helper_a))
            .into_iter()
            .map(|c| (c.path, c.from))
            .collect();
        assert_eq!(
            callers,
            [
                ("tool/a/run.py".to_string(), Some("main".to_string())),
                ("tool/use_a.py".to_string(), Some("go".to_string())),
            ]
        );

        let main_def = index
            .defs_named("main", "tool/a/main.py")
            .into_iter()
            .find(|d| d.path == "tool/a/main.py")
            .cloned()
            .unwrap();
        let lines: Vec<u32> = index
            .callers("main", &main_def.path, Some(&main_def))
            .into_iter()
            .map(|c| c.line)
            .collect();
        assert_eq!(
            lines,
            [6],
            "entry.main() reaches it, unittest.main() does not"
        );
    }

    #[test]
    fn values_count_as_references_and_hooks_know_their_framework() {
        let root = temp_root("symbols-refs");
        write(
            &root,
            "svc/handlers.py",
            "from dataclasses import dataclass\nfrom trainer import Callback\n\n\
             def on_claude(x):\n    pass\n\ndef on_codex(x):\n    pass\n\n\
             READERS = {\"claude\": on_claude, \"codex\": on_codex}\n\n\
             @dataclass\nclass Args:\n    def __post_init__(self):\n        pass\n\n\
             class Save(Callback):\n    def on_step_end(self, state):\n        pass\n",
        );
        write(
            &root,
            "svc/server.rs",
            "use axum::routing::get;\nasync fn pull() {}\nfn app() { let r = Router::new().route(\"/p\", get(pull)); }\n\
             impl Handler for Svc { fn handle(&self) {} }\n",
        );
        let paths: Vec<String> = ["svc/handlers.py", "svc/server.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let index = SymbolIndex::build(&root, &paths);
        std::fs::remove_dir_all(&root).unwrap();

        let on_claude = index.defs_named("on_claude", "svc/handlers.py")[0].clone();
        let callers = index.callers("on_claude", &on_claude.path, Some(&on_claude));
        assert_eq!(callers.len(), 1);
        assert!(callers[0].is_reference);
        assert_eq!(callers[0].from, None, "the table sits at top level");

        let pull = index.defs_named("pull", "svc/server.rs")[0].clone();
        let callers = index.callers("pull", &pull.path, Some(&pull));
        assert_eq!(callers.len(), 1);
        assert!(callers[0].is_reference);
        assert_eq!(callers[0].from.as_deref(), Some("app"));

        let hook = |name: &str, path: &str| -> Option<String> {
            index.defs_named(name, path)[0].hook_of.clone()
        };
        assert_eq!(
            hook("__post_init__", "svc/handlers.py").as_deref(),
            Some("the runtime")
        );
        assert_eq!(
            hook("on_step_end", "svc/handlers.py").as_deref(),
            Some("Callback")
        );
        assert_eq!(hook("handle", "svc/server.rs").as_deref(), Some("Handler"));
        assert_eq!(hook("on_claude", "svc/handlers.py"), None);
    }

    #[test]
    fn every_language_query_compiles() {
        let mut parsers = Parsers::default();
        for lang in [
            Lang::Rust,
            Lang::Python,
            Lang::Go,
            Lang::JavaScript,
            Lang::TypeScript,
            Lang::Tsx,
        ] {
            parsers.get(lang);
        }
    }

    #[test]
    fn rust_definitions_calls_and_containers() {
        let source = r#"
pub struct App { x: u32 }
impl App {
    pub fn new() -> Self { Self::default_with(helper()) }
    fn default_with(x: u32) -> Self { App { x } }
}
fn helper() -> u32 { format!("{}", 1).len() as u32 }
fn main() { let app = App::new(); app.default_with(2); }
"#;
        let mut parsers = Parsers::default();
        let file = parse_source(&mut parsers, Lang::Rust, "src/lib.rs", source.as_bytes());
        let defs: Vec<(&str, DefKind, u32)> = file
            .defs
            .iter()
            .map(|d| (d.display.as_str(), d.kind, d.line))
            .collect();
        assert_eq!(
            defs,
            vec![
                ("App", DefKind::Type, 2),
                ("App::new", DefKind::Function, 4),
                ("App::default_with", DefKind::Function, 5),
                ("helper", DefKind::Function, 7),
                ("main", DefKind::Function, 8),
            ]
        );
        let calls: Vec<(&str, u32, Option<&str>)> = file
            .calls
            .iter()
            .map(|c| {
                (
                    c.name.as_str(),
                    c.line,
                    c.enclosing.map(|i| file.defs[i].display.as_str()),
                )
            })
            .collect();
        assert!(calls.contains(&("default_with", 4, Some("App::new"))));
        assert!(calls.contains(&("helper", 4, Some("App::new"))));
        // `format!` and `.len()` are ubiquitous and dropped; `new` with a stated receiver stays.
        assert!(!calls.iter().any(|c| c.0 == "format" || c.0 == "len"));
        assert!(calls.contains(&("new", 8, Some("main"))));
        assert!(calls.contains(&("default_with", 8, Some("main"))));
    }

    #[test]
    fn calls_inside_rust_macros_are_indexed() {
        let source = r#"
fn helper(x: u32) -> String { x.to_string() }
struct T; impl T { fn size(&self) -> usize { 1 } fn go(&self) { println!("{}", self.size()); } }
fn main() { println!("{} {}", helper(1), T::size(&T)); assert_eq!(helper(2), "2"); }
"#;
        let mut parsers = Parsers::default();
        let file = parse_source(&mut parsers, Lang::Rust, "m.rs", source.as_bytes());
        let calls: Vec<(&str, Option<&str>)> = file
            .calls
            .iter()
            .filter(|c| c.name == "helper" || c.name == "size")
            .map(|c| {
                (
                    c.name.as_str(),
                    c.enclosing.map(|i| file.defs[i].display.as_str()),
                )
            })
            .collect();
        assert_eq!(
            calls,
            vec![
                ("size", Some("T::go")),
                ("helper", Some("main")),
                ("size", Some("main")),
                ("helper", Some("main")),
            ]
        );
        let self_size = file
            .calls
            .iter()
            .find(|c| c.name == "size" && c.line == 3)
            .unwrap();
        assert_eq!(self_size.qualifier.as_deref(), Some("T"));
    }

    #[test]
    fn paths_run_from_entry_points_down_to_the_function() {
        let root = std::env::temp_dir().join(format!("changes-paths-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("app.rs"),
            "fn main() { run(); }\nfn run() { handle_event(); }\nfn handle_event() { deep(); }\nfn deep() { leaf(); }\nfn leaf() {}\nfn cron() { deep(); }\nfn orphan() { leaf(); }\n",
        )
        .unwrap();
        let index = SymbolIndex::build(&root, &["app.rs".to_string()]);
        let leaf = index.defs_named("leaf", "app.rs")[0].clone();
        let paths = index.paths_to_roots(&leaf, 3);
        let rendered: Vec<String> = paths
            .iter()
            .map(|p| {
                p.steps
                    .iter()
                    .map(|s| s.display.as_str())
                    .collect::<Vec<_>>()
                    .join(" → ")
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                "orphan → leaf",
                "cron → deep → leaf",
                "main → run → handle_event → deep → leaf",
            ]
        );
        assert!(paths.iter().all(|p| p.complete));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stated_receivers_disambiguate_same_named_methods() {
        let source = r#"
struct A; struct B;
impl A { fn new() -> A { A } fn go(&self) { self.step(); B::new(); } fn step(&self) {} }
impl B { fn new() -> B { B } fn step(&self) {} }
fn free() { let v: Vec<u8> = Vec::new(); let a = A::new(); a.step(); }
"#;
        let root = std::env::temp_dir().join(format!("changes-qual-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("q.rs"), source).unwrap();
        let index = SymbolIndex::build(&root, &["q.rs".to_string()]);
        let a_go = index.defs_named("go", "q.rs")[0].clone();
        let callees: Vec<String> = index
            .callees(&a_go)
            .into_iter()
            .map(|(_, targets)| {
                targets
                    .iter()
                    .map(|t| t.display.clone())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect();
        // self.step() resolves to A::step only; B::new() to B::new only.
        assert_eq!(callees, vec!["A::step", "B::new"]);

        let free = index.defs_named("free", "q.rs")[0].clone();
        let callees: Vec<String> = index
            .callees(&free)
            .into_iter()
            .map(|(_, targets)| {
                targets
                    .iter()
                    .map(|t| t.display.clone())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect();
        // Vec::new() is external and dropped; a.step() has an unknown receiver so both match.
        assert_eq!(callees, vec!["A::new", "A::step,B::step"]);

        let b_new = index
            .defs_named("new", "q.rs")
            .into_iter()
            .find(|d| d.display == "B::new")
            .unwrap()
            .clone();
        let callers: Vec<Option<String>> = index
            .callers("new", "q.rs", Some(&b_new))
            .into_iter()
            .map(|c| c.from)
            .collect();
        assert_eq!(callers, vec![Some("A::go".to_string())]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn python_and_typescript_definitions() {
        let mut parsers = Parsers::default();
        let py = parse_source(
            &mut parsers,
            Lang::Python,
            "a.py",
            b"class Widget:\n    def render(self):\n        return draw(self)\n\ndef draw(w):\n    pass\n",
        );
        let names: Vec<&str> = py.defs.iter().map(|d| d.display.as_str()).collect();
        assert_eq!(names, vec!["Widget", "Widget.render", "draw"]);
        assert_eq!(py.calls[0].name, "draw");
        assert_eq!(
            py.calls[0].enclosing.map(|i| py.defs[i].display.as_str()),
            Some("Widget.render")
        );

        let ts = parse_source(
            &mut parsers,
            Lang::TypeScript,
            "a.ts",
            b"export const load = async (id: string) => { return fetchIt(id); }\nclass Store { get(k: string) { return this.load(k); } }\ninterface Shape { area(): number }\n",
        );
        let names: Vec<&str> = ts.defs.iter().map(|d| d.display.as_str()).collect();
        assert_eq!(
            names,
            vec!["load", "Store", "Store.get", "Shape", "Shape.area"]
        );
        assert!(
            ts.calls
                .iter()
                .any(|c| c.name == "load" && c.enclosing.is_some())
        );
    }

    #[test]
    fn index_answers_callers_and_callees_across_files() {
        let root = std::env::temp_dir().join(format!(
            "changes-symbols-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn parse(s: &str) -> u32 { s.len() as u32 }\npub fn unused() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/main.rs"),
            "use changes::parse;\nfn main() { let n = changes::parse(\"x\"); println!(\"{n}\"); }\nfn other() { parse(\"y\"); }\n",
        )
        .unwrap();
        let paths = vec!["src/lib.rs".to_string(), "src/main.rs".to_string()];
        let index = SymbolIndex::build(&root, &paths);
        assert_eq!(index.file_count(), 2);

        let parse_def = index.defs_named("parse", "src/lib.rs")[0].clone();
        let callers = index.callers("parse", &parse_def.path, Some(&parse_def));
        let from: Vec<(&str, u32, Option<&str>)> = callers
            .iter()
            .map(|c| (c.path.as_str(), c.line, c.from.as_deref()))
            .collect();
        assert_eq!(
            from,
            vec![
                ("src/main.rs", 2, Some("main")),
                ("src/main.rs", 3, Some("other"))
            ]
        );
        assert!(index.callers("unused", "src/lib.rs", None).is_empty());

        let main_def = index.defs_named("main", "src/main.rs")[0].clone();
        let callees: Vec<String> = index
            .callees(&main_def)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            callees,
            vec!["parse"],
            "library calls like println are dropped"
        );

        // Incremental update: removing a caller changes the answer without a full rebuild.
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let updated = index.with_updated_files(&root, &["src/main.rs".to_string()]);
        assert!(updated.callers("parse", "src/lib.rs", None).is_empty());
        assert_eq!(index.callers("parse", "src/lib.rs", None).len(), 2);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
