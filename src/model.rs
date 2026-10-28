//! Byte-delimited Git records become immutable domain values; file names are never shell fragments.
use std::collections::{BTreeMap, BTreeSet};
mod history_changes;
pub(crate) use history_changes::{HistoryChange, history_changes};

/// A status keeps index and worktree columns separate, including partially staged paths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Change {
    pub path: String,
    pub index: char,
    pub worktree: char,
    pub conflict: bool,
    /// Exact known index stages. Intent-to-add hides its actual mode/blob in porcelain, so its
    /// empty value is not proof that no index entry exists; `intent_status` guards that case.
    pub index_entry: Vec<u8>,
    /// Recheck this complete porcelain record for intent-to-add rather than inventing an absent
    /// ls-files entry or a hash-format-dependent empty blob. External index changes retire it.
    pub intent_status: Option<Vec<u8>>,
    /// Rename pairs remain one selected change when restoring index entries or reading HEAD.
    pub original: Option<String>,
}

/// Parent identities, display labels and timestamps come from the same paged log record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Commit {
    pub oid: String,
    pub parents: Vec<String>,
    pub title: String,
    pub labels: String,
    pub author: String,
    pub time: String,
}

/// Porcelain v2 -z names remain unquoted, so spaces/newlines cannot split a path into arguments.
pub(crate) fn status(bytes: &[u8]) -> Result<Vec<Change>, String> {
    let mut records = bytes.split(|b| *b == 0).filter(|s| !s.is_empty());
    let mut result = Vec::new();
    while let Some(record) = records.next() {
        let record=std::str::from_utf8(record).map_err(|_| "Git path is not UTF-8; this file cannot be operated on through the current argv interface")?;
        let (count, conflict) = match record.as_bytes().first() {
            Some(b'1') => (9, false),
            Some(b'2') => (10, false),
            Some(b'u') => (11, true),
            Some(b'?') => {
                result.push(Change {
                    path: record[2..].into(),
                    index: '.',
                    worktree: '?',
                    conflict: false,
                    index_entry: Vec::new(),
                    intent_status: None,
                    original: None,
                });
                continue;
            }
            _ => continue,
        };
        let fields: Vec<_> = record.splitn(count, ' ').collect();
        if fields.len() != count {
            return Err("Incomplete porcelain record".into());
        }
        let xy = fields[1].as_bytes();
        if xy.len() != 2 {
            return Err("Invalid porcelain status".into());
        }
        let path = fields[count - 1];
        let intent_status = (!conflict && fields[4] == "000000" && xy[1] == b'A').then(|| {
            let mut bytes = record.as_bytes().to_vec();
            bytes.push(0);
            bytes
        });
        let index_entry = if conflict {
            (1..=3)
                .filter(|stage| fields[stage + 2] != "000000")
                .map(|stage| {
                    format!(
                        "{} {} {stage}\t{path}\0",
                        fields[stage + 2],
                        fields[stage + 6]
                    )
                })
                .collect::<String>()
                .into_bytes()
        } else if fields[4] == "000000" {
            Vec::new()
        } else {
            format!("{} {} 0\t{}\0", fields[4], fields[7], path).into_bytes()
        };
        let original = if count == 10 {
            Some(
                std::str::from_utf8(records.next().ok_or("Missing rename source")?)
                    .map_err(|_| "Rename source is not UTF-8")?
                    .to_owned(),
            )
        } else {
            None
        };
        result.push(Change {
            path: path.into(),
            index: xy[0] as char,
            worktree: xy[1] as char,
            conflict,
            index_entry,
            intent_status,
            original,
        });
    }
    Ok(result)
}

