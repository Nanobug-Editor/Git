//! Pure preview planning validates every record and constructs literal immutable execution targets.
use super::*;
use std::collections::BTreeSet;
#[cfg(test)]
mod tests;

/// Modes are intentionally explicit: path resets, continuation and helper-backed plans do not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Reset,
    Rebase,
    Merge,
    CherryPick,
}
impl Kind {
    pub(super) fn recognize(command: &Command) -> Result<Option<Self>, api::Failure> {
        if command.service.is_some() {
            return Ok(None);
        }
        let mut args = command.args.iter().map(String::as_str).collect::<Vec<_>>();
        // Reconsideration after an explicit commit receives already frozen argv. Accept exactly
        // our policy flags as well as initial UI commands, without accepting arbitrary options.
        if args.first() == Some(&"merge") && args.get(1) == Some(&"--no-autostash") {
            args.remove(1);
        }
        if args.first() == Some(&"reset") && args.get(2) == Some(&"--no-recurse-submodules") {
            args.remove(2);
        }
        let kind = match args.as_slice() {
            ["reset", "--soft" | "--mixed" | "--hard", _] => Self::Reset,
            ["rebase", target] if !target.starts_with('-') => Self::Rebase,
            [
                "rebase",
                "--no-rebase-merges",
                "--no-update-refs",
                "--no-fork-point",
                "--no-autosquash",
                "--no-autostash",
                _,
            ] => Self::Rebase,
            ["merge", "--no-edit", "--", _]
            | ["merge", "--ff-only", "--", _]
            | ["merge", "--no-ff", "--no-edit", "--", _]
            | ["merge", "--squash", "--", _] => Self::Merge,
            ["cherry-pick", "--", targets @ ..] if !targets.is_empty() => Self::CherryPick,
            _ => return Ok(None),
        };
        Ok(Some(kind))
    }
}

/// Git's current ref observation and resolved commits remain immutable while the UI is interactive.
pub(super) struct Draft {
    pub(super) panel: String,
    pub(super) dirty: bool,
    command: Command,
    kind: Kind,
    pub(super) targets: Vec<String>,
    pub(super) resolved: Vec<String>,
    pub(super) branch: String,
    pub(super) branch_bytes: Vec<u8>,
    pub(super) head: String,
    pub(super) head_bytes: Vec<u8>,
    pub(super) commits: Vec<CommitImpact>,
    pub(super) hard_reset: Option<reset_impact::HardResetImpact>,
}

/// Original commits are not claimed to disappear globally; remote reachability is a local observation.
pub(super) struct CommitImpact {
    pub(super) oid: String,
    parents: Vec<String>,
    title: String,
    pub(super) remotes: Vec<String>,
}

impl Draft {
    pub(super) fn new(
        command: Command,
        kind: Kind,
        panel: String,
        dirty: bool,
    ) -> Result<Self, api::Failure> {
        let targets = if kind == Kind::CherryPick {
            command.args[2..].to_vec()
        } else {
            vec![command.args.last().expect("recognized target").clone()]
        };
        if targets.len() > 64
            || targets.iter().any(|target| {
                target.is_empty() || target.starts_with('-') || target.contains(['\0', '\r', '\n'])
            })
        {
            return Err(failure(
                "需提供 1–64 个明确提交目标 / Expected 1–64 explicit commit targets",
            ));
        }
        let hard_reset = (kind == Kind::Reset
            && command.args.get(1).is_some_and(|arg| arg == "--hard"))
        .then(reset_impact::HardResetImpact::default);
        Ok(Self {
            panel,
            dirty,
            command,
            kind,
            targets,
            resolved: Vec::new(),
            branch: String::new(),
            branch_bytes: Vec::new(),
            head: String::new(),
            head_bytes: Vec::new(),
            commits: Vec::new(),
            hard_reset,
        })
    }

    /// Worktree impact is read after history, keeping its snapshot close to confirmation publication.
    pub(super) fn after_history(&self) -> Step {
        if self.hard_reset.is_some() {
            Step::ResetTree
        } else {
            Step::CheckBranch
        }
    }

    pub(super) fn validate_targets(&self) -> Result<(), api::Failure> {
        if self.resolved.iter().collect::<BTreeSet<_>>().len() != self.resolved.len() {
            return Err(failure(
                "不能重复选择同一提交 / Duplicate resolved commits are not accepted",
            ));
        }
        Ok(())
    }

