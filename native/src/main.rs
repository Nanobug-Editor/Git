//! A hash-verified private Git helper batches operation reads and edits approved rebase plans.
//! No code is loaded into the host.
//! User plans travel through stdin and a temporary file; no user message/ref becomes shell source.
#[path = "../../src/rebase_model.rs"]
mod model;
#[path = "../../src/operation_observation.rs"]
mod operation_observation;
use model::{Plan, Request};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Return a failing exit status without leaking plan/environment contents to logs.
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("Git helper: {error}");
            std::process::exit(2);
        }
    }
}
fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
/// Validate a closed plan before touching Git state or a Git-owned editor file.
fn validate(plan: &Plan) -> Result<(), String> {
    if !valid_oid(&plan.base)
        || !valid_oid(&plan.head)
        || plan.steps.is_empty()
        || plan.steps.len() > 64
        || plan.original.len() != plan.steps.len()
    {
        return Err("Invalid bounded commit range".into());
    }
    if !Path::new(&plan.git).is_absolute() || !Path::new(&plan.workspace).is_absolute() {
        return Err("Expected resolved tool and workspace paths".into());
    }
    if plan.branch.as_ref().is_some_and(|branch| {
        !branch.starts_with("refs/heads/") || branch.contains(['\0', '\n', '\r'])
    }) {
        return Err("Invalid approved branch identity".into());
    }
    let original = plan.original.iter().collect::<BTreeSet<_>>();
    let proposed = plan
        .steps
        .iter()
        .map(|step| &step.oid)
        .collect::<BTreeSet<_>>();
    if original.len() != plan.steps.len()
        || original != proposed
        || plan.original.iter().any(|oid| !valid_oid(oid))
    {
        return Err("Every original commit must occur exactly once".into());
    }
    let mut prior = false;
    for step in &plan.steps {
        if !matches!(
            step.action.as_str(),
            "pick" | "reword" | "squash" | "fixup" | "drop"
        ) || step.message.len() > 65536
            || step.message.contains('\0')
        {
            return Err("Unsupported todo action or message".into());
        }
        if step.action == "reword" && step.message.trim().is_empty() {
            return Err("A reword action requires a message".into());
        }
        if matches!(step.action.as_str(), "squash" | "fixup") && !prior {
            return Err("Squash/fixup requires an earlier retained commit".into());
        }
        if step.action != "drop" {
            prior = true;
        }
    }
    Ok(())
}
/// Literal argv and a hidden child process preserve the host's external-tool execution model.
fn git(plan: &Plan) -> Command {
    let mut command = Command::new(&plan.git);
    command
        .current_dir(&plan.workspace)
        .arg("--no-pager")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x08000000);
    }
    command
}
fn query(plan: &Plan, args: &[&str]) -> Result<String, String> {
    let output = git(plan)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("Cannot validate the current Git state".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "Git state is not UTF-8".into())
}
/// Git's editor variable is a command string. Quote only the verified installed executable path;
/// commit messages and the editable todo never appear in that string.
fn editor_command(mode: &str) -> Result<String, String> {
    let path = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    Ok(format!("'{}' {mode}", path.replace('\'', "'\\''")))
}
/// Recheck Git's exact internal todo/message path, including linked worktrees outside the root.
fn checked_editor_file(plan: &Plan, provided: &Path, relative: &str) -> Result<PathBuf, String> {
    let expected = query(
        plan,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            relative,
        ],
    )?;
    let expected = Path::new(expected.trim())
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let provided = provided.canonicalize().map_err(|error| error.to_string())?;
    if expected != provided {
        return Err("Editor file is outside the active Git operation".into());
    }
    Ok(provided)
}
/// Invocations from Git receive no new authority: the bounded parent plan and Git-owned path are checked again.
fn editor_mode(mode: &str, provided: &Path) -> Result<i32, String> {
    let file = std::env::var_os("NANOBUG_GIT_REBASE_PLAN_FILE").ok_or("Missing rebase plan")?;
    let mut encoded = Vec::new();
    std::fs::File::open(file)
        .map_err(|_| "Cannot read approved plan")?
        .take(262145)
        .read_to_end(&mut encoded)
        .map_err(|_| "Cannot read approved plan")?;
    if encoded.len() > 262144 {
        return Err("Rebase plan limit exceeded".into());
    }
    let plan: Plan = serde_json::from_slice(&encoded).map_err(|_| "Invalid rebase plan")?;
    validate(&plan)?;
    if mode == "--sequence" {
        let file = checked_editor_file(&plan, provided, "rebase-merge/git-rebase-todo")?;
        let todo = plan
            .steps
            .iter()
            .map(|step| format!("{} {}\n", step.action, step.oid))
            .collect::<String>();
        std::fs::write(file, todo).map_err(|error| error.to_string())?;
    } else {
        let file = checked_editor_file(&plan, provided, "COMMIT_EDITMSG")?;
        let done = query(
            &plan,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "rebase-merge/done",
            ],
        )?;
        let done = std::fs::read_to_string(done.trim()).map_err(|error| error.to_string())?;
        let oid = done
            .lines()
            .rev()
            .find_map(|line| line.split_whitespace().nth(1))
            .ok_or("Missing current rebase action")?;
        let step = plan
            .steps
            .iter()
            .find(|step| step.oid == oid || step.oid.starts_with(oid))
            .ok_or("Current action is outside the approved plan")?;
        if !step.message.trim().is_empty() {
            std::fs::write(file, &step.message).map_err(|error| error.to_string())?;
        }
    }
    Ok(0)
}
/// A persisted UI plan may resume only its own unchanged operation. In particular an external
/// todo edit must not smuggle exec/edit/merge instructions through a later Continue click.
fn validate_resume(plan: &Plan) -> Result<(), String> {
    let git_file = |name: &str| -> Result<String, String> {
        let path = query(
            plan,
            &["rev-parse", "--path-format=absolute", "--git-path", name],
        )?;
        let mut bytes = Vec::new();
        std::fs::File::open(path.trim())
            .map_err(|_| "The approved rebase is no longer active")?
            .take(262145)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read active rebase")?;
        if bytes.len() > 262144 {
            return Err("Active rebase metadata exceeds the limit".into());
        }
        String::from_utf8(bytes).map_err(|_| "Invalid active rebase metadata".into())
    };
    if git_file("rebase-merge/orig-head")?.trim() != plan.head
        || git_file("rebase-merge/onto")?.trim() != plan.base
    {
        return Err("The active rebase differs from the approved plan".into());
    }
    let records = format!(
        "{}\n{}",
        git_file("rebase-merge/done")?,
        git_file("rebase-merge/git-rebase-todo")?
    );
    let mut actions = Vec::new();
    for line in records
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let mut parts = line.split_whitespace();
        let action = parts.next().unwrap();
        let oid = parts.next().ok_or("Invalid active rebase action")?;
        // The helper writes full OIDs. Git may append a subject, which has no executable meaning.
        if !valid_oid(oid) {
            return Err("Active rebase identity changed".into());
        }
        actions.push((action, oid));
    }
    if actions.len() != plan.steps.len()
        || actions
            .iter()
            .zip(&plan.steps)
            .any(|((action, oid), step)| *action != step.action || *oid != step.oid)
    {
        return Err("The active todo changed; review it outside this saved plan".into());
    }
    Ok(())
}
/// Start/continue one real Git rebase. The helper never reimplements refs, the index or conflict recovery.
fn run() -> Result<i32, String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if let Some(mode) = args.first().and_then(|arg| arg.to_str()) {
        if matches!(mode, "--sequence" | "--message") && args.len() == 2 {
            return editor_mode(mode, Path::new(&args[1]));
        }
        return Err("Unsupported helper invocation".into());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(262145)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 262144 {
        return Err("Rebase request limit exceeded".into());
    }
    // The already installed helper also batches read-only marker inspection; this mode never starts
    // Git, acquires an index lock, or runs a plan. Identity is derived again from the current checkout.
    if let Ok(request) = serde_json::from_slice::<operation_observation::Request>(&bytes) {
        let cwd = std::env::current_dir().map_err(|_| "Missing current checkout")?;
        return match operation_observation::read(&request, &cwd) {
            Ok(output) => {
                print!("{output}");
                Ok(0)
            }
            Err(error) if error == operation_observation::UNSUPPORTED_STORAGE => {
                Ok(operation_observation::UNSUPPORTED_STORAGE_EXIT as i32)
            }
            Err(error) => Err(error),
        };
    }
    let request: Request = serde_json::from_slice(&bytes).map_err(|_| "Invalid rebase request")?;
    validate(&request.plan)?;
    let plan = &request.plan;
    let cwd = std::env::current_dir()
        .map_err(|error| error.to_string())?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if cwd
        != Path::new(&plan.workspace)
            .canonicalize()
            .map_err(|error| error.to_string())?
    {
        return Err("Workspace changed".into());
    }
    if request.mode == "start" {
        let branch = git(plan)
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .output()
            .map_err(|_| "Cannot validate target branch")?;
        let current = match branch.status.code() {
            Some(0) => Some(
                String::from_utf8(branch.stdout)
                    .map_err(|_| "Invalid branch identity")?
                    .trim()
                    .to_owned(),
            ),
            Some(1) => None,
            _ => return Err("Cannot validate target branch".into()),
        };
        if current != plan.branch {
            return Err("The target branch changed after preview".into());
        }
        if query(plan, &["rev-parse", "--verify", "HEAD"])?.trim() != plan.head {
            return Err("HEAD changed after preview; prepare a new plan".into());
        }
        if !query(
            plan,
            &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        )?
        .is_empty()
        {
            return Err("Commit or stash the worktree before rebase".into());
        }
        let range = format!("{}..{}", plan.base, plan.head);
        let original = query(
            plan,
            &["rev-list", "--reverse", "--topo-order", &range, "--"],
        )?;
        if original
            .lines()
            .ne(plan.original.iter().map(String::as_str))
        {
            return Err("The commit range changed after preview".into());
        }
        if !query(plan, &["rev-list", "--merges", &range, "--"])?
            .trim()
            .is_empty()
        {
            return Err("Interactive merge topology is outside this feature".into());
        }
    } else if !matches!(request.mode.as_str(), "continue" | "skip") {
        return Err("Unsupported rebase lifecycle action".into());
    } else {
        validate_resume(plan)?;
    }
    // A plan can exceed Windows' environment block limit. Only a private, random temporary
    // pathname is inherited by Git/editor children; normal completion removes the file.
    let mut plan_file = tempfile::Builder::new()
        .prefix("nanobug-rebase-")
        .tempfile()
        .map_err(|_| "Cannot prepare private rebase plan")?;
    serde_json::to_writer(&mut plan_file, plan).map_err(|_| "Cannot write approved plan")?;
    plan_file
        .flush()
        .map_err(|_| "Cannot flush approved plan")?;
    let mut command = git(plan);
    command
        .env("NANOBUG_GIT_REBASE_PLAN_FILE", plan_file.path())
        .env("GIT_SEQUENCE_EDITOR", editor_command("--sequence")?)
        .env("GIT_EDITOR", editor_command("--message")?);
    if request.mode == "start" {
        command.args([
            "rebase",
            "--interactive",
            "--no-autosquash",
            "--no-update-refs",
            "--no-autostash",
            "--no-rebase-merges",
            &plan.base,
        ]);
    } else {
        command.args(["rebase", &format!("--{}", request.mode)]);
    }
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| error.to_string())?;
    io::stdout().flush().map_err(|error| error.to_string())?;
    Ok(status.code().unwrap_or(2))
}
