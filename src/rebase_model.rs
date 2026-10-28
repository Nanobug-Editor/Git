//! Private stdin contract shared by the WASM planner and the hash-verified native Git editor.
use serde::{Deserialize, Serialize};

/// A supported action retains its original identity even after the user moves the row.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Step {
    pub oid: String,
    pub action: String,
    pub message: String,
    pub title: String,
}
/// Git's original range is immutable; only order, action and explicit messages may be edited.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    pub workspace: String,
    pub git: String,
    pub base: String,
    pub head: String,
    /// Exact symbolic HEAD, or None for an explicitly detached HEAD.
    #[serde(default)]
    pub branch: Option<String>,
    pub original: Vec<String>,
    pub steps: Vec<Step>,
}
/// Lifecycle requests reuse the same persisted plan after a conflict or an application restart.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub mode: String,
    pub plan: Plan,
}
