//! Hot-path timings for the TUI. Run with `cargo bench`.
//!
//! Scenarios cover the shapes that hurt in practice: many changed files, one enormous
//! file, very long (minified) lines, and a refresh while the user is mid-scroll.

use changes::app::App;
use changes::diff::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};
use changes::git::{self, DiffMode, RepoInfo};
use changes::highlight::Highlighter;
use changes::ui::{self, LayoutHints};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const COLS: u16 = 200;
const ROWS: u16 = 60;

fn time<R>(label: &str, iterations: usize, mut f: impl FnMut() -> R) -> R {
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    let mut last = None;
    for _ in 0..iterations {
        let start = Instant::now();
        let result = f();
        let elapsed = start.elapsed();
        best = best.min(elapsed);
        total += elapsed;
        last = Some(result);
    }
    let avg = total / iterations as u32;
    println!("{label:<58} best {best:>9.2?}   avg {avg:>9.2?}");
    last.unwrap()
}

fn synthetic_file(path: &str, hunks: usize, lines_per_hunk: usize, line_len: usize) -> FileDiff {
    let body: String = "let value = compute(alpha, beta) + gamma * delta; "
        .chars()
        .cycle()
        .take(line_len)
        .collect();
    let mut lineno = 1u32;
    let hunks: Vec<Hunk> = (0..hunks)
        .map(|h| {
            let start = lineno;
            let lines = (0..lines_per_hunk)
                .map(|i| {
                    let kind = match i % 4 {
                        0 => LineKind::Context,
                        1 => LineKind::Deletion,
                        2 => LineKind::Addition,
                        _ => LineKind::Context,
                    };
                    let (old, new) = match kind {
                        LineKind::Context => (Some(lineno), Some(lineno)),
                        LineKind::Deletion => (Some(lineno), None),
                        LineKind::Addition => (None, Some(lineno)),
                    };
                    if kind != LineKind::Deletion {
                        lineno += 1;
                    }
                    DiffLine {
                        kind,
                        content: format!("{body} // h{h} l{i}"),
                        old_lineno: old,
                        new_lineno: new,
                    }
                })
                .collect();
            lineno += 40; // gap before the next hunk
            Hunk {
                header: format!(
                    "@@ -{start},{lines_per_hunk} +{start},{lines_per_hunk} @@ fn f{h}()"
                ),
                lines,
            }
        })
        .collect();
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
        status: FileStatus::Modified,
        hunks,
        additions,
        deletions,
        collapsed: false,
        total_new_lines: lineno as usize + 100,
        sbs_cache: None,
    }
}

fn app_with(files: Vec<FileDiff>) -> App {
    let mut app = App::new(vec![RepoInfo {
        name: "bench".to_string(),
        path: PathBuf::from("/bench"),
    }]);
    let inner = ui::diff_inner_area(ratatui::layout::Rect::new(0, 0, COLS, ROWS), 0);
    app.layout.content_y = inner.y;
    app.layout.content_height = inner.height;
    app.layout.content_width = inner.width;
    app.apply_diff_result(0, Ok(files));
    app
}

fn draw_frame(terminal: &mut Terminal<TestBackend>, app: &mut App, highlighter: &Highlighter) {
    app.prepare_active_layout();
    let mut hints = LayoutHints::default();
    terminal
        .draw(|f| ui::draw(f, app, highlighter, &mut hints))
        .unwrap();
    app.layout = hints;
}

