//! A complete Git observation owns proposal lifetime; native documents/Undo have an independent owner.
use crate::*;

/// Stable metadata are fixed OID files: the installed Windows helper batches them; other platforms
/// use Git's ref resolver. Neither path grants a new host filesystem root or writes Git objects.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Marker {
    Merge,
    CherryPick,
    RebaseStep,
    RebaseOriginal,
    RebaseOnto,
    ApplyOriginal,
    ApplyOnto,
    SequencerOriginal,
}
impl Marker {
    pub(crate) const ALL: [Self; 8] = [
        Self::Merge,
        Self::CherryPick,
        Self::RebaseStep,
        Self::RebaseOriginal,
        Self::RebaseOnto,
        Self::ApplyOriginal,
        Self::ApplyOnto,
        Self::SequencerOriginal,
    ];
    pub(crate) fn reference(self) -> &'static str {
        operation_observation::MARKERS[self as usize]
    }
}

/// Return Git's current-worktree ref prefix without granting filesystem access. Ordinary metadata
/// uses an empty prefix; linked metadata must be an exact single child of common/worktrees.
/// Invalid/relative paths or an unsafe ref component are errors, never an unscoped fallback.
pub(crate) fn worktree_prefix(git_dir: &str, common_dir: &str) -> Result<String, &'static str> {
    let normalize = |value: &str| {
        let value = value.replace('\\', "/");
        let bytes = value.as_bytes();
        let absolute = value.starts_with('/')
            || (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && bytes[2] == b'/');
        if !absolute
            || value.chars().any(char::is_control)
            || value.split('/').any(|part| matches!(part, "." | ".."))
        {
            return Err("Invalid absolute Git metadata path");
        }
        Ok(value.trim_end_matches('/').to_owned())
    };
    let git_dir = normalize(git_dir)?;
    let common_dir = normalize(common_dir)?;
    if git_dir == common_dir {
        return Ok(String::new());
    }
    let base = format!("{common_dir}/worktrees/");
    let id = git_dir
        .strip_prefix(&base)
        .ok_or("Git worktree metadata is outside its common worktrees directory")?;
    if id.is_empty()
        || matches!(id, "." | "..")
        || id
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err("Invalid Git worktree ref component");
    }
    Ok(format!("worktrees/{id}/"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Scope {
    Merge(String),
    CherryPick(String),
    Sequence(String),
    Rebase {
        apply: bool,
        original: String,
        onto: String,
    },
    /// An unmarked stash/index conflict lasts until one complete observation has no unmerged paths.
    Index(u64),
}
struct Round {
    id: u64,
    status: Option<Vec<Change>>,
    status_received: bool,
    values: [Option<String>; 8],
    received: u16,
    failed: bool,
    /// An optional read can yield to input before publication without revoking the last sample.
    previous_ready: bool,
}

/// Half-complete refreshes never pretend that an operation ended. Old proposal buttons are sealed
/// immediately, then reopened only after every bounded read agrees with a complete status sample.
#[derive(Default)]
pub(crate) struct Lifecycle {
    next: u64,
    pending: Option<Round>,
    scope: Option<Scope>,
    ready: bool,
}
pub(crate) struct Publication {
    pub(crate) changes: Vec<Change>,
    pub(crate) changed: bool,
    pub(crate) operation: Option<String>,
    pub(crate) oid: Option<String>,
}
impl Lifecycle {
    pub(crate) fn begin(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        let previous_ready = self.ready;
        self.ready = false;
        self.pending = Some(Round {
            id: self.next,
            status: None,
            status_received: false,
            values: Default::default(),
            received: 0,
            failed: false,
            previous_ready,
        });
        self.next
    }
    pub(crate) fn ready(&self) -> bool {
        self.ready
    }
    /// A known failed partial sample cannot be cancelled back into a previously valid proposal.
    /// The remaining reads still complete through the ordinary atomic unknown-state publication.
    pub(crate) fn pending_failed(&self) -> bool {
        self.pending.as_ref().is_some_and(|round| round.failed)
    }
    /// Cancel only the incomplete observation; published scope and monotonic round IDs survive.
    pub(crate) fn cancel_observation(&mut self) {
        if let Some(round) = self.pending.take() {
            self.ready = round.previous_ready && !round.failed;
        }
    }
    pub(crate) fn status(
        &mut self,
        id: u64,
        status: Option<Vec<Change>>,
    ) -> Option<Result<Publication, ()>> {
        let round = self.pending.as_mut().filter(|round| round.id == id)?;
        round.status_received = true;
        round.failed |= status.is_none();
        round.status = status;
        self.finish()
    }
    /// Validate the complete batch before publishing any marker. Wrong names, malformed OIDs,
    /// truncated/extra replies and command failures must never look like an ended operation.
    pub(crate) fn probes(
        &mut self,
        id: u64,
        prefix: &str,
        bytes: &[u8],
        code: u32,
    ) -> Option<Result<Publication, ()>> {
        let lines = std::str::from_utf8(bytes)
            .ok()
            .map(|text| text.lines().collect::<Vec<_>>());
        let valid = code == 0
            && lines.as_ref().is_some_and(|lines| {
                lines.len() == Marker::ALL.len()
                    && Marker::ALL.iter().zip(lines).all(|(marker, line)| {
                        *line == format!("{prefix}{} missing", marker.reference())
                            || (matches!(line.len(), 40 | 64)
                                && line.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    })
            });
        let mut publication = None;
        for (index, marker) in Marker::ALL.into_iter().enumerate() {
            let (value, code) = if valid {
                let value = lines.as_ref().unwrap()[index];
                (value.as_bytes(), u32::from(value.ends_with(" missing")))
            } else {
                (b"".as_slice(), 2)
            };
            publication = self.probe(id, marker, value, code);
        }
        publication
    }
    pub(crate) fn probe(
        &mut self,
        id: u64,
        marker: Marker,
        bytes: &[u8],
        code: u32,
    ) -> Option<Result<Publication, ()>> {
        let round = self.pending.as_mut().filter(|round| round.id == id)?;
        let index = marker as usize;
        round.received |= 1 << index;
        round.values[index] = match code {
            1 => None,
            0 => match std::str::from_utf8(bytes)
                .ok()
                .map(str::trim)
                .filter(|oid| {
                    matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) {
                Some(oid) => Some(oid.into()),
                None => {
                    round.failed = true;
                    None
                }
            },
            _ => {
                round.failed = true;
                None
            }
        };
        self.finish()
    }
    fn finish(&mut self) -> Option<Result<Publication, ()>> {
        if !self
            .pending
            .as_ref()
            .is_some_and(|round| round.status_received && round.received == 255)
        {
            return None;
        }
        let round = self.pending.take().unwrap();
        if round.failed {
            return Some(Err(()));
        }
        let changes = round.status.unwrap();
        let v = &round.values;
        // Partial stable identity or two backends are unknown, so old proposals remain sealed.
        // Git can retain REBASE_HEAD after success; only orig-head/onto own the rebase lifetime.
        let pair = |a: usize, b: usize| match (&v[a], &v[b]) {
            (Some(a), Some(b)) => Ok(Some((a.clone(), b.clone()))),
            (None, None) => Ok(None),
            _ => Err(()),
        };
        let resolved = (|| {
            let merge = pair(Marker::RebaseOriginal as usize, Marker::RebaseOnto as usize)?;
            let apply = pair(Marker::ApplyOriginal as usize, Marker::ApplyOnto as usize)?;
            if merge.is_some() && apply.is_some() {
                return Err(());
            }
            let scope = if let Some((original, onto)) = merge {
                Some(Scope::Rebase {
                    apply: false,
                    original,
                    onto,
                })
            } else if let Some((original, onto)) = apply {
                Some(Scope::Rebase {
                    apply: true,
                    original,
                    onto,
                })
            } else if let Some(original) = &v[Marker::SequencerOriginal as usize] {
                Some(Scope::Sequence(original.clone()))
            } else if let Some(oid) = &v[Marker::CherryPick as usize] {
                Some(Scope::CherryPick(oid.clone()))
            } else if let Some(oid) = &v[Marker::Merge as usize] {
                Some(Scope::Merge(oid.clone()))
            } else if changes.iter().any(|change| change.conflict) {
                Some(match &self.scope {
                    Some(scope @ Scope::Index(_)) => scope.clone(),
                    _ => Scope::Index(round.id),
                })
            } else {
                None
            };
            Ok(scope)
        })();
        let scope = match resolved {
            Ok(scope) => scope,
            Err(()) => return Some(Err(())),
        };
        let (operation, oid) = match &scope {
            Some(Scope::Rebase { onto, .. }) => (
                Some("rebase".into()),
                v[Marker::RebaseStep as usize]
                    .clone()
                    .or_else(|| Some(onto.clone())),
            ),
            Some(Scope::Sequence(original)) => (
                Some("cherry-pick".into()),
                v[Marker::CherryPick as usize]
                    .clone()
                    .or_else(|| Some(original.clone())),
            ),
            Some(Scope::CherryPick(oid)) => (Some("cherry-pick".into()), Some(oid.clone())),
            Some(Scope::Merge(oid)) => (Some("merge".into()), Some(oid.clone())),
            _ => (None, None),
        };
        let changed = self.scope != scope;
        self.scope = scope;
        self.ready = true;
        Some(Ok(Publication {
            changes,
            changed,
            operation,
            oid,
        }))
    }
}

impl State {
    /// This is the sole publication point: retire only plugin proposals, then reconcile current
    /// index stages. Native document entities and their draft/IME/Undo lifetime are never closed.
    pub(crate) fn publish_conflict_observation(
        &mut self,
        publication: Option<Result<Publication, ()>>,
    ) -> Result<(), api::Failure> {
        // Even before the complete identity is known, an observed failure withdraws silence
        // and preemption. Input must not restore old proposal authority by cancelling this round.
        if self.conflict_lifecycle.pending_failed() {
            self.automatic_observation = false;
            self.background_read = false;
        }
        let Some(publication) = publication else {
            return Ok(());
        };
        let publication = match publication {
            Ok(publication) => publication,
            Err(()) => {
                self.automatic_observation = false;
                self.background_read = false;
                self.operation = None;
                self.operation_oid = None;
                for file in self.merge_files.values_mut() {
                    file.proposals_suspended = true;
                    file.read_valid = false;
                }
                self.report(Severity::Warning, "无法确认 Git 操作身份，冲突建议已暂停，请刷新 / Cannot verify Git operation identity; conflict proposals paused; refresh".into());
                return Ok(());
            }
        };
        // Polling identical status is not a new tree source. Preserve its loaded pages and
        // native expansion; only changed status/operation authority can revoke these trees.
        let trees_changed = publication.changed || self.changes != publication.changes;
        if trees_changed || self.operation_oid != publication.oid {
            self.automatic_observation = false;
        }
        let selected = self.merge_selected.clone();
        if publication.changed {
            self.merge_files.clear();
            self.merge_selected.clear();
        }
        self.changes = publication.changes;
        self.retain_commit_selection();
        self.operation = publication.operation;
        self.operation_oid = publication.oid;
        let stale = self
            .merge_files
            .iter()
            .filter_map(|(path, file)| {
                self.changes
                    .iter()
                    .find(|change| change.conflict && &change.path == path)
                    .filter(|change| file.resolved || change.index_entry != file.index_entry)
                    .map(|_| path.clone())
            })
            .collect::<Vec<_>>();
        for path in &stale {
            self.merge_files.remove(path);
        }
        for (path, file) in &mut self.merge_files {
            file.resolved = !self
                .changes
                .iter()
                .any(|change| change.conflict && &change.path == path);
            file.proposals_suspended = false;
            if file.resolved {
                file.read_valid = false;
            }
        }
        if !selected.is_empty()
            && self
                .changes
                .iter()
                .any(|change| change.conflict && change.path == selected)
            && (publication.changed || stale.contains(&selected))
        {
            self.open_conflict(&selected)?;
        }
        // An unchanged sample can reuse a still-current read. A native change received while
        // sampling instead needs one fresh exact read; this must not reopen a hidden work window.
        let reread = self
            .merge_files
            .get(&selected)
            .filter(|file| {
                !file.resolved && !file.opaque && !file.read_valid && !file.read_inflight
            })
            .and_then(|file| file.document.clone());
        if let Some(document) = reread {
            self.merge_files.get_mut(&selected).unwrap().read_inflight = true;
            self.editor(
                api::EditorOperation::ReadDocument {
                    document,
                    range: None,
                },
                &format!("merge-read:{selected}"),
            )?;
        }
        if trees_changed {
            for name in ["staging", "conflicts"] {
                self.reconcile_tree(name)?;
            }
        }
        if self.operation.as_deref() != Some("rebase")
            && self.interactive_plan.is_some()
            && self.pending.is_none()
            && self.deferred_operation.is_none()
            && self.rebase_draft.is_none()
        {
            self.clear_rebase_plan()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod tests;
