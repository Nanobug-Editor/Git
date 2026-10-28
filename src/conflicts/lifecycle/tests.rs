//! Complete synthetic Git receipts test operation epochs independently of native buffers and UI.
use super::*;

fn oid(character: char) -> String {
    character.to_string().repeat(40)
}
fn conflict(path: &str, stages: &str) -> Change {
    Change {
        path: path.into(),
        conflict: true,
        index_entry: stages.as_bytes().to_vec(),
        ..Default::default()
    }
}
fn sample(
    lifecycle: &mut Lifecycle,
    changes: Vec<Change>,
    markers: &[(Marker, char)],
) -> Result<Publication, ()> {
    let round = lifecycle.begin();
    assert!(!lifecycle.ready());
    assert!(lifecycle.status(round, Some(changes)).is_none());
    let mut publication = None;
    for marker in Marker::ALL {
        let value = markers
            .iter()
            .find(|(kind, _)| *kind as usize == marker as usize)
            .map(|(_, character)| oid(*character));
        publication = lifecycle.probe(
            round,
            marker,
            value.as_deref().unwrap_or("").as_bytes(),
            if value.is_some() { 0 } else { 1 },
        );
    }
    publication.unwrap()
}

/// A completed merge may retain a resolved file until its marker disappears, but the next Git
/// operation never inherits that file or its old alternatives. No editor write/close is requested.
#[test]
fn conflict_lifecycle_merge_end_and_next_merge_retire_only_old_proposals() {
    let mut state = State::default();
    let first = sample(
        &mut state.conflict_lifecycle,
        vec![conflict("old.txt", "stages-a")],
        &[(Marker::Merge, 'a')],
    );
    state.publish_conflict_observation(Some(first)).unwrap();
    state.merge_files.insert(
        "old.txt".into(),
        MergeFile {
            resolved: true,
            columns: vec!["old proposal".into(); 3],
            ..Default::default()
        },
    );
    let same = sample(
        &mut state.conflict_lifecycle,
        vec![],
        &[(Marker::Merge, 'a')],
    );
    state.publish_conflict_observation(Some(same)).unwrap();
    assert!(state.merge_files.contains_key("old.txt"));
    let ended = sample(&mut state.conflict_lifecycle, vec![], &[]);
    state.publish_conflict_observation(Some(ended)).unwrap();
    assert!(state.merge_files.is_empty());
    let next = sample(
        &mut state.conflict_lifecycle,
        vec![conflict("new.txt", "stages-b")],
        &[(Marker::Merge, 'b')],
    );
    state.publish_conflict_observation(Some(next)).unwrap();
    assert!(!state.merge_files.contains_key("old.txt"));
    assert!(
        state.editor_tasks.is_empty(),
        "proposal retirement cannot close or alter native documents"
    );
}

/// REBASE_HEAD identifies the current step, not the lifetime. Same-path new stages retire that
/// proposal alone; other resolved files survive until orig-head/onto changes or rebase ends.
#[test]
fn conflict_lifecycle_rebase_steps_retain_resolved_list_and_refresh_new_stages() {
    let mut state = State::default();
    let identity = [
        (Marker::RebaseOriginal, 'a'),
        (Marker::RebaseOnto, 'b'),
        (Marker::RebaseStep, 'c'),
    ];
    let first = sample(
        &mut state.conflict_lifecycle,
        vec![conflict("again.txt", "old")],
        &identity,
    );
    state.publish_conflict_observation(Some(first)).unwrap();
    state.merge_files.insert(
        "done.txt".into(),
        MergeFile {
            resolved: true,
            ..Default::default()
        },
    );
    state.merge_files.insert(
        "again.txt".into(),
        MergeFile {
            index_entry: b"old".to_vec(),
            resolved: true,
            ..Default::default()
        },
    );
    let advanced = [
        (Marker::RebaseOriginal, 'a'),
        (Marker::RebaseOnto, 'b'),
        (Marker::RebaseStep, 'd'),
    ];
    let next = sample(
        &mut state.conflict_lifecycle,
        vec![conflict("again.txt", "new")],
        &advanced,
    )
    .unwrap();
    assert!(!next.changed);
    state.publish_conflict_observation(Some(Ok(next))).unwrap();
    assert!(state.merge_files.contains_key("done.txt"));
    assert!(!state.merge_files.contains_key("again.txt"));
    let different = [
        (Marker::RebaseOriginal, 'e'),
        (Marker::RebaseOnto, 'b'),
        (Marker::RebaseStep, 'f'),
    ];
    let next = sample(
        &mut state.conflict_lifecycle,
        vec![conflict("other.txt", "next")],
        &different,
    );
    state.publish_conflict_observation(Some(next)).unwrap();
    assert!(state.merge_files.is_empty());
}