fn bench_scenario(name: &str, files: Vec<FileDiff>) {
    let total_lines: usize = files
        .iter()
        .flat_map(|f| &f.hunks)
        .map(|h| h.lines.len())
        .sum();
    println!(
        "\n== {name}: {} files, {total_lines} diff lines",
        files.len()
    );

    let highlighter = Highlighter::new();
    let mut terminal = Terminal::new(TestBackend::new(COLS, ROWS)).unwrap();

    let width = ui::diff_inner_area(ratatui::layout::Rect::new(0, 0, COLS, ROWS), 0).width as usize;
    time("DiffLayout::build unified (isolated)", 3, || {
        changes::viewport::DiffLayout::build(
            &files,
            changes::viewport::ViewKind::Unified,
            &[],
            width,
        )
    });
    time("compute_side_by_side all files (isolated)", 2, || {
        files
            .iter()
            .map(|f| changes::diff::compute_side_by_side(&f.hunks))
            .collect::<Vec<_>>()
    });

    let mut app = time(
        "apply_diff_result (new diff arrives, layout built)",
        3,
        || app_with(files.clone()),
    );

    time("first frame (cold highlight cache)", 1, || {
        highlighter.clear_highlight_cache();
        draw_frame(&mut terminal, &mut app, &highlighter)
    });
    time("frame (warm cache)", 20, || {
        draw_frame(&mut terminal, &mut app, &highlighter)
    });

    time("scroll 1 line + frame  (j)", 20, || {
        app.scroll_active_viewport(1);
        draw_frame(&mut terminal, &mut app, &highlighter)
    });
    time("page down + frame", 10, || {
        app.scroll_active_viewport(app.page_size() as isize);
        draw_frame(&mut terminal, &mut app, &highlighter)
    });
    time("next hunk + frame  (])", 10, || {
        app.select_next_hunk();
        draw_frame(&mut terminal, &mut app, &highlighter)
    });
    time("jump to bottom + frame  (G)", 3, || {
        app.jump_active_viewport_bottom();
        draw_frame(&mut terminal, &mut app, &highlighter)
    });
    time("collapse file (layout rebuild) + frame", 3, || {
        app.toggle_collapsed(0);
        draw_frame(&mut terminal, &mut app, &highlighter)
    });
    time("add note (layout rebuild) + frame", 3, || {
        app.add_or_update_comment(0, 0, "note".to_string());
        draw_frame(&mut terminal, &mut app, &highlighter)
    });
    time(
        "refresh mid-scroll (apply + cache clear + frame)",
        3,
        || {
            app.apply_diff_result(0, Ok(files.clone()));
            highlighter.clear_highlight_cache();
            draw_frame(&mut terminal, &mut app, &highlighter)
        },
    );

    app.jump_active_viewport_top();
    time(
        "toggle side-by-side (sbs caches + layout) + frame",
        1,
        || {
            app.toggle_view();
            draw_frame(&mut terminal, &mut app, &highlighter)
        },
    );
    time("sbs frame (warm)", 20, || {
        draw_frame(&mut terminal, &mut app, &highlighter)
    });
    time("sbs scroll 1 line + frame", 20, || {
        app.scroll_active_viewport(1);
        draw_frame(&mut terminal, &mut app, &highlighter)
    });
    time("sbs refresh (apply incl. sbs caches + frame)", 2, || {
        app.apply_diff_result(0, Ok(files.clone()));
        highlighter.clear_highlight_cache();
        draw_frame(&mut terminal, &mut app, &highlighter)
    });
    app.toggle_view();
}

fn temp_repo(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "changes-bench-{label}-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn commit_all(repo: &git2::Repository, message: &str) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("bench", "bench@example.com").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .unwrap();
}

fn write_source(root: &Path, files: usize, lines: usize, salt: &str) {
    for f in 0..files {
        let dir = root.join(format!("src/mod{}", f % 20));
        std::fs::create_dir_all(&dir).unwrap();
        let body: String = (0..lines)
            .map(|l| format!("fn item_{f}_{l}() -> u32 {{ {l} + {salt}.len() as u32 }}\n"))
            .collect();
        std::fs::write(dir.join(format!("file{f}.rs")), body).unwrap();
    }
}

