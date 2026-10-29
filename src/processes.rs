//! A single owned queue serializes writes and drains byte-exact output before completing a Git command.
use plugin_protocol::{api, process};

/// Commands are literal argv and optional bounded stdin; no shell evaluates user text.
#[derive(Clone)]
pub(crate) struct Command {
    pub args: Vec<String>,
    pub input: Option<Vec<u8>>,
    pub purpose: Purpose,
    pub service: Option<String>,
    pub display: Option<String>,
}
#[derive(Clone)]
pub(crate) enum Purpose {
    Repository(crate::repository::Query),
    OperationPreview(crate::operation_preview::Query),
    RebaseBase,
    RebaseHead {
        base: String,
    },
    RebaseRange {
        base: String,
        head: String,
    },
    Status {
        round: u64,
    },
    Refs,
    /// A file chooser accepts only its own frozen branch comparison generation.
    BranchFiles {
        generation: u64,
    },
    Head,
    /// A filter incarnation rejects late success/error receipts after another selection wins.
    Log {
        generation: u64,
        filter: Option<String>,
    },
    Files(String),
    /// Read the complete message only for the selected immutable OID, never for every log row.
    CommitMessage(String),
    Snapshot {
        column: usize,
    },
    DiffSnapshot {
        column: usize,
    },
    HistoryBlob {
        column: usize,
    },
    /// One immutable batch resolves every uncached side of the selected historical file.
    HistoryBatch {
        columns: Vec<usize>,
        path: String,
    },
    /// Optional immutable neighbor contents never own a native source, selection or diagnostic.
    HistoryPrefetch(crate::comparisons::HistoryPrefetch),
    MergeSnapshot {
        path: String,
        column: usize,
    },
    MergeSize {
        path: String,
        column: usize,
        stage: usize,
    },
    Action,
    /// Literal multi-step Git operations keep their next step only after a successful exit.
    Then(Box<Command>),
    /// This push follows a successful local commit; failures must retain and report that commit.
    PushAfterCommit {
        next: Option<Box<Command>>,
    },
    /// Commit/stash must succeed and leave a clean tree before the already previewed operation.
    CleanAfterWrite(Box<Command>),
    /// A successful commit changes HEAD; frozen approvals must be rebuilt before any mutation.
    ReviewAfterCommit(Box<Command>),
    CheckClean(Box<Command>),
    /// The lease is sampled only by a manual network action and rechecked atomically by push.
    LeaseSource {
        remote: String,
        reference: String,
    },
    Lease {
        remote: String,
        reference: String,
        local: String,
    },
    OperationProbe {
        round: u64,
        marker: crate::conflicts::lifecycle::Marker,
    },
    /// Windows uses the already installed helper for one bounded, read-only metadata receipt.
    OperationBatch {
        round: u64,
    },
    Patch {
        staged: bool,
    },
    Guard {
        expected: Vec<u8>,
        next: Box<Command>,
    },
    /// Submit rechecks its frozen range and HEAD/branch, including an unborn branch, in one status read.
    CommitSelection {
        expected: Vec<crate::model::Change>,
        head: Option<String>,
        branch: String,
        next: Box<Command>,
    },
}

