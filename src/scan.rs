use std::{
    collections::HashSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
};

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub directory: bool,
    pub logical: u64,
    pub allocated: u64,
    pub dev: u64,
    pub ino: u64,
}

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub folders: AtomicU64,
    pub bytes: AtomicU64,
    pub cancel: AtomicBool,
}

#[derive(Debug)]
pub struct Report {
    pub entries: Vec<Entry>,
    pub errors: Vec<String>,
    pub error_count: usize,
    pub skipped_links: usize,
    pub skipped_mounts: usize,
    pub cancelled: bool,
}

impl Report {
    fn error(&mut self, path: &Path, error: impl std::fmt::Display) {
        self.error_count += 1;
        if self.errors.len() < 50 {
            self.errors.push(format!("{}：{error}", path.display()));
        }
    }
}

/// Iterative traversal: no symlink following, no crossing devices, no recursion overflow.
/// Hard links count once; APFS clones/snapshots still cannot be attributed uniquely.
pub fn scan(root: &Path, progress: &Progress) -> Result<Report, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let meta = fs::symlink_metadata(&root).map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Err("請選擇資料夾".into());
    }
    let device = meta.dev();
    let mut report = Report {
        entries: vec![Entry {
            path: root,
            parent: None,
            children: vec![],
            directory: true,
            logical: 0,
            allocated: 0,
            dev: device,
            ino: meta.ino(),
        }],
        errors: vec![],
        error_count: 0,
        skipped_links: 0,
        skipped_mounts: 0,
        cancelled: false,
    };
    let mut stack = vec![0];
    let mut seen = HashSet::new();
    progress.folders.store(1, Ordering::Relaxed);
    while let Some(parent) = stack.pop() {
        if progress.cancel.load(Ordering::Relaxed) {
            break;
        }
        let path = report.entries[parent].path.clone();
        let children = match fs::read_dir(&path) {
            Ok(it) => it,
            Err(e) => {
                report.error(&path, e);
                continue;
            }
        };
        for child in children {
            if progress.cancel.load(Ordering::Relaxed) {
                break;
            }
            let child = match child {
                Ok(child) => child,
                Err(e) => {
                    report.error(&path, e);
                    continue;
                }
            };
            let path = child.path();
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(e) => {
                    report.error(&path, e);
                    continue;
                }
            };
            if meta.file_type().is_symlink() {
                report.skipped_links += 1;
                continue;
            }
            if meta.dev() != device {
                report.skipped_mounts += 1;
                continue;
            }
            if !meta.is_file() && !meta.is_dir() {
                continue;
            }
            let directory = meta.is_dir();
            let unique = directory || seen.insert((meta.dev(), meta.ino()));
            let logical = if !directory && unique { meta.len() } else { 0 };
            let allocated = if !directory && unique {
                meta.blocks().saturating_mul(512)
            } else {
                0
            };
            let id = report.entries.len();
            report.entries.push(Entry {
                path,
                parent: Some(parent),
                children: vec![],
                directory,
                logical,
                allocated,
                dev: meta.dev(),
                ino: meta.ino(),
            });
            report.entries[parent].children.push(id);
            if directory {
                stack.push(id);
                progress.folders.fetch_add(1, Ordering::Relaxed);
            } else {
                progress.files.fetch_add(1, Ordering::Relaxed);
                progress.bytes.fetch_add(logical, Ordering::Relaxed);
            }
        }
    }
    report.cancelled = progress.cancel.load(Ordering::Relaxed);
    // Parents precede children in the arena. Reverse aggregation is linear.
    for id in (1..report.entries.len()).rev() {
        let parent = report.entries[id].parent.unwrap();
        let logical = report.entries[id].logical;
        let allocated = report.entries[id].allocated;
        report.entries[parent].logical = report.entries[parent].logical.saturating_add(logical);
        report.entries[parent].allocated =
            report.entries[parent].allocated.saturating_add(allocated);
    }
    Ok(report)
}

/// Revalidate identity immediately before moving anything to Trash.
pub fn validate_trash(root: &Path, entry: &Entry) -> Result<(), String> {
    if entry.path == root || !entry.path.starts_with(root) {
        return Err("不能移除掃描根目錄或範圍外的項目".into());
    }
    let canonical = entry.path.canonicalize().map_err(|e| e.to_string())?;
    if canonical != entry.path {
        return Err("路徑已改變，請重新掃描".into());
    }
    let blocked = [
        "/System", "/Library", "/usr", "/bin", "/sbin", "/private", "/dev",
    ];
    if blocked.iter().any(|p| canonical.starts_with(p)) {
        return Err("此版本不支援移除系統目錄中的項目".into());
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if canonical == home || canonical == home.join("Library") {
            return Err("不能移除個人主目錄或整個資料庫目錄".into());
        }
    }
    let meta = fs::symlink_metadata(&entry.path).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() || meta.dev() != entry.dev || meta.ino() != entry.ino {
        return Err("項目已在掃描後被替換，請重新掃描".into());
    }
    Ok(())
}

pub fn move_to_trash(path: &Path) -> Result<(), String> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path).map_err(|e| e.to_string())
}

