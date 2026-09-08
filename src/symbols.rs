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
}

impl Call {
    /// Whether this call could target `def`, given what the call site says about the
    /// receiver. A stated qualifier must match the definition's container.
    fn may_target(&self, def: &Def) -> bool {
        match (&self.qualifier, &def.container) {
            (Some(qualifier), Some(container)) => qualifier == container,
            (Some(_), None) => false,
            (None, _) => true,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileSymbols {
    pub path: String,
    pub defs: Vec<Def>,
    pub calls: Vec<Call>,
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
}

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
        for path in paths {
            self.files.remove(path);
        }
        let parsed = parse_files(root, paths);
        for file in parsed {
            self.files.insert(file.path.clone(), Arc::new(file));
        }
        self.rebuild_lookups();
    }

    fn rebuild_lookups(&mut self) {
        self.defs_by_name.clear();
        self.calls_by_name.clear();
        let mut paths: Vec<&String> = self.files.keys().collect();
        paths.sort();
        for path in paths {
            let file = &self.files[path];
            for (idx, def) in file.defs.iter().enumerate() {
                self.defs_by_name
                    .entry(def.name.clone())
                    .or_default()
                    .push((path.clone(), idx));
            }
            for (idx, call) in file.calls.iter().enumerate() {
                self.calls_by_name
                    .entry(call.name.clone())
                    .or_default()
                    .push((path.clone(), idx));
            }
        }
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn def_count(&self) -> usize {
        self.files.values().map(|f| f.defs.len()).sum()
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

    /// Every call site that may target `name` (or `def` precisely, when known), outside
    /// the definition itself. Same-file callers first.
    pub fn callers(&self, name: &str, def_path: &str, def: Option<&Def>) -> Vec<Caller> {
        let Some(entries) = self.calls_by_name.get(name) else {
            return Vec::new();
        };
        let def_line = def.map(|d| d.line);
        let mut callers: Vec<Caller> = entries
            .iter()
            .filter_map(|(path, idx)| {
                let file = &self.files[path];
                let call = &file.calls[*idx];
                if let Some(def) = def
                    && !call.may_target(def)
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
                })
            })
            .collect();
        callers.sort_by(|a, b| {
            (a.path != def_path, &a.path, a.line).cmp(&(b.path != def_path, &b.path, b.line))
        });
        callers
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
            let targets = self.defs_named(&call.name, &def.path);
            let targets: Vec<&Def> = targets
                .into_iter()
                .filter(|target| target.kind == DefKind::Function && call.may_target(target))
                .collect();
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

fn language_for_path(path: &str) -> Option<Lang> {
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
            Lang::TypeScript | Lang::Tsx => TYPESCRIPT_QUERY,
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
"#;

const PYTHON_QUERY: &str = r#"
(function_definition name: (identifier) @name) @def.function
(class_definition name: (identifier) @name) @def.type
(call function: (identifier) @name) @call
(call function: (attribute object: (_) @receiver attribute: (identifier) @name)) @call
"#;

const GO_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(method_declaration name: (field_identifier) @name) @def.function
(type_spec name: (type_identifier) @name) @def.type
(call_expression function: (identifier) @name) @call
(call_expression function: (selector_expression operand: (_) @receiver field: (field_identifier) @name)) @call
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
    // name, line, byte offset, receiver qualifier
    let mut raw_calls: Vec<(String, u32, usize, Qualifier)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        let mut name: Option<&str> = None;
        let mut node: Option<(Node, &str)> = None;
        let mut qualifier = Qualifier::Unknown;
        for capture in m.captures() {
            if Some(capture.index) == name_capture {
                name = capture.node.utf8_text(source).ok();
            } else if Some(capture.index) == qualifier_capture {
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
                if let Ok(text) = capture.node.utf8_text(source) {
                    qualifier = match text {
                        "self" | "Self" | "this" | "cls" => Qualifier::SelfType,
                        _ => Qualifier::Unknown,
                    };
                }
            } else {
                node = Some((capture.node, capture_names[capture.index as usize]));
            }
        }
        if let Some((node, "macro_body")) = node {
            raw_calls.extend(macro_body_calls(node, source));
            continue;
        }
        let (Some(name), Some((node, capture_name))) = (name, node) else {
            continue;
        };
        let line = node.start_position().row as u32 + 1;
        match capture_name {
            "call" => raw_calls.push((name.to_string(), line, node.start_byte(), qualifier)),
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
                defs.push(Def {
                    name: name.to_string(),
                    display,
                    container,
                    kind,
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

    let calls = raw_calls
        .into_iter()
        // `x.len()` on an unknown receiver is almost always the standard library, and
        // would otherwise link every collection call to any local method named `len`.
        .filter(|(name, _, _, qualifier)| {
            !(matches!(qualifier, Qualifier::Unknown) && is_ubiquitous_method(name))
        })
        .map(|(name, line, offset, qualifier)| {
            let enclosing = innermost_def(&defs, offset);
            let qualifier = match qualifier {
                Qualifier::Named(name) => Some(name),
                Qualifier::SelfType => enclosing.and_then(|i| defs[i].container.clone()),
                Qualifier::Unknown => None,
            };
            Call {
                name,
                path: path.to_string(),
                line,
                enclosing,
                qualifier,
            }
        })
        .collect();

    FileSymbols {
        path: path.to_string(),
        defs,
        calls,
    }
}

/// Calls written inside a Rust macro invocation. tree-sitter parses `format!(...)` and
/// friends as opaque token trees, so `name(`, `Type::name(` and `self.name(` are found by
/// scanning the tokens in order.
fn macro_body_calls(body: Node, source: &[u8]) -> Vec<(String, u32, usize, Qualifier)> {
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
        calls.push((
            name.to_string(),
            leaf.start_position().row as u32 + 1,
            leaf.start_byte(),
            qualifier,
        ));
    }
    calls
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
    use super::{DefKind, Lang, Parsers, SymbolIndex, parse_source};

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
            "fn main() { let n = changes::parse(\"x\"); println!(\"{n}\"); }\nfn other() { parse(\"y\"); }\n",
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
                ("src/main.rs", 1, Some("main")),
                ("src/main.rs", 2, Some("other"))
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
