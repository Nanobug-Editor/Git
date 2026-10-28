//! Native Git regressions run the real read-only preview queries and guarded mutations in owned temp repos.
//! No user repository, global Git configuration, service, network or native UI is involved.
use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command as Process, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_REPO: AtomicU64 = AtomicU64::new(0);

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "nanobug-operation-preview-{}-{stamp}-{}",
            std::process::id(),
            NEXT_REPO.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let repo = Self { root };
        repo.ok(&["init", "-q", "-b", "main"]);
        repo.ok(&["config", "user.name", "Preview Test"]);
        repo.ok(&["config", "user.email", "preview@example.invalid"]);
        repo.write("tracked", b"baseline\n");
        repo.write(".gitignore", b"ignored.bin\n");
        repo.ok(&["add", "."]);
        repo.ok(&["commit", "-q", "--no-gpg-sign", "-m", "baseline"]);
        repo
    }

    fn git(&self, args: &[&str]) -> Output {
        let mut process = Process::new("git");
        // An inherited GIT_DIR/WORK_TREE/INDEX_FILE must never redirect a destructive fixture
        // command out of its owned temporary repository; discard all inherited Git overrides.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                process.env_remove(name);
            }
        }
        process
            .args(args)
            .current_dir(&self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join(".git/no-global-config"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("Git must be available for native preview regression")
    }

    fn ok(&self, args: &[&str]) -> Vec<u8> {
        let output = self.git(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    /// Drive actual query output through the production preparation state machine.
    fn preview(&self, args: &[&str]) -> Result<Command, api::Failure> {
        let mut state = State::default();
        assert!(state.prepare_operation_preview(
            Command::new(args.iter().copied(), Purpose::Action),
            "history".into(),
            false
        )?);
        while let Some(command) = state.queue.pop_front() {
            let Purpose::OperationPreview(query) = command.purpose else {
                panic!("only preparation queries")
            };
            let output = self.git(&command.args.iter().map(String::as_str).collect::<Vec<_>>());
            state.operation_preview_finished(
                query,
                &output.stdout,
                &output.stderr,
                output.status.code().unwrap_or(255) as u32,
            )?;
        }
        Ok(state.pending.expect("complete confirmation"))
    }

    /// Exercise generic guard receipts, stopping before the destructive action on any stale input.
    fn execute(&self, mut command: Command) -> Result<Output, String> {
        loop {
            let output = self.git(&command.args.iter().map(String::as_str).collect::<Vec<_>>());
            if let Purpose::Guard { expected, next } = command.purpose {
                if !output.status.success() || output.stdout != expected {
                    return Err(format!("stale guard: {:?}", command.args));
                }
                command = *next;
            } else {
                return Ok(output);
            }
        }
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        // Only the uniquely created, direct child of the temp directory is owned by this fixture.
        assert_eq!(self.root.parent(), Some(std::env::temp_dir().as_path()));
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn hard_reset_reviews_real_file_directory_ignored_and_binary_obstructions() {
    let repo = Repo::new();
    repo.write("obsolete", b"old tracked file");
    repo.ok(&["add", "obsolete"]);
    repo.ok(&["commit", "-q", "--no-gpg-sign", "-m", "old path"]);
    repo.ok(&["checkout", "-q", "-b", "target"]);
    repo.ok(&["rm", "-q", "obsolete"]);
    for path in ["exact.bin", "dir/file", "replace-dir", "ignored.bin"] {
        repo.write(path, b"target\0bytes");
    }
    repo.ok(&["add", "-f", "."]);
    repo.ok(&["commit", "-q", "--no-gpg-sign", "-m", "target tree"]);
    repo.ok(&["checkout", "-q", "main"]);
    fs::remove_file(repo.root.join("obsolete")).unwrap();
    let paths = [
        "exact.bin",
        "dir",
        "replace-dir/child.bin",
        "ignored.bin",
        "spare.bin",
        "obsolete/child.bin",
    ];
    for path in paths {
        repo.write(path, b"untracked\0original");
    }
    let command = repo.preview(&["reset", "--hard", "target"]).unwrap();
    let preview = command.preview();
    for path in ["exact.bin", "dir", "replace-dir/child.bin", "ignored.bin"] {
        assert!(
            preview.contains(&format!("\"{path}\"")),
            "missing obstruction {path}: {preview}"
        );
    }
    assert!(!preview.contains("spare.bin"));
    let output = repo.execute(command).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for path in paths {
        if fs::read(repo.root.join(path)).ok().as_deref() != Some(b"untracked\0original") {
            assert!(
                preview.contains(&format!("\"{path}\"")),
                "Git destroyed an unreviewed path: {path}"
            );
        }
    }
    assert_eq!(
        fs::read(repo.root.join("spare.bin")).unwrap(),
        b"untracked\0original"
    );
}

#[test]
fn new_untracked_blocker_after_review_prevents_reset_execution() {
    let repo = Repo::new();
    repo.ok(&["checkout", "-q", "-b", "target"]);
    repo.write("new-blocker", b"target");
    repo.ok(&["add", "."]);
    repo.ok(&["commit", "-q", "--no-gpg-sign", "-m", "target"]);
    repo.ok(&["checkout", "-q", "main"]);
    let before = repo.ok(&["rev-parse", "HEAD"]);
    let command = repo.preview(&["reset", "--hard", "target"]).unwrap();
    repo.write("new-blocker", b"created after review");
    assert!(repo.execute(command).unwrap_err().contains("ls-files"));
    assert_eq!(repo.ok(&["rev-parse", "HEAD"]), before);
    assert_eq!(
        fs::read(repo.root.join("new-blocker")).unwrap(),
        b"created after review"
    );
}

#[test]
fn merge_autostash_configuration_cannot_stash_or_rewrite_dirty_files() {
    let repo = Repo::new();
    repo.ok(&["checkout", "-q", "-b", "target"]);
    repo.write("tracked", b"target\n");
    repo.ok(&["add", "."]);
    repo.ok(&["commit", "-q", "--no-gpg-sign", "-m", "target"]);
    repo.ok(&["checkout", "-q", "main"]);
    repo.ok(&["config", "merge.autoStash", "true"]);
    repo.write("tracked", b"uncommitted work\n");
    let before = repo.ok(&["rev-parse", "HEAD"]);
    for args in [
        vec!["merge", "--no-edit", "--", "target"],
        vec!["merge", "--ff-only", "--", "target"],
        vec!["merge", "--no-ff", "--no-edit", "--", "target"],
        vec!["merge", "--squash", "--", "target"],
    ] {
        let command = repo.preview(&args).unwrap();
        let output = repo.execute(command).unwrap();
        assert!(
            !output.status.success(),
            "dirty merge must fail without implicit stash: {args:?}"
        );
        assert_eq!(repo.ok(&["rev-parse", "HEAD"]), before);
        assert_eq!(
            fs::read(repo.root.join("tracked")).unwrap(),
            b"uncommitted work\n"
        );
        assert!(repo.ok(&["stash", "list"]).is_empty());
        assert!(!repo.root.join(".git/MERGE_AUTOSTASH").exists());
    }
}

#[test]
fn nested_repository_obstruction_is_explicitly_refused() {
    let repo = Repo::new();
    repo.ok(&["checkout", "-q", "-b", "target"]);
    repo.write("nested/file", b"target");
    repo.ok(&["add", "."]);
    repo.ok(&["commit", "-q", "--no-gpg-sign", "-m", "target"]);
    repo.ok(&["checkout", "-q", "main"]);
    let nested = repo.root.join("nested");
    fs::create_dir(&nested).unwrap();
    repo.ok(&["init", "-q", nested.to_str().unwrap()]);
    let error = repo
        .preview(&["reset", "--hard", "target"])
        .err()
        .expect("unsafe nested inventory rejected");
    assert!(error.to_string().contains("nested repository"), "{error}");
    assert!(Path::new(&nested).join(".git").is_dir());
}