#[derive(Default)]
pub struct TrashOutcome {
    pub moved: Vec<PathBuf>,
    pub failed: Vec<String>,
    pub failed_entries: Vec<Entry>,
    pub root: PathBuf,
    pub permanent: bool,
}

#[derive(Default)]
pub struct DeleteProgress {
    pub total: AtomicU64,
    pub completed: AtomicU64,
    pub succeeded: AtomicU64,
    pub failed: AtomicU64,
    pub current: Mutex<String>,
}

pub fn delete_batch_progress(
    root: &Path,
    entries: &[Entry],
    permanent: bool,
    progress: &DeleteProgress,
) -> TrashOutcome {
    progress
        .total
        .store(entries.len() as u64, Ordering::Relaxed);
    let mut outcome = TrashOutcome {
        root: root.to_path_buf(),
        permanent,
        ..Default::default()
    };
    for entry in entries {
        if let Ok(mut current) = progress.current.lock() {
            *current = entry.path.display().to_string();
        }
        let result = validate_trash(root, entry).and_then(|()| {
            if permanent {
                remove_permanently(&entry.path)
            } else {
                move_to_trash(&entry.path)
            }
        });
        match result {
            Ok(()) => {
                outcome.moved.push(entry.path.clone());
                progress.succeeded.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                outcome
                    .failed
                    .push(format!("{}：{error}", entry.path.display()));
                outcome.failed_entries.push(entry.clone());
                progress.failed.fetch_add(1, Ordering::Relaxed);
            }
        }
        progress.completed.fetch_add(1, Ordering::Relaxed);
    }
    outcome
}

#[cfg(test)]
pub fn delete_batch(root: &Path, entries: &[Entry], permanent: bool) -> TrashOutcome {
    delete_batch_progress(root, entries, permanent, &DeleteProgress::default())
}

