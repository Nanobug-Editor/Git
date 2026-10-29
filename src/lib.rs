//! Git policy lives in this independent WASM guest; native tools, UI and documents remain host-owned.
mod branches;
mod commit_panel;
mod comparisons;
mod completion;
mod conflicts;
mod diagnostic;
mod events;
mod feedback;
mod graph;
mod history;
#[cfg(test)]
mod history_dispatch_tests;
mod history_preview;
mod lifecycle;
mod merge;
mod model;
mod observation;
mod operation_observation;
mod operation_preview;
mod operations;
mod panel_activity;
mod patch;
mod processes;
mod rebase;
mod rebase_model;
mod repository;
#[cfg(test)]
mod scene_receipt_tests;
#[cfg(test)]
mod tree_request_tests;
mod trees;
mod views;
mod workspace_ui;
mod writes;
use diagnostic::safe_diagnostic;
use model::{Change, Commit};
use plugin_protocol::interaction::Severity;
use plugin_protocol::{
    Environment, api,
    bindings::{Guest, export},
    tree, ui,
};
use processes::{Command, Purpose, Running};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
};

/// Background reads are at least thirty seconds apart; explicit user and save requests stay immediate.
const BACKGROUND_REFRESH_MS: u32 = 30_000;

/// Derived repository views never become a second editable document or Undo stack.
#[derive(Default)]
struct State {
    /// Workspace presentation and recent checkout metadata stay independent of history selection.
    workspace_ui: workspace_ui::WorkspaceUi,
    environment: Environment,
    /// The group uniquely owns background resources; the identities below only match receipts.
    background_resources: api::guest::ResourceGroup,
    /// Only this repeating timer may request a periodic refresh.
    refresh_timer: Option<api::ResourceHandle>,
    /// Document closure remains observed while all Git panels are hidden.
    document_subscription: Option<api::ResourceHandle>,
    changes: Vec<Change>,
    commits: Vec<Commit>,
    /// Admission index deduplicates only arriving rows, without rebuilding a set for the whole log.
    history_oids: std::collections::BTreeSet<String>,
    refs: Vec<String>,
    /// Tracking targets come from the same local ref observation; no automatic network fetch.
    ref_upstreams: BTreeMap<String, String>,
    queue: VecDeque<Command>,
    running: Option<Running>,
    trees: BTreeMap<String, api::ResourceHandle>,
    revision: u64,
    /// Last admitted document per surface; unrelated interactions retain their input authority.
    published_views: BTreeMap<String, ui::Document>,
    /// A comparison in one surface cannot retire another surface's pending native source.
    pending_comparisons: VecDeque<panel_activity::ComparisonSelection>,
    /// Passive visibility/focus/save observations coalesce without taking over the active panel.
    passive_refresh_pending: bool,
    message: String,
    /// Typed host delivery is independent from business queries and retained UI scenes.
    feedback: feedback::Feedback,
    /// Last historical operation status remains internal; the host presents its diagnostic.
    history_notice: String,
    commit_message: String,
    /// Submission errors retain operation state while the host presents their diagnostic.
    commit_submit_error: Option<String>,
    argument: String,
    selected: String,
    /// The readonly detail source survives log pagination, filtering and later ref observations.
    history_selection: Option<Commit>,
    history_files: Vec<String>,
    /// Immutable path states for the selected commit, including the union of merge-parent changes.
    history_file_status: BTreeMap<String, model::HistoryChange>,
    /// The full message belongs to the selected immutable commit, not to the moving log filter.
    history_message: String,
    /// Last comparison failure is retained for lifecycle decisions; presentation belongs to the host.
    history_error: String,
    history_loading: bool,
    columns: Vec<String>,
    snapshot_targets: Vec<String>,
    filter: Option<String>,
    offset: usize,
    /// A refresh replaces only the loaded log window after its bounded pages complete.
    history_refresh: Option<history::Refresh>,
    /// A short successful page marks this observed filter's end; explicit refresh can reopen it.
    history_exhausted: bool,
    /// Current viewport demand survives a queued read/write without replaying its native event.
    history_extend_pending: bool,
    /// Last fully decoded commit shown beside its retained file tree while a new query runs.
    history_preview_commit: Option<Commit>,
    /// File metadata waits for the same OID's complete message before the preview is published.
    pending_history_files: Option<BTreeMap<String, model::HistoryChange>>,
    /// The minimum background interval starts after completion, including delayed timer delivery.
    history_refreshed_at: Option<std::time::Instant>,
    /// Explicit workbench input and the last busy-to-idle transition each start a complete quiet
    /// interval. Passive layout/resource receipts cannot keep postponing periodic observation.
    refresh_quiet_at: Option<std::time::Instant>,
    /// Only explicit focus/save/manual requests coalesce here; timer skips never request a retry.
    explicit_refresh_pending: bool,
    /// Optional local reads keep the admitted native scene until data changes; user input can
    /// cancel these read-only commands without queuing or replaying an old UI envelope.
    automatic_observation: bool,
    /// Optional Git reads remain preemptible after changed data publishes; they do not own UI busy state.
    background_read: bool,
    log_scroll: f32,
    /// Derived commit topology and vector tiles; content identity outlives drawing-only frames.
    history_drawing: history::HistoryDrawing,
    /// Host-persisted user colors are reapplied before activation and never enter guest snapshots.
    history_palette: history::Palette,
    /// Workspace-local bounded log/preview snapshots; filtering never discards an admitted canvas.
    history_cache: history::Cache,
    /// Ref targets invalidate history snapshots even when branch names remain unchanged.
    ref_tips: BTreeMap<String, String>,
    canvas_width: f32,
    canvas_height: f32,
    /// Four history text columns own layout only; resizing never changes commit or scroll identity.
    history_columns: history::Columns,
    pending: Option<Command>,
    selected_path: String,
    result: Option<api::DocumentVersion>,
    editor_tasks: BTreeMap<u64, String>,
    conflict_mode: bool,
    diff_mode: Option<bool>,
    left_version: Option<api::DocumentVersion>,
    right_version: Option<api::DocumentVersion>,
    third_version: Option<api::DocumentVersion>,
    snapshot_ready: usize,
    diff_source_requested: bool,
    diff_columns: Vec<String>,
    /// A captured historical file may have three readonly sources; worktree/index comparisons have two.
    diff_history: bool,
    /// Pending history navigation is frozen before reads and transferred only on native admission.
    diff_navigation: Option<comparisons::HistoryNavigation>,
    diff_title: String,
    diff_path: String,
    history_path: String,
    history_parents: [usize; 2],
    merge_files: BTreeMap<String, MergeFile>,
    merge_selected: String,
    /// Publish one complete Git status/operation observation before retiring conflict proposals.
    conflict_lifecycle: conflicts::lifecycle::Lifecycle,
    stage_intent: Option<(String, Command)>,
    /// Submit freezes selected paths and its guarded command until native dirty-buffer inspection.
    commit_stage_intent: Option<(Vec<String>, Command)>,
    /// Explicit expand-all continues through ordinary lazy pages; collapsing ends that intent.
    commit_expand_all: bool,
    /// Top-level groups start open once, without overriding later native directory choices.
    commit_tree_initialized: bool,
    /// Only the first history tree reply seeds its total open; native disclosure stays host-owned.
    history_files_tree_initialized: bool,
    /// Hunk actions use a bounded popup, keeping the commit form stationary.
    commit_hunks_open: bool,
    /// Tracked and unversioned inclusion is local to this workspace, independent of Git's index.
    commit_selected: std::collections::BTreeSet<String>,
    /// One frozen submission remains exclusive through validation, commit and optional push.
    commit_submitting: bool,
    patch: patch::Patch,
    operation: Option<String>,
    operation_oid: Option<String>,
    current_ref: String,
    /// Seed the native branch location once; later user choices remain owned by the native tree.
    branch_initialized: bool,
    last_fetch: Option<u64>,
    pending_panel: String,
    /// A context prompt retains only operation input and its immutable source ref.
    branch_prompt: Option<branches::Prompt>,
    /// Menu applicability updates cached node metadata without invalidating provider pages.
    branch_menu_stamp: Option<branches::MenuStamp>,
    /// A node's published kind and path stay together through paging and later native selection.
    tree_paths: BTreeMap<String, trees::TreePath>,
    /// Exact accepted lazy prefixes provide the baseline for structural deltas, never native UI state.
    tree_pages: BTreeMap<String, BTreeMap<Option<String>, trees::incremental::CachedBranch>>,
    /// An explicit dirty-worktree choice precedes branch-changing operations.
    pending_dirty: bool,
    /// Dirty native-buffer refusal revokes approval; a rebuilt preview needs explicit confirmation.
    pending_recheck: bool,
    deferred_operation: Option<Command>,
    selected_commits: Vec<String>,
    history_anchor: Option<String>,
    /// Keyboard cursor belongs to the log, independently of the commit displayed in its window.
    history_cursor: String,
    head: Option<String>,
    diff_key: String,
    /// Cached versions identify immutable owned sources, never their editable native value.
    diff_views: BTreeMap<String, DiffSources>,
    /// Immutable historical text is bounded and separate from editable worktree/index documents.
    history_sources: comparisons::HistorySources,
    /// At most two neighbors wait outside the business queue and yield to every explicit action.
    history_prefetch: VecDeque<comparisons::HistoryPrefetch>,
    /// One comparison intent owns only its pending requests and newly allocated readonly sources.
    diff_flight: Option<comparisons::Flight>,
    rebase_draft: Option<rebase_model::Plan>,
    interactive_plan: Option<rebase_model::Plan>,
    private_data: Option<api::ResourceHandle>,
    write_intent: Option<Command>,
    repository: repository::Repository,
    visible_panels: BTreeMap<String, bool>,
    operation_preview: operation_preview::PreviewState,
    input_epochs: BTreeMap<String, u64>,
}
#[derive(Clone, Default)]
struct DiffSources {
    left: Option<api::DocumentVersion>,
    right: Option<api::DocumentVersion>,
    third: Option<api::DocumentVersion>,
    /// Source closure evicts this immutable navigation context with the retained native view.
    navigation: Option<comparisons::HistoryNavigation>,
}
/// Editable text remains in the host. Per-file results retain only versions and immutable Git sources.
#[derive(Default)]
struct MergeFile {
    columns: Vec<String>,
    document: Option<api::DocumentVersion>,
    blocks: Vec<merge::Block>,
    read_inflight: bool,
    read_valid: bool,
    /// Sampling/unknown operation authority withdraws proposals without touching native text/Undo.
    proposals_suspended: bool,
    has_markers: bool,
    index_entry: Vec<u8>,
    sources_pending: usize,
    /// Complete text is optional; whole-version operations remain available for opaque/large files.
    opaque: bool,
    regions: Vec<merge::plan::Region>,
    reviewed: BTreeMap<usize, u64>,
    decisions: BTreeMap<usize, (u64, [u8; 2])>,
    pending_review: Option<(usize, u64)>,
    nonconflicting: Vec<api::TextEdit>,
    result_len: usize,
    /// Alternatives follow the native result's line-ending convention without rewriting its text.
    result_eol: merge::Eol,
    resolved: bool,
    show_base: bool,
}
thread_local! {static STATE:RefCell<State>=RefCell::new(State::default());}
struct GitPlugin;
impl Guest for GitPlugin {
    /// Each event is correlated to this incarnation; late resource notifications are ignored.
    fn dispatch(payload: String) -> Result<String, String> {
        dispatch_with(
            payload,
            Command::launch,
            plugin_protocol::interaction::start,
        )
    }
}