/// Only one child is active; its tail output and completion retain exact resource ownership.
pub(crate) struct Running {
    pub handle: api::ResourceHandle,
    pub command: Command,
    pub output: Vec<u8>,
    pub stderr: Vec<u8>,
}
impl Command {
    /// A validation query is not the user-visible effect being confirmed.
    pub fn effective_args(&self) -> &[String] {
        match &self.purpose {
            Purpose::Guard { next, .. }
            | Purpose::CommitSelection { next, .. }
            | Purpose::CheckClean(next)
            | Purpose::ReviewAfterCommit(next) => next.effective_args(),
            _ => &self.args,
        }
    }
    /// Multi-step effects are reviewed in execution order; stdin messages never become shell text.
    pub fn preview(&self) -> String {
        if let Some(display) = &self.display {
            return display.clone();
        }
        match &self.purpose {
            Purpose::Guard { next, .. }
            | Purpose::CommitSelection { next, .. }
            | Purpose::CheckClean(next)
            | Purpose::ReviewAfterCommit(next) => next.preview(),
            Purpose::Then(next) | Purpose::CleanAfterWrite(next) => {
                format!("{}\n{}", self.args.join(" "), next.preview())
            }
            _ => self.args.join(" "),
        }
    }
    pub fn new(args: impl IntoIterator<Item = impl Into<String>>, purpose: Purpose) -> Self {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            input: None,
            purpose,
            service: None,
            display: None,
        }
    }
    /// EOF is independent of exit and never releases output or kills a child.
    pub fn launch(&self, cwd: &str) -> Result<Running, api::Failure> {
        let operation = if let Some(service) = &self.service {
            process::Operation::StartService {
                service: service.clone(),
            }
        } else {
            process::Operation::Execute {
                program: "git".into(),
                // `--` ends option parsing but does not disable Git's glob/magic pathspec syntax.
                // Every path originated from status/tree selection and must remain an exact file.
                args: [
                    {
                        let mut prefix = vec!["--no-pager".into()];
                        // Literal paths are essential for user-selected filenames. Applying this
                        // global mode to stash leaks into Git's internal clean invocation and can
                        // leave untracked files behind after --include-untracked on Git for Windows.
                        if self.args.first().is_some_and(|command| {
                            matches!(
                                command.as_str(),
                                "add"
                                    | "commit"
                                    | "restore"
                                    | "rm"
                                    | "diff"
                                    | "diff-tree"
                                    | "ls-files"
                                    | "status"
                                    | "ls-tree"
                                    | "checkout"
                                    | "clean"
                            )
                        }) {
                            prefix.push("--literal-pathspecs".into());
                        }
                        prefix
                    },
                    self.args.clone(),
                ]
                .concat(),
                transport: process::Transport::Stdio,
                cwd: Some(cwd.into()),
                env: std::collections::BTreeMap::from([
                    ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                    ("GIT_EDITOR".into(), "true".into()),
                    // Cancelable status/diff queries must not refresh index stat data or own
                    // index.lock. Required locks for add/restore/commit remain enforced by Git.
                    ("GIT_OPTIONAL_LOCKS".into(), "0".into()),
                ]),
            }
        };
        let api::Value::Resource(handle) =
            api::guest::request(api::Operation::Process { operation })?
        else {
            return Err(api::Failure::new(
                api::ErrorCode::OperationFailed,
                "Missing process handle",
            ));
        };
        if let Some(bytes) = &self.input {
            if let Err(error) = api::guest::request(api::Operation::Process {
                operation: process::Operation::Write {
                    handle: handle.clone(),
                    bytes: bytes.clone(),
                },
            }) {
                return self.failed_launch(handle, error, api::guest::close_resource);
            }
        }
        if let Err(error) = api::guest::request(api::Operation::Process {
            operation: process::Operation::CloseStdin {
                handle: handle.clone(),
            },
        }) {
            return self.failed_launch(handle, error, api::guest::close_resource);
        }
        Ok(Running {
            handle,
            command: self.clone(),
            output: Vec::new(),
            stderr: Vec::new(),
        })
    }

    /// Optional stdin/EOF failures must retain an allocated child if its release also fails.
    /// The discarded request cannot cache bytes and remains owned until exit or a later close.
    pub(crate) fn failed_launch(
        &self,
        handle: api::ResourceHandle,
        error: api::Failure,
        close: impl FnOnce(api::ResourceHandle) -> Result<(), api::Failure>,
    ) -> Result<Running, api::Failure> {
        let closed = close(handle.clone());
        let mut command = self.clone();
        if closed.is_err()
            && let Purpose::HistoryPrefetch(request) = &mut command.purpose
        {
            request.discarded = true;
            return Ok(Running {
                handle,
                command,
                output: Vec::new(),
                stderr: Vec::new(),
            });
        }
        Err(error)
    }
}
