//! Git's background observation owns its timers/subscriptions independently of panels and queries.
use super::*;

impl State {
    /// Only this observer pair may affect background work; other notification kinds keep their
    /// own existing routing. Full handle equality includes the instance and workspace scope.
    pub(super) fn matches_background_resource(&self, event: &api::Notification) -> bool {
        match event {
            api::Notification::Timer { handle, .. } => self.refresh_timer.as_ref() == Some(handle),
            api::Notification::DocumentEvent { subscription, .. }
            | api::Notification::SubscriptionFailed { subscription, .. } => {
                self.document_subscription.as_ref() == Some(subscription)
            }
            _ => true,
        }
    }

    /// Start one observer pair and reuse existing data/trees on repeated activation.
    /// Failed observer acquisition closes earlier resources without hiding the acquisition error.
    pub(super) fn activate(&mut self) -> Result<(), api::Failure> {
        self.start_background_resources()?;
        self.activate_workbench().map_err(|mut error| {
            // A later data/tree failure also ends this initialization's observation. Keep the
            // original failure and any incomplete owner until retry or instance retirement.
            if let Err(cleanup) = self.close_background_resources() {
                error.message = format!("{}; {}", error.message, cleanup.message);
            }
            error
        })
    }

    /// Reuse previously acquired data/tree resources while completing workbench initialization.
    fn activate_workbench(&mut self) -> Result<(), api::Failure> {
        if self.workspace_ui.resource.is_none() {
            self.workspace_ui.resource = Some(plugin_protocol::presentation::create()?);
        }
        if self.private_data.is_none() {
            let data = api::guest::open_data()?;
            if let Ok(bytes) = api::guest::read_file(&data, "rebase-plan.json") {
                match serde_json::from_slice::<Option<rebase_model::Plan>>(&bytes) {
                    Ok(plan) => self.interactive_plan = plan,
                    Err(_) => {
                        self.message =
                            "保存的变基计划无法读取 / Saved rebase plan is invalid".into();
                    }
                }
            }
            self.private_data = Some(data);
        }
        for name in ["staging", "branches", "files", "conflicts"] {
            if self.trees.contains_key(name) {
                continue;
            }
            // Tree gesture and ownership stay independent of the background resource group.
            let handle = if name == "branches" {
                tree::create_with_options(tree::Options {
                    selection: tree::SelectionGesture::DoubleClick,
                    single_click_expand: true,
                    ..Default::default()
                })?
            } else if name == "staging" {
                tree::create_with_options(tree::Options {
                    selection: tree::SelectionGesture::SingleClick,
                    single_click_expand: true,
                    ..Default::default()
                })?
            } else if name == "files" {
                tree::create_with_options(tree::Options {
                    proportional_labels: true,
                    ..Default::default()
                })?
            } else {
                tree::create()?
            };
            self.trees.insert(name.into(), handle);
        }
        self.refresh();
        Ok(())
    }

    /// Ensure one thirty-second timer and one document subscription. Explicit reactivation may
    /// retry a partial cleanup once; ordinary notifications never resubscribe or retry cleanup.
    fn start_background_resources(&mut self) -> Result<(), api::Failure> {
        if self.refresh_timer.is_some() && self.document_subscription.is_some() {
            return Ok(());
        }
        self.close_background_resources()?;
        self.refresh_timer = Some(
            self.background_resources
                .timer(BACKGROUND_REFRESH_MS, true)?,
        );
        match self.background_resources.documents() {
            Ok(subscription) => {
                self.document_subscription = Some(subscription);
                Ok(())
            }
            Err(mut error) => {
                if let Err(cleanup) = self.close_background_resources() {
                    // Keep the original code/message and the group that still owns failed releases.
                    error.message = format!("{}; {}", error.message, cleanup.message);
                }
                Err(error)
            }
        }
    }

    /// Revoke event matching before one bounded cleanup pass. An incomplete report leaves the
    /// group in State and prevents reset/replacement until a later explicit attempt succeeds.
    pub(super) fn close_background_resources(&mut self) -> Result<(), api::Failure> {
        // Preparing a replacement State must not discard a failed speculative-process release.
        self.cancel_history_prefetch(api::guest::close_resource)?;
        self.refresh_timer = None;
        self.document_subscription = None;
        let report = self.background_resources.close_all();
        if report.is_complete() {
            Ok(())
        } else {
            Err(api::Failure::new(
                api::ErrorCode::InvalidState,
                format!(
                    "后台资源清理未完成（{}），请重试激活 / Background cleanup incomplete ({}); retry activation",
                    report.pending.len(),
                    report.pending.len()
                ),
            ))
        }
    }
}