/// A half refresh and failed/partial identity cannot apply an old ancestor, even if a delayed
/// document receipt could make its old geometry/read flag appear current again.
#[test]
fn conflict_lifecycle_unknown_and_resolved_entries_reject_old_write_actions() {
    let mut state = State::default();
    let first = sample(
        &mut state.conflict_lifecycle,
        vec![conflict("old.txt", "old")],
        &[(Marker::Merge, 'a')],
    );
    state.publish_conflict_observation(Some(first)).unwrap();
    state.merge_selected = "old.txt".into();
    state.merge_files.insert(
        "old.txt".into(),
        MergeFile {
            resolved: true,
            read_valid: true,
            ..Default::default()
        },
    );
    for action in [
        "merge-from-base",
        "merge-nonconflicting",
        "merge-block-0-left",
        "resolved",
    ] {
        assert!(state.conflict_action(action).is_err());
    }
    assert!(state.choose_conflict_version(2).is_err());
    state.merge_files.get_mut("old.txt").unwrap().resolved = false;
    let round = state.conflict_lifecycle.begin();
    assert!(
        state
            .conflict_lifecycle
            .status(round, Some(vec![conflict("new.txt", "new")]))
            .is_none()
    );
    assert!(
        state.merge_files.contains_key("old.txt"),
        "a partial sample cannot retire the old session"
    );
    assert!(state.conflict_action("merge-from-base").is_err());
    let mut publication = None;
    for marker in Marker::ALL {
        let value = (marker as usize == Marker::RebaseOriginal as usize).then(|| oid('a'));
        publication = state.conflict_lifecycle.probe(
            round,
            marker,
            value.as_deref().unwrap_or("").as_bytes(),
            if value.is_some() { 0 } else { 1 },
        );
    }
    state.publish_conflict_observation(publication).unwrap();
    assert!(!state.conflict_lifecycle.ready());
    assert!(state.conflict_action("merge-from-base").is_err());
    assert!(state.choose_conflict_version(3).is_err());
    assert!(state.editor_tasks.is_empty());
}

/// Unmarked stash/index conflicts are a bounded independent epoch rather than a permanent cache.
#[test]
fn conflict_lifecycle_unmarked_conflict_epoch_finishes_at_a_complete_clean_index() {
    let mut lifecycle = Lifecycle::default();
    assert!(
        sample(&mut lifecycle, vec![conflict("stash.txt", "one")], &[])
            .unwrap()
            .changed
    );
    assert!(
        !sample(&mut lifecycle, vec![conflict("stash.txt", "two")], &[])
            .unwrap()
            .changed
    );
    assert!(sample(&mut lifecycle, vec![], &[]).unwrap().changed);
    assert!(
        sample(&mut lifecycle, vec![conflict("new.txt", "three")], &[])
            .unwrap()
            .changed
    );
}

