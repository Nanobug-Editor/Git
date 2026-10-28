//! Newline regressions exercise raw offset mapping and real autocrlf conflict snapshots.
use super::{Eol, equivalent, fingerprint, plan};
use crate::MergeFile;
use std::path::PathBuf;
use std::process::{Command, Output};

/// Before OpenDocument completes the center pane is still the ancestor, whose offsets differ from
/// both sides. Its temporary declaration must validate even when a side has longer UTF-8 text.
#[test]
fn opening_conflict_uses_each_readonly_snapshot_own_ranges() {
    let columns = vec!["base\n".to_owned(), "A\n".to_owned(), "B 中😀\n".to_owned()];
    let regions = plan::regions(&columns[1], &columns[0], &columns[2]).unwrap();
    let mut state = crate::State::default();
    state.merge_selected = "merge.txt".into();
    state.merge_files.insert(
        "merge.txt".into(),
        MergeFile {
            columns,
            regions,
            ..Default::default()
        },
    );
    let view = state
        .views()
        .into_iter()
        .find(|view| view.panel == "conflicts")
        .unwrap();
    view.document
        .validate()
        .expect("all three snapshots must validate before the native open receipt");
    let plugin_protocol::ui::Kind::Editor(plugin_protocol::ui::Editor {
        source: plugin_protocol::ui::EditorSource::Snapshot { text },
        changes,
        ..
    }) = &view.document.active_node("snapshot-1").unwrap().kind
    else {
        panic!("temporary ancestor pane")
    };
    assert_eq!(text, "A\n");
    assert_eq!(
        changes,
        &vec![plugin_protocol::api::TextRange { start: 0, end: 2 }]
    );
}

#[test]
fn newline_conversion_preserves_final_newline_and_literal_cr_content() {
    assert!(equivalent("中文\n😀\n", "中文\r\n😀\r\n"));
    assert!(!equivalent("中文\n", "中文"));
    assert!(!equivalent("a\r", "a"));
    assert_eq!(Eol::Crlf.convert("中文\n😀"), "中文\r\n😀");
    assert_eq!(Eol::Lf.convert("a\r\nb\r"), "a\nb\r");
    assert_eq!(Eol::Crlf.convert("a\r\nb\n"), "a\r\nb\r\n");
    assert_eq!(Eol::detect("a\r\nb\n"), Eol::Crlf);
    assert_eq!(Eol::detect("a\nb\r\nc\r\n"), Eol::Crlf);
    assert_eq!(Eol::detect("without newline"), Eol::Lf);
}

#[test]
fn logical_comparison_keeps_original_unicode_and_crlf_offsets() {
    let base = "start\n中文\nend\n";
    let result = "start\r\n中文😀\r\nend\r\n";
    let edits = plan::edits(base, result).unwrap();
    assert_eq!(edits.iter().filter(|edit| !edit.eol_only).count(), 1);
    let base_range = "start\n".len().."start\n中文\n".len();
    let mapped = plan::locate(&base_range, &edits).unwrap();
    assert_eq!(mapped, "start\r\n".len().."start\r\n中文😀\r\n".len());
    assert_eq!(&result[mapped], "中文😀\r\n");

    let reverse = plan::edits(result, base).unwrap();
    assert_eq!(
        plan::locate(&("start\r\n".len().."start\r\n中文😀\r\n".len()), &reverse),
        Some(base_range)
    );

    // Both formatting-only edits and content edits retain enough original bytes to reconstruct it.
    for (before, after) in [(base, result), (result, base)] {
        let mut reconstructed = before.to_owned();
        let mut edits = plan::edits(before, after).unwrap();
        edits.reverse();
        for edit in edits {
            reconstructed.replace_range(edit.base, &after[edit.other]);
        }
        assert_eq!(reconstructed, after);
    }
}

