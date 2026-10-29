//! Comparison intent cleanup distinguishes new hidden sources from previously retained native tabs.
use super::*;

/// One native request retains its exact capability and whether it allocates a hidden source.
struct Request {
    /// Exact issued request capability, retained for peer cancellation rather than reconstructed.
    handle: api::ResourceHandle,
    /// Only OpenVirtualSource allocates a provisional readonly source on successful completion.
    new_source: bool,
}

/// Sources become native-view-owned only after the final comparison receipt succeeds.
pub(crate) struct Flight {
    /// Immutable key captured before any asynchronous Git or native editor work.
    key: String,
    /// Errors for historical comparisons remain visible in their inline commit preview.
    history: bool,
    /// Existing sources stay open on failure; successful refreshes advance these saved versions.
    previous: Option<DiffSources>,
    /// Only allocations from this intent may be revoked when the comparison never opens.
    created: BTreeMap<u64, api::ResourceHandle>,
    /// Worktree preparations borrow editable native sessions; release them once a view takes over.
    local_sources: BTreeMap<String, api::DocumentVersion>,
    /// Removing every peer correlation prevents late native receipts from opening another tab.
    requests: BTreeMap<u64, Request>,
}

/// These request intents share the one serialized comparison lifecycle, independently of Git kind.
pub(crate) fn is_diff_intent(intent: &str) -> bool {
    matches!(
        intent,
        "diff-open" | "diff-left" | "diff-right" | "diff-third" | "compare"
    )
}

impl State {
    /// A terminal metadata gap removes reuse authority, while existing native tabs keep their
    /// own resources and frozen navigation. A failed replacement must not restore those versions.
    pub(crate) fn forget_unobserved_diff_sources(&mut self) {
        let clear = |sources: &mut DiffSources| {
            sources.left = None;
            sources.right = None;
            sources.third = None;
        };
        for sources in self.diff_views.values_mut() {
            clear(sources);
        }
        if let Some(previous) = self
            .diff_flight
            .as_mut()
            .and_then(|flight| flight.previous.as_mut())
        {
            clear(previous);
        }
    }

    /// A user-requested comparison may reacquire its observation once after a terminal failure.
    /// Ordinary notifications never retry; failed admission remains visible to that same intent.
    pub(crate) fn ensure_diff_observation(&mut self) -> Result<(), api::Failure> {
        if self.document_subscription.is_none() {
            self.forget_unobserved_diff_sources();
            self.document_subscription = Some(self.background_resources.documents()?);
        }
        Ok(())
    }

    /// Forget only the rejected cached pane, including the rollback copy of this same flight.
    /// Native source ownership is untouched; the next operation must obtain a fresh capability.
    pub(crate) fn forget_rejected_diff_source(&mut self, column: usize) {
        let clear = |sources: &mut DiffSources| match column {
            0 => sources.left = None,
            1 => sources.right = None,
            _ => sources.third = None,
        };
        if let Some(sources) = self.diff_views.get_mut(&self.diff_key) {
            clear(sources);
        }
        if let Some(previous) = self
            .diff_flight
            .as_mut()
            .and_then(|flight| flight.previous.as_mut())
        {
            clear(previous);
        }
    }

    /// Only a rejected refresh can be renewed. Its replacement is tracked as a new allocation,
    /// so another failure cannot retry again or bypass the host's ordinary ownership admission.
    pub(crate) fn renew_diff_snapshot(
        &mut self,
        intent: &str,
        new_source: bool,
        error: &api::Failure,
    ) -> Result<bool, api::Failure> {
        if new_source || self.diff_flight.is_none() || !expired_source(error) {
            return Ok(false);
        }
        let column = match intent {
            "diff-left" => 0,
            "diff-right" => 1,
            "diff-third" => 2,
            _ => return Ok(false),
        };
        self.forget_rejected_diff_source(column);
        self.open_diff_snapshot(column)?;
        Ok(true)
    }

    /// Capture an intent's existing cache before a new hidden source can replace any entry.
    pub(crate) fn begin_diff_flight(&mut self, history: bool) {
        self.diff_flight = Some(Flight {
            key: self.diff_key.clone(),
            history,
            previous: self.diff_views.get(&self.diff_key).cloned(),
            created: BTreeMap::new(),
            local_sources: BTreeMap::new(),
            requests: BTreeMap::new(),
        });
    }

    /// Store issued capabilities rather than reconstructing their instance or scope at cancellation.
    pub(crate) fn track_diff_request(&mut self, handle: api::ResourceHandle, new_source: bool) {
        if let Some(flight) = &mut self.diff_flight {
            flight
                .requests
                .insert(handle.resource, Request { handle, new_source });
        }
    }

    /// A terminal receipt removes only its own pending request and reports allocation provenance.
    pub(crate) fn take_diff_request(&mut self, resource: u64) -> bool {
        self.diff_flight
            .as_mut()
            .and_then(|flight| flight.requests.remove(&resource))
            .is_some_and(|request| request.new_source)
    }