/// Successful Git rebase can leave REBASE_HEAD behind after removing its operation directory.
/// That step reference alone must not indefinitely retain the completed operation's proposals.
#[test]
fn conflict_lifecycle_completed_rebase_ignores_a_residual_step_reference() {
    let mut lifecycle = Lifecycle::default();
    let started = sample(
        &mut lifecycle,
        vec![conflict("again.txt", "one")],
        &[
            (Marker::RebaseOriginal, 'a'),
            (Marker::RebaseOnto, 'b'),
            (Marker::RebaseStep, 'c'),
        ],
    )
    .unwrap();
    assert!(started.changed);
    let ended = sample(&mut lifecycle, vec![], &[(Marker::RebaseStep, 'd')]).unwrap();
    assert!(ended.changed);
    assert!(ended.operation.is_none());
    assert!(lifecycle.ready());
    let index_conflict = sample(
        &mut lifecycle,
        vec![conflict("stash.txt", "new")],
        &[(Marker::RebaseStep, 'd')],
    )
    .unwrap();
    assert!(index_conflict.changed);
    assert!(index_conflict.operation.is_none());
}

/// A linked metadata reference cannot escape to another worktree through path or ref syntax.
#[test]
fn conflict_lifecycle_worktree_prefix_is_exact_and_rejects_unsafe_components() {
    assert_eq!(
        worktree_prefix("C:/repo/.git", "C:/repo/.git"),
        Ok(String::new())
    );
    assert_eq!(
        worktree_prefix("D:\\repo\\.git\\worktrees\\link-中", "D:\\repo\\.git"),
        Ok("worktrees/link-中/".into())
    );
    assert_eq!(
        worktree_prefix("C:/repo/.git/worktrees/linked worktree", "C:/repo/.git"),
        Ok("worktrees/linked worktree/".into())
    );
    for directory in [
        ".git/worktrees/linked",
        "C:/repo/.git-other/worktrees/linked",
        "C:/repo/.git/worktrees/../other",
        "C:/repo/.git/worktrees/linked/nested",
        "C:/repo/.git/worktrees/linked\nother",
        "C:/repo/.git/worktrees/",
    ] {
        assert!(
            worktree_prefix(directory, "C:/repo/.git").is_err(),
            "{directory}"
        );
    }
    assert!(worktree_prefix("C:/repo/.git", "C:/repo/../other/.git").is_err());
}

