//! Pure fixtures verify frozen identities, complete impact parsing and bounded review policy.
use super::*;

fn hash(character: char) -> String {
    character.to_string().repeat(40)
}

/// The fixture has exact captured stdout, as the live query chain does before building its guards.
fn draft(args: &[&str]) -> Draft {
    let command = Command::new(args.iter().copied(), Purpose::Action);
    let kind = Kind::recognize(&command)
        .unwrap()
        .expect("recognized operation");
    let mut draft = Draft::new(command, kind, "history".into(), false).unwrap();
    draft.branch = "refs/heads/topic".into();
    draft.branch_bytes = b"refs/heads/topic\n".to_vec();
    draft.head = hash('a');
    draft.head_bytes = format!("{}\n", draft.head).into_bytes();
    draft.resolved = draft
        .targets
        .iter()
        .enumerate()
        .map(|(index, _)| hash(if index == 0 { 'b' } else { 'c' }))
        .collect();
    if let Some(impact) = &mut draft.hard_reset {
        impact.read_tree(b"").unwrap();
        impact.read_untracked(b"").unwrap();
    }
    draft
}

#[test]
fn ordinary_modes_are_recognized_without_claiming_continuation_or_services() {
    for args in [
        vec!["reset", "--soft", "main"],
        vec!["reset", "--mixed", "main"],
        vec!["reset", "--hard", "main"],
        vec!["merge", "--no-edit", "--", "main"],
        vec!["merge", "--no-ff", "--no-edit", "--", "main"],
        vec!["merge", "--squash", "--", "main"],
        vec!["rebase", "main"],
        vec!["cherry-pick", "--", "main", "topic"],
    ] {
        assert!(
            Kind::recognize(&Command::new(args, Purpose::Action))
                .unwrap()
                .is_some()
        );
    }
    for args in [
        vec!["switch", "main"],
        vec!["rebase", "--continue"],
        vec!["merge", "--abort"],
        vec!["reset", "--", "path"],
    ] {
        assert!(
            Kind::recognize(&Command::new(args, Purpose::Action))
                .unwrap()
                .is_none()
        );
    }
    let mut helper = Command::new(["rebase", "main"], Purpose::Action);
    helper.service = Some("rebase".into());
    assert!(Kind::recognize(&helper).unwrap().is_none());
}

#[test]
fn frozen_reset_checks_branch_and_head_before_using_target_oid() {
    let draft = draft(&["reset", "--hard", "moving-ref"]);
    let command = draft.frozen_command().unwrap();
    assert_eq!(command.args, ["rev-parse", "--symbolic-full-name", "HEAD"]);
    let Purpose::Guard { expected, next } = &command.purpose else {
        panic!("branch guard")
    };
    assert_eq!(expected, b"refs/heads/topic\n");
    assert_eq!(next.args, ["rev-parse", "--verify", "HEAD"]);
    let Purpose::Guard { expected, next } = &next.purpose else {
        panic!("HEAD guard")
    };
    assert_eq!(expected, &draft.head_bytes);
    assert_eq!(next.args, ["ls-files", "--others", "-z"]);
    let Purpose::Guard { expected, next } = &next.purpose else {
        panic!("untracked inventory guard")
    };
    assert!(expected.is_empty());
    assert_eq!(
        next.args,
        vec![
            "reset".to_owned(),
            "--hard".into(),
            "--no-recurse-submodules".into(),
            hash('b')
        ]
    );
    assert_eq!(command.effective_args(), next.args);
    assert!(command.preview().contains("moving-ref →"));
    assert!(command.preview().contains("refs/heads/topic"));
    assert!(command.preview().contains("may be stale"));
}

#[test]
fn frozen_merge_disables_autostash_and_can_be_reviewed_again() {
    for args in [
        vec!["merge", "--no-edit", "--", "main"],
        vec!["merge", "--ff-only", "--", "main"],
        vec!["merge", "--no-ff", "--no-edit", "--", "main"],
        vec!["merge", "--squash", "--", "main"],
    ] {
        let prepared = draft(&args).frozen_command().unwrap();
        let command = Command::new(prepared.effective_args().iter().cloned(), Purpose::Action);
        assert_eq!(command.args[1], "--no-autostash");
        assert_eq!(Kind::recognize(&command).unwrap(), Some(Kind::Merge));
        let mut second = Draft::new(command, Kind::Merge, "history".into(), false).unwrap();
        second.resolved = vec![hash('c')];
        let again = second.frozen_command().unwrap();
        assert_eq!(
            again
                .effective_args()
                .iter()
                .filter(|arg| *arg == "--no-autostash")
                .count(),
            1
        );
    }
    for args in [vec!["reset", "--hard", "main"], vec!["rebase", "main"]] {
        let prepared = draft(&args).frozen_command().unwrap();
        let command = Command::new(prepared.effective_args().iter().cloned(), Purpose::Action);
        assert!(Kind::recognize(&command).unwrap().is_some());
    }
}

