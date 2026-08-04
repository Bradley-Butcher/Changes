use crate::app::{App, RepoState};
use crate::diff::FileDiff;
use crate::git::{self, DiffMode};
use crate::screen::ViewportSize;
use std::collections::HashMap;
use tokio::sync::mpsc;

pub(crate) struct DiffResult {
    pub repo_id: u64,
    pub mode: DiffMode,
    pub result: anyhow::Result<Vec<FileDiff>>,
}

pub(crate) struct BaseBranchResult {
    pub repo_id: u64,
    pub branch: Option<String>,
    pub branch_name: Option<String>,
}

#[derive(Default)]
struct RefreshGate {
    in_flight: bool,
    pending: bool,
}

impl RefreshGate {
    fn request(&mut self) -> bool {
        if self.in_flight {
            self.pending = true;
            false
        } else {
            self.in_flight = true;
            true
        }
    }

    fn complete(&mut self) -> bool {
        self.in_flight = false;
        std::mem::take(&mut self.pending)
    }
}

pub(crate) struct RefreshCoordinator {
    diff_tx: mpsc::Sender<DiffResult>,
    base_tx: mpsc::Sender<BaseBranchResult>,
    diff_gates: HashMap<u64, RefreshGate>,
    base_gates: HashMap<u64, RefreshGate>,
}

impl RefreshCoordinator {
    pub fn new(diff_tx: mpsc::Sender<DiffResult>, base_tx: mpsc::Sender<BaseBranchResult>) -> Self {
        Self {
            diff_tx,
            base_tx,
            diff_gates: HashMap::new(),
            base_gates: HashMap::new(),
        }
    }

    pub fn request_diff(&mut self, repo: &RepoState) {
        let id = repo.id;
        if !self.diff_gates.entry(id).or_default().request() {
            return;
        }
        let path = repo.info.path.clone();
        let mode = repo.mode;
        let base = repo.base_branch.clone();
        let tx = self.diff_tx.clone();
        std::thread::spawn(move || {
            let result = git::compute_diff(&path, mode, base.as_deref());
            let _ = tx.blocking_send(DiffResult {
                repo_id: id,
                mode,
                result,
            });
        });
    }

    pub fn request_base(&mut self, repo: &RepoState) {
        let id = repo.id;
        if !self.base_gates.entry(id).or_default().request() {
            return;
        }
        let path = repo.info.path.clone();
        let tx = self.base_tx.clone();
        std::thread::spawn(move || {
            let branch = git::find_base_branch(&path);
            let branch_name = git::current_branch(&path);
            let _ = tx.blocking_send(BaseBranchResult {
                repo_id: id,
                branch,
                branch_name,
            });
        });
    }

    pub fn apply_diff(
        &mut self,
        app: &mut App,
        result: DiffResult,
        viewport: ViewportSize,
    ) -> bool {
        let Some(idx) = app.find_repo(result.repo_id) else {
            self.forget(result.repo_id);
            return false;
        };

        let pending = self
            .diff_gates
            .entry(result.repo_id)
            .or_default()
            .complete();
        if pending {
            self.request_diff(&app.repos[idx]);
            return false;
        }
        if result.mode != app.repos[idx].mode {
            return false;
        }

        app.apply_diff_result(idx, result.result, viewport);
        true
    }

    pub fn apply_base(&mut self, app: &mut App, result: BaseBranchResult) -> bool {
        let Some(idx) = app.find_repo(result.repo_id) else {
            self.forget(result.repo_id);
            return false;
        };

        let pending = self
            .base_gates
            .entry(result.repo_id)
            .or_default()
            .complete();
        if pending {
            self.request_base(&app.repos[idx]);
            return false;
        }

        let branch_changed = app.repos[idx].base_branch != result.branch;
        app.repos[idx].base_branch = result.branch;
        app.repos[idx].branch_name = result.branch_name;
        if branch_changed && app.repos[idx].mode == DiffMode::Branch {
            self.request_diff(&app.repos[idx]);
        }
        true
    }

    pub fn forget(&mut self, repo_id: u64) {
        self.diff_gates.remove(&repo_id);
        self.base_gates.remove(&repo_id);
    }
}

#[cfg(test)]
mod tests {
    use super::{DiffResult, RefreshCoordinator, RefreshGate};
    use crate::app::App;
    use crate::diff::{FileDiff, FileStatus};
    use crate::git::RepoInfo;
    use crate::screen::ViewportSize;
    use std::path::PathBuf;

    fn app_with_file(path: &str) -> App {
        let mut app = App::new(vec![RepoInfo {
            name: "repo".to_string(),
            path: PathBuf::from("/repo"),
        }]);
        app.repos[0].files.push(FileDiff {
            path: path.to_string(),
            old_path: None,
            status: FileStatus::Modified,
            hunks: Vec::new(),
            additions: 0,
            deletions: 0,
            collapsed: false,
            total_new_lines: 0,
            sbs_cache: None,
        });
        app
    }

    #[test]
    fn refresh_gate_coalesces_work() {
        let mut gate = RefreshGate::default();
        assert!(gate.request());
        assert!(!gate.request());
        assert!(gate.complete());
        assert!(gate.request());
    }

    #[test]
    fn pending_refresh_drops_the_old_result() {
        let mut app = app_with_file("old.rs");
        let repo_id = app.repos[0].id;
        let mode = app.repos[0].mode;
        let (diff_tx, _diff_rx) = tokio::sync::mpsc::channel(8);
        let (base_tx, _base_rx) = tokio::sync::mpsc::channel(8);
        let mut refresh = RefreshCoordinator::new(diff_tx, base_tx);
        refresh.request_diff(&app.repos[0]);
        refresh.request_diff(&app.repos[0]);

        let stale = app_with_file("stale.rs").repos.remove(0).files;
        assert!(!refresh.apply_diff(
            &mut app,
            DiffResult {
                repo_id,
                mode,
                result: Ok(stale),
            },
            ViewportSize::default(),
        ));
        assert_eq!(app.repos[0].files[0].path, "old.rs");
    }
}