#[cfg(not(target_arch = "wasm32"))]
mod real_git {
    use super::*;
    use std::{
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    /// Each fixture owns one fresh temp root, including any linked checkout. Git only receives
    /// argument arrays and fixture-local configuration; no user repository or hooks are used.
    struct Repository {
        root: PathBuf,
        checkout: PathBuf,
    }
    impl Repository {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "nanobug-conflict-lifecycle-{}-{nonce}",
                std::process::id()
            ));
            let checkout = root.join("main");
            std::fs::create_dir_all(&checkout).unwrap();
            let repository = Self { root, checkout };
            success(&repository.checkout, &["init", "-b", "main"]);
            repository
        }
        fn commit(&self, text: &str, title: &str) {
            std::fs::write(self.checkout.join("again.txt"), text).unwrap();
            success(&self.checkout, &["add", "--", "again.txt"]);
            success(&self.checkout, &["commit", "-m", title]);
        }
    }
    impl Drop for Repository {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn git(path: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(path)
            .args([
                "--literal-pathspecs",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.hooksPath=.no-hooks",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "rerere.enabled=false",
                "-c",
                "rebase.updateRefs=false",
                "-c",
                "user.name=Lifecycle Test",
                "-c",
                "user.email=lifecycle@example.invalid",
            ])
            .args(args)
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .output()
            .unwrap()
    }
    fn success(path: &Path, args: &[&str]) -> Vec<u8> {
        let output = git(path, args);
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    fn reference(path: &Path, name: &str) -> String {
        String::from_utf8(success(path, &["rev-parse", "--verify", name]))
            .unwrap()
            .trim()
            .into()
    }

    /// A dangling marker is still an active operation, just as Git's rev-parse reports it.
    #[test]
    fn real_operation_batch_retains_dangling_marker_identity() {
        let repository = Repository::new();
        std::fs::write(
            repository.checkout.join(".git/MERGE_HEAD"),
            format!("{}\n", "a".repeat(40)),
        )
        .unwrap();
        let mut lifecycle = Lifecycle::default();
        let publication = observe(&mut lifecycle, &repository.checkout);
        assert_eq!(publication.operation.as_deref(), Some("merge"));
        assert_eq!(
            publication.oid.as_deref(),
            Some(reference(&repository.checkout, "MERGE_HEAD").as_str())
        );
    }

    /// Cached paths cannot redirect the helper into another checkout, and unreadable metadata is unknown.
    #[test]
    fn real_operation_batch_checks_checkout_identity_and_bounded_marker_bytes() {
        let repository = Repository::new();
        let other = Repository::new();
        let path = &repository.checkout;
        let mut request = operation_observation::Request {
            mode: "observe-operations".into(),
            workspace: path.to_str().unwrap().into(),
            git_dir: path.join(".git").to_str().unwrap().into(),
            common_dir: path.join(".git").to_str().unwrap().into(),
        };
        assert!(operation_observation::read(&request, path).is_ok());
        assert!(operation_observation::read(&request, &other.checkout).is_err());
        let expected = request.git_dir.clone();
        request.git_dir = other.checkout.join(".git").to_str().unwrap().into();
        assert!(operation_observation::read(&request, path).is_err());
        request.git_dir = expected;
        for bytes in [b"invalid identity\n".to_vec(), vec![b'a'; 4097]] {
            std::fs::write(path.join(".git/MERGE_HEAD"), bytes).unwrap();
            assert!(operation_observation::read(&request, path).is_err());
        }
    }

    /// Feed the same complete, bounded porcelain/ref receipts that the controlled process path
    /// supplies in production. Git itself resolves metadata inside a linked worktree's gitdir.
    fn observe(lifecycle: &mut Lifecycle, path: &Path) -> Publication {
        let directories = String::from_utf8(success(
            path,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-dir",
                "--git-common-dir",
            ],
        ))
        .unwrap();
        let directories = directories.lines().collect::<Vec<_>>();
        assert_eq!(directories.len(), 2);
        let prefix = worktree_prefix(directories[0], directories[1]).unwrap();
        if path.file_name().and_then(|name| name.to_str()) == Some("linked worktree") {
            assert_eq!(prefix, "worktrees/linked-worktree/");
        }
        let round = lifecycle.begin();
        let changes = model::status(&success(
            path,
            &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        ))
        .unwrap();
        assert!(lifecycle.status(round, Some(changes)).is_none());
        let request = operation_observation::Request {
            mode: "observe-operations".into(),
            workspace: path.to_str().unwrap().into(),
            git_dir: directories[0].into(),
            common_dir: directories[1].into(),
        };
        let output = operation_observation::read(&request, path).unwrap();
        lifecycle
            .probes(round, "", output.as_bytes(), 0)
            .expect("the complete identity batch arrived")
            .unwrap_or_else(|()| panic!("incomplete Git operation identity: {:?}", output))
    }

    /// Both rebase backends must retain resolved entries between steps. The step OID and index
    /// stages change, while orig-head/onto keep the operation identity stable until completion.
    fn two_conflicts(linked: bool, backend: &str) {
        let repository = Repository::new();
        repository.commit("base\n", "base");
        success(&repository.checkout, &["switch", "-c", "topic"]);
        repository.commit("topic first\n", "first");
        let first_commit = reference(&repository.checkout, "HEAD");
        repository.commit("topic second\n", "second");
        let original = reference(&repository.checkout, "HEAD");
        success(&repository.checkout, &["switch", "main"]);
        repository.commit("main\n", "upstream");
        let onto = reference(&repository.checkout, "HEAD");
        let checkout = if linked {
            let path = repository.root.join("linked worktree");
            success(
                &repository.checkout,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "linked",
                    path.to_str().unwrap(),
                    "topic",
                ],
            );
            assert!(path.join(".git").is_file());
            path
        } else {
            success(&repository.checkout, &["switch", "topic"]);
            repository.checkout.clone()
        };
        let first_conflict = git(&checkout, &["rebase", backend, "main"]);
        assert!(!first_conflict.status.success());
        let mut state = State::default();
        let first = observe(&mut state.conflict_lifecycle, &checkout);
        assert!(first.changed);
        assert_eq!(
            first.operation.as_deref(),
            Some("rebase"),
            "backend={backend} linked={linked}; stderr={}; changes={:?}",
            String::from_utf8_lossy(&first_conflict.stderr),
            first.changes
        );
        assert_eq!(first.oid.as_deref(), Some(first_commit.as_str()));
        assert_eq!(
            state.conflict_lifecycle.scope,
            Some(Scope::Rebase {
                apply: backend == "--apply",
                original,
                onto,
            })
        );
        let first_stages = first
            .changes
            .iter()
            .find(|change| change.path == "again.txt" && change.conflict)
            .expect("first real unmerged porcelain entry")
            .index_entry
            .clone();
        state.publish_conflict_observation(Some(Ok(first))).unwrap();
        state.merge_files.insert(
            "again.txt".into(),
            MergeFile {
                index_entry: first_stages.clone(),
                ..Default::default()
            },
        );
        state.merge_files.insert(
            "resolved-before.txt".into(),
            MergeFile {
                resolved: true,
                ..Default::default()
            },
        );
        std::fs::write(checkout.join("again.txt"), "resolved first\n").unwrap();
        success(&checkout, &["add", "--", "again.txt"]);
        let staged = observe(&mut state.conflict_lifecycle, &checkout);
        assert!(!staged.changed);
        state
            .publish_conflict_observation(Some(Ok(staged)))
            .unwrap();
        assert!(state.merge_files["again.txt"].resolved);
        let second_conflict = git(&checkout, &["rebase", "--continue"]);
        assert!(!second_conflict.status.success());
        let second = observe(&mut state.conflict_lifecycle, &checkout);
        assert!(
            !second.changed,
            "changing REBASE_HEAD cannot start a new session"
        );
        assert_eq!(second.operation.as_deref(), Some("rebase"));
        assert_ne!(second.oid.as_deref(), Some(first_commit.as_str()));
        let second_stages = &second
            .changes
            .iter()
            .find(|change| change.path == "again.txt" && change.conflict)
            .expect("second real unmerged porcelain entry")
            .index_entry;
        assert_ne!(second_stages, &first_stages);
        state
            .publish_conflict_observation(Some(Ok(second)))
            .unwrap();
        assert!(state.merge_files.contains_key("resolved-before.txt"));
        assert!(!state.merge_files.contains_key("again.txt"));
        std::fs::write(checkout.join("again.txt"), "resolved second\n").unwrap();
        success(&checkout, &["add", "--", "again.txt"]);
        success(&checkout, &["rebase", "--continue"]);
        let ended = observe(&mut state.conflict_lifecycle, &checkout);
        assert!(ended.changed);
        assert!(ended.operation.is_none());
        assert!(ended.changes.iter().all(|change| !change.conflict));
        state.publish_conflict_observation(Some(Ok(ended))).unwrap();
        assert!(state.merge_files.is_empty());
        assert!(
            state.editor_tasks.is_empty(),
            "retirement does not touch native drafts"
        );
    }

    #[test]
    fn conflict_lifecycle_real_rebase_identity_survives_two_same_path_conflicts() {
        for backend in ["--merge", "--apply"] {
            two_conflicts(false, backend);
        }
    }

    #[test]
    fn conflict_lifecycle_real_linked_worktree_uses_its_own_rebase_identity() {
        for backend in ["--merge", "--apply"] {
            two_conflicts(true, backend);
        }
    }
}