fn remove_permanently(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        // Reject mounted subtrees before recursive deletion. Never traverse symlinks.
        let mut stack = vec![path.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for child in fs::read_dir(directory).map_err(|e| e.to_string())? {
                let child = child.map_err(|e| e.to_string())?;
                let metadata = fs::symlink_metadata(child.path()).map_err(|e| e.to_string())?;
                if metadata.dev() != meta.dev() {
                    return Err("資料夾包含另一個磁碟，已停止刪除".into());
                }
                if metadata.is_dir() {
                    stack.push(child.path());
                }
            }
        }
        fs::remove_dir_all(path).map_err(|e| format!("刪除未完成，部分內容可能已移除：{e}"))
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
pub fn trash_batch(root: &Path, entries: &[Entry]) -> TrashOutcome {
    trash_batch_with(root, entries, move_to_trash)
}

#[cfg(test)]
fn trash_batch_with(
    root: &Path,
    entries: &[Entry],
    mut mover: impl FnMut(&Path) -> Result<(), String>,
) -> TrashOutcome {
    let mut outcome = TrashOutcome {
        root: root.to_path_buf(),
        ..Default::default()
    };
    for entry in entries {
        match validate_trash(root, entry).and_then(|()| mover(&entry.path)) {
            Ok(()) => outcome.moved.push(entry.path.clone()),
            Err(error) => {
                outcome
                    .failed
                    .push(format!("{}：{error}", entry.path.display()));
                outcome.failed_entries.push(entry.clone());
            }
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn deletion_progress_counts_success_and_failure_and_keeps_parent() {
        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(dir.path().join("a.mp4"), b"test").unwrap();
        fs::write(dir.path().join("b.jpg"), b"test").unwrap();
        let report = scan(dir.path(), &Progress::default()).unwrap();
        let entries: Vec<_> = report.entries[0]
            .children
            .iter()
            .map(|&id| report.entries[id].clone())
            .collect();
        fs::remove_file(&entries[1].path).unwrap();
        let progress = DeleteProgress::default();
        let result = delete_batch_progress(&report.entries[0].path, &entries, true, &progress);
        assert_eq!(progress.total.load(Ordering::Relaxed), 2);
        assert_eq!(progress.completed.load(Ordering::Relaxed), 2);
        assert_eq!(progress.succeeded.load(Ordering::Relaxed), 1);
        assert_eq!(progress.failed.load(Ordering::Relaxed), 1);
        assert_eq!(result.moved.len(), 1);
        assert_eq!(result.failed.len(), 1);
        assert_eq!(
            *progress.current.lock().unwrap(),
            entries[1].path.display().to_string()
        );
        assert!(dir.path().is_dir());
    }

    #[test]
    fn permanent_batch_removes_nested_and_empty_folders_without_following_links() {
        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        fs::create_dir_all(dir.path().join("folder/nested")).unwrap();
        fs::create_dir(dir.path().join("empty")).unwrap();
        fs::write(dir.path().join("folder/nested/file"), b"test").unwrap();
        symlink(outside.path(), dir.path().join("folder/link")).unwrap();
        let report = scan(dir.path(), &Progress::default()).unwrap();
        let entries: Vec<_> = report.entries[0]
            .children
            .iter()
            .map(|&id| report.entries[id].clone())
            .collect();
        let outcome = delete_batch(&report.entries[0].path, &entries, true);
        assert_eq!(outcome.moved.len(), 2);
        assert!(outcome.failed.is_empty());
        assert!(outcome.permanent);
        assert!(outside.path().join("keep").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        let root_attempt =
            delete_batch(&report.entries[0].path, &[report.entries[0].clone()], true);
        assert_eq!(root_attempt.failed.len(), 1);
        assert!(dir.path().exists());
    }

    #[test]
    fn batch_reports_partial_failure_and_revalidates_each_item() {
        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(dir.path().join("ok"), b"ok").unwrap();
        fs::write(dir.path().join("changed"), b"old").unwrap();
        let report = scan(dir.path(), &Progress::default()).unwrap();
        fs::rename(dir.path().join("changed"), dir.path().join("old")).unwrap();
        fs::write(dir.path().join("changed"), b"new").unwrap();
        let mut calls = 0;
        let result = trash_batch_with(&report.entries[0].path, &report.entries[1..], |_| {
            calls += 1;
            Ok(())
        });
        assert_eq!(calls, 1);
        assert_eq!(result.moved.len(), 1);
        assert_eq!(result.failed.len(), 1);
        let result = trash_batch_with(
            &report.entries[0].path,
            &[report.entries[0].clone()],
            |_| panic!("must not trash root"),
        );
        assert_eq!(result.failed.len(), 1);
    }

    #[test]
    fn aggregates_nested_files_and_counts_hardlinks_once() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(dir.path().join("one"), [0; 17]).unwrap();
        fs::write(dir.path().join("nested/two"), [0; 31]).unwrap();
        fs::hard_link(dir.path().join("one"), dir.path().join("alias")).unwrap();
        symlink(dir.path(), dir.path().join("loop")).unwrap();
        let report = scan(dir.path(), &Progress::default()).unwrap();
        assert_eq!(report.entries[0].logical, 48);
        assert_eq!(report.skipped_links, 1);
        assert_eq!(report.error_count, 0);
        assert_eq!(report.entries.len(), 5);
    }

    #[test]
    fn sparse_files_distinguish_logical_from_allocated() {
        let dir = tempfile::tempdir().unwrap();
        fs::File::create(dir.path().join("sparse"))
            .unwrap()
            .set_len(16 * 1024 * 1024)
            .unwrap();
        let report = scan(dir.path(), &Progress::default()).unwrap();
        assert_eq!(report.entries[0].logical, 16 * 1024 * 1024);
        assert!(report.entries[0].allocated < report.entries[0].logical);
    }

    #[test]
    fn cancellation_is_explicit_and_root_cannot_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let progress = Progress::default();
        progress.cancel.store(true, Ordering::Relaxed);
        let report = scan(dir.path(), &progress).unwrap();
        assert!(report.cancelled);
        assert!(validate_trash(&report.entries[0].path, &report.entries[0]).is_err());
    }

    #[test]
    fn replacing_parent_with_symlink_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("a")).unwrap();
        fs::write(dir.path().join("a/file"), b"hello").unwrap();
        let report = scan(dir.path(), &Progress::default()).unwrap();
        let entry = report
            .entries
            .iter()
            .find(|e| e.path.ends_with("a/file"))
            .unwrap();
        fs::rename(dir.path().join("a"), dir.path().join("b")).unwrap();
        symlink(dir.path().join("b"), dir.path().join("a")).unwrap();
        assert_eq!(
            validate_trash(&report.entries[0].path, entry).unwrap_err(),
            "路徑已改變，請重新掃描"
        );
    }

    #[test]
    fn validates_real_item_and_rejects_replacement() {
        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let path = dir.path().join("original");
        fs::write(&path, b"original").unwrap();
        let report = scan(dir.path(), &Progress::default()).unwrap();
        let entry = &report.entries[1];
        assert!(validate_trash(&report.entries[0].path, entry).is_ok());
        fs::rename(&path, dir.path().join("moved")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert_eq!(
            validate_trash(&report.entries[0].path, entry).unwrap_err(),
            "項目已在掃描後被替換，請重新掃描"
        );
    }

    #[test]
    #[ignore = "Moves only a newly generated 26-byte fixture to the macOS Trash"]
    fn native_trash_smoke_test() {
        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let path = dir.path().join(format!(
            "SpaceLens-disposable-test-{}.txt",
            std::process::id()
        ));
        fs::write(&path, b"Space Lens disposable test").unwrap();
        let folder = dir.path().join(format!(
            "SpaceLens-disposable-folder-{}",
            std::process::id()
        ));
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("nested.txt"), b"test").unwrap();
        let report = scan(dir.path(), &Progress::default()).unwrap();
        let entries: Vec<_> = report.entries[0]
            .children
            .iter()
            .map(|&id| report.entries[id].clone())
            .collect();
        let outcome = trash_batch(&report.entries[0].path, &entries);
        assert!(outcome.failed.is_empty());
        assert_eq!(outcome.moved.len(), 2);
        assert!(!path.exists());
        assert!(!folder.exists());
    }
}