/// Keep process transport at the dispatcher boundary while every receipt uses the same state
/// transitions and publication filtering. Native unit tests substitute only the launch transport.
fn dispatch_with(
    payload: String,
    mut launch: impl FnMut(&Command, &str) -> Result<Running, api::Failure>,
    mut notify: impl FnMut(
        plugin_protocol::interaction::Operation,
        u32,
    ) -> Result<api::ResourceHandle, api::Failure>,
) -> Result<String, String> {
    api::guest::dispatch(&payload, |input| {
        STATE.with(|cell| {
                let mut state = cell.borrow_mut();
                // Configuration has its own pure validation/hot repaint path. It must not cancel
                // an automatic Git read, pump queued commands or republish the commit form.
                if let api::Input::Event { event: api::Notification::Configuration { phase, values }, .. } = &input {
                    return state.configure_palette(*phase, values);
                }
                if let api::Input::Event { event, .. } = &input {
                    if state.observe_feedback(event) {
                        if let Err(error) = state.flush_feedback(&mut notify) {
                            state.feedback_failed(&error);
                        }
                        return Ok(api::Output::default());
                    }
                    // Tree/canvas routes bypass State::event, but every helper sees its receipt first.
                    state.background_resources.observe(event);
                    if !state.matches_background_resource(event) {
                        // Reject foreign receipts before they can preempt an owned Git query.
                        return Ok(api::Output::default());
                    }
                }
                let initializing = matches!(&input, api::Input::Prepare { .. } | api::Input::Activate);
                let was_busy = state.busy();
                let was_history_loading = state.history_loading;
                let feedback_owner = state.feedback_owner(&input);
                let previous_message = state.message.clone();
                let comparison_only = state.history_comparison_event(&input);
                let previous_history_error = state.history_error.clone();
                let previous_submit_error = state.commit_submit_error.clone();
                // Idle range movement cannot change sibling controls or comparison sources.
                // Query/error/busy transitions still take the ordinary complete publication path.
                let range_only = !state.ui_busy() && !state.explicit_refresh_pending && matches!(&input,
                    api::Input::Event { panel: Some(panel), event: api::Notification::Ui(ui::UiEvent {
                        node, action: ui::Action::Canvas(ui::CanvasEvent::ScrollInput { .. }
                            | ui::CanvasEvent::Wheel { .. } | ui::CanvasEvent::Scroll { .. }
                            | ui::CanvasEvent::Resize { .. } | ui::CanvasEvent::ColumnsResized { .. }), ..
                    }) } if panel == "history" && node == "log");
                let range_only = range_only || (state.history_columns.dragging() && matches!(&input,
                    api::Input::Event { panel: Some(panel), event: api::Notification::Ui(ui::UiEvent {
                        node, action: ui::Action::Canvas(ui::CanvasEvent::Pointer { .. }), ..
                    }) } if panel == "history" && node == "log"));
                // Filter selection and its pure readonly receipts change only the history surface.
                // Keeping sibling scenes admitted also preserves staging input through a cold read.
                let filter_only = !state.ui_busy() && matches!(&input,
                    api::Input::Event { event: api::Notification::Ui(ui::UiEvent {
                        action: ui::Action::Tree(tree::Event::Selected { tree, .. }), ..
                    }), .. } if state.trees.get("branches") == Some(tree));
                let history_receipt = state.history_reads_only() && matches!(&input,
                    api::Input::Event { event: api::Notification::Process { .. }, .. });
                let background_event = observation::is_background_event(&input);
                if observation::interrupts(&input) {
                    // A failed optional close keeps the handle owned until exit or another input.
                    let _ = state.cancel_history_prefetch(api::guest::close_resource);
                    state.cancel_automatic_observation()?;
                }
                if state.is_user_activity(&input) {
                    state.refresh_quiet_at = Some(std::time::Instant::now());
                }
                // Capture both outcomes before consuming input, without cloning process output.
                let repaint = repaint_after(&input, false);
                let repaint_on_error = repaint_after(&input, true);
                // Resource-only selection/Canvas receipts can share a scene with pending
                // input and tree pages. Their handlers decide whether business state changed.
                let mut event_repaint = None;
                let result = match input {
                        api::Input::Prepare { environment, snapshot, .. } => {
                        // A failed release retains its owner in the old State for an explicit retry.
                        state.close_background_resources()?;
                        *state = State {
                            environment,
                            ..Default::default()
                        };
                            if let Some(snapshot)=snapshot {
                                if snapshot.schema!=1{return Err(failure("Unsupported Git snapshot schema"))}
                                let saved:serde_json::Value=serde_json::from_str(&snapshot.data).map_err(|_|failure("Invalid Git snapshot"))?;
                                state.commit_message=saved.get("commit_message").and_then(|value|value.as_str()).filter(|value|value.len()<=65536).unwrap_or_default().into();
                                state.filter=saved.get("filter").and_then(|value|value.as_str()).filter(|value|value.starts_with("refs/")&&!value.contains(['\0','\n','\r'])).map(str::to_owned);
                                state.history_columns.restore(saved.get("history_columns"));
                            }
                        Ok(())
                    }
                    api::Input::Activate => state.activate(),
                    api::Input::Event {
                        event: api::Notification::Ui(ui::UiEvent {
                            action: ui::Action::Tree(event), ..
                        }), ..
                    } => state.tree_event(event).map(|changed| event_repaint = Some(changed)),
                    api::Input::Event {
                        event: api::Notification::Ui(ui::UiEvent {
                            action: ui::Action::Canvas(event), ..
                        }), ..
                    } => {
                        event_repaint = Some(state.history_canvas(event));
                        Ok(())
                    }
                    api::Input::Event { panel, event } => state.event(panel.as_deref(), event)
                        .map(|changed| event_repaint = Some(repaint && changed)),
                    api::Input::Snapshot => {
                        return Ok(api::Output {
                                snapshot: Some(plugin_protocol::Snapshot{schema:1,data:serde_json::json!({"commit_message":state.commit_message,"filter":state.filter,"history_columns":state.history_columns.trailing}).to_string()}),
                            ..Default::default()
                        });
                    }
                };
                let failed = result.is_err();
                let mut repaint = if failed { repaint_on_error } else { event_repaint.unwrap_or(repaint) };
                if let Err(error) = result {
                    // The manager must reject a failed candidate instead of committing its error UI.
                    // Keep any incomplete group owner until an explicit retry or host retirement.
                    if initializing {
                        return Err(error);
                    }
                    state.automatic_observation = false;
                    state.background_read = false;
                    state.report(Severity::Error, error.to_string());
                    state.fail_panel_activity(feedback_owner, &error);
                }
                // Reading an immutable comparison cannot alter the log/tree document. Errors
                // retain ordinary publication; explicit queued observations below can still repaint.
                if comparison_only && !failed && state.history_error == previous_history_error {
                    repaint = false;
                }
                if state.resume_explicit_refresh() {
                    repaint = true;
                }
                state.resume_pending_history_filter();
                state.resume_history_page();
                state.resume_comparison_selection()?;
                state.resume_history_prefetch();
                repaint |= state.pump_with(&mut launch);
                // A synchronous launch failure can finish the last business item. Resume an
                // already requested explicit observation once; internal refreshes never set it.
                if state.resume_explicit_refresh() {
                    repaint = true;
                    repaint |= state.pump_with(&mut launch);
                }
                if was_busy && !state.busy() {
                    state.refresh_quiet_at = Some(std::time::Instant::now());
                }
                // A timer's internal busy/probe transitions do not own the user's controls.
                // Changed data and failures explicitly end the silent observation first.
                state.retain_panel_feedback(feedback_owner, &previous_message);
                // Completing the selected preview owns one required publication. A focus/save
                // received during that query may already have started the next silent read above;
                // its new observation cannot suppress the previous query's now-clickable tree.
                let preview_ready = was_history_loading && !state.history_loading && !failed
                    && state.history_preview_commit.as_ref()
                        .is_some_and(|commit| commit.oid == state.selected);
                let views = if (repaint || preview_ready)
                    && (preview_ready || !(background_event && state.automatic_observation)) {
                    if (range_only || filter_only || history_receipt) && !failed && !state.ui_busy() {
                        let views = state.history_scroll_view();
                        state.changed_views(views)
                    } else {
                        let views = state.views();
                        state.changed_views(views)
                    }
                } else {
                    Vec::new()
                };
                if !state.busy() {
                    state.automatic_observation = false;
                    state.background_read = false;
                }
                state.collect_feedback(&previous_history_error, previous_submit_error.as_deref());
                if let Err(error) = state.flush_feedback(&mut notify) {
                    state.feedback_failed(&error);
                }
                Ok(api::Output {
                    views,
                    ..Default::default()
                })
            })
    })
}
export!(GitPlugin);