#[test]
fn detached_head_remains_an_explicit_identity() {
    let mut draft = draft(&["reset", "--soft", "main"]);
    draft.branch = branch(b"HEAD\n").unwrap();
    draft.branch_bytes = b"HEAD\n".to_vec();
    let command = draft.frozen_command().unwrap();
    assert!(command.preview().contains("detached HEAD"));
    let Purpose::Guard { expected, .. } = command.purpose else {
        panic!("identity guard")
    };
    assert_eq!(expected, b"HEAD\n");
    assert!(branch(b"refs/heads/a\nrefs/heads/b\n").is_err());
}

#[test]
fn replay_order_and_remote_reachability_are_preserved_in_review() {
    let mut draft = draft(&["cherry-pick", "--", "first", "second"]);
    let bytes = format!(
        "{}\0{}\0中文 first\0{}\0{}\0second\0",
        hash('b'),
        hash('a'),
        hash('c'),
        hash('b')
    )
    .into_bytes();
    draft.read_commits(&bytes).unwrap();
    draft.commits[0].remotes =
        remote_refs(format!("refs/remotes/origin/main\t{}\n", hash('c')).as_bytes()).unwrap();
    let command = draft.frozen_command().unwrap();
    assert_eq!(
        command.effective_args(),
        ["cherry-pick".into(), "--".into(), hash('b'), hash('c')]
    );
    assert!(command.preview().contains("origin/main"));
    assert!(command.preview().contains("中文 first"));
    draft.resolved.swap(0, 1);
    assert!(
        draft.read_commits(&bytes).is_err(),
        "reordered or missing sources cannot be confirmed"
    );
}

#[test]
fn preview_refuses_truncated_or_oversized_commit_lists() {
    let entry = format!("{}\0\0title\0", hash('a'));
    assert_eq!(commits(entry.repeat(128).as_bytes()).unwrap().len(), 128);
    assert!(commits(entry.repeat(129).as_bytes()).is_err());
    assert!(commits(entry.trim_end_matches('\0').as_bytes()).is_err());
    assert!(commits(b"not-an-oid\0\0title\0").is_err());
    assert!(oid(format!("{}\n{}", hash('a'), hash('b')).as_bytes()).is_err());
}

#[test]
fn ordinary_rebase_range_includes_merges_and_disables_unreviewed_automatic_policy() {
    let mut draft = draft(&["rebase", "upstream"]);
    draft
        .read_commits(
            format!("{}\0{} {}\0merge title\0", hash('c'), hash('a'), hash('b')).as_bytes(),
        )
        .unwrap();
    let command = draft.frozen_command().unwrap();
    assert!(command.preview().contains("flattens merges"));
    assert!(command.preview().contains("[merge / 合并提交]"));
    assert!(
        command
            .effective_args()
            .iter()
            .any(|arg| arg == "--no-autostash")
    );
    assert!(
        command
            .effective_args()
            .iter()
            .any(|arg| arg == "--no-update-refs")
    );
    assert_eq!(command.effective_args().last(), Some(&hash('b')));
    let args = draft.log_args();
    assert!(args.contains(&"--max-count=129".into()));
    assert!(!args.contains(&"--no-merges".into()));
}

#[test]
fn cherry_pick_does_not_guess_merge_mainline_or_silently_drop_duplicates() {
    let mut draft = draft(&["cherry-pick", "--", "merge"]);
    assert!(
        draft
            .read_commits(format!("{}\0{} {}\0merge\0", hash('b'), hash('a'), hash('c')).as_bytes())
            .is_err()
    );
    draft.resolved.push(hash('b'));
    assert!(draft.validate_targets().is_err());
}

#[test]
fn remote_limits_and_oversized_review_fail_instead_of_omitting_impact() {
    let remote = format!("refs/remotes/origin/main\t{}\n", hash('a'));
    assert_eq!(
        remote_refs(remote.repeat(128).as_bytes()).unwrap().len(),
        128
    );
    assert!(remote_refs(remote.repeat(129).as_bytes()).is_err());
    let mut draft = draft(&["reset", "--mixed", "target"]);
    draft
        .read_commits(format!("{}\0\0{}\0", hash('a'), "x".repeat(65536)).as_bytes())
        .unwrap();
    assert!(draft.frozen_command().is_err());
}