#[test]
fn eol_offsets_respect_line_boundaries_and_an_unterminated_unicode_tail() {
    let base = "甲\n乙\n尾😀";
    let result = "甲\r\n乙\r\n尾😀";
    let edits = plan::edits(base, result).unwrap();
    assert!(edits.iter().all(|edit| edit.eol_only));
    assert_eq!(plan::locate(&(0.."甲".len()), &edits), Some(0.."甲".len()));
    assert_eq!(
        plan::locate(&(0.."甲\n".len()), &edits),
        Some(0.."甲\r\n".len())
    );
    assert_eq!(
        plan::locate(&("甲\n".len().."甲\n乙\n".len()), &edits),
        Some("甲\r\n".len().."甲\r\n乙\r\n".len())
    );
    assert_eq!(
        plan::locate(&("甲\n乙\n".len()..base.len()), &edits),
        Some("甲\r\n乙\r\n".len()..result.len())
    );
    assert_eq!(
        plan::locate(&(base.len()..base.len()), &edits),
        Some(result.len()..result.len())
    );
    let changed = "甲\r\n乙\r\n新尾😀";
    assert_eq!(
        plan::locate(
            &("甲\n乙\n".len()..base.len()),
            &plan::edits(base, changed).unwrap()
        ),
        Some("甲\r\n乙\r\n".len()..changed.len())
    );
}

#[test]
fn eol_only_mappings_do_not_create_conflicts_or_lose_eof_insertions() {
    let base = "a\nb\nc\n";
    assert!(
        plan::regions(base, "a\r\nb\r\nc\r\n", base)
            .unwrap()
            .is_empty()
    );
    let changes = plan::regions(base, "a\r\nb\r\nc\r\n", "a\nB\nc\n").unwrap();
    assert_eq!(changes.len(), 1);
    assert!(!changes[0].conflict);
    assert_eq!(&"a\r\nb\r\nc\r\n"[changes[0].left.clone()], "b\r\n");

    let changes = plan::regions(base, "a\r\nB\r\nc\r\n", "a\nB\nc\n").unwrap();
    assert_eq!(changes.len(), 1);
    assert!(!changes[0].conflict);

    let changes = plan::regions("a\n", "a\nnew\n", "a\r\n").unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].base, 2..2);
    assert_eq!(changes[0].right, 3..3);
    let edits = plan::edits("a\n", "a").unwrap();
    assert!(edits.iter().any(|edit| !edit.eol_only));
}

/// Test-owned repositories isolate user Git configuration, hooks, credentials and filesystem cleanup.
struct GitFixture {
    temp_root: PathBuf,
    path: PathBuf,
}