/// Resource pages redraw through their shared tree snapshot, preserving sibling request authority.
/// Failed callbacks still publish their diagnostic; process chunks retain the existing batching.
fn repaint_after(input: &api::Input, failed: bool) -> bool {
    match input {
        api::Input::Event {
            event:
                api::Notification::Process {
                    update: plugin_protocol::process::Update::Output { .. },
                    ..
                },
            ..
        } => false,
        api::Input::Event {
            event:
                api::Notification::Ui(ui::UiEvent {
                    action: ui::Action::Tree(tree::Event::Request(_)),
                    ..
                }),
            ..
        } => failed,
        _ => true,
    }
}

impl State {
    /// Count real workbench intent even when a boundary key/wheel cannot move. Owned tree leaves
    /// are actions; directory restoration, resource pages and passive canvas observations are not.
    fn is_user_activity(&self, input: &api::Input) -> bool {
        match input {
            api::Input::Event {
                event: api::Notification::Command { .. } | api::Notification::CommandInvocation(_),
                ..
            } => true,
            api::Input::Event {
                event: api::Notification::Ui(ui::UiEvent { action, .. }),
                ..
            } => match action {
                ui::Action::Canvas(ui::CanvasEvent::Key {
                    key,
                    ctrl: false,
                    alt: false,
                    ..
                }) => matches!(
                    key.as_str(),
                    "up" | "down" | "home" | "end" | "pageup" | "pagedown" | "enter"
                ),
                ui::Action::Canvas(
                    ui::CanvasEvent::Wheel { .. }
                    | ui::CanvasEvent::ScrollInput { .. }
                    | ui::CanvasEvent::Pointer {
                        phase: ui::PointerPhase::Down,
                        ..
                    },
                ) => true,
                ui::Action::Canvas(_) | ui::Action::Tree(tree::Event::Request(_)) => false,
                ui::Action::Tree(tree::Event::Selected { tree, node, .. }) => self
                    .trees
                    .iter()
                    .find(|(_, handle)| *handle == tree)
                    .is_some_and(|(name, _)| {
                        matches!(
                            self.tree_paths.get(&format!("{name}:{node}")),
                            Some(trees::TreePath::Leaf(_))
                        )
                    }),
                _ => true,
            },
            _ => false,
        }
    }

