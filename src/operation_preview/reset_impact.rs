//! Hard reset reviews obstructing untracked paths, including ignored files, without reading content.
//! Path-only review also covers binary files. Incomplete inventories fail closed, never truncate.
use super::*;
use std::collections::BTreeSet;

const MAX_PATHS: usize = 4096;

/// No ignore filters: reset --hard can destroy files that ordinary status intentionally omits.
pub(super) fn untracked_args() -> Vec<String> {
    vec!["ls-files".into(), "--others".into(), "-z".into()]
}

/// Target tree is immutable by OID; the untracked inventory is rechecked before execution.
#[derive(Default)]
pub(super) struct HardResetImpact {
    target_paths: Option<BTreeSet<String>>,
    untracked_bytes: Option<Vec<u8>>,
    blocked: Vec<String>,
}

impl HardResetImpact {
    /// Parse full ls-tree records rather than names only, so unsupported gitlinks are not invisible.
    pub(super) fn read_tree(&mut self, bytes: &[u8]) -> Result<(), api::Failure> {
        let mut paths = BTreeSet::new();
        for record in records(bytes)? {
            let (metadata, path) = record.split_once('\t').ok_or_else(incomplete)?;
            let fields = metadata.split(' ').collect::<Vec<_>>();
            if fields.len() != 3
                || fields[1] != "blob"
                || !matches!(fields[0], "100644" | "100755" | "120000")
            {
                return Err(failure(
                    "无法完整预览含子模块或未知条目的硬重置 / Hard reset preview does not support submodules or unknown tree entries",
                ));
            }
            model::oid(fields[2].as_bytes())?;
            validate_path(path, false)?;
            // Conservative case folding includes collisions on Windows. Review may include an
            // additional same-name candidate on a case-sensitive filesystem, but never hides one.
            paths.insert(path.to_uppercase());
        }
        self.target_paths = Some(paths);
        self.untracked_bytes = None;
        self.blocked.clear();
        Ok(())
    }

    /// File/file, file/directory and directory/file collisions all cause worktree data loss.
    pub(super) fn read_untracked(&mut self, bytes: &[u8]) -> Result<(), api::Failure> {
        let targets = self.target_paths.as_ref().ok_or_else(incomplete)?;
        let mut blocked = BTreeSet::new();
        for path in records(bytes)? {
            validate_path(path, true)?;
            let normalized = path.trim_end_matches('/').to_uppercase();
            let prefix = format!("{normalized}/");
            let descendant = targets
                .range(prefix.clone()..)
                .next()
                .is_some_and(|target| target.starts_with(&prefix));
            let ancestor = normalized
                .match_indices('/')
                .any(|(index, _)| targets.contains(&normalized[..index]));
            if targets.contains(&normalized) || descendant || ancestor {
                // Git reports an untracked nested repository as one directory, without inventorying
                // its contents. Do not imply that an uninspected nested repository is safe to erase.
                if path.ends_with('/') {
                    return Err(failure(format!(
                        "阻挡路径包含无法完整枚举的嵌套仓库，拒绝硬重置 / Obstruction includes an unenumerated nested repository; refusing hard reset: {}",
                        quoted(path)
                    )));
                }
                blocked.insert(path.to_owned());
            }
        }
        self.blocked = blocked.into_iter().collect();
        self.untracked_bytes = Some(bytes.to_vec());
        Ok(())
    }

    /// The snapshot is intentionally the entire inventory: new blockers require fresh approval.
    pub(super) fn guard(&self, next: Command) -> Result<Command, api::Failure> {
        Ok(Command::new(
            untracked_args(),
            Purpose::Guard {
                expected: self.untracked_bytes.clone().ok_or_else(incomplete)?,
                next: Box::new(next),
            },
        ))
    }

    pub(super) fn description(&self) -> Result<String, api::Failure> {
        if self.untracked_bytes.is_none() {
            return Err(incomplete());
        }
        let mut text = String::from(
            "硬重置将丢弃已跟踪文件的未提交更改。以下未跟踪或忽略路径阻挡目标文件，可能被删除或覆盖；列出的是完整路径，包含二进制文件，不提供内容恢复。大小写相同的路径也按可能冲突列出。\nHard reset discards uncommitted tracked changes. These untracked or ignored paths obstruct target files and may be deleted or overwritten. The complete path list includes binary files; content recovery is not provided. Case-equivalent paths are conservatively included:\n",
        );
        if self.blocked.is_empty() {
            text.push_str("（未发现阻挡路径 / no obstructing paths found）\n");
        }
        for path in &self.blocked {
            text.push_str(&format!("  {}\n", quoted(path)));
        }
        text.push_str("确认前若未跟踪路径清单变化，将拒绝执行并要求重新预览；外部 Git 操作仍可能与执行竞争。\nA changed untracked inventory invalidates confirmation; external Git operations can still race execution.\n");
        Ok(text)
    }
}