impl GitFixture {
    fn new() -> Self {
        let temp_root = std::env::temp_dir().canonicalize().unwrap();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = temp_root.join(format!("nanobug-merge-eol-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let path = path.canonicalize().unwrap();
        assert_eq!(path.parent(), Some(temp_root.as_path()));
        std::fs::write(path.join("empty-global-config"), "").unwrap();
        let fixture = Self { temp_root, path };
        fixture.success(&["init", "-q", "-b", "left"]);
        fixture.success(&["config", "user.name", "Merge test"]);
        fixture.success(&["config", "user.email", "merge@example.invalid"]);
        fixture.success(&["config", "core.autocrlf", "true"]);
        fixture
    }

    fn git(&self, args: &[&str]) -> Output {
        // Git for Windows cannot read a verbatim \\?\ config filename; cleanup still uses canonical
        // paths, while this controlled fixture supplies Git its ordinary absolute spelling.
        let config_path = self.path.join("empty-global-config");
        let config_path = config_path.to_string_lossy();
        let config_path = if let Some(unc) = config_path.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            config_path
                .strip_prefix(r"\\?\")
                .unwrap_or(&config_path)
                .to_owned()
        };
        Command::new("git")
            .arg("--no-pager")
            .args(args)
            .current_dir(&self.path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", config_path)
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("Git must be available for the real autocrlf regression")
    }

    fn success(&self, args: &[&str]) -> Vec<u8> {
        let output = self.git(args);
        assert!(
            output.status.success(),
            "git {args:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn commit_text(&self, text: &str, message: &str) {
        std::fs::write(self.path.join("merge.txt"), text).unwrap();
        self.success(&["add", "--", "merge.txt"]);
        self.success(&["commit", "-q", "-m", message]);
    }
}

impl Drop for GitFixture {
    fn drop(&mut self) {
        // The only recursive removal target is this canonical, uniquely created direct temp child.
        if self.path.parent() == Some(self.temp_root.as_path())
            && self
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("nanobug-merge-eol-"))
        {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Apply the same exact block proposal and pending fingerprint consumed by the native edit response.
fn accept(file: &mut MergeFile, result: &mut String, key: usize, left: bool) {
    let block = file.blocks.iter().find(|block| block.key == key).unwrap();
    assert!(
        !block.ambiguous,
        "CRLF must not expand a block to the entire document"
    );
    let text = if left { &block.left } else { &block.right };
    let hash = fingerprint(text);
    file.pending_review = Some((key, hash));
    result.replace_range(block.range.clone(), text);
    file.observe(result).unwrap();
    assert_eq!(file.reviewed.get(&key), Some(&hash));
}

#[test]
fn real_autocrlf_merge_keeps_multiple_choices_and_nonconflicting_edits_mapped() {
    let git = GitFixture::new();
    let base = (0..40)
        .map(|line| format!("line {line:02}\n"))
        .collect::<String>();
    git.commit_text(&base, "base");
    git.success(&["tag", "base"]);
    let left = base
        .replace("line 04\n", "left one 中文\n")
        .replace("line 15\n", "left independent\n")
        .replace("line 30\n", "left two 😀\n");
    git.commit_text(&Eol::Crlf.convert(&left), "left");
    git.success(&["checkout", "-q", "-b", "right", "base"]);
    let right = base
        .replace("line 04\n", "right one 中文\n")
        .replace("line 22\n", "right independent\n")
        .replace("line 30\n", "right two 😀\n");
    git.commit_text(&Eol::Crlf.convert(&right), "right");
    git.success(&["checkout", "-q", "left"]);
    assert_eq!(
        git.git(&["merge", "--no-edit", "right"]).status.code(),
        Some(1)
    );
    let read_stage =
        |stage| String::from_utf8(git.success(&["show", &format!(":{stage}:merge.txt")])).unwrap();
    let columns = vec![read_stage(2), read_stage(1), read_stage(3)];
    assert!(columns.iter().all(|column| !column.contains('\r')));
    let mut result = std::fs::read_to_string(git.path.join("merge.txt")).unwrap();
    assert_eq!(Eol::detect(&result), Eol::Crlf);
    assert_eq!(super::blocks(&result).len(), 2);
    let original = result.clone();
    let regions = plan::regions(&columns[1], &columns[0], &columns[2]).unwrap();
    let mut file = MergeFile {
        columns,
        regions,
        ..Default::default()
    };
    file.observe(&result).unwrap();
    assert_eq!(file.blocks.len(), 2);
    assert!(
        file.nonconflicting.is_empty(),
        "Git already applied independent edits"
    );
    let keys = file
        .blocks
        .iter()
        .map(|block| block.key)
        .collect::<Vec<_>>();
    accept(&mut file, &mut result, keys[0], true);
    accept(&mut file, &mut result, keys[1], false);
    assert!(file.all_reviewed());
    assert!(result.contains("left independent\r\n"));
    assert!(result.contains("right independent\r\n"));
    assert!(result.contains("left one 中文\r\n"));
    assert!(result.contains("right two 😀\r\n"));
    assert_eq!(
        result,
        Eol::Crlf.convert(&result),
        "Proposals must not introduce mixed EOL"
    );

    // Native Undo invalidates a review by the original bytes; the immutable LF index stays intact.
    file.observe(&original).unwrap();
    assert!(file.reviewed.is_empty());
    assert!(!file.all_reviewed());
    assert_eq!(file.columns[1], base);

    // Starting again from the ancestor must still locate independent edits through all CRLF shifts.
    result = Eol::Crlf.convert(&file.columns[1]);
    file.observe(&result).unwrap();
    assert_eq!(file.nonconflicting.len(), 2);
    let mut edits = file.nonconflicting.clone();
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.range.start));
    for edit in edits {
        result.replace_range(edit.range.start..edit.range.end, &edit.text);
    }
    file.observe(&result).unwrap();
    assert!(file.nonconflicting.is_empty());
    assert!(result.contains("left independent\r\n"));
    assert!(result.contains("right independent\r\n"));
    accept(&mut file, &mut result, keys[0], false);
    accept(&mut file, &mut result, keys[1], true);
    assert!(file.all_reviewed());
    assert_eq!(result, Eol::Crlf.convert(&result));
}