    /// New allocations remain provisional; refreshed old sources retain their updated exact revision.
    pub(crate) fn observe_diff_source(&mut self, info: &api::DocumentInfo, new_source: bool) {
        let Some(flight) = &mut self.diff_flight else {
            return;
        };
        if matches!(info.resource, api::ResourceIdentity::Local { .. }) {
            flight
                .local_sources
                .insert(info.document.id.clone(), info.document.clone());
        }
        if new_source {
            if let api::ResourceIdentity::Virtual { handle } = &info.resource {
                flight.created.insert(handle.resource, handle.clone());
            }
        } else if let Some(previous) = &mut flight.previous {
            for source in [&mut previous.left, &mut previous.right, &mut previous.third] {
                if source
                    .as_ref()
                    .is_some_and(|source| source.id == info.document.id)
                {
                    *source = Some(info.document.clone());
                }
            }
        }
    }

    /// A closed original source cannot be restored into the cache after another request fails.
    pub(crate) fn forget_closed_diff_source(&mut self, document: &api::DocumentVersion) {
        if let Some(flight) = &mut self.diff_flight
            && flight.previous.as_ref().is_some_and(|sources| {
                [&sources.left, &sources.right, &sources.third]
                    .into_iter()
                    .any(|source| {
                        source
                            .as_ref()
                            .is_some_and(|source| source.id == document.id)
                    })
            })
        {
            flight.previous = None;
        }
    }

    /// Successful native admission transfers source cleanup to its retained view descriptor.
    pub(crate) fn complete_diff_flight(&mut self) {
        // Reveal follows native success. A tree failure cannot roll back sources already owned
        // by the visible comparison, but remains visible as a separate history diagnostic.
        if let Err(error) = self.reveal_completed_history_file() {
            self.history_error = error.to_string();
        }
        if let Some(sources) = self.diff_views.get_mut(&self.diff_key) {
            sources.navigation = self.diff_navigation.take();
        }
        if let Some(flight) = self.diff_flight.take() {
            self.release_diff_local_sources(flight.local_sources.into_values());
        }
        self.diff_history = false;
    }

    /// Return only this intent's disposable capabilities, clearing all late-completion authority.
    /// Existing tabs and refreshed old sources survive; errors are visible beside historical intent.
    pub(crate) fn take_diff_cleanup(
        &mut self,
        error: Option<&api::Failure>,
    ) -> Vec<api::ResourceHandle> {
        let Some(mut flight) = self.diff_flight.take() else {
            return Vec::new();
        };
        if let Some(error) = error {
            if flight.history {
                self.history_error = error.to_string();
            }
            if matches!(
                error.code,
                api::ErrorCode::InvalidHandle
                    | api::ErrorCode::StaleRevision
                    | api::ErrorCode::NotFound
            ) {
                // A later retry can open fresh sources, while the old native tab remains untouched.
                flight.previous = None;
            }
        }
        if let Some(previous) = flight.previous {
            self.diff_views.insert(flight.key, previous);
        } else {
            self.diff_views.remove(&flight.key);
        }
        self.left_version = None;
        self.right_version = None;
        self.third_version = None;
        self.diff_mode = None;
        self.diff_history = false;
        self.diff_source_requested = false;
        self.diff_navigation = None;
        self.snapshot_ready = 0;
        // A comparison owns only its blob/patch chain; sibling history and index work survives.
        self.queue.retain(|command| {
            !matches!(
                command.purpose,
                Purpose::DiffSnapshot { .. }
                    | Purpose::HistoryBlob { .. }
                    | Purpose::HistoryBatch { .. }
                    | Purpose::Snapshot { .. }
                    | Purpose::Patch { .. }
            )
        });
        let mut cleanup = flight.created;
        for (id, request) in flight.requests {
            self.editor_tasks.remove(&id);
            cleanup.insert(id, request.handle);
        }
        cleanup.into_values().collect()
    }

    /// Retire real owned capabilities after the pure state transition has revoked every correlation.
    pub(crate) fn abort_diff(&mut self, error: Option<&api::Failure>) {
        let local_sources = self
            .diff_flight
            .as_mut()
            .map(|flight| std::mem::take(&mut flight.local_sources))
            .unwrap_or_default();
        for handle in self.take_diff_cleanup(error) {
            let _ = api::guest::close_resource(handle);
        }
        self.release_diff_local_sources(local_sources.into_values());
    }

    /// Dropping the preparation never closes a comparison's borrowed source or a user-opened tab.
    fn release_diff_local_sources(
        &mut self,
        documents: impl Iterator<Item = api::DocumentVersion>,
    ) {
        for document in documents {
            let _ = self.editor(
                api::EditorOperation::ReleaseDocumentSource { document },
                "release-source",
            );
        }
    }
}

/// A cached document may outlive its close notification; unrelated operational failures are final.
pub(super) fn expired_source(error: &api::Failure) -> bool {
    matches!(
        error.code,
        api::ErrorCode::PermissionDenied
            | api::ErrorCode::InvalidHandle
            | api::ErrorCode::StaleRevision
            | api::ErrorCode::NotFound
    )
}