    /// One sentinel entry beyond the display limit detects truncation instead of hiding extra work.
    pub(super) fn log_args(&self) -> Vec<String> {
        let mut args = vec![
            "-c".into(),
            "i18n.logOutputEncoding=UTF-8".into(),
            "log".into(),
            "-z".into(),
            "--format=%H%x00%P%x00%s".into(),
            "--max-count=129".into(),
        ];
        match self.kind {
            Kind::Reset | Kind::Rebase => args.extend([
                "--reverse".into(),
                self.head.clone(),
                "--not".into(),
                self.resolved[0].clone(),
            ]),
            Kind::Merge => args.extend([
                "--reverse".into(),
                self.resolved[0].clone(),
                "--not".into(),
                self.head.clone(),
            ]),
            Kind::CherryPick => {
                args.push("--no-walk=unsorted".into());
                args.extend(self.resolved.iter().cloned());
            }
        }
        args.push("--".into());
        args
    }

    pub(super) fn read_commits(&mut self, bytes: &[u8]) -> Result<(), api::Failure> {
        self.commits = commits(bytes)?;
        if self.kind == Kind::CherryPick {
            if self.commits.iter().any(|commit| commit.parents.len() > 1) {
                return Err(failure(
                    "拣选合并提交需要明确主线父提交 / Cherry-picking a merge requires an explicit mainline parent",
                ));
            }
            if self
                .commits
                .iter()
                .map(|commit| &commit.oid)
                .collect::<Vec<_>>()
                != self.resolved.iter().collect::<Vec<_>>()
            {
                return Err(failure(
                    "拣选预览没有完整保留提交顺序 / Cherry-pick preview did not preserve every selected commit in order",
                ));
            }
        }
        Ok(())
    }

    /// Recheck both current branch identity and HEAD immediately before the literal mutation.
    /// These checks detect stale UI approval; they do not claim a lock against external Git clients.
    pub(super) fn frozen_command(&self) -> Result<Command, api::Failure> {
        let mut action = self.command.clone();
        match self.kind {
            Kind::CherryPick => {
                action.args.truncate(2);
                action.args.extend(self.resolved.iter().cloned());
            }
            Kind::Rebase => {
                // Preview describes ordinary flattened replay, without unreviewed automatic stash
                // or autosquash behavior inherited from per-repository/user Git configuration.
                action.args = vec![
                    "rebase".into(),
                    "--no-rebase-merges".into(),
                    "--no-update-refs".into(),
                    "--no-fork-point".into(),
                    "--no-autosquash".into(),
                    "--no-autostash".into(),
                    self.resolved[0].clone(),
                ];
            }
            Kind::Merge => {
                *action.args.last_mut().unwrap() = self.resolved[0].clone();
                if action.args.get(1).is_none_or(|arg| arg != "--no-autostash") {
                    action.args.insert(1, "--no-autostash".into());
                }
            }
            Kind::Reset if self.hard_reset.is_some() => {
                // Never inherit submodule.recurse and silently reset another worktree.
                action.args = vec![
                    "reset".into(),
                    "--hard".into(),
                    "--no-recurse-submodules".into(),
                    self.resolved[0].clone(),
                ];
            }
            _ => *action.args.last_mut().unwrap() = self.resolved[0].clone(),
        }
        action.display = Some(self.description(&action)?);
        if let Some(impact) = &self.hard_reset {
            action = impact.guard(action)?;
        }
        let head = Command::new(
            ["rev-parse", "--verify", "HEAD"],
            Purpose::Guard {
                expected: self.head_bytes.clone(),
                next: Box::new(action),
            },
        );
        Ok(Command::new(
            ["rev-parse", "--symbolic-full-name", "HEAD"],
            Purpose::Guard {
                expected: self.branch_bytes.clone(),
                next: Box::new(head),
            },
        ))
    }

