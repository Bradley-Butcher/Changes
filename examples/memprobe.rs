//! Tracks RSS across repeated diff refreshes. Usage: memprobe <repo>
use changes::git::{self, DiffMode};
use std::path::PathBuf;

fn rss_mb() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .unwrap()
        / 1024.0
}

fn main() {
    let repo = PathBuf::from(std::env::args().nth(1).expect("repo path"));
    println!("baseline {:.1} MB", rss_mb());
    for round in 1..=4 {
        for _ in 0..10 {
            let _ = git::compute_diff(&repo, DiffMode::Unstaged, None).unwrap();
        }
        println!(
            "same thread, {:>3} calls                {:7.1} MB",
            round * 10,
            rss_mb()
        );
    }
    for round in 1..=4 {
        for _ in 0..10 {
            let repo = repo.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let files = git::compute_diff(&repo, DiffMode::Unstaged, None).unwrap();
                let _ = tx.send(files);
            });
            let files = rx.recv().unwrap();
            drop(files); // freed on the main thread, as the app does
        }
        println!(
            "spawned thread, freed on main, {:>3}   {:7.1} MB",
            round * 10,
            rss_mb()
        );
    }
    for round in 1..=4 {
        for _ in 0..10 {
            let repo = repo.clone();
            std::thread::spawn(move || {
                let files = git::compute_diff(&repo, DiffMode::Unstaged, None).unwrap();
                drop(files); // freed on the worker itself
            })
            .join()
            .unwrap();
        }
        println!(
            "spawned thread, freed on worker, {:>3} {:7.1} MB",
            round * 10,
            rss_mb()
        );
    }
}