fn bench_git(files: usize, lines: usize, changed_every: usize) {
    let root = temp_repo("git");
    let repo = git2::Repository::init(&root).unwrap();
    write_source(&root, files, lines, "\"a\"");
    commit_all(&repo, "base");
    // Touch every Nth line of every file, plus add some untracked files.
    for f in 0..files {
        let path = root.join(format!("src/mod{}/file{f}.rs", f % 20));
        let text = std::fs::read_to_string(&path).unwrap();
        let edited: String = text
            .lines()
            .enumerate()
            .map(|(i, l)| {
                if i % changed_every == 0 {
                    format!("{l} // edited\n")
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        std::fs::write(&path, edited).unwrap();
    }
    for u in 0..20 {
        std::fs::write(
            root.join(format!("untracked{u}.rs")),
            "fn new() {}\n".repeat(200),
        )
        .unwrap();
    }
    let changed_lines = files * (lines / changed_every);
    println!(
        "\n== git compute_diff: {files} files x {lines} lines, ~{changed_lines} changed lines + 20 untracked"
    );
    time("  raw git2: open + diff_index_to_workdir only", 3, || {
        let repo = git2::Repository::open(&root).unwrap();
        let mut opts = git2::DiffOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .context_lines(3);
        let diff = repo.diff_index_to_workdir(None, Some(&mut opts)).unwrap();
        diff.deltas().count()
    });
    time("  raw git2: + print(Patch) with no-op callback", 3, || {
        let repo = git2::Repository::open(&root).unwrap();
        let mut opts = git2::DiffOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .context_lines(3);
        let diff = repo.diff_index_to_workdir(None, Some(&mut opts)).unwrap();
        let mut n = 0usize;
        diff.print(git2::DiffFormat::Patch, |_, _, _| {
            n += 1;
            true
        })
        .unwrap();
        n
    });
    let unstaged = time("compute_diff Unstaged", 3, || {
        git::compute_diff(&root, &DiffMode::Unstaged, None).unwrap()
    });
    time("compute_diff Staged (nothing staged)", 3, || {
        git::compute_diff(&root, &DiffMode::Staged, None).unwrap()
    });
    println!("   -> {} files in unstaged diff", unstaged.len());
    let _ = std::fs::remove_dir_all(&root);

    bench_scenario("real git diff loaded into the app", unstaged);
}

fn main() {
    println!(
        "terminal {COLS}x{ROWS}, release build = {}",
        !cfg!(debug_assertions)
    );
    time(
        "Highlighter::new (syntect syntax + theme load)",
        2,
        Highlighter::new,
    );

    bench_scenario(
        "typical agent session",
        (0..12)
            .map(|f| synthetic_file(&format!("src/mod/file{f}.rs"), 6, 30, 60))
            .collect(),
    );
    bench_scenario(
        "many files",
        (0..400)
            .map(|f| synthetic_file(&format!("src/mod{}/file{f}.rs", f % 30), 4, 20, 70))
            .collect(),
    );
    bench_scenario(
        "one enormous file",
        vec![synthetic_file("src/generated.rs", 2000, 50, 80)],
    );
    bench_scenario(
        "minified: 300 lines of 20 KiB",
        vec![synthetic_file("dist/bundle.min.js", 3, 100, 20 * 1024)],
    );

    bench_git(300, 400, 25);
}
// touch 1
// touch 2
// touch 3
// touch 4
// touch 5
// touch 6
// touch 7
// touch 8
// touch 9
// touch 10
// touch 11
// touch 12
// touch 13
// touch 14
// touch 15
// touch 16
// touch 17
// touch 18
// touch 19
// touch 20
// touch 21
// touch 22
// touch 23
// touch 24
// touch 25
// touch 26
// touch 27
// touch 28
// touch 29
// touch 30
// touch 1
// touch 2
// touch 3
// touch 4
// touch 5
// touch 6
// touch 7
// touch 8
// touch 9
// touch 10
// touch 11
// touch 12
// touch 13
// touch 14
// touch 15
// touch 16
// touch 17
// touch 18
// touch 19
// touch 20
// touch 21
// touch 22
// touch 23
// touch 24
// touch 25
// touch 26
// touch 27
// touch 28
// touch 29
// touch 30