    /// Replacing a comparison waits for native queries and editor acknowledgements together.
    fn busy(&self) -> bool {
        self.running.is_some()
            || !self.queue.is_empty()
            || self.merge_files.values().any(|file| file.read_inflight)
            || self
                .editor_tasks
                .values()
                .any(|intent| !matches!(intent.as_str(), "visibility" | "write-check-cancelled"))
    }
    /// Only business queues own disabled controls; current input can retire optional Git reads first.
    fn ui_busy(&self) -> bool {
        !self.history_prefetch_only()
            && !self.background_read
            && !self.history_reads_only()
            && !self.history_comparison_only()
            && self.busy()
    }
    /// Background observations respect both their minimum interval and the user's quiet interval.
    /// Explicit refresh requests use their separate coalescing entry and never wait for this gate.
    fn automatic_refresh(&mut self) -> bool {
        self.automatic_refresh_at(std::time::Instant::now())
    }
    /// Sample monotonic time once so the refresh decision has one consistent interval boundary.
    fn automatic_refresh_at(&mut self, now: std::time::Instant) -> bool {
        if self.busy()
            || self.history_refreshed_at.is_some_and(|last| {
                now.checked_duration_since(last).unwrap_or_default()
                    < std::time::Duration::from_millis(u64::from(BACKGROUND_REFRESH_MS))
            })
            || self.refresh_quiet_at.is_some_and(|last| {
                now.checked_duration_since(last).unwrap_or_default()
                    < std::time::Duration::from_secs(3)
            })
        {
            return false;
        }
        // Establish silence before enqueueing: a synchronous validation failure may revoke it.
        self.automatic_observation = true;
        self.background_read = true;
        self.refresh();
        if !self.busy() {
            self.automatic_observation = false;
            self.background_read = false;
        }
        true
    }
    /// Focus, saved documents and the manual action require a fresh local observation. While
    /// business owns the queue they share one pending bit, rather than dropping the request.
    fn request_refresh(&mut self) {
        if self.busy() {
            self.explicit_refresh_pending = true;
        } else {
            self.explicit_refresh_pending = false;
            self.refresh();
        }
    }
    /// Resume only the explicit entry's coalesced work once the existing business queue is idle.
    fn resume_explicit_refresh(&mut self) -> bool {
        if self.explicit_refresh_pending && !self.busy() {
            self.explicit_refresh_pending = false;
            self.refresh();
            true
        } else if self.passive_refresh_pending && !self.busy() {
            self.passive_refresh_pending = false;
            self.request_observation();
            false
        } else {
            false
        }
    }
    /// Full refresh is local-only; no timer/focus callback ever starts a fetch.
    fn refresh(&mut self) {
        if self.busy() {
            return;
        }
        if self.repository_discover() {
            return;
        }
        self.refresh_worktree();
        self.queue.push_back(Command::new(
            [
                "for-each-ref",
                "--format=%(HEAD)%09%(refname)%09%(objectname)%09%(upstream)",
            ],
            Purpose::Refs,
        ));
        self.queue.push_back(Command::new(
            ["rev-parse", "--verify", "--quiet", "HEAD"],
            Purpose::Head,
        ));
        if self.visible_panels.is_empty() || self.visible_panels.values().any(|visible| *visible) {
            self.refresh_log();
        }
    }
    fn queue_log(&mut self) {
        if self.history_refresh.is_none() && self.offset == 0 {
            // A changed filter owns a new viewport/range; an already open detail keeps its OID.
            // Keep the last complete log painted while the new filter's first page is read.
            self.log_scroll = 0.;
            self.selected_commits.clear();
            self.history_cursor.clear();
            self.history_anchor = None;
        }
        let (offset, count) = self
            .history_refresh
            .as_ref()
            .map_or((self.offset, 128), |refresh| {
                (refresh.read, refresh.next_count())
            });
        let mut args = vec![
            "log".into(),
            "-z".into(),
            "--format=%H%x00%P%x00%s%x00%D%x00%an%x00%aI".into(),
            "--topo-order".into(),
            format!("--max-count={count}"),
            format!("--skip={offset}"),
        ];
        if let Some(branch) = &self.filter {
            args.push(branch.clone())
        } else {
            args.push("--all".into())
        }
        args.push("--".into());
        self.queue.push_back(Command::new(
            args,
            Purpose::Log {
                generation: self.history_cache.generation,
                filter: self.filter.clone(),
            },
        ));
    }
    /// The process boundary reports synchronous errors separately from later completion receipts.
    fn pump_with(
        &mut self,
        launch: impl FnOnce(&Command, &str) -> Result<Running, api::Failure>,
    ) -> bool {
        if self.running.is_some() {
            return false;
        }
        if let Some(command) = self.queue.pop_front() {
            match launch(&command, &self.environment.workspace) {
                Ok(running) => self.running = Some(running),
                Err(error) => {
                    if matches!(command.purpose, Purpose::HistoryPrefetch(_)) {
                        // An optional miss cannot revoke queued writes or the visible comparison.
                        return false;
                    }
                    self.automatic_observation = false;
                    // Context augments this one launch failure; do not emit its raw form first.
                    let error = if matches!(command.purpose, Purpose::PushAfterCommit { .. }) {
                        failure(format!(
                            "提交已成功，无法启动推送；本地提交已保留 / Commit succeeded, push could not start; local commit retained: {}",
                            error
                        ))
                    } else {
                        error
                    };
                    self.report(Severity::Error, error.to_string());
                    if matches!(command.purpose, Purpose::PushAfterCommit { .. })
                        || command.args.first().is_some_and(|arg| arg == "commit")
                    {
                        self.commit_submit_error = Some(self.message.clone());
                    }
                    if self.commit_submitting {
                        self.commit_submit_error = Some(self.message.clone());
                    }
                    self.commit_submitting = false;
                    self.commit_stage_intent = None;
                    let _ = self.reconcile_tree("staging");
                    self.abort_diff(Some(&error));
                    self.fail_history_preview();
                    self.queue.clear();
                    self.history_refresh = None;
                    self.history_cache.pending = false;
                    self.history_extend_pending = false;
                    if matches!(command.purpose, Purpose::PushAfterCommit { .. }) {
                        self.refresh();
                    }
                    return true;
                }
            }
        }
        false
    }
    fn editor(
        &mut self,
        operation: api::EditorOperation,
        intent: &str,
    ) -> Result<(), api::Failure> {
        let new_source = matches!(operation, api::EditorOperation::OpenVirtualSource { .. });
        let handle = api::guest::editor(operation, 30_000)?;
        self.editor_tasks.insert(handle.resource, intent.into());
        if comparisons::is_diff_intent(intent) {
            self.track_diff_request(handle.clone(), new_source);
        }
        // Native recovery reads have their own receipt/version lifetime. Publish their busy
        // authority normally; cancelling an optional Git child cannot cancel these requests.
        if intent != "visibility" {
            self.automatic_observation = false;
            self.background_read = false;
        }
        Ok(())
    }
    fn show_window(&mut self, panel: &str) -> Result<(), api::Failure> {
        self.editor(
            api::EditorOperation::SetPanelVisibility {
                panel: panel.into(),
                visible: true,
            },
            "visibility",
        )
    }
}
fn failure(message: impl Into<String>) -> api::Failure {
    api::Failure::new(api::ErrorCode::OperationFailed, message)
}
