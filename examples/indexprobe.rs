//! Times a full symbol index of a repository. Usage: indexprobe <repo> [<repo>...]
use changes::symbols::{SymbolIndex, indexable_paths};
use std::time::Instant;

fn main() {
    for arg in std::env::args().skip(1) {
        let root = std::path::PathBuf::from(&arg);
        let repo = git2::Repository::open(&root).expect("open repo");
        let start = Instant::now();
        let paths = indexable_paths(&repo);
        let listed = start.elapsed();
        let lines: usize = paths
            .iter()
            .filter_map(|p| std::fs::read(root.join(p)).ok())
            .map(|b| b.iter().filter(|&&c| c == b'\n').count())
            .sum();
        let start = Instant::now();
        let index = SymbolIndex::build(&root, &paths);
        let built = start.elapsed();
        let start = Instant::now();
        let updated = index.with_updated_files(&root, &paths[..paths.len().min(3)]);
        let incremental = start.elapsed();
        println!(
            "{arg}: {} files / {lines} lines -> {} defs | list {listed:.1?} | full index {built:.1?} | 3-file update {incremental:.1?} (files={})",
            paths.len(),
            index.def_count(),
            updated.file_count()
        );
    }
}
