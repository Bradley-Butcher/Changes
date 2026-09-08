//! Prints what the index knows about one function. Usage: callprobe <repo> <name>
use changes::symbols::{SymbolIndex, indexable_paths};
fn main() {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("repo"));
    let name = args.next().expect("name");
    let repo = git2::Repository::open(&root).unwrap();
    let index = SymbolIndex::build(&root, &indexable_paths(&repo));
    for def in index.defs_named(&name, "") {
        println!(
            "def {} at {}:{} container={:?}",
            def.display, def.path, def.line, def.container
        );
        for caller in index.callers(&name, &def.path, Some(def)) {
            println!(
                "  caller {:?} at {}:{}",
                caller.from, caller.path, caller.line
            );
        }
        for (callee, targets) in index.callees(def) {
            println!(
                "  callee {callee} -> {:?}",
                targets.iter().map(|t| &t.display).collect::<Vec<_>>()
            );
        }
    }
    println!(
        "unfiltered callers: {}",
        index.callers(&name, "", None).len()
    );
}