    fn description(&self, action: &Command) -> Result<String, api::Failure> {
        let branch = if self.branch == "HEAD" {
            "detached HEAD / 分离 HEAD"
        } else {
            &self.branch
        };
        let mut text = format!(
            "当前分支 / Current branch: {branch}\n当前 HEAD / Current HEAD: {}\n",
            self.head
        );
        for (name, oid) in self.targets.iter().zip(&self.resolved) {
            text.push_str(&format!("冻结目标 / Frozen target: {name} → {oid}\n"));
        }
        if let Some(impact) = &self.hard_reset {
            text.push_str(&impact.description()?);
        }
        text.push_str(match self.kind {
            Kind::Reset => "以下提交将退出当前分支的可达历史；其他引用仍可能保留它们 / These commits leave the current branch's reachable history; other refs may retain them:\n",
            Kind::Rebase => "以下为目标之外的完整原始范围；普通提交按 Git 规则重放，等价补丁可能跳过，合并提交会展平 / Complete original range outside the target; Git replays ordinary commits, may skip equivalent patches, and flattens merges:\n",
            Kind::Merge => "以下为将合入的来源提交；不改写原提交 / Incoming commits; original commits are not rewritten:\n",
            Kind::CherryPick => "以下按所列顺序复制为新提交；不改写来源提交 / Replay in this exact order as new commits; source commits are not rewritten:\n",
        });
        if self.commits.is_empty() {
            text.push_str("（无 / none）\n");
        }
        for commit in &self.commits {
            text.push_str(&format!(
                "{} {}{}\n",
                commit.oid,
                commit.title,
                if commit.parents.len() > 1 {
                    " [merge / 合并提交]"
                } else {
                    ""
                }
            ));
            for reference in &commit.remotes {
                text.push_str(&format!("  已知远程跟踪引用包含该提交 / Known remote-tracking ref contains this commit: {reference}\n"));
            }
        }
        text.push_str("远程信息仅来自本地跟踪引用，可能过期；未发起网络请求，也不会修改远程引用。未显示远程引用不代表从未发布。已发布历史允许在检查后确认继续。\nRemote information is local and may be stale; no network request was made and remote refs are not modified. No matching ref does not prove unpublished history. You may explicitly confirm after reviewing published-history implications.\n");
        text.push_str(&format!(
            "执行参数 / Literal argv:\n{}",
            action.args.join(" ")
        ));
        if text.len() > 65536 {
            return Err(failure(
                "影响预览超过 64 KiB，拒绝不完整确认 / Impact preview exceeds 64 KiB; incomplete confirmation refused",
            ));
        }
        Ok(text)
    }
}

pub(super) fn changed() -> api::Failure {
    failure(
        "当前分支或 HEAD 已变化，请重新预览 / Current branch or HEAD changed; prepare a new preview",
    )
}

pub(super) fn oid(bytes: &[u8]) -> Result<String, api::Failure> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| failure("Invalid Git commit encoding"))?
        .trim();
    if !matches!(text.len(), 40 | 64) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(failure(
            "Git 未返回唯一完整提交 ID / Git did not return one full commit OID",
        ));
    }
    Ok(text.to_ascii_lowercase())
}

pub(super) fn branch(bytes: &[u8]) -> Result<String, api::Failure> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| failure("Invalid Git branch encoding"))?
        .trim();
    if (text != "HEAD" && !text.starts_with("refs/heads/"))
        || text.chars().any(char::is_control)
        || text.is_empty()
    {
        return Err(failure(
            "Git 未返回当前分支身份 / Git did not return a current branch identity",
        ));
    }
    Ok(text.into())
}

fn commits(bytes: &[u8]) -> Result<Vec<CommitImpact>, api::Failure> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.last() != Some(&0) {
        return Err(failure("提交预览不完整 / Incomplete commit preview"));
    }
    let fields = bytes[..bytes.len() - 1]
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
    if fields.len() % 3 != 0 || fields.len() / 3 > 128 {
        return Err(failure(
            "提交预览不完整或超过 128 个提交 / Incomplete preview or more than 128 commits",
        ));
    }
    fields
        .chunks_exact(3)
        .map(|fields| {
            let parents = std::str::from_utf8(fields[1])
                .map_err(|_| failure("Invalid parent encoding"))?
                .split_whitespace()
                .map(|parent| oid(parent.as_bytes()))
                .collect::<Result<Vec<_>, _>>()?;
            let title = std::str::from_utf8(fields[2])
                .map_err(|_| failure("Commit title is not UTF-8"))?
                .to_owned();
            Ok(CommitImpact {
                oid: oid(fields[0])?,
                parents,
                title,
                remotes: Vec::new(),
            })
        })
        .collect()
}

pub(super) fn remote_refs(bytes: &[u8]) -> Result<Vec<String>, api::Failure> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| failure("Remote ref encoding is not UTF-8"))?;
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > 128 {
        return Err(failure(
            "包含该提交的远程引用超过 128 个 / More than 128 containing remote refs",
        ));
    }
    lines
        .into_iter()
        .map(|line| {
            let (name, tip) = line
                .split_once('\t')
                .ok_or_else(|| failure("Incomplete remote ref"))?;
            if !name.starts_with("refs/remotes/") || name.chars().any(char::is_control) {
                return Err(failure("Invalid remote ref"));
            }
            Ok(format!("{name} ({})", oid(tip.as_bytes())?))
        })
        .collect()
}
