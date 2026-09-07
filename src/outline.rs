//! The "shape" of a diff: a directory tree of changed files and the declarations each
//! hunk adds, removes, or touches. This is the altitude a reviewer wants before reading
//! lines, and it is derived purely from the diff text — no language servers involved.

use crate::diff::{FileDiff, FileStatus, LineKind};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolChange {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub change: SymbolChange,
    pub hunk_idx: usize,
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
        symbol: Symbol,
    },
    /// Stands in for declarations beyond `MAX_SYMBOLS_PER_FILE`.
    More {
        prefix: String,
        file_idx: usize,
        count: usize,
    },
}

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
        }
    }
}

/// Build the outline rows for a set of files.
pub fn build_outline(files: &[FileDiff]) -> Vec<OutlineRow> {
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

    let mut rows = Vec::new();
    render_dir(&root, files, "", true, &mut rows);
    rows
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
    files: &[FileDiff],
    prefix: &str,
    is_root: bool,
    rows: &mut Vec<OutlineRow>,
) {
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
        render_dir(child, files, &child_prefix, false, rows);
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
            let (branch, _) = connectors(&child_prefix, position + 1 == count, false);
            rows.push(OutlineRow::Symbol {
                prefix: branch,
                file_idx: *file_idx,
                symbol,
            });
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
        symbols.push(Symbol {
            name,
            change,
            hunk_idx,
        });
    };

    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        let mut declared_here = false;
        for line in &hunk.lines {
            let change = match (line.kind, whole_file) {
                (_, Some(change)) => change,
                (LineKind::Addition, None) => SymbolChange::Added,
                (LineKind::Deletion, None) => SymbolChange::Removed,
                (LineKind::Context, None) => continue,
            };
            if let Some(name) = declaration_name(&line.content) {
                push(name, change, hunk_idx);
                declared_here = true;
            }
        }
        // A hunk that edits the body of something is a modification of the enclosing
        // declaration, which git names in the hunk header.
        if !declared_here
            && whole_file.is_none()
            && let Some(context) = hunk_context(&hunk.header)
            && let Some(name) = declaration_name(context).or_else(|| short_context(context))
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
        let rows = build_outline(&files);
        let rendered: Vec<String> = rows
            .iter()
            .map(|row| match row {
                OutlineRow::Dir { prefix, name, .. } => format!("{prefix}{name}/"),
                OutlineRow::File { prefix, name, .. } => format!("{prefix}{name}"),
                OutlineRow::Symbol { prefix, symbol, .. } => format!("{prefix}{}", symbol.name),
                OutlineRow::More { prefix, count, .. } => format!("{prefix}… {count} more"),
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