/// NUL records preserve tabs/newlines in names; strict UTF-8 refuses lossy or ambiguous display.
fn records(bytes: &[u8]) -> Result<Vec<&str>, api::Failure> {
    if bytes.len() > 262144 {
        return Err(incomplete());
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.last() != Some(&0) {
        return Err(incomplete());
    }
    let records = bytes[..bytes.len() - 1].split(|byte| *byte == 0)
        .map(|record| std::str::from_utf8(record).map_err(|_| failure("路径不是 UTF-8，无法无损显示完整风险清单；拒绝硬重置 / Non-UTF-8 path cannot be reviewed losslessly; refusing hard reset")))
        .collect::<Result<Vec<_>, _>>()?;
    if records.len() > MAX_PATHS || records.iter().any(|record| record.is_empty()) {
        return Err(incomplete());
    }
    Ok(records)
}

fn validate_path(path: &str, directory: bool) -> Result<(), api::Failure> {
    let path = if directory {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        path
    };
    if path
        .split('/')
        .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(incomplete());
    }
    Ok(())
}

/// JSON string quoting makes control characters and delimiter-like file names unambiguous in UI.
fn quoted(path: &str) -> String {
    serde_json::to_string(path).expect("serializable path")
}

fn incomplete() -> api::Failure {
    failure(
        "硬重置路径预览不完整，或超过 4096 条路径 / Hard reset path preview is incomplete or exceeds 4096 paths",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str]) -> Vec<u8> {
        paths
            .iter()
            .map(|path| format!("100644 blob {}\t{path}\0", "a".repeat(40)))
            .collect::<String>()
            .into_bytes()
    }

    #[test]
    fn collisions_cover_file_directory_ignored_binary_and_case_equivalent_paths() {
        let mut impact = HardResetImpact::default();
        impact
            .read_tree(&tree(&["exact", "dir/file", "replace-dir", "Case"]))
            .unwrap();
        impact
            .read_untracked(b"exact\0dir\0replace-dir/ignored.bin\0case\0spare\0directory\0")
            .unwrap();
        assert_eq!(
            impact.blocked,
            ["case", "dir", "exact", "replace-dir/ignored.bin"]
        );
        assert!(impact.description().unwrap().contains("ignored.bin"));
    }

    #[test]
    fn incomplete_non_utf8_and_nested_repository_inventory_cannot_be_confirmed() {
        let mut impact = HardResetImpact::default();
        assert!(
            impact
                .guard(Command::new(["reset"], Purpose::Action))
                .is_err()
        );
        impact.read_tree(&tree(&["nested/file"])).unwrap();
        assert!(impact.read_untracked(b"nested/\0").is_err());
        assert!(impact.read_untracked(b"bad\xff\0").is_err());
        assert!(impact.read_untracked(b"truncated").is_err());
        assert!(
            impact
                .read_untracked(b"a\0".repeat(MAX_PATHS + 1).as_slice())
                .is_err()
        );
        assert!(
            impact
                .read_tree(format!("160000 commit {}\tsubmodule\0", "a".repeat(40)).as_bytes())
                .is_err()
        );
    }

    #[test]
    fn delimiter_paths_are_quoted_and_entire_snapshot_is_guarded() {
        let mut impact = HardResetImpact::default();
        impact.read_tree(&tree(&["line\nname"])).unwrap();
        let inventory = b"line\nname\0unrelated\0";
        impact.read_untracked(inventory).unwrap();
        assert!(impact.description().unwrap().contains("\"line\\nname\""));
        let command = impact
            .guard(Command::new(["reset"], Purpose::Action))
            .unwrap();
        let Purpose::Guard { expected, .. } = command.purpose else {
            panic!("inventory guard")
        };
        assert_eq!(expected, inventory);
        assert_eq!(command.args, ["ls-files", "--others", "-z"]);
    }
}