/// Six NUL fields per commit avoid parsing human-formatted Git log output.
pub(crate) fn log(bytes: &[u8]) -> Result<Vec<Commit>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Commit metadata is not UTF-8")?;
    let fields: Vec<_> = text.split('\0').collect();
    let mut commits = Vec::new();
    for record in fields.chunks(6) {
        if record.len() < 6 {
            if record.iter().any(|s| !s.trim().is_empty()) {
                return Err("Incomplete commit record".into());
            }
            break;
        }
        commits.push(Commit {
            oid: record[0].trim().into(),
            parents: record[1].split_whitespace().map(str::to_owned).collect(),
            title: record[2].into(),
            labels: record[3].into(),
            author: record[4].into(),
            time: record[5].into(),
        });
    }
    Ok(commits)
}

/// A directory tree stores unique original paths in their first-seen presentation order.
#[derive(Default)]
pub(crate) struct Directory {
    pub children: BTreeMap<String, Directory>,
    pub files: Vec<(String, String)>,
    /// Live Git status and retained conflict drafts may identify the same leaf. Membership is
    /// separate from presentation order so repeated insertion cannot duplicate a paged node.
    file_paths: BTreeSet<String>,
}
impl Directory {
    /// Insert each repository-relative path once, retaining its first slot and original spelling.
    pub fn insert(&mut self, path: &str) {
        let parts: Vec<_> = path.split('/').collect();
        let mut current = self;
        for part in &parts[..parts.len() - 1] {
            current = current.children.entry((*part).into()).or_default();
        }
        if current.file_paths.insert(path.into()) {
            current
                .files
                .push((parts.last().unwrap().to_string(), path.into()));
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod directory_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::{path::PathBuf, process::Command};

    /// The fixture owns a newly created absolute temp directory; it never touches a user repository.
    struct Repository(PathBuf);
    impl Repository {
        fn new(format: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "nanobug-index-{}-{nonce}-{format}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            let repository = Self(path);
            repository.git(&["init", "-b", "main", &format!("--object-format={format}")]);
            repository
        }
        fn git(&self, args: &[&str]) -> Vec<u8> {
            let output = Command::new("git")
                .current_dir(&self.0)
                .args(["--literal-pathspecs", "-c", "core.autocrlf=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        }
    }
    impl Drop for Repository {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Real Git is the oracle for zero porcelain identities and nonzero ls-files entries. Both
    /// object formats must use the same observation without hardcoding an empty-blob hash/mode.
    #[test]
    fn index_edges_intent_observation_matches_git_for_sha1_sha256_and_literal_paths() {
        for format in ["sha1", "sha256"] {
            let repository = Repository::new(format);
            for path in ["[ab] new.txt", "a new.txt"] {
                std::fs::write(repository.0.join(path), "intent 中😀\n").unwrap();
                repository.git(&["add", "-N", "--", path]);
            }
            let observed = status(&repository.git(&[
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
            ]))
            .unwrap();
            let change = observed
                .iter()
                .find(|change| change.path == "[ab] new.txt")
                .unwrap();
            assert_eq!((change.index, change.worktree), ('.', 'A'));
            assert!(
                !repository
                    .git(&["ls-files", "--stage", "-z", "--", "[ab] new.txt"])
                    .is_empty()
            );
            assert_eq!(
                change.intent_status.as_ref().unwrap(),
                &repository.git(&[
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--untracked-files=all",
                    "--",
                    "[ab] new.txt",
                ])
            );

            repository.git(&["add", "--", "[ab] new.txt"]);
            std::fs::write(repository.0.join("[ab] new.txt"), "later disk bytes\n").unwrap();
            let refreshed = repository.git(&[
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
                "--",
                "[ab] new.txt",
            ]);
            assert_ne!(change.intent_status.as_ref().unwrap(), &refreshed);
            let staged = status(&refreshed).unwrap();
            assert!(staged[0].intent_status.is_none());
            assert_eq!(
                staged[0].index_entry,
                repository.git(&["ls-files", "--stage", "-z", "--", "[ab] new.txt",])
            );
            assert_eq!(repository.git(&["show", ":a new.txt"]), b"");
        }
    }
}
