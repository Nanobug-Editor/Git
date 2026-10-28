//! Private, read-only operation metadata request shared by the guest and its installed native helper.
use serde::{Deserialize, Serialize};

/// A storage backend outside fixed loose marker files uses the guest's existing Git query path.
pub(crate) const UNSUPPORTED_STORAGE: &str = "Git ref storage requires the Git resolver";
pub(crate) const UNSUPPORTED_STORAGE_EXIT: u32 = 3;

/// Ordering is fixed across the native reply and the guest's atomic conflict lifecycle publication.
pub(crate) const MARKERS: [&str; 8] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REBASE_HEAD",
    "rebase-merge/orig-head",
    "rebase-merge/onto",
    "rebase-apply/orig-head",
    "rebase-apply/onto",
    "sequencer/head",
];

/// A cached identity is a comparison target, never permission to read arbitrary supplied directories.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub mode: String,
    pub workspace: String,
    pub git_dir: String,
    pub common_dir: String,
}

/// The native helper derives metadata from its actual checkout and reads only eight fixed files.
/// Missing files are absent markers; permission errors, oversized data and malformed OIDs fail closed.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read(request: &Request, cwd: &std::path::Path) -> Result<String, String> {
    use std::{
        fs,
        io::{ErrorKind, Read},
        path::Path,
    };
    fn text(path: &Path) -> Result<Option<String>, String> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Cannot read Git operation metadata".into()),
        };
        let mut bytes = Vec::new();
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read Git operation metadata")?;
        if bytes.len() > 4096 {
            return Err("Git operation metadata is too large".into());
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| "Invalid Git metadata encoding".into())
    }
    fn canonical(path: &Path) -> Result<std::path::PathBuf, String> {
        path.canonicalize()
            .map_err(|_| "Git metadata identity changed".into())
    }
    if request.mode != "observe-operations"
        || !Path::new(&request.workspace).is_absolute()
        || !Path::new(&request.git_dir).is_absolute()
        || !Path::new(&request.common_dir).is_absolute()
        || canonical(cwd)? != canonical(Path::new(&request.workspace))?
    {
        return Err("Workspace identity changed".into());
    }
    let dot_git = cwd.join(".git");
    let git_dir = if dot_git.is_dir() {
        canonical(&dot_git)?
    } else {
        let file = text(&dot_git)?.ok_or("Missing Git checkout metadata")?;
        let relative = file
            .trim()
            .strip_prefix("gitdir: ")
            .ok_or("Invalid Git checkout metadata")?;
        canonical(&cwd.join(relative))?
    };
    let common_dir = match text(&git_dir.join("commondir"))? {
        Some(value) => canonical(&git_dir.join(value.trim()))?,
        None => git_dir.clone(),
    };
    if git_dir != canonical(Path::new(&request.git_dir))?
        || common_dir != canonical(Path::new(&request.common_dir))?
    {
        return Err("Git metadata identity changed".into());
    }
    if git_dir.join("reftable").exists() || common_dir.join("reftable").exists() {
        return Err(UNSUPPORTED_STORAGE.into());
    }
    let mut output = String::new();
    for marker in MARKERS {
        if let Some(value) = text(&git_dir.join(marker))? {
            // MERGE_HEAD may contain several parents; Git's ref resolver reports its first OID.
            let lines = value.lines().collect::<Vec<_>>();
            if lines.is_empty()
                || lines.iter().any(|line| {
                    !matches!(line.len(), 40 | 64)
                        || !line.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            {
                return Err("Invalid Git operation identity".into());
            }
            output.push_str(lines[0]);
            output.push('\n');
        } else {
            output.push_str(&format!("{marker} missing\n"));
        }
    }
    Ok(output)
}
